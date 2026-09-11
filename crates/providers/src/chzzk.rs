use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, Response, redirect::Policy};
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, LiveStatus, ProviderAvailability,
    ProviderKind, VerifiedChannel, VerifyChannelInput,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use crate::{ProviderError, StreamProvider};

pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(30);
pub const MAX_POLL_INTERVAL: Duration = Duration::from_mins(1);

pub struct ChzzkCredentials {
    client_id: SecretString,
    client_secret: SecretString,
}

impl ChzzkCredentials {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let client_id = client_id.into();
        let client_secret = client_secret.into();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(ProviderError::InvalidConfiguration(
                "CHZZK client ID and secret must both be set",
            ));
        }
        Ok(Self {
            client_id: SecretString::from(client_id),
            client_secret: SecretString::from(client_secret),
        })
    }
}

pub struct ChzzkConfig {
    credentials: Option<ChzzkCredentials>,
    api_base_url: Url,
    request_timeout: Duration,
    scan_budget: Duration,
    max_live_pages: usize,
}

impl ChzzkConfig {
    pub fn new(credentials: Option<ChzzkCredentials>) -> Result<Self, ProviderError> {
        Ok(Self {
            credentials,
            api_base_url: Url::parse("https://openapi.chzzk.naver.com/open/v1/")
                .map_err(|_| ProviderError::InvalidConfiguration("CHZZK Open API base URL"))?,
            request_timeout: Duration::from_secs(10),
            scan_budget: Duration::from_secs(45),
            max_live_pages: 100,
        })
    }

    /// Overrides trusted operator configuration, primarily for local contract tests.
    #[must_use]
    pub fn with_api_base_url(mut self, api_base_url: Url) -> Self {
        self.api_base_url = api_base_url;
        self
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    #[must_use]
    pub const fn with_scan_budget(mut self, budget: Duration) -> Self {
        self.scan_budget = budget;
        self
    }

    pub fn with_max_live_pages(mut self, max_live_pages: usize) -> Result<Self, ProviderError> {
        if max_live_pages == 0 || max_live_pages > 1_000 {
            return Err(ProviderError::InvalidConfiguration(
                "CHZZK max live pages must be in 1..=1000",
            ));
        }
        self.max_live_pages = max_live_pages;
        Ok(self)
    }
}

pub struct ChzzkProvider {
    client: Client,
    config: ChzzkConfig,
}

impl ChzzkProvider {
    pub fn new(config: ChzzkConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(config.request_timeout)
            .build()
            .map_err(|source| ProviderError::Transport {
                provider: ProviderKind::Chzzk,
                source,
            })?;
        Ok(Self { client, config })
    }

    #[must_use]
    pub const fn poll_interval_bounds(&self) -> (Duration, Duration) {
        (MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
    }

    fn credentials(&self) -> Result<&ChzzkCredentials, ProviderError> {
        self.config
            .credentials
            .as_ref()
            .ok_or(ProviderError::Disabled {
                provider: ProviderKind::Chzzk,
                reason: "credentials are not configured",
            })
    }

    async fn get<T>(&self, path: &str, query: &[(String, String)]) -> Result<T, ProviderError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let credentials = self.credentials()?;
        let mut url = self
            .config
            .api_base_url
            .join(path)
            .map_err(|_| ProviderError::InvalidConfiguration("CHZZK endpoint"))?;
        url.query_pairs_mut().extend_pairs(query);
        let response = self
            .client
            .get(url)
            .header("Client-Id", credentials.client_id.expose_secret())
            .header("Client-Secret", credentials.client_secret.expose_secret())
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|source| ProviderError::Transport {
                provider: ProviderKind::Chzzk,
                source,
            })?;
        decode_response(response).await
    }

    async fn scan_lives(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<(HashMap<String, ChzzkLive>, bool), ProviderError> {
        let wanted = channels
            .iter()
            .map(|channel| channel.provider_channel_id.as_str())
            .collect::<HashSet<_>>();
        let mut found = HashMap::with_capacity(channels.len());
        let mut next: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let started = Instant::now();

        for _ in 0..self.config.max_live_pages {
            if started.elapsed() >= self.config.scan_budget {
                return Ok((found, false));
            }
            let mut query = vec![("size".to_owned(), "20".to_owned())];
            if let Some(cursor) = next.as_ref() {
                query.push(("next".to_owned(), cursor.clone()));
            }
            let response: ChzzkEnvelope<ChzzkLivesContent> = self.get("lives", &query).await?;
            ensure_envelope_code(&response)?;
            for live in response.content.data {
                if wanted.contains(live.channel_id.as_str()) {
                    found.insert(live.channel_id.clone(), live);
                }
            }
            if found.len() == wanted.len() {
                return Ok((found, true));
            }
            let Some(cursor) = response.content.page.next.filter(|value| !value.is_empty()) else {
                return Ok((found, true));
            };
            if !seen_cursors.insert(cursor.clone()) {
                return Err(ProviderError::InvalidResponse {
                    provider: ProviderKind::Chzzk,
                    reason: "live pagination cursor repeated",
                });
            }
            next = Some(cursor);
        }
        Ok((found, false))
    }
}

#[async_trait]
impl StreamProvider for ChzzkProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Chzzk
    }

    fn embed_capability(&self) -> EmbedCapability {
        EmbedCapability::LinkOnly
    }

    fn availability(&self) -> ProviderAvailability {
        if self.config.credentials.is_some() {
            ProviderAvailability::Enabled
        } else {
            ProviderAvailability::Disabled
        }
    }

    fn disabled_reason(&self) -> Option<&str> {
        self.config
            .credentials
            .is_none()
            .then_some("credentials are not configured")
    }

    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError> {
        validate_channel_id(&input.provider_channel_id)?;
        let response: ChzzkEnvelope<ChzzkChannelsContent> = self
            .get(
                "channels",
                &[("channelIds".to_owned(), input.provider_channel_id.clone())],
            )
            .await?;
        ensure_envelope_code(&response)?;
        let channel = response
            .content
            .data
            .into_iter()
            .find(|channel| channel.id == input.provider_channel_id)
            .ok_or(ProviderError::NotFound {
                provider: ProviderKind::Chzzk,
            })?;
        Ok(VerifiedChannel {
            provider: ProviderKind::Chzzk,
            provider_channel_id: channel.id.clone(),
            handle: None,
            display_name: Some(channel.name),
            canonical_url: channel_url(&channel.id)?,
            current_live_url: None,
            thumbnail_url: parse_https_url(&channel.image_url),
            embed_capability: EmbedCapability::LinkOnly,
        })
    }

    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError> {
        self.reconcile_many(std::slice::from_ref(channel))
            .await?
            .pop()
            .ok_or(ProviderError::InvalidResponse {
                provider: ProviderKind::Chzzk,
                reason: "reconciliation omitted requested channel",
            })
    }

    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError> {
        for channel in channels {
            if channel.provider != ProviderKind::Chzzk {
                return Err(ProviderError::InvalidInput {
                    provider: ProviderKind::Chzzk,
                    field: "provider",
                    reason: "channel belongs to a different provider",
                });
            }
            validate_channel_id(&channel.provider_channel_id)?;
        }
        if channels.is_empty() {
            return Ok(Vec::new());
        }

        let observed_at = Utc::now();
        let (live_by_channel, complete_scan) = self.scan_lives(channels).await?;
        let states = channels
            .iter()
            .filter_map(|channel| {
                let Some(live) = live_by_channel.get(&channel.provider_channel_id) else {
                    return complete_scan
                        .then(|| LiveState::new(channel, LiveStatus::Offline, observed_at));
                };
                let mut state = LiveState::new(channel, LiveStatus::Online, observed_at);
                state.provider_session_id = Some(live.live_id.to_string());
                state.title = Some(live.live_title.clone());
                state.category = nonempty(live.live_category_value.clone());
                state.thumbnail_url = parse_chzzk_thumbnail(&live.live_thumbnail_image_url);
                state.started_at = Some(live.open_date);
                Some(state)
            })
            .collect();
        Ok(states)
    }

    fn external_url(&self, channel: &ExternalChannel) -> Url {
        channel.canonical_url.clone()
    }

    fn embed_descriptor(&self, _live: &LiveState) -> Option<EmbedDescriptor> {
        None
    }
}

fn validate_channel_id(channel_id: &str) -> Result<(), ProviderError> {
    if channel_id.len() == 32 && channel_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ProviderError::InvalidInput {
            provider: ProviderKind::Chzzk,
            field: "provider_channel_id",
            reason: "must be a 32-character hexadecimal CHZZK channel ID",
        })
    }
}

fn channel_url(channel_id: &str) -> Result<Url, ProviderError> {
    Url::parse(&format!("https://chzzk.naver.com/{channel_id}"))
        .map_err(|_| ProviderError::InvalidConfiguration("CHZZK channel base URL"))
}

fn parse_chzzk_thumbnail(raw: &str) -> Option<Url> {
    parse_https_url(&raw.replace("{type}", "480"))
}

fn parse_https_url(raw: &str) -> Option<Url> {
    Url::parse(raw)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host().is_some())
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn ensure_envelope_code<T>(response: &ChzzkEnvelope<T>) -> Result<(), ProviderError> {
    if response.code == 200 {
        Ok(())
    } else {
        Err(ProviderError::InvalidResponse {
            provider: ProviderKind::Chzzk,
            reason: "Open API envelope code is not 200",
        })
    }
}

async fn decode_response<T>(response: Response) -> Result<T, ProviderError>
where
    T: for<'de> Deserialize<'de>,
{
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
    if status.as_u16() == 429 {
        return Err(ProviderError::RateLimited {
            provider: ProviderKind::Chzzk,
            retry_after,
        });
    }
    if status.is_server_error() {
        return Err(ProviderError::TransientHttp {
            provider: ProviderKind::Chzzk,
            status: status.as_u16(),
            retry_after,
        });
    }
    if !status.is_success() {
        return Err(ProviderError::RejectedHttp {
            provider: ProviderKind::Chzzk,
            status: status.as_u16(),
        });
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|source| ProviderError::Transport {
            provider: ProviderKind::Chzzk,
            source,
        })?;
    serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse {
        provider: ProviderKind::Chzzk,
        reason: "response is not valid expected JSON",
    })
}

#[derive(Deserialize)]
struct ChzzkEnvelope<T> {
    code: i64,
    #[allow(dead_code)]
    message: Option<String>,
    content: T,
}

#[derive(Deserialize)]
struct ChzzkChannelsContent {
    data: Vec<ChzzkChannel>,
}

#[derive(Deserialize)]
struct ChzzkChannel {
    #[serde(rename = "channelId")]
    id: String,
    #[serde(rename = "channelName")]
    name: String,
    #[serde(rename = "channelImageUrl")]
    image_url: String,
}

#[derive(Deserialize)]
struct ChzzkLivesContent {
    data: Vec<ChzzkLive>,
    page: ChzzkPage,
}

#[derive(Deserialize)]
struct ChzzkPage {
    next: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChzzkLive {
    live_id: i64,
    live_title: String,
    live_thumbnail_image_url: String,
    open_date: DateTime<Utc>,
    live_category_value: String,
    channel_id: String,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use rill3_domain::{VerificationState, VerifyChannelInput};
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path, query_param},
    };

    use super::*;

    const CHANNEL_ID: &str = "0123456789abcdef0123456789abcdef";

    fn provider(server: &MockServer) -> ChzzkProvider {
        let credentials = ChzzkCredentials::new("client-id", "client-secret").unwrap();
        let config = ChzzkConfig::new(Some(credentials))
            .unwrap()
            .with_api_base_url(Url::parse(&format!("{}/open/v1/", server.uri())).unwrap());
        ChzzkProvider::new(config).unwrap()
    }

    fn channel() -> ExternalChannel {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        ExternalChannel {
            id: Uuid::nil(),
            creator_id: Uuid::nil(),
            provider: ProviderKind::Chzzk,
            provider_channel_id: CHANNEL_ID.to_owned(),
            handle: None,
            canonical_url: Url::parse(&format!("https://chzzk.naver.com/{CHANNEL_ID}")).unwrap(),
            current_live_url: None,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: EmbedCapability::LinkOnly,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn official_channel_contract_uses_client_auth_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/open/v1/channels"))
            .and(query_param("channelIds", CHANNEL_ID))
            .and(header("Client-Id", "client-id"))
            .and(header("Client-Secret", "client-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": null,
                "content": {"data": [{
                    "channelId": CHANNEL_ID,
                    "channelName": "RILL3",
                    "channelImageUrl": "https://ssl.pstatic.net/channel.png",
                    "followerCount": 3,
                    "verifiedMark": false
                }]}
            })))
            .mount(&server)
            .await;
        let verified = provider(&server)
            .verify_channel(VerifyChannelInput::new(CHANNEL_ID))
            .await
            .unwrap();
        assert_eq!(verified.provider_channel_id, CHANNEL_ID);
        assert_eq!(verified.display_name.as_deref(), Some("RILL3"));
        assert_eq!(verified.embed_capability, EmbedCapability::LinkOnly);
    }

    #[tokio::test]
    async fn official_lives_contract_normalizes_live_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/open/v1/lives"))
            .and(query_param("size", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": null,
                "content": {
                    "data": [{
                        "liveId": 42,
                        "liveTitle": "RILL3 live",
                        "liveThumbnailImageUrl": "https://livecloud-thumb.akamaized.net/{type}/image.jpg",
                        "concurrentUserCount": 12,
                        "openDate": "2026-09-02T12:00:00Z",
                        "adult": false,
                        "tags": [],
                        "categoryType": "ETC",
                        "liveCategory": "talk",
                        "liveCategoryValue": "Talk",
                        "channelId": CHANNEL_ID,
                        "channelName": "RILL3",
                        "channelImageUrl": "https://ssl.pstatic.net/channel.png"
                    }],
                    "page": {"next": null}
                }
            })))
            .mount(&server)
            .await;
        let state = provider(&server)
            .fetch_live_state(&channel())
            .await
            .unwrap();
        assert_eq!(state.status, LiveStatus::Online);
        assert_eq!(state.provider_session_id.as_deref(), Some("42"));
        assert_eq!(state.category.as_deref(), Some("Talk"));
    }

    #[tokio::test]
    async fn quota_response_is_typed_and_does_not_retry_in_adapter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/open/v1/lives"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "45"))
            .expect(1)
            .mount(&server)
            .await;
        let error = provider(&server)
            .fetch_live_state(&channel())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::RateLimited {
                provider: ProviderKind::Chzzk,
                retry_after: Some(delay)
            } if delay == Duration::from_secs(45)
        ));
    }

    #[tokio::test]
    async fn incomplete_directory_scan_omits_unobserved_channels() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/open/v1/lives"))
            .and(query_param("size", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": null,
                "content": {
                    "data": [],
                    "page": {"next": "more-results"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let credentials = ChzzkCredentials::new("client-id", "client-secret").unwrap();
        let config = ChzzkConfig::new(Some(credentials))
            .unwrap()
            .with_api_base_url(Url::parse(&format!("{}/open/v1/", server.uri())).unwrap())
            .with_max_live_pages(1)
            .unwrap();
        let provider = ChzzkProvider::new(config).unwrap();
        let channels = vec![channel()];

        let states = provider.reconcile_many(&channels).await.unwrap();

        assert!(states.is_empty());
    }
}
