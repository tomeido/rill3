use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Response, redirect::Policy};
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, LiveStatus, ProviderAvailability,
    ProviderKind, TwitchEmbedDescriptor, VerifiedChannel, VerifyChannelInput,
    validate_twitch_channel, validate_twitch_parent,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::Sha256;
use thiserror::Error;
use url::{Url, form_urlencoded};

use crate::{ProviderError, StreamProvider};

const DEFAULT_EVENTSUB_TOLERANCE: Duration = Duration::from_mins(10);
const TOKEN_EXPIRY_SAFETY_MARGIN: Duration = Duration::from_mins(1);

pub struct TwitchCredentials {
    client_id: SecretString,
    client_secret: SecretString,
    eventsub_secret: SecretString,
}

impl TwitchCredentials {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        eventsub_secret: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let client_id = client_id.into();
        let client_secret = client_secret.into();
        let eventsub_secret = eventsub_secret.into();
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(ProviderError::InvalidConfiguration(
                "Twitch client ID and secret must both be set",
            ));
        }
        if !(10..=100).contains(&eventsub_secret.len()) || !eventsub_secret.is_ascii() {
            return Err(ProviderError::InvalidConfiguration(
                "Twitch EventSub secret must be 10..=100 ASCII characters",
            ));
        }
        Ok(Self {
            client_id: SecretString::from(client_id),
            client_secret: SecretString::from(client_secret),
            eventsub_secret: SecretString::from(eventsub_secret),
        })
    }
}

pub struct TwitchConfig {
    parent: String,
    credentials: Option<TwitchCredentials>,
    helix_base_url: Url,
    oauth_token_url: Url,
    request_timeout: Duration,
    eventsub_tolerance: Duration,
}

impl TwitchConfig {
    pub fn new(
        parent: impl Into<String>,
        credentials: Option<TwitchCredentials>,
    ) -> Result<Self, ProviderError> {
        let parent = parent.into();
        validate_twitch_parent(&parent).map_err(|_| {
            ProviderError::InvalidConfiguration("Twitch parent must be a bare DNS host")
        })?;
        Ok(Self {
            parent: parent.to_ascii_lowercase(),
            credentials,
            helix_base_url: Url::parse("https://api.twitch.tv/helix/")
                .map_err(|_| ProviderError::InvalidConfiguration("Twitch Helix base URL"))?,
            oauth_token_url: Url::parse("https://id.twitch.tv/oauth2/token")
                .map_err(|_| ProviderError::InvalidConfiguration("Twitch OAuth URL"))?,
            request_timeout: Duration::from_secs(10),
            eventsub_tolerance: DEFAULT_EVENTSUB_TOLERANCE,
        })
    }

    /// Overrides trusted operator configuration, primarily for local contract tests.
    #[must_use]
    pub fn with_endpoints(mut self, helix_base_url: Url, oauth_token_url: Url) -> Self {
        self.helix_base_url = helix_base_url;
        self.oauth_token_url = oauth_token_url;
        self
    }

    #[must_use]
    pub const fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    #[must_use]
    pub const fn with_eventsub_tolerance(mut self, tolerance: Duration) -> Self {
        self.eventsub_tolerance = tolerance;
        self
    }
}

struct CachedToken {
    value: String,
    expires_at: Instant,
}

pub struct TwitchProvider {
    client: Client,
    config: TwitchConfig,
    token: RwLock<Option<Arc<CachedToken>>>,
}

impl TwitchProvider {
    pub fn new(config: TwitchConfig) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(config.request_timeout)
            .build()
            .map_err(|source| ProviderError::Transport {
                provider: ProviderKind::Twitch,
                source,
            })?;
        Ok(Self {
            client,
            config,
            token: RwLock::new(None),
        })
    }

    fn credentials(&self) -> Result<&TwitchCredentials, ProviderError> {
        self.config
            .credentials
            .as_ref()
            .ok_or(ProviderError::Disabled {
                provider: ProviderKind::Twitch,
                reason: "credentials are not configured",
            })
    }

    async fn app_access_token(&self) -> Result<Arc<CachedToken>, ProviderError> {
        if let Some(value) = self
            .token
            .read()
            .map_err(|_| ProviderError::StateLockPoisoned)?
            .as_ref()
            .filter(|token| token.expires_at > Instant::now())
            .map(Arc::clone)
        {
            return Ok(value);
        }

        let credentials = self.credentials()?;
        let body = form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", credentials.client_id.expose_secret())
            .append_pair("client_secret", credentials.client_secret.expose_secret())
            .append_pair("grant_type", "client_credentials")
            .finish();
        let response = self
            .client
            .post(self.config.oauth_token_url.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|source| ProviderError::Transport {
                provider: ProviderKind::Twitch,
                source,
            })?;
        let token: TokenResponse = decode_response(response, ProviderKind::Twitch).await?;
        if token.access_token.is_empty() {
            return Err(ProviderError::InvalidResponse {
                provider: ProviderKind::Twitch,
                reason: "OAuth response contains an empty access token",
            });
        }
        let usable_for =
            Duration::from_secs(token.expires_in).saturating_sub(TOKEN_EXPIRY_SAFETY_MARGIN);
        let expires_at = Instant::now() + usable_for;
        let cached_token = Arc::new(CachedToken {
            value: token.access_token,
            expires_at,
        });
        self.token
            .write()
            .map_err(|_| ProviderError::StateLockPoisoned)?
            .replace(Arc::clone(&cached_token));
        Ok(cached_token)
    }

    fn invalidate_app_access_token(
        &self,
        rejected_token: &Arc<CachedToken>,
    ) -> Result<(), ProviderError> {
        let mut cached = self
            .token
            .write()
            .map_err(|_| ProviderError::StateLockPoisoned)?;
        if cached
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, rejected_token))
        {
            *cached = None;
        }
        Ok(())
    }

    async fn helix_get<T>(&self, path: &str, query: &[(String, String)]) -> Result<T, ProviderError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let token = self.app_access_token().await?;
        match self
            .helix_get_with_token(path, query, token.value.as_str())
            .await
        {
            Err(ProviderError::RejectedHttp {
                provider: ProviderKind::Twitch,
                status: 401,
            }) => self.invalidate_app_access_token(&token)?,
            result => return result,
        }

        let retry_token = self.app_access_token().await?;
        let result = self
            .helix_get_with_token(path, query, retry_token.value.as_str())
            .await;
        if matches!(
            &result,
            Err(ProviderError::RejectedHttp {
                provider: ProviderKind::Twitch,
                status: 401,
            })
        ) {
            self.invalidate_app_access_token(&retry_token)?;
        }
        result
    }

    async fn helix_get_with_token<T>(
        &self,
        path: &str,
        query: &[(String, String)],
        token: &str,
    ) -> Result<T, ProviderError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let credentials = self.credentials()?;
        let mut url = self
            .config
            .helix_base_url
            .join(path)
            .map_err(|_| ProviderError::InvalidConfiguration("Twitch Helix endpoint"))?;
        url.query_pairs_mut().extend_pairs(query);
        let response = self
            .client
            .get(url)
            .header("Client-Id", credentials.client_id.expose_secret())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|source| ProviderError::Transport {
                provider: ProviderKind::Twitch,
                source,
            })?;
        decode_response(response, ProviderKind::Twitch).await
    }

    pub fn verify_eventsub(
        &self,
        headers: &TwitchEventSubHeaders<'_>,
        raw_body: &[u8],
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, TwitchWebhookError> {
        let credentials = self
            .config
            .credentials
            .as_ref()
            .ok_or(TwitchWebhookError::Disabled)?;
        verify_eventsub_signature(
            credentials.eventsub_secret.expose_secret(),
            headers,
            raw_body,
            now,
            self.config.eventsub_tolerance,
        )
    }

    pub fn parse_stream_event(raw_body: &[u8]) -> Result<EventSubStreamEvent, TwitchWebhookError> {
        let envelope: EventSubEnvelope =
            serde_json::from_slice(raw_body).map_err(|_| TwitchWebhookError::InvalidPayload)?;
        let status = match envelope.subscription.event_type.as_str() {
            "stream.online" => LiveStatus::Online,
            "stream.offline" => LiveStatus::Offline,
            _ => return Err(TwitchWebhookError::UnsupportedEventType),
        };
        Ok(EventSubStreamEvent {
            provider_channel_id: envelope.event.broadcaster_user_id,
            handle: envelope.event.broadcaster_user_login,
            provider_session_id: envelope.event.stream_id,
            status,
            occurred_at: envelope.event.started_at,
        })
    }

    async fn get_streams(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError> {
        if channels.len() > 100 {
            return Err(ProviderError::InvalidInput {
                provider: ProviderKind::Twitch,
                field: "channels",
                reason: "one Helix request accepts at most 100 channels",
            });
        }
        for channel in channels {
            validate_twitch_channel_record(channel)?;
        }
        let query = channels
            .iter()
            .map(|channel| ("user_id".to_owned(), channel.provider_channel_id.clone()))
            .collect::<Vec<_>>();
        // A reconciliation response describes a snapshot taken no later than the
        // request that produced it. Capturing before I/O ensures an EventSub
        // delivery received while the request is in flight remains newer.
        let observed_at = Utc::now();
        let response: HelixData<TwitchStream> = self.helix_get("streams", &query).await?;
        let streams = response
            .data
            .into_iter()
            .map(|stream| (stream.user_id.clone(), stream))
            .collect::<HashMap<_, _>>();
        channels
            .iter()
            .map(|channel| {
                let Some(stream) = streams.get(&channel.provider_channel_id) else {
                    return Ok(LiveState::new(channel, LiveStatus::Offline, observed_at));
                };
                let mut state = LiveState::new(channel, LiveStatus::Online, observed_at);
                state.handle = Some(stream.user_login.clone());
                state.provider_session_id = Some(stream.id.clone());
                state.title = Some(stream.title.clone());
                state.category = nonempty(stream.game_name.clone());
                state.thumbnail_url = parse_twitch_thumbnail(&stream.thumbnail_url);
                state.started_at = Some(stream.started_at);
                Ok(state)
            })
            .collect()
    }
}

#[async_trait]
impl StreamProvider for TwitchProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Twitch
    }

    fn embed_capability(&self) -> EmbedCapability {
        EmbedCapability::OfficialEmbed
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
        let (query_key, query_value) = if let Some(handle) = input.handle.as_deref() {
            validate_twitch_channel(handle).map_err(|_| invalid_twitch_identifier("handle"))?;
            ("login", handle.to_ascii_lowercase())
        } else if input
            .provider_channel_id
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            && !input.provider_channel_id.is_empty()
        {
            ("id", input.provider_channel_id.clone())
        } else {
            validate_twitch_channel(&input.provider_channel_id)
                .map_err(|_| invalid_twitch_identifier("provider_channel_id"))?;
            ("login", input.provider_channel_id.to_ascii_lowercase())
        };
        let response: HelixData<TwitchUser> = self
            .helix_get("users", &[(query_key.to_owned(), query_value)])
            .await?;
        let user = response
            .data
            .into_iter()
            .next()
            .ok_or(ProviderError::NotFound {
                provider: ProviderKind::Twitch,
            })?;
        let canonical_url = twitch_channel_url(&user.login)?;
        Ok(VerifiedChannel {
            provider: ProviderKind::Twitch,
            provider_channel_id: user.id,
            handle: Some(user.login),
            display_name: Some(user.display_name),
            canonical_url,
            current_live_url: None,
            thumbnail_url: parse_https_url(&user.profile_image_url),
            embed_capability: EmbedCapability::OfficialEmbed,
        })
    }

    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError> {
        self.get_streams(std::slice::from_ref(channel))
            .await?
            .pop()
            .ok_or(ProviderError::InvalidResponse {
                provider: ProviderKind::Twitch,
                reason: "reconciliation omitted requested channel",
            })
    }

    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError> {
        let mut states = Vec::with_capacity(channels.len());
        for chunk in channels.chunks(100) {
            states.extend(self.get_streams(chunk).await?);
        }
        Ok(states)
    }

    fn external_url(&self, channel: &ExternalChannel) -> Url {
        channel.canonical_url.clone()
    }

    fn embed_descriptor(&self, live: &LiveState) -> Option<EmbedDescriptor> {
        if live.provider != ProviderKind::Twitch {
            return None;
        }
        let handle = live.handle.as_deref()?;
        TwitchEmbedDescriptor::new(handle, &self.config.parent)
            .ok()
            .map(EmbedDescriptor::Twitch)
    }
}

fn validate_twitch_channel_record(channel: &ExternalChannel) -> Result<(), ProviderError> {
    if channel.provider != ProviderKind::Twitch {
        return Err(ProviderError::InvalidInput {
            provider: ProviderKind::Twitch,
            field: "provider",
            reason: "channel belongs to a different provider",
        });
    }
    if channel.provider_channel_id.is_empty()
        || !channel
            .provider_channel_id
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_twitch_identifier("provider_channel_id"));
    }
    Ok(())
}

fn invalid_twitch_identifier(field: &'static str) -> ProviderError {
    ProviderError::InvalidInput {
        provider: ProviderKind::Twitch,
        field,
        reason: "must be a Twitch user ID or login",
    }
}

fn twitch_channel_url(login: &str) -> Result<Url, ProviderError> {
    validate_twitch_channel(login).map_err(|_| invalid_twitch_identifier("handle"))?;
    Url::parse(&format!("https://www.twitch.tv/{login}"))
        .map_err(|_| ProviderError::InvalidConfiguration("Twitch channel base URL"))
}

fn parse_twitch_thumbnail(raw: &str) -> Option<Url> {
    parse_https_url(&raw.replace("{width}", "640").replace("{height}", "360"))
}

fn parse_https_url(raw: &str) -> Option<Url> {
    Url::parse(raw)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host().is_some())
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

async fn decode_response<T>(response: Response, provider: ProviderKind) -> Result<T, ProviderError>
where
    T: for<'de> Deserialize<'de>,
{
    let status = response.status();
    let retry_after = parse_retry_after(response.headers());
    if status.as_u16() == 429 {
        return Err(ProviderError::RateLimited {
            provider,
            retry_after,
        });
    }
    if status.is_server_error() {
        return Err(ProviderError::TransientHttp {
            provider,
            status: status.as_u16(),
            retry_after,
        });
    }
    if !status.is_success() {
        return Err(ProviderError::RejectedHttp {
            provider,
            status: status.as_u16(),
        });
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|source| ProviderError::Transport { provider, source })?;
    serde_json::from_slice(&bytes).map_err(|_| ProviderError::InvalidResponse {
        provider,
        reason: "response is not valid expected JSON",
    })
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if let Some(seconds) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(seconds));
    }
    let reset = headers
        .get("ratelimit-reset")
        .and_then(|value| value.to_str().ok())?
        .parse::<u64>()
        .ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(Duration::from_secs(reset.saturating_sub(now)))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct HelixData<T> {
    data: Vec<T>,
}

#[derive(Deserialize)]
struct TwitchUser {
    id: String,
    login: String,
    display_name: String,
    profile_image_url: String,
}

#[derive(Deserialize)]
struct TwitchStream {
    id: String,
    user_id: String,
    user_login: String,
    game_name: String,
    title: String,
    thumbnail_url: String,
    started_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug)]
pub struct TwitchEventSubHeaders<'a> {
    pub message_id: &'a str,
    pub message_timestamp: &'a str,
    pub message_signature: &'a str,
}

/// Verifies Twitch's `message-id + timestamp + raw body` HMAC-SHA256 construction.
pub fn verify_eventsub_signature(
    secret: &str,
    headers: &TwitchEventSubHeaders<'_>,
    raw_body: &[u8],
    now: DateTime<Utc>,
    tolerance: Duration,
) -> Result<DateTime<Utc>, TwitchWebhookError> {
    if !(10..=100).contains(&secret.len()) || !secret.is_ascii() {
        return Err(TwitchWebhookError::InvalidSecret);
    }
    if headers.message_id.is_empty() {
        return Err(TwitchWebhookError::MissingMessageId);
    }
    let timestamp = DateTime::parse_from_rfc3339(headers.message_timestamp)
        .map_err(|_| TwitchWebhookError::InvalidTimestamp)?
        .with_timezone(&Utc);
    let tolerance =
        chrono::TimeDelta::from_std(tolerance).map_err(|_| TwitchWebhookError::InvalidTolerance)?;
    let age = now.signed_duration_since(timestamp);
    if age > tolerance || age < -tolerance {
        return Err(TwitchWebhookError::TimestampOutsideTolerance);
    }

    let signature = headers
        .message_signature
        .strip_prefix("sha256=")
        .ok_or(TwitchWebhookError::InvalidSignatureEncoding)?;
    let signature =
        hex::decode(signature).map_err(|_| TwitchWebhookError::InvalidSignatureEncoding)?;
    if signature.len() != 32 {
        return Err(TwitchWebhookError::InvalidSignatureEncoding);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| TwitchWebhookError::InvalidSecret)?;
    mac.update(headers.message_id.as_bytes());
    mac.update(headers.message_timestamp.as_bytes());
    mac.update(raw_body);
    mac.verify_slice(&signature)
        .map_err(|_| TwitchWebhookError::SignatureMismatch)?;
    Ok(timestamp)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventSubStreamEvent {
    pub provider_channel_id: String,
    pub handle: String,
    pub provider_session_id: Option<String>,
    pub status: LiveStatus,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TwitchWebhookError {
    #[error("Twitch provider is disabled")]
    Disabled,
    #[error("Twitch EventSub message ID is missing")]
    MissingMessageId,
    #[error("Twitch EventSub timestamp is invalid")]
    InvalidTimestamp,
    #[error("Twitch EventSub timestamp is outside the accepted window")]
    TimestampOutsideTolerance,
    #[error("Twitch EventSub signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("Twitch EventSub signature does not match")]
    SignatureMismatch,
    #[error("Twitch EventSub secret is invalid")]
    InvalidSecret,
    #[error("Twitch EventSub tolerance is invalid")]
    InvalidTolerance,
    #[error("Twitch EventSub JSON payload is invalid")]
    InvalidPayload,
    #[error("Twitch EventSub event type is not stream.online or stream.offline")]
    UnsupportedEventType,
}

#[derive(Deserialize)]
struct EventSubEnvelope {
    subscription: EventSubSubscription,
    event: EventSubEvent,
}

#[derive(Deserialize)]
struct EventSubSubscription {
    #[serde(rename = "type")]
    event_type: String,
}

#[derive(Deserialize)]
struct EventSubEvent {
    #[serde(rename = "id")]
    stream_id: Option<String>,
    broadcaster_user_id: String,
    broadcaster_user_login: String,
    started_at: Option<DateTime<Utc>>,
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

    fn sign(secret: &str, message_id: &str, timestamp: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message_id.as_bytes());
        mac.update(timestamp.as_bytes());
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verifies_valid_hmac_and_rejects_invalid_or_old_messages() {
        let secret = "eventsub-secret-for-tests";
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let timestamp = now.to_rfc3339();
        let body = br#"{"subscription":{"type":"stream.online"}}"#;
        let signature = sign(secret, "message-1", &timestamp, body);
        let headers = TwitchEventSubHeaders {
            message_id: "message-1",
            message_timestamp: &timestamp,
            message_signature: &signature,
        };
        assert_eq!(
            verify_eventsub_signature(secret, &headers, body, now, DEFAULT_EVENTSUB_TOLERANCE),
            Ok(now)
        );

        let invalid = TwitchEventSubHeaders {
            message_signature: "sha256=0000000000000000000000000000000000000000000000000000000000000000",
            ..headers
        };
        assert_eq!(
            verify_eventsub_signature(secret, &invalid, body, now, DEFAULT_EVENTSUB_TOLERANCE),
            Err(TwitchWebhookError::SignatureMismatch)
        );

        let old_timestamp = (now - chrono::TimeDelta::minutes(11)).to_rfc3339();
        let old_signature = sign(secret, "message-2", &old_timestamp, body);
        let old = TwitchEventSubHeaders {
            message_id: "message-2",
            message_timestamp: &old_timestamp,
            message_signature: &old_signature,
        };
        assert_eq!(
            verify_eventsub_signature(secret, &old, body, now, DEFAULT_EVENTSUB_TOLERANCE),
            Err(TwitchWebhookError::TimestampOutsideTolerance)
        );
    }

    fn test_config(server: &MockServer) -> TwitchConfig {
        let credentials =
            TwitchCredentials::new("client-id", "client-secret", "eventsub-secret").unwrap();
        TwitchConfig::new("rill3.example", Some(credentials))
            .unwrap()
            .with_endpoints(
                Url::parse(&format!("{}/helix/", server.uri())).unwrap(),
                Url::parse(&format!("{}/oauth2/token", server.uri())).unwrap(),
            )
    }

    fn channel() -> ExternalChannel {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        ExternalChannel {
            id: Uuid::nil(),
            creator_id: Uuid::nil(),
            provider: ProviderKind::Twitch,
            provider_channel_id: "141981764".to_owned(),
            handle: Some("twitchdev".to_owned()),
            canonical_url: Url::parse("https://www.twitch.tv/twitchdev").unwrap(),
            current_live_url: None,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: EmbedCapability::OfficialEmbed,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    async fn mount_token(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fixture-token",
                "expires_in": 3600,
                "token_type": "bearer"
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn mount_rotating_tokens(server: &MockServer, first: &str, second: &str) {
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": first,
                "expires_in": 3600,
                "token_type": "bearer"
            })))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": second,
                "expires_in": 3600,
                "token_type": "bearer"
            })))
            .with_priority(2)
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn helix_contract_normalizes_live_stream_and_reuses_token() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .and(query_param("user_id", "141981764"))
            .and(header("Client-Id", "client-id"))
            .and(header("authorization", "Bearer fixture-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": "stream-1",
                    "user_id": "141981764",
                    "user_login": "twitchdev",
                    "game_name": "Science & Technology",
                    "title": "Building RILL3",
                    "thumbnail_url": "https://static-cdn.jtvnw.net/previews/{width}x{height}.jpg",
                    "started_at": "2026-09-02T12:00:00Z"
                }],
                "pagination": {}
            })))
            .expect(2)
            .mount(&server)
            .await;

        let provider = TwitchProvider::new(test_config(&server)).unwrap();
        for _ in 0..2 {
            let state = provider.fetch_live_state(&channel()).await.unwrap();
            assert_eq!(state.status, LiveStatus::Online);
            assert_eq!(state.provider_session_id.as_deref(), Some("stream-1"));
            assert_eq!(state.thumbnail_url.unwrap().scheme(), "https");
        }
    }

    #[tokio::test]
    async fn helix_401_invalidates_cached_token_and_retries_once() {
        let server = MockServer::start().await;
        mount_rotating_tokens(&server, "rejected-token", "replacement-token").await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .and(header("authorization", "Bearer rejected-token"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .and(header("authorization", "Bearer replacement-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;

        let provider = TwitchProvider::new(test_config(&server)).unwrap();
        let state = provider.fetch_live_state(&channel()).await.unwrap();

        assert_eq!(state.status, LiveStatus::Offline);
        server.verify().await;
    }

    #[tokio::test]
    async fn repeated_helix_401_stops_after_one_retry_and_clears_retry_token() {
        let server = MockServer::start().await;
        mount_rotating_tokens(&server, "rejected-token", "also-rejected-token").await;
        for token in ["rejected-token", "also-rejected-token"] {
            Mock::given(method("GET"))
                .and(path("/helix/streams"))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(401))
                .expect(1)
                .mount(&server)
                .await;
        }

        let provider = TwitchProvider::new(test_config(&server)).unwrap();
        let error = provider.fetch_live_state(&channel()).await.unwrap_err();

        assert!(matches!(
            error,
            ProviderError::RejectedHttp {
                provider: ProviderKind::Twitch,
                status: 401
            }
        ));
        assert!(provider.token.read().unwrap().is_none());
        server.verify().await;
    }

    #[test]
    fn invalidating_an_old_token_does_not_clear_a_newer_cached_token() {
        let credentials =
            TwitchCredentials::new("client-id", "client-secret", "eventsub-secret").unwrap();
        let provider =
            TwitchProvider::new(TwitchConfig::new("rill3.example", Some(credentials)).unwrap())
                .unwrap();
        let rejected = Arc::new(CachedToken {
            value: "same-token-value".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(1),
        });
        provider
            .token
            .write()
            .unwrap()
            .replace(Arc::clone(&rejected));
        let replacement = Arc::new(CachedToken {
            value: "same-token-value".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(2),
        });
        provider
            .token
            .write()
            .unwrap()
            .replace(Arc::clone(&replacement));

        provider.invalidate_app_access_token(&rejected).unwrap();

        assert!(
            provider
                .token
                .read()
                .unwrap()
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, &replacement)),
            "a late 401 must not clear a newer token instance with the same value"
        );
    }

    #[tokio::test]
    async fn delayed_helix_snapshot_keeps_request_start_observation_time() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(120))
                    .set_body_json(serde_json::json!({"data": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let provider = TwitchProvider::new(test_config(&server)).unwrap();

        let test_channel = channel();
        let fetch = provider.fetch_live_state(&test_channel);
        let simulated_eventsub = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Utc::now()
        };
        let (state, eventsub_observed_at) = tokio::join!(fetch, simulated_eventsub);
        let state = state.unwrap();

        assert!(
            state.observed_at < eventsub_observed_at,
            "a snapshot returned after EventSub must retain its earlier request-start ordering"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn helix_429_is_a_typed_transient_error_with_retry_hint() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/streams"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "7")
                    .set_body_json(serde_json::json!({"error": "Too Many Requests"})),
            )
            .mount(&server)
            .await;
        let provider = TwitchProvider::new(test_config(&server)).unwrap();
        let error = provider.fetch_live_state(&channel()).await.unwrap_err();
        assert!(matches!(
            error,
            ProviderError::RateLimited {
                provider: ProviderKind::Twitch,
                retry_after: Some(delay)
            } if delay == Duration::from_secs(7)
        ));
    }

    #[tokio::test]
    async fn helix_users_contract_returns_canonical_verified_channel() {
        let server = MockServer::start().await;
        mount_token(&server).await;
        Mock::given(method("GET"))
            .and(path("/helix/users"))
            .and(query_param("login", "twitchdev"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": "141981764",
                    "login": "twitchdev",
                    "display_name": "TwitchDev",
                    "profile_image_url": "https://static-cdn.jtvnw.net/profile.png"
                }]
            })))
            .mount(&server)
            .await;
        let provider = TwitchProvider::new(test_config(&server)).unwrap();
        let verified = provider
            .verify_channel(VerifyChannelInput::new("twitchdev"))
            .await
            .unwrap();
        assert_eq!(verified.provider_channel_id, "141981764");
        assert_eq!(verified.handle.as_deref(), Some("twitchdev"));
        assert_eq!(
            verified.canonical_url.as_str(),
            "https://www.twitch.tv/twitchdev"
        );
    }
}
