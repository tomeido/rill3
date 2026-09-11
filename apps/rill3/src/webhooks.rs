use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use quick_xml::{Reader, events::Event};
use rill3_domain::{LiveState, LiveStatus, ProviderEvent, ProviderEventStatus, ProviderKind};
use rill3_providers::{
    TwitchEventSubHeaders, TwitchWebhookError, WebSubChallenge, YOUTUBE_WEBSUB_TOPIC_PREFIX,
    YouTubeWebSubVerifier, verify_eventsub_signature,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::http::HttpState;

const EVENTSUB_TOLERANCE: std::time::Duration = std::time::Duration::from_mins(10);

#[derive(Clone)]
pub(crate) struct WebhookConfig {
    pub(crate) max_body_bytes: usize,
    twitch_secret: Option<Arc<SecretString>>,
    youtube_verify_token: Option<Arc<SecretString>>,
    youtube_hub_secret: Option<Arc<SecretString>>,
    youtube_topic_prefix: Arc<str>,
}

impl WebhookConfig {
    pub(crate) fn new(
        max_body_bytes: usize,
        twitch_secret: Option<String>,
        youtube_verify_token: Option<String>,
        youtube_hub_secret: Option<String>,
        youtube_topic_prefix: String,
    ) -> Self {
        Self {
            max_body_bytes,
            twitch_secret: twitch_secret
                .filter(|value| !value.is_empty())
                .map(|value| Arc::new(SecretString::from(value))),
            youtube_verify_token: youtube_verify_token
                .filter(|value| !value.is_empty())
                .map(|value| Arc::new(SecretString::from(value))),
            youtube_hub_secret: youtube_hub_secret
                .filter(|value| !value.is_empty())
                .map(|value| Arc::new(SecretString::from(value))),
            youtube_topic_prefix: Arc::from(youtube_topic_prefix),
        }
    }
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self::new(
            65_536,
            None,
            None,
            None,
            YOUTUBE_WEBSUB_TOPIC_PREFIX.to_owned(),
        )
    }
}

pub(crate) fn router(max_body_bytes: usize) -> Router<HttpState> {
    Router::new()
        .route("/webhooks/twitch", post(twitch_eventsub))
        .route(
            "/webhooks/youtube",
            get(youtube_challenge).post(youtube_notification),
        )
        .layer(DefaultBodyLimit::max(max_body_bytes))
}

async fn twitch_eventsub(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.webhooks.twitch_secret.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(eventsub_headers) = eventsub_headers(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = Utc::now();
    let observed_at = match verify_eventsub_signature(
        secret.expose_secret(),
        &eventsub_headers,
        &body,
        now,
        EVENTSUB_TOLERANCE,
    ) {
        Ok(timestamp) => timestamp,
        Err(error) => {
            tracing::warn!(reason = %error, "rejected Twitch EventSub delivery");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    match header_str(&headers, "twitch-eventsub-message-type") {
        Some("webhook_callback_verification") => twitch_challenge(&body),
        Some("notification") => {
            apply_twitch_notification(&state, &eventsub_headers, &body, observed_at, now).await
        }
        Some("revocation") => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn twitch_challenge(body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct ChallengeEnvelope {
        challenge: String,
    }

    match serde_json::from_slice::<ChallengeEnvelope>(body) {
        Ok(value) if !value.challenge.is_empty() && value.challenge.len() <= 1024 => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            value.challenge,
        )
            .into_response(),
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn apply_twitch_notification(
    state: &HttpState,
    headers: &TwitchEventSubHeaders<'_>,
    body: &[u8],
    observed_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> Response {
    let event = match rill3_providers::TwitchProvider::parse_stream_event(body) {
        Ok(event) => event,
        Err(TwitchWebhookError::UnsupportedEventType) => {
            return StatusCode::NO_CONTENT.into_response();
        }
        Err(error) => {
            tracing::warn!(reason = %error, "invalid Twitch EventSub body");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let channel = match state
        .store
        .channel_by_provider_id(ProviderKind::Twitch, &event.provider_channel_id)
        .await
    {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            tracing::info!(
                provider_channel_id = %event.provider_channel_id,
                "ignored EventSub delivery for an unregistered channel"
            );
            return StatusCode::NO_CONTENT.into_response();
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to resolve EventSub channel");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    let mut live = LiveState::new(&channel, event.status, observed_at);
    live.handle = Some(event.handle);
    live.provider_session_id = event.provider_session_id;
    live.started_at = (event.status == LiveStatus::Online)
        .then_some(event.occurred_at)
        .flatten();
    let provider_event = ProviderEvent {
        provider: ProviderKind::Twitch,
        external_event_id: headers.message_id.to_owned(),
        channel_id: Some(channel.id),
        event_type: match event.status {
            LiveStatus::Online => "stream.online",
            LiveStatus::Offline => "stream.offline",
            LiveStatus::Unknown => "stream.unknown",
        }
        .to_owned(),
        occurred_at: observed_at,
        received_at,
        payload_hash: digest_hex(body),
        handled_at: None,
        status: ProviderEventStatus::Received,
    };

    match state
        .store
        .apply_provider_event(&provider_event, &live)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to persist EventSub delivery");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

#[derive(Deserialize)]
struct WebSubQuery {
    #[serde(rename = "hub.mode")]
    mode: String,
    #[serde(rename = "hub.topic")]
    topic: String,
    #[serde(rename = "hub.challenge")]
    challenge: String,
    #[serde(rename = "hub.verify_token")]
    verify_token: String,
    #[serde(rename = "hub.lease_seconds")]
    lease_seconds: Option<u64>,
}

async fn youtube_challenge(
    State(state): State<HttpState>,
    Query(query): Query<WebSubQuery>,
) -> Response {
    let Some(token) = state.webhooks.youtube_verify_token.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(channel_id) = query
        .topic
        .strip_prefix(state.webhooks.youtube_topic_prefix.as_ref())
        .filter(|value| valid_youtube_channel_id(value))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match state
        .store
        .channel_by_provider_id(ProviderKind::YouTube, channel_id)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to resolve WebSub topic");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }

    let challenge = WebSubChallenge {
        mode: query.mode,
        topic: query.topic.clone(),
        challenge: query.challenge,
        verify_token: query.verify_token,
        lease_seconds: query.lease_seconds,
    };
    let verifier = YouTubeWebSubVerifier::new(
        SecretString::from(token.expose_secret().to_owned()),
        [query.topic],
    );
    match verifier.verify(&challenge) {
        Ok(response) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            response.to_owned(),
        )
            .into_response(),
        Err(error) => {
            tracing::warn!(reason = %error, "rejected YouTube WebSub challenge");
            StatusCode::UNAUTHORIZED.into_response()
        }
    }
}

async fn youtube_notification(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.webhooks.youtube_hub_secret.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(signature) = header_str(&headers, "x-hub-signature") else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Err(error) =
        rill3_providers::verify_websub_signature(secret.expose_secret(), signature, &body)
    {
        tracing::warn!(reason = %error, "rejected YouTube WebSub notification");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let Ok(notification) = parse_youtube_feed(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let channel = match state
        .store
        .channel_by_provider_id(ProviderKind::YouTube, &notification.channel_id)
        .await
    {
        Ok(Some(channel)) => channel,
        Ok(None) => return StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to resolve WebSub notification channel");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let now = Utc::now();
    let payload_hash = digest_hex(&body);
    let event = ProviderEvent {
        provider: ProviderKind::YouTube,
        external_event_id: format!("websub-{payload_hash}"),
        channel_id: Some(channel.id),
        event_type: "websub.change".to_owned(),
        occurred_at: notification.updated.unwrap_or(now),
        received_at: now,
        payload_hash,
        handled_at: None,
        status: ProviderEventStatus::Received,
    };
    match state.store.record_provider_signal(&event).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to persist WebSub signal");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

struct FeedNotification {
    channel_id: String,
    updated: Option<DateTime<Utc>>,
}

fn parse_youtube_feed(body: &[u8]) -> Result<FeedNotification, ()> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut channel_id = None;
    let mut updated = None;
    loop {
        match reader.read_event().map_err(|_| ())? {
            Event::Start(element) if element.local_name().as_ref() == "channelId" => {
                let text = reader.read_text(element.name()).map_err(|_| ())?;
                if valid_youtube_channel_id(text.as_ref()) {
                    channel_id = Some(text.into_inner().into_owned());
                } else {
                    return Err(());
                }
            }
            Event::Start(element) if element.local_name().as_ref() == "updated" => {
                let text = reader.read_text(element.name()).map_err(|_| ())?;
                updated = DateTime::parse_from_rfc3339(text.as_ref())
                    .ok()
                    .map(|value| value.with_timezone(&Utc));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    channel_id
        .map(|channel_id| FeedNotification {
            channel_id,
            updated,
        })
        .ok_or(())
}

fn eventsub_headers(headers: &HeaderMap) -> Result<TwitchEventSubHeaders<'_>, ()> {
    Ok(TwitchEventSubHeaders {
        message_id: header_str(headers, "twitch-eventsub-message-id").ok_or(())?,
        message_timestamp: header_str(headers, "twitch-eventsub-message-timestamp").ok_or(())?,
        message_signature: header_str(headers, "twitch-eventsub-message-signature").ok_or(())?,
    })
}

fn header_str<'headers>(headers: &'headers HeaderMap, name: &str) -> Option<&'headers str> {
    headers.get(name)?.to_str().ok()
}

fn valid_youtube_channel_id(value: &str) -> bool {
    value.len() == 24
        && value.starts_with("UC")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn digest_hex(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_youtube_atom_shape() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
          <feed xmlns:yt="http://www.youtube.com/xml/schemas/2015">
            <entry><yt:videoId>dQw4w9WgXcQ</yt:videoId>
            <yt:channelId>UC_x5XG1OV2P6uZZ5FSM9Ttw</yt:channelId>
            <updated>2026-09-02T12:00:00Z</updated></entry>
          </feed>"#;
        let parsed = parse_youtube_feed(body).unwrap();
        assert_eq!(parsed.channel_id, "UC_x5XG1OV2P6uZZ5FSM9Ttw");
        assert_eq!(
            parsed.updated.unwrap().to_rfc3339(),
            "2026-09-02T12:00:00+00:00"
        );
    }

    #[test]
    fn rejects_feed_without_a_canonical_channel_id() {
        assert!(parse_youtube_feed(b"<feed><channelId>attacker</channelId></feed>").is_err());
    }

    #[test]
    fn default_webhook_config_uses_the_official_youtube_topic() {
        let config = WebhookConfig::default();
        assert_eq!(
            config.youtube_topic_prefix.as_ref(),
            YOUTUBE_WEBSUB_TOPIC_PREFIX
        );
    }
}
