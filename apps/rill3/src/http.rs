use std::{sync::Arc, time::Duration};

use anyhow::Result;
use askama::Template;
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Body,
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use chrono::Utc;
use rill3_domain::{
    Creator, ExternalChannel, LiveSession, LiveState, ProviderEvent, ProviderKind,
    StateApplyOutcome, effective_live_status,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

use crate::assets::CSS;
use crate::views::{
    ChannelCard, ChannelsTemplate, CreatorTemplate, EmbedConfig, HomeTemplate, LiveCard,
    NotFoundTemplate, PagePaths, ProviderStatusView, session_is_stale,
};
use crate::watch_url::watch_url;
use crate::webhooks::WebhookConfig;

const PUBLIC_LIST_LIMIT: u32 = 60;
const HOME_CHANNEL_LIMIT: u32 = 8;
const CHANNEL_PAGE_LIMIT: u32 = 24;
const CONNECTED_CHANNEL_LIMIT: u32 = 16;
const LIVE_CACHE_CONTROL: &str = "public, max-age=10, stale-while-revalidate=60";
const CREATOR_CACHE_CONTROL: &str = "public, max-age=30, stale-while-revalidate=120";
const MANUAL_CACHE_CONTROL: &str = "public, max-age=0, must-revalidate";

#[derive(Clone, Debug)]
pub(crate) struct LiveRecord {
    pub(crate) creator: Creator,
    pub(crate) channel: ExternalChannel,
    pub(crate) session: LiveSession,
}

#[derive(Clone, Debug)]
pub(crate) struct CreatorRecord {
    pub(crate) creator: Creator,
    pub(crate) channel: ExternalChannel,
    pub(crate) session: Option<LiveSession>,
}

#[async_trait]
pub(crate) trait PublicStore: Send + Sync {
    async fn readiness(&self) -> Result<()>;
    async fn list_live(&self, limit: u32) -> Result<Vec<LiveRecord>>;
    async fn creator_by_slug(&self, slug: &str) -> Result<Option<CreatorRecord>>;
    async fn creator_channel(
        &self,
        slug: &str,
        channel: Option<Uuid>,
    ) -> Result<Option<CreatorRecord>> {
        Ok(self
            .creator_by_slug(slug)
            .await?
            .filter(|record| channel.is_none_or(|id| record.channel.id == id)))
    }
    async fn registered_channels(
        &self,
        _after: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<CreatorRecord>> {
        Ok(Vec::new())
    }
    async fn connected_channels(
        &self,
        slug: &str,
        after: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<CreatorRecord>> {
        Ok(self
            .creator_by_slug(slug)
            .await?
            .filter(|record| after.is_none_or(|id| record.channel.id > id))
            .into_iter()
            .collect())
    }
    async fn channel_by_provider_id(
        &self,
        _provider: ProviderKind,
        _provider_channel_id: &str,
    ) -> Result<Option<ExternalChannel>> {
        Ok(None)
    }
    async fn apply_provider_event(
        &self,
        _event: &ProviderEvent,
        _state: &LiveState,
    ) -> Result<StateApplyOutcome> {
        anyhow::bail!("provider event writes are unavailable")
    }
    async fn record_provider_signal(&self, _event: &ProviderEvent) -> Result<bool> {
        anyhow::bail!("provider signal writes are unavailable")
    }
}

#[derive(Clone)]
pub(crate) struct HttpState {
    pub(crate) store: Arc<dyn PublicStore>,
    paths: PagePaths,
    embed: EmbedConfig,
    providers: Vec<ProviderStatusView>,
    pub(crate) webhooks: WebhookConfig,
}

impl HttpState {
    pub(crate) fn new(
        store: Arc<dyn PublicStore>,
        base_path: &str,
        embed: EmbedConfig,
        providers: Vec<ProviderStatusView>,
        webhooks: WebhookConfig,
    ) -> Self {
        Self {
            store,
            paths: PagePaths::new(base_path),
            embed,
            providers,
            webhooks,
        }
    }
}

pub(crate) fn router(state: HttpState, timeout: Duration) -> Router {
    let request_id = HeaderName::from_static("x-request-id");
    let webhook_max_bytes = state.webhooks.max_body_bytes;
    let public_routes = Router::new()
        .route("/", get(home))
        .route("/c/{slug}", get(creator))
        .route("/channels", get(channels))
        .route("/live.json", get(live_json))
        .route("/health/live", get(liveness))
        .route("/health/ready", get(readiness))
        .route("/openapi.json", get(openapi))
        .route("/static/app.css", get(stylesheet))
        .merge(crate::webhooks::router(webhook_max_bytes));
    let app = if state.paths.base.is_empty() {
        public_routes
    } else {
        Router::new()
            .nest(&state.paths.base, public_routes)
            .route(&format!("{}/", state.paths.base), get(redirect_home))
            // Keep container probes independent of the public mount path.
            .route("/health/live", get(liveness))
            .route("/health/ready", get(readiness))
    };
    app.fallback(not_found)
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
                .layer(TraceLayer::new_for_http().make_span_with(
                    |request: &axum::http::Request<Body>| {
                        let request_id = request
                            .headers()
                            .get("x-request-id")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("missing");
                        tracing::info_span!(
                            "http.request",
                            method = %request.method(),
                            path = %request.uri().path(),
                            request_id
                        )
                    },
                ))
                .layer(PropagateRequestIdLayer::new(request_id))
                .layer(CompressionLayer::new())
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    timeout,
                ))
                .layer(CatchPanicLayer::new()),
        )
        .layer(axum::middleware::from_fn(security_headers))
}

async fn redirect_home(State(state): State<HttpState>, OriginalUri(uri): OriginalUri) -> Redirect {
    let mut destination = state.paths.home;
    if let Some(query) = uri.query() {
        destination.push('?');
        destination.push_str(query);
    }
    Redirect::permanent(&destination)
}

async fn security_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'self'; frame-ancestors 'none'; frame-src https://player.twitch.tv https://www.youtube.com https://www.youtube-nocookie.com; img-src 'self' data: https:; media-src 'none'; connect-src 'self'; script-src 'self'; style-src 'self'",
        ),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    response
}

#[utoipa::path(
    get,
    path = "/health/live",
    responses((status = 200, description = "Process is alive", body = HealthResponse))
)]
async fn liveness() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

#[utoipa::path(
    get,
    path = "/health/ready",
    responses(
        (status = 200, description = "PostgreSQL is reachable", body = HealthResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = HealthResponse)
    )
)]
async fn readiness(State(state): State<HttpState>) -> Response {
    match state.store.readiness().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ready" })).into_response(),
        Err(error) => {
            tracing::warn!(error = %error, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse {
                    status: "not_ready",
                }),
            )
                .into_response()
        }
    }
}

async fn home(State(state): State<HttpState>) -> Response {
    match tokio::try_join!(
        state.store.list_live(PUBLIC_LIST_LIMIT),
        state.store.registered_channels(None, HOME_CHANNEL_LIMIT)
    ) {
        Ok((records, channels)) => {
            let now = Utc::now();
            let cache_control = if records
                .iter()
                .any(|record| is_manual_channel(&record.channel))
                || channels
                    .iter()
                    .any(|record| is_manual_channel(&record.channel))
            {
                MANUAL_CACHE_CONTROL
            } else {
                LIVE_CACHE_CONTROL
            };
            let providers = state
                .providers
                .iter()
                .map(|provider| {
                    let degraded = provider.css_status != "disabled"
                        && records.iter().any(|record| {
                            record.channel.provider == provider.kind
                                && session_is_stale(record.channel.provider, &record.session, now)
                        });
                    if degraded {
                        ProviderStatusView::degraded(provider.kind)
                    } else {
                        provider.clone()
                    }
                })
                .collect::<Vec<_>>();
            let has_degraded_providers = providers
                .iter()
                .any(|provider| provider.css_status == "degraded");
            let template = HomeTemplate {
                paths: state.paths.clone(),
                lives: records
                    .iter()
                    .filter(|record| {
                        effective_live_status(&record.channel, Some(&record.session), now)
                            == rill3_domain::LiveStatus::Online
                    })
                    .map(|record| {
                        LiveCard::new(&record.creator, &record.channel, &record.session, now)
                    })
                    .collect(),
                channels: channels
                    .iter()
                    .map(|record| {
                        ChannelCard::new(
                            &record.creator,
                            &record.channel,
                            record.session.as_ref(),
                            now,
                        )
                    })
                    .collect(),
                providers,
                has_degraded_providers,
            };
            template_response(&template, StatusCode::OK, Some(cache_control))
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to render home snapshot");
            plain_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "라이브 목록을 잠시 불러올 수 없습니다.",
            )
        }
    }
}

#[derive(Default, Deserialize)]
struct CreatorQuery {
    channel: Option<Uuid>,
    channels_after: Option<Uuid>,
}

#[derive(Default, Deserialize)]
struct ChannelsQuery {
    after: Option<Uuid>,
}

async fn channels(Query(query): Query<ChannelsQuery>, State(state): State<HttpState>) -> Response {
    match state
        .store
        .registered_channels(query.after, CHANNEL_PAGE_LIMIT + 1)
        .await
    {
        Ok(mut records) => {
            let cache_control = if records
                .iter()
                .any(|record| is_manual_channel(&record.channel))
            {
                MANUAL_CACHE_CONTROL
            } else {
                LIVE_CACHE_CONTROL
            };
            let has_next = records.len() > CHANNEL_PAGE_LIMIT as usize;
            records.truncate(CHANNEL_PAGE_LIMIT as usize);
            let next_url = if has_next {
                records.last().map_or_else(String::new, |record| {
                    format!("{}?after={}", state.paths.channels, record.channel.id)
                })
            } else {
                String::new()
            };
            let now = Utc::now();
            template_response(
                &ChannelsTemplate {
                    paths: state.paths,
                    channels: records
                        .iter()
                        .map(|record| {
                            ChannelCard::new(
                                &record.creator,
                                &record.channel,
                                record.session.as_ref(),
                                now,
                            )
                        })
                        .collect(),
                    next_url,
                    has_previous: query.after.is_some(),
                },
                StatusCode::OK,
                Some(cache_control),
            )
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to load channel directory");
            plain_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "등록 채널을 잠시 불러올 수 없습니다.",
            )
        }
    }
}

async fn creator(
    Path(slug): Path<String>,
    Query(query): Query<CreatorQuery>,
    State(state): State<HttpState>,
) -> Response {
    if !valid_slug(&slug) {
        return not_found(State(state)).await;
    }

    match state.store.creator_channel(&slug, query.channel).await {
        Ok(Some(record)) => {
            let now = Utc::now();
            let mut has_manual_channels = is_manual_channel(&record.channel);
            let mut template = CreatorTemplate::new(
                &record.creator,
                &record.channel,
                record.session.as_ref(),
                &state.embed,
                &state.paths,
                now,
            );
            match state
                .store
                .connected_channels(&slug, query.channels_after, CONNECTED_CHANNEL_LIMIT + 1)
                .await
            {
                Ok(mut connected) => {
                    has_manual_channels |= connected
                        .iter()
                        .any(|channel| is_manual_channel(&channel.channel));
                    let has_next = connected.len() > CONNECTED_CHANNEL_LIMIT as usize;
                    connected.truncate(CONNECTED_CHANNEL_LIMIT as usize);
                    if has_next && let Some(last) = connected.last() {
                        template.next_channels_url = format!(
                            "{}/c/{slug}?channel={}&channels_after={}",
                            state.paths.base, record.channel.id, last.channel.id
                        );
                    }
                    template.channels = connected
                        .iter()
                        .map(|channel| {
                            ChannelCard::new(
                                &channel.creator,
                                &channel.channel,
                                channel.session.as_ref(),
                                now,
                            )
                        })
                        .collect();
                }
                Err(error) => {
                    tracing::error!(error = %error, %slug, "failed to load connected channels");
                    return plain_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "연결된 채널을 잠시 불러올 수 없습니다.",
                    );
                }
            }
            template_response(
                &template,
                StatusCode::OK,
                Some(if has_manual_channels {
                    MANUAL_CACHE_CONTROL
                } else {
                    CREATOR_CACHE_CONTROL
                }),
            )
        }
        Ok(None) => not_found(State(state)).await,
        Err(error) => {
            tracing::error!(error = %error, %slug, "failed to load creator");
            plain_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "방송인 페이지를 잠시 불러올 수 없습니다.",
            )
        }
    }
}

#[utoipa::path(
    get,
    path = "/live.json",
    responses(
        (status = 200, description = "Normalized bounded live snapshot", body = LiveSnapshot),
        (status = 304, description = "ETag matched"),
        (status = 503, description = "PostgreSQL is unavailable")
    )
)]
async fn live_json(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    let records = match state.store.list_live(PUBLIC_LIST_LIMIT).await {
        Ok(records) => records,
        Err(error) => {
            tracing::error!(error = %error, "failed to load JSON live snapshot");
            return plain_error(StatusCode::SERVICE_UNAVAILABLE, "snapshot unavailable");
        }
    };

    let now = Utc::now();
    let cache_control = if records
        .iter()
        .any(|record| is_manual_channel(&record.channel))
    {
        MANUAL_CACHE_CONTROL
    } else {
        LIVE_CACHE_CONTROL
    };
    let snapshot = LiveSnapshot {
        lives: records
            .iter()
            .filter(|record| {
                effective_live_status(&record.channel, Some(&record.session), now)
                    == rill3_domain::LiveStatus::Online
            })
            .map(|record| LiveItem::new(record, now))
            .collect(),
    };
    let Ok(body) = serde_json::to_vec(&snapshot) else {
        return plain_error(StatusCode::INTERNAL_SERVER_ERROR, "serialization failed");
    };
    let etag = format!("\"{}\"", hex_digest(&body));

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        return response_with_cache(
            StatusCode::NOT_MODIFIED,
            Body::empty(),
            &etag,
            cache_control,
        );
    }

    let mut response = response_with_cache(StatusCode::OK, Body::from(body), &etag, cache_control);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

fn response_with_cache(
    status: StatusCode,
    body: Body,
    etag: &str,
    cache_control: &'static str,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

async fn stylesheet() -> Response {
    let mut response = Response::new(Body::from(CSS));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600, stale-while-revalidate=86400"),
    );
    response
}

async fn openapi(State(state): State<HttpState>) -> impl IntoResponse {
    let mut document = ApiDoc::openapi();
    document.servers = Some(vec![utoipa::openapi::Server::new(state.paths.home)]);
    Json(document)
}

async fn not_found(State(state): State<HttpState>) -> Response {
    template_response(
        &NotFoundTemplate { paths: state.paths },
        StatusCode::NOT_FOUND,
        None,
    )
}

fn template_response<T: Template>(
    template: &T,
    status: StatusCode,
    cache_control: Option<&'static str>,
) -> Response {
    match template.render() {
        Ok(body) => {
            let mut response = (status, Html(body)).into_response();
            if let Some(value) = cache_control {
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
            }
            response
        }
        Err(error) => {
            tracing::error!(error = %error, "template rendering failed");
            plain_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "template rendering failed",
            )
        }
    }
}

fn plain_error(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}

fn valid_slug(slug: &str) -> bool {
    (1..=63).contains(&slug.len())
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn hex_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Serialize, ToSchema)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize, ToSchema)]
struct LiveSnapshot {
    lives: Vec<LiveItem>,
}

#[derive(Serialize, ToSchema)]
struct LiveItem {
    creator_slug: String,
    creator_name: String,
    provider: String,
    provider_channel_id: String,
    channel_id: Uuid,
    status_source: &'static str,
    manual_state_expires_at: Option<String>,
    /// Validated HTTPS destination for this broadcast, or null if unavailable.
    watch_url: Option<String>,
    status: String,
    stale: bool,
    title: Option<String>,
    category: Option<String>,
    thumbnail_url: Option<String>,
    started_at: Option<String>,
    observed_at: String,
}

impl LiveItem {
    fn new(record: &LiveRecord, now: chrono::DateTime<Utc>) -> Self {
        Self {
            creator_slug: record.creator.slug.clone(),
            creator_name: record.creator.display_name.clone(),
            provider: record.channel.provider.to_string(),
            provider_channel_id: record.channel.provider_channel_id.clone(),
            channel_id: record.channel.id,
            status_source: if is_manual_channel(&record.channel) {
                "manual"
            } else {
                "provider"
            },
            manual_state_expires_at: record
                .channel
                .manual_state_expires_at
                .map(|time| time.to_rfc3339()),
            watch_url: watch_url(&record.channel, Some(&record.session)).map(|url| url.to_string()),
            status: match record.session.state {
                rill3_domain::LiveStatus::Online => "online",
                rill3_domain::LiveStatus::Offline => "offline",
                rill3_domain::LiveStatus::Unknown => "unknown",
            }
            .to_owned(),
            stale: session_is_stale(record.channel.provider, &record.session, now),
            title: record.session.title.clone(),
            category: record.session.category.clone(),
            thumbnail_url: record
                .session
                .thumbnail_url
                .as_ref()
                .map(ToString::to_string),
            started_at: record.session.started_at.map(|value| value.to_rfc3339()),
            observed_at: record.session.observed_at.to_rfc3339(),
        }
    }
}

fn is_manual_channel(channel: &ExternalChannel) -> bool {
    matches!(
        channel.provider,
        ProviderKind::YouTube | ProviderKind::LinkOnly
    )
}

#[derive(OpenApi)]
#[openapi(
    paths(liveness, readiness, live_json),
    components(schemas(HealthResponse, LiveSnapshot, LiveItem)),
    tags((name = "public", description = "RILL3 public read API"))
)]
struct ApiDoc;

pub(crate) fn default_provider_statuses(
    twitch_enabled: bool,
    chzzk_enabled: bool,
) -> Vec<ProviderStatusView> {
    vec![
        if twitch_enabled {
            ProviderStatusView::enabled(ProviderKind::Twitch)
        } else {
            ProviderStatusView::disabled(ProviderKind::Twitch)
        },
        ProviderStatusView::enabled(ProviderKind::YouTube),
        if chzzk_enabled {
            ProviderStatusView::enabled(ProviderKind::Chzzk)
        } else {
            ProviderStatusView::disabled(ProviderKind::Chzzk)
        },
        ProviderStatusView::enabled(ProviderKind::LinkOnly),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Utc;
    use http_body_util::BodyExt;
    use rill3_domain::{CreatorStatus, EmbedCapability, LiveStatus, VerificationState};
    use tower::ServiceExt;
    use url::Url;
    use uuid::Uuid;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        readiness_fails: bool,
        list_calls: AtomicUsize,
        live: Option<LiveRecord>,
        registered: Vec<CreatorRecord>,
    }

    impl FakeStore {
        fn visible_channels(&self) -> Vec<CreatorRecord> {
            let mut records = self.registered.clone();
            if let Some(live) = &self.live {
                records.push(CreatorRecord {
                    creator: live.creator.clone(),
                    channel: live.channel.clone(),
                    session: Some(live.session.clone()),
                });
            }
            records.retain(|record| {
                record.creator.status == CreatorStatus::Active
                    && record.channel.enabled
                    && record.channel.verification_state != VerificationState::Disabled
            });
            records.sort_by_key(|record| record.channel.id);
            records.dedup_by_key(|record| record.channel.id);
            records
        }
    }

    #[async_trait]
    impl PublicStore for FakeStore {
        async fn readiness(&self) -> Result<()> {
            if self.readiness_fails {
                anyhow::bail!("database unavailable");
            }
            Ok(())
        }

        async fn list_live(&self, _limit: u32) -> Result<Vec<LiveRecord>> {
            self.list_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.live.iter().cloned().collect())
        }

        async fn creator_by_slug(&self, slug: &str) -> Result<Option<CreatorRecord>> {
            self.creator_channel(slug, None).await
        }

        async fn creator_channel(
            &self,
            slug: &str,
            channel: Option<Uuid>,
        ) -> Result<Option<CreatorRecord>> {
            Ok(self.visible_channels().into_iter().find(|record| {
                record.creator.slug == slug && channel.is_none_or(|id| record.channel.id == id)
            }))
        }

        async fn registered_channels(
            &self,
            after: Option<Uuid>,
            limit: u32,
        ) -> Result<Vec<CreatorRecord>> {
            Ok(self
                .visible_channels()
                .into_iter()
                .filter(|record| after.is_none_or(|id| record.channel.id > id))
                .take(limit as usize)
                .collect())
        }

        async fn connected_channels(
            &self,
            slug: &str,
            after: Option<Uuid>,
            limit: u32,
        ) -> Result<Vec<CreatorRecord>> {
            Ok(self
                .visible_channels()
                .into_iter()
                .filter(|record| {
                    record.creator.slug == slug && after.is_none_or(|id| record.channel.id > id)
                })
                .take(limit as usize)
                .collect())
        }
    }

    fn test_state(store: Arc<dyn PublicStore>) -> HttpState {
        test_state_at_path(store, "")
    }

    fn test_state_at_path(store: Arc<dyn PublicStore>, base_path: &str) -> HttpState {
        HttpState::new(
            store,
            base_path,
            EmbedConfig {
                twitch_parent: "rill3.example".to_owned(),
                public_origin: Url::parse("https://rill3.example").unwrap(),
            },
            default_provider_statuses(false, false),
            WebhookConfig::default(),
        )
    }

    #[tokio::test]
    async fn home_contains_no_iframe() {
        let app = router(
            test_state(Arc::new(FakeStore::default())),
            Duration::from_secs(1),
        );
        let response = app
            .oneshot(axum::http::Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();

        assert!(!html.to_ascii_lowercase().contains("<iframe"));
        assert!(html.contains(&format!("href=\"{}\"", crate::assets::stylesheet_path(""))));
        assert!(html.contains("아직 등록된 채널이 없습니다"));
        for destination in [
            "https://chzzk.naver.com/",
            "https://www.youtube.com/",
            "https://www.twitch.tv/directory",
            "https://www.sooplive.com/",
        ] {
            assert!(html.contains(&format!("href=\"{destination}\"")));
        }
        assert!(html.contains("라이브 새로고침"));
        assert!(!html.contains("Provider worker"));
        assert!(!html.contains("자동으로 나타납니다"));
    }

    #[tokio::test]
    async fn broadcast_links_are_consistent_across_home_creator_and_json() {
        for (provider, channel_id, canonical_url, current_url, expected_url) in [
            (
                ProviderKind::Twitch,
                "42",
                "https://www.twitch.tv/safe_channel",
                None,
                "https://www.twitch.tv/safe_channel",
            ),
            (
                ProviderKind::YouTube,
                "UC_x5XG1OV2P6uZZ5FSM9Ttw",
                "https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw",
                Some("https://youtu.be/dQw4w9WgXcQ?t=30"),
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            ),
            (
                ProviderKind::Chzzk,
                "0123456789abcdef0123456789abcdef",
                "https://chzzk.naver.com/0123456789abcdef0123456789abcdef",
                None,
                "https://chzzk.naver.com/live/0123456789abcdef0123456789abcdef",
            ),
            (
                ProviderKind::LinkOnly,
                "safe_channel",
                "https://video.example/channel",
                Some("https://video.example/live/42"),
                "https://video.example/live/42",
            ),
        ] {
            let mut record = live_fixture();
            record.channel.provider = provider;
            record.channel.provider_channel_id = channel_id.to_owned();
            record.channel.canonical_url = Url::parse(canonical_url).unwrap();
            record.channel.current_live_url = current_url.map(|url| Url::parse(url).unwrap());
            if is_manual_channel(&record.channel) {
                record.channel.manual_live_status = Some(LiveStatus::Online);
                record.channel.manual_state_expires_at =
                    Some(Utc::now() + chrono::TimeDelta::hours(2));
            }
            let app = router(
                test_state_at_path(
                    Arc::new(FakeStore {
                        live: Some(record),
                        ..FakeStore::default()
                    }),
                    "/rill3",
                ),
                Duration::from_secs(1),
            );

            for path in ["/rill3", "/rill3/c/safe", "/rill3/live.json"] {
                let response = app
                    .clone()
                    .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body = response.into_body().collect().await.unwrap().to_bytes();
                if path == "/rill3/live.json" {
                    let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(snapshot["lives"][0]["watch_url"], expected_url);
                } else {
                    let html = String::from_utf8(body.to_vec()).unwrap();
                    assert!(
                        html.contains(&format!("href=\"{expected_url}\"")),
                        "{provider} {path}"
                    );
                    assert!(!html.contains("href=\"/rill3/https:"));
                    if path == "/rill3" {
                        assert!(!html.contains("<iframe"));
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn unsafe_broadcast_links_are_omitted_from_pages_and_json() {
        let mut record = live_fixture();
        record.channel.provider = ProviderKind::LinkOnly;
        record.channel.canonical_url = Url::parse("javascript:alert(1)").unwrap();
        record.channel.current_live_url = Some(Url::parse("https://127.0.0.1/live").unwrap());
        record.channel.manual_live_status = Some(LiveStatus::Online);
        record.channel.manual_state_expires_at = Some(Utc::now() + chrono::TimeDelta::hours(2));
        let app = router(
            test_state(Arc::new(FakeStore {
                live: Some(record),
                ..FakeStore::default()
            })),
            Duration::from_secs(1),
        );
        for path in ["/", "/c/safe", "/live.json"] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();
            assert!(!text.contains("javascript:"));
            assert!(!text.contains("127.0.0.1"));
            if path == "/live.json" {
                let snapshot: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert!(snapshot["lives"][0].get("watch_url").unwrap().is_null());
            }
        }
    }

    #[tokio::test]
    async fn database_outage_only_fails_readiness() {
        let app = router(
            test_state(Arc::new(FakeStore {
                readiness_fails: true,
                ..FakeStore::default()
            })),
            Duration::from_secs(1),
        );

        let live = app
            .clone()
            .oneshot(
                axum::http::Request::get("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ready = app
            .oneshot(
                axum::http::Request::get("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(live.status(), StatusCode::OK);
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn offline_channels_remain_discoverable_without_a_live_session() {
        let live = live_fixture();
        let id = live.channel.id;
        let app = router(
            test_state_at_path(
                Arc::new(FakeStore {
                    registered: vec![CreatorRecord {
                        creator: live.creator,
                        channel: live.channel,
                        session: None,
                    }],
                    ..FakeStore::default()
                }),
                "/rill3",
            ),
            Duration::from_secs(1),
        );
        for path in [
            "/rill3".to_owned(),
            "/rill3/channels".to_owned(),
            format!("/rill3/c/safe?channel={id}"),
        ] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(&path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains("https://www.twitch.tv/safe_channel"));
            assert!(!html.contains("<iframe"));
            if path == "/rill3" {
                assert!(html.contains("지금은 확인된 라이브가 없습니다"));
                assert!(!html.contains("아직 등록된 채널이 없습니다"));
            }
        }
    }

    #[tokio::test]
    async fn channel_directory_pagination_preserves_mount_and_cursor() {
        let registered = (1..=25)
            .map(|index| {
                let mut live = live_fixture();
                live.creator.slug = format!("channel-{index}");
                live.channel.id = Uuid::from_u128(index);
                CreatorRecord {
                    creator: live.creator,
                    channel: live.channel,
                    session: None,
                }
            })
            .collect();
        let app = router(
            test_state_at_path(
                Arc::new(FakeStore {
                    registered,
                    ..FakeStore::default()
                }),
                "/rill3",
            ),
            Duration::from_secs(1),
        );
        let next = format!("/rill3/channels?after={}", Uuid::from_u128(24));
        for (path, expected_count) in [("/rill3/channels", 24), (next.as_str(), 1)] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert_eq!(
                html.matches("class=\"channel-card\"").count(),
                expected_count
            );
            if expected_count == 24 {
                assert!(html.contains(&format!("href=\"{next}\"")));
                assert!(!html.contains("/c/channel-25?"));
            } else {
                assert!(html.contains("/rill3/c/channel-25?"));
                assert!(!html.contains("다음 채널 보기"));
            }
        }
        let invalid = app
            .oneshot(
                axum::http::Request::get("/rill3/channels?after=invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn selected_creator_channel_cannot_switch_to_a_foreign_or_disabled_channel() {
        let twitch = live_fixture();
        let mut youtube = twitch.clone();
        youtube.channel.id = Uuid::new_v4();
        youtube.channel.provider = ProviderKind::YouTube;
        youtube.channel.canonical_url =
            Url::parse("https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw").unwrap();
        youtube.channel.current_live_url =
            Some(Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap());
        youtube.session.channel_id = youtube.channel.id;
        youtube.channel.manual_live_status = Some(LiveStatus::Online);
        youtube.channel.manual_state_expires_at = Some(Utc::now() + chrono::TimeDelta::hours(2));
        let mut disabled = twitch.clone();
        disabled.channel.id = Uuid::new_v4();
        disabled.channel.enabled = false;
        let mut foreign = live_fixture();
        foreign.creator.slug = "foreign".to_owned();
        let registered = [&twitch, &youtube, &disabled, &foreign]
            .into_iter()
            .map(|live| CreatorRecord {
                creator: live.creator.clone(),
                channel: live.channel.clone(),
                session: Some(live.session.clone()),
            })
            .collect();
        let app = router(
            test_state(Arc::new(FakeStore {
                registered,
                ..FakeStore::default()
            })),
            Duration::from_secs(1),
        );
        for (id, status) in [
            (youtube.channel.id, StatusCode::OK),
            (disabled.channel.id, StatusCode::NOT_FOUND),
            (foreign.channel.id, StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::get(format!("/c/safe?channel={id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            if status == StatusCode::OK {
                let body = response.into_body().collect().await.unwrap().to_bytes();
                let html = String::from_utf8(body.to_vec()).unwrap();
                assert!(html.contains("player-frame--youtube"));
                assert!(!html.contains("player-frame--twitch"));
                assert!(html.contains(&format!("/c/safe?channel={}", twitch.channel.id)));
                assert!(html.contains(&format!(
                    "href=\"/c/safe?channel={id}\" aria-current=\"page\""
                )));
                assert!(html.contains("등록된 방송 정보 기준"));
            }
        }
        let invalid = app
            .oneshot(
                axum::http::Request::get("/c/safe?channel=invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn live_json_has_cache_headers_and_supports_etag() {
        let app = router(
            test_state(Arc::new(FakeStore::default())),
            Duration::from_secs(1),
        );
        let first = app
            .clone()
            .oneshot(
                axum::http::Request::get("/live.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = first.headers().get(header::ETAG).unwrap().clone();
        assert_eq!(
            first.headers().get(header::CACHE_CONTROL).unwrap(),
            LIVE_CACHE_CONTROL
        );

        let second = app
            .oneshot(
                axum::http::Request::get("/live.json")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn expired_manual_broadcast_is_hidden_without_waiting_for_the_worker() {
        let mut live = live_fixture();
        live.channel.provider = ProviderKind::YouTube;
        live.channel.canonical_url =
            Url::parse("https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw").unwrap();
        live.channel.current_live_url =
            Some(Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap());
        live.channel.manual_live_status = Some(LiveStatus::Online);
        live.channel.manual_state_expires_at = Some(Utc::now() - chrono::TimeDelta::seconds(1));
        let app = router(
            test_state(Arc::new(FakeStore {
                live: Some(live),
                ..FakeStore::default()
            })),
            Duration::from_secs(1),
        );
        for path in ["/", "/channels", "/c/safe", "/live.json"] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                MANUAL_CACHE_CONTROL
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(body.to_vec()).unwrap();
            if path == "/live.json" {
                let snapshot: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(snapshot["lives"], serde_json::json!([]));
            } else {
                assert!(!text.contains("<iframe"));
                assert!(!text.contains("class=\"live-card"));
                assert!(!text.contains("href=\"https://www.youtube.com/watch?"));
                assert!(text.contains("https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw"));
            }
        }
    }

    #[tokio::test]
    async fn subpath_pages_assets_and_navigation_stay_under_the_mount() {
        let stylesheet_path = crate::assets::stylesheet_path("/rill3");
        let css_digest = hex_digest(CSS.as_bytes());
        assert_eq!(
            stylesheet_path,
            format!("/rill3/static/app.css?v={css_digest:.16}")
        );
        let app = router(
            test_state_at_path(
                Arc::new(FakeStore {
                    live: Some(live_fixture()),
                    ..FakeStore::default()
                }),
                "/rill3",
            ),
            Duration::from_secs(1),
        );
        for (path, status) in [
            ("/rill3", StatusCode::OK),
            ("/rill3/c/safe", StatusCode::OK),
            ("/rill3/c/missing", StatusCode::NOT_FOUND),
            ("/rill3/c/INVALID", StatusCode::NOT_FOUND),
            ("/rill3/missing", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{path}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(
                html.contains(&format!("href=\"{stylesheet_path}\"")),
                "{path}"
            );
            assert!(html.contains("href=\"/rill3\""), "{path}");
            assert!(!html.contains("href=\"/\""), "{path}");
            if path == "/rill3" {
                assert!(html.contains("href=\"/rill3/c/safe?channel="));
            }
            if path == "/rill3/c/safe" {
                assert!(html.contains("parent=rill3.example"));
            }
        }
        let css = app
            .oneshot(
                axum::http::Request::get(&stylesheet_path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(css.status(), StatusCode::OK);
        assert_eq!(
            css.headers()[header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );
        let body = css.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), CSS.as_bytes());
    }

    #[tokio::test]
    async fn subpath_redirect_preserves_query_and_health_checks_work_at_both_paths() {
        let app = router(
            test_state_at_path(Arc::new(FakeStore::default()), "/rill3"),
            Duration::from_secs(1),
        );
        let redirect = app
            .clone()
            .oneshot(
                axum::http::Request::get("/rill3/?from=bookmark")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(redirect.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(redirect.headers()[header::LOCATION], "/rill3?from=bookmark");

        for path in [
            "/health/live",
            "/health/ready",
            "/rill3/health/live",
            "/rill3/health/ready",
        ] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }
        for path in ["/", "/live.json", "/static/app.css", "/rill3-other"] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn subpath_json_openapi_and_webhooks_are_mounted() {
        let app = router(
            test_state_at_path(Arc::new(FakeStore::default()), "/rill3"),
            Duration::from_secs(1),
        );
        let snapshot = app
            .clone()
            .oneshot(
                axum::http::Request::get("/rill3/live.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot.status(), StatusCode::OK);
        assert!(snapshot.headers().contains_key(header::ETAG));

        let schema = app
            .clone()
            .oneshot(
                axum::http::Request::get("/rill3/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(schema.status(), StatusCode::OK);
        let body = schema.into_body().collect().await.unwrap().to_bytes();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["servers"][0]["url"], "/rill3");
        assert!(document["paths"]["/live.json"].is_object());

        for path in ["/rill3/webhooks/twitch", "/rill3/webhooks/youtube"] {
            let response = app
                .clone()
                .oneshot(axum::http::Request::post(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            // Disabled credentials reject the request after it reaches the webhook handler.
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        }
    }

    fn live_fixture() -> LiveRecord {
        let now = Utc::now();
        let creator = Creator {
            id: Uuid::new_v4(),
            slug: "safe".to_owned(),
            display_name: "Safe".to_owned(),
            bio: None,
            locale: "ko-KR".to_owned(),
            status: CreatorStatus::Active,
            created_at: now,
            updated_at: now,
        };
        let channel = ExternalChannel {
            id: Uuid::new_v4(),
            creator_id: creator.id,
            provider: ProviderKind::Twitch,
            provider_channel_id: "42".to_owned(),
            handle: Some("safe_channel".to_owned()),
            canonical_url: Url::parse("https://www.twitch.tv/safe_channel").unwrap(),
            current_live_url: None,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: EmbedCapability::OfficialEmbed,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let state = rill3_domain::LiveState::new(&channel, LiveStatus::Online, now);
        let session = LiveSession::from_state(Uuid::new_v4(), &state);
        LiveRecord {
            creator,
            channel,
            session,
        }
    }

    #[test]
    fn creator_template_uses_only_trusted_twitch_parent() {
        let record = live_fixture();
        let template = CreatorTemplate::new(
            &record.creator,
            &record.channel,
            Some(&record.session),
            &EmbedConfig {
                twitch_parent: "rill3.example".to_owned(),
                public_origin: Url::parse("https://rill3.example").unwrap(),
            },
            &PagePaths::new(""),
            Utc::now(),
        )
        .render()
        .unwrap();

        assert!(template.contains("parent=rill3.example"));
        assert!(!template.contains("parent=evil"));
        assert_eq!(template.matches("<iframe").count(), 1);
    }
}
