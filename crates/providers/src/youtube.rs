use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, LiveStatus, ProviderAvailability,
    ProviderKind, VerifiedChannel, VerifyChannelInput, YouTubeEmbedDescriptor, manual_live_status,
    validate_youtube_origin, validate_youtube_video_id,
};
use secrecy::{ExposeSecret, SecretString};
use sha1::Sha1;
use sha2::Sha256;
use thiserror::Error;
use url::Url;

use crate::{ProviderError, StreamProvider};

/// Official `YouTube` channel-feed topic prefix used for `WebSub` subscriptions.
pub const YOUTUBE_WEBSUB_TOPIC_PREFIX: &str =
    "https://www.youtube.com/feeds/videos.xml?channel_id=";

#[derive(Clone, Debug)]
pub struct YouTubeConfig {
    trusted_origin: Url,
}

impl YouTubeConfig {
    pub fn new(trusted_origin: Url) -> Result<Self, ProviderError> {
        validate_youtube_origin(&trusted_origin).map_err(|_| {
            ProviderError::InvalidConfiguration(
                "YouTube origin must be HTTPS or loopback HTTP for local development",
            )
        })?;
        Ok(Self { trusted_origin })
    }

    #[must_use]
    pub fn trusted_origin(&self) -> &Url {
        &self.trusted_origin
    }
}

#[derive(Clone, Debug)]
pub struct YouTubeProvider {
    config: YouTubeConfig,
}

impl YouTubeProvider {
    #[must_use]
    pub const fn new(config: YouTubeConfig) -> Self {
        Self { config }
    }
}

/// M1 seam for an owner-authorized `liveBroadcasts` client. Implementations are
/// intentionally deferred until creator OAuth and encrypted token storage ship.
#[async_trait]
pub trait YouTubeOAuthLiveApi: Send + Sync {
    async fn fetch_owned_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError>;
}

fn validate_channel_id(channel_id: &str) -> Result<(), ProviderError> {
    let valid = channel_id.len() == 24
        && channel_id.starts_with("UC")
        && channel_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(ProviderError::InvalidInput {
            provider: ProviderKind::YouTube,
            field: "provider_channel_id",
            reason: "must be a canonical YouTube channel ID",
        })
    }
}

/// Extracts and validates a video ID only from documented `YouTube` URL shapes.
pub fn parse_youtube_video_id(url: &Url) -> Result<String, ProviderError> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err(invalid_live_url());
    }
    let host = url.host_str().ok_or_else(invalid_live_url)?;
    let video_id = match host.to_ascii_lowercase().as_str() {
        "youtu.be" => {
            let mut segments = url.path_segments().ok_or_else(invalid_live_url)?;
            let first = segments.next().ok_or_else(invalid_live_url)?;
            if segments.next().is_some() {
                return Err(invalid_live_url());
            }
            first.to_owned()
        }
        "youtube.com" | "www.youtube.com" | "m.youtube.com" => {
            let segments = url
                .path_segments()
                .ok_or_else(invalid_live_url)?
                .collect::<Vec<_>>();
            match segments.as_slice() {
                ["watch"] => url
                    .query_pairs()
                    .find_map(|(key, value)| (key == "v").then(|| value.into_owned()))
                    .ok_or_else(invalid_live_url)?,
                ["live" | "embed", video_id] => (*video_id).to_owned(),
                _ => return Err(invalid_live_url()),
            }
        }
        _ => return Err(invalid_live_url()),
    };
    validate_youtube_video_id(&video_id).map_err(|_| invalid_live_url())?;
    Ok(video_id)
}

fn invalid_live_url() -> ProviderError {
    ProviderError::InvalidInput {
        provider: ProviderKind::YouTube,
        field: "current_live_url",
        reason: "must be a supported HTTPS YouTube video URL",
    }
}

fn canonical_channel_url(channel_id: &str) -> Result<Url, ProviderError> {
    Url::parse(&format!("https://www.youtube.com/channel/{channel_id}"))
        .map_err(|_| ProviderError::InvalidConfiguration("YouTube channel base URL"))
}

fn canonical_video_url(video_id: &str) -> Result<Url, ProviderError> {
    let mut url = Url::parse("https://www.youtube.com/watch")
        .map_err(|_| ProviderError::InvalidConfiguration("YouTube video base URL"))?;
    url.query_pairs_mut().append_pair("v", video_id);
    Ok(url)
}

#[async_trait]
impl StreamProvider for YouTubeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YouTube
    }

    fn embed_capability(&self) -> EmbedCapability {
        EmbedCapability::OfficialEmbed
    }

    fn availability(&self) -> ProviderAvailability {
        ProviderAvailability::Enabled
    }

    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError> {
        validate_channel_id(&input.provider_channel_id)?;
        let current_video = input
            .current_live_url
            .map(|raw| {
                let url = Url::parse(&raw).map_err(|_| invalid_live_url())?;
                parse_youtube_video_id(&url)
            })
            .transpose()?;
        let current_live_url = current_video
            .as_deref()
            .map(canonical_video_url)
            .transpose()?;

        Ok(VerifiedChannel {
            provider: ProviderKind::YouTube,
            provider_channel_id: input.provider_channel_id.clone(),
            handle: input.handle,
            display_name: None,
            canonical_url: canonical_channel_url(&input.provider_channel_id)?,
            current_live_url,
            thumbnail_url: None,
            embed_capability: EmbedCapability::OfficialEmbed,
        })
    }

    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError> {
        if channel.provider != ProviderKind::YouTube {
            return Err(ProviderError::InvalidInput {
                provider: ProviderKind::YouTube,
                field: "provider",
                reason: "channel belongs to a different provider",
            });
        }
        let observed_at = Utc::now();
        let status = manual_live_status(channel, observed_at);
        if status != LiveStatus::Online {
            return Ok(LiveState::new(channel, status, observed_at));
        }
        // A stored URL alone is not evidence that a broadcast is still live.
        // Online requires an explicit manual observation with a future expiry.
        let Some(live_url) = channel.current_live_url.as_ref() else {
            return Ok(LiveState::new(channel, LiveStatus::Unknown, observed_at));
        };
        let video_id = parse_youtube_video_id(live_url)?;
        let mut state = LiveState::new(channel, LiveStatus::Online, observed_at);
        state.provider_session_id = Some(video_id);
        Ok(state)
    }

    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError> {
        let mut states = Vec::with_capacity(channels.len());
        for channel in channels {
            states.push(self.fetch_live_state(channel).await?);
        }
        Ok(states)
    }

    fn external_url(&self, channel: &ExternalChannel) -> Url {
        channel.canonical_url.clone()
    }

    fn embed_descriptor(&self, live: &LiveState) -> Option<EmbedDescriptor> {
        if live.provider != ProviderKind::YouTube || live.status != LiveStatus::Online {
            return None;
        }
        let video_id = live.provider_session_id.as_deref()?;
        YouTubeEmbedDescriptor::new(video_id, &self.config.trusted_origin)
            .ok()
            .map(EmbedDescriptor::YouTube)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebSubChallenge {
    pub mode: String,
    pub topic: String,
    pub challenge: String,
    pub verify_token: String,
    pub lease_seconds: Option<u64>,
}

pub struct YouTubeWebSubVerifier {
    verify_token: SecretString,
    hub_secret: Option<SecretString>,
    allowed_topics: BTreeSet<String>,
}

impl YouTubeWebSubVerifier {
    #[must_use]
    pub fn new(
        verify_token: SecretString,
        allowed_topics: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            verify_token,
            hub_secret: None,
            allowed_topics: allowed_topics.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn with_hub_secret(mut self, hub_secret: SecretString) -> Self {
        self.hub_secret = Some(hub_secret);
        self
    }

    pub fn verify<'challenge>(
        &self,
        challenge: &'challenge WebSubChallenge,
    ) -> Result<&'challenge str, WebSubError> {
        if challenge.verify_token != self.verify_token.expose_secret() {
            return Err(WebSubError::InvalidToken);
        }
        if !matches!(challenge.mode.as_str(), "subscribe" | "unsubscribe") {
            return Err(WebSubError::InvalidMode);
        }
        if !self.allowed_topics.contains(&challenge.topic) {
            return Err(WebSubError::UnexpectedTopic);
        }
        if challenge.challenge.is_empty() || challenge.challenge.len() > 1024 {
            return Err(WebSubError::InvalidChallenge);
        }
        Ok(&challenge.challenge)
    }

    pub fn verify_notification(
        &self,
        signature_header: &str,
        raw_body: &[u8],
    ) -> Result<(), WebSubSignatureError> {
        let secret = self
            .hub_secret
            .as_ref()
            .ok_or(WebSubSignatureError::MissingSecret)?;
        verify_websub_signature(secret.expose_secret(), signature_header, raw_body)
    }
}

/// Verifies a `WebSub` `X-Hub-Signature` over the exact raw callback body.
/// SHA-256 is preferred; SHA-1 remains supported for hub interoperability.
pub fn verify_websub_signature(
    secret: &str,
    signature_header: &str,
    raw_body: &[u8],
) -> Result<(), WebSubSignatureError> {
    if secret.is_empty() {
        return Err(WebSubSignatureError::MissingSecret);
    }
    let (algorithm, encoded_signature) = signature_header
        .split_once('=')
        .ok_or(WebSubSignatureError::Malformed)?;
    if encoded_signature.is_empty() || encoded_signature.contains('=') {
        return Err(WebSubSignatureError::Malformed);
    }
    let signature = hex::decode(encoded_signature).map_err(|_| WebSubSignatureError::Malformed)?;
    match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => {
            if signature.len() != 32 {
                return Err(WebSubSignatureError::Malformed);
            }
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                .map_err(|_| WebSubSignatureError::MissingSecret)?;
            mac.update(raw_body);
            mac.verify_slice(&signature)
                .map_err(|_| WebSubSignatureError::Mismatch)
        }
        "sha1" => {
            if signature.len() != 20 {
                return Err(WebSubSignatureError::Malformed);
            }
            let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
                .map_err(|_| WebSubSignatureError::MissingSecret)?;
            mac.update(raw_body);
            mac.verify_slice(&signature)
                .map_err(|_| WebSubSignatureError::Mismatch)
        }
        _ => Err(WebSubSignatureError::UnsupportedAlgorithm),
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WebSubError {
    #[error("WebSub verify token does not match")]
    InvalidToken,
    #[error("WebSub mode is invalid")]
    InvalidMode,
    #[error("WebSub topic is not registered")]
    UnexpectedTopic,
    #[error("WebSub challenge is empty or too large")]
    InvalidChallenge,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WebSubSignatureError {
    #[error("WebSub hub secret is not configured")]
    MissingSecret,
    #[error("WebSub signature header is malformed")]
    Malformed,
    #[error("WebSub signature algorithm is unsupported")]
    UnsupportedAlgorithm,
    #[error("WebSub signature does not match the raw callback body")]
    Mismatch,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use rill3_domain::VerificationState;
    use uuid::Uuid;

    use super::*;

    fn channel() -> ExternalChannel {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        ExternalChannel {
            id: Uuid::nil(),
            creator_id: Uuid::nil(),
            provider: ProviderKind::YouTube,
            provider_channel_id: "UC0123456789abcdefghijkl".to_owned(),
            handle: None,
            canonical_url: Url::parse("https://www.youtube.com/channel/UC0123456789abcdefghijkl")
                .unwrap(),
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

    fn provider() -> YouTubeProvider {
        YouTubeProvider::new(
            YouTubeConfig::new(Url::parse("https://example.com").unwrap()).unwrap(),
        )
    }

    #[tokio::test]
    async fn manual_state_requires_explicit_unexpired_online_status_and_video() {
        let provider = provider();
        let now = Utc::now();
        for manual_status in [
            None,
            Some(LiveStatus::Online),
            Some(LiveStatus::Offline),
            Some(LiveStatus::Unknown),
        ] {
            for expires_at in [
                None,
                Some(now - Duration::seconds(1)),
                Some(now + Duration::hours(1)),
            ] {
                for live_url in [
                    None,
                    Some(Url::parse("https://youtu.be/dQw4w9WgXcQ").unwrap()),
                ] {
                    let channel = ExternalChannel {
                        manual_live_status: manual_status,
                        manual_state_expires_at: expires_at,
                        current_live_url: live_url.clone(),
                        ..channel()
                    };
                    let expected = match (manual_status, expires_at, live_url.is_some()) {
                        (Some(LiveStatus::Online), Some(expiry), true) if expiry > now => {
                            LiveStatus::Online
                        }
                        (Some(LiveStatus::Offline), Some(expiry), _) if expiry > now => {
                            LiveStatus::Offline
                        }
                        (Some(LiveStatus::Offline), None, _) => LiveStatus::Offline,
                        _ => LiveStatus::Unknown,
                    };
                    let state = provider.fetch_live_state(&channel).await.unwrap();
                    assert_eq!(
                        state.status, expected,
                        "status={manual_status:?}, expiry={expires_at:?}, url={live_url:?}"
                    );
                    assert_eq!(
                        state.provider_session_id.as_deref(),
                        (expected == LiveStatus::Online).then_some("dQw4w9WgXcQ")
                    );
                    assert_eq!(
                        provider.embed_descriptor(&state).is_some(),
                        expected == LiveStatus::Online
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn online_state_validates_video_but_inactive_state_does_not_use_it() {
        let provider = provider();
        let mut channel = ExternalChannel {
            manual_live_status: Some(LiveStatus::Online),
            manual_state_expires_at: Some(Utc::now() + Duration::hours(1)),
            current_live_url: Some(Url::parse("https://example.com/watch?v=dQw4w9WgXcQ").unwrap()),
            ..channel()
        };
        assert!(provider.fetch_live_state(&channel).await.is_err());
        for status in [LiveStatus::Offline, LiveStatus::Unknown] {
            channel.manual_live_status = Some(status);
            let state = provider.fetch_live_state(&channel).await.unwrap();
            assert_eq!(state.status, status);
            assert!(state.provider_session_id.is_none());
        }
    }

    #[test]
    fn parses_only_official_youtube_video_urls() {
        for raw in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                parse_youtube_video_id(&Url::parse(raw).unwrap()).unwrap(),
                "dQw4w9WgXcQ"
            );
        }
        for raw in [
            "https://evil.test/watch?v=dQw4w9WgXcQ",
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=../../etc",
        ] {
            assert!(parse_youtube_video_id(&Url::parse(raw).unwrap()).is_err());
        }
    }

    #[test]
    fn websub_challenge_requires_token_and_known_topic() {
        let topic = format!("{YOUTUBE_WEBSUB_TOPIC_PREFIX}UC123");
        let verifier = YouTubeWebSubVerifier::new(
            SecretString::from("secret-token".to_owned()),
            [topic.clone()],
        );
        let challenge = WebSubChallenge {
            mode: "subscribe".to_owned(),
            topic,
            challenge: "challenge-value".to_owned(),
            verify_token: "secret-token".to_owned(),
            lease_seconds: Some(86_400),
        };
        assert_eq!(verifier.verify(&challenge), Ok("challenge-value"));

        let mut invalid = challenge;
        invalid.verify_token = "attacker".to_owned();
        assert_eq!(verifier.verify(&invalid), Err(WebSubError::InvalidToken));
    }

    #[test]
    fn websub_rejects_the_legacy_xml_topic_path() {
        let official_topic = format!("{YOUTUBE_WEBSUB_TOPIC_PREFIX}UC123");
        let verifier = YouTubeWebSubVerifier::new(
            SecretString::from("secret-token".to_owned()),
            [official_topic.clone()],
        );
        let challenge = WebSubChallenge {
            mode: "subscribe".to_owned(),
            topic: official_topic,
            challenge: "challenge-value".to_owned(),
            verify_token: "secret-token".to_owned(),
            lease_seconds: Some(86_400),
        };
        assert_eq!(verifier.verify(&challenge), Ok("challenge-value"));

        let legacy = WebSubChallenge {
            topic: "https://www.youtube.com/xml/feeds/videos.xml?channel_id=UC123".to_owned(),
            ..challenge
        };
        assert_eq!(verifier.verify(&legacy), Err(WebSubError::UnexpectedTopic));
    }

    fn sha256_signature(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn sha1_signature(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha1={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn websub_raw_body_hmac_supports_sha256_and_sha1() {
        let secret = "youtube-hub-secret";
        let body = br#"<?xml version="1.0"?><feed><title>RILL3</title></feed>"#;
        assert_eq!(
            verify_websub_signature(secret, &sha256_signature(secret, body), body),
            Ok(())
        );
        assert_eq!(
            verify_websub_signature(secret, &sha1_signature(secret, body), body),
            Ok(())
        );
    }

    #[test]
    fn websub_raw_body_hmac_has_typed_failures() {
        let body = b"notification";
        assert_eq!(
            verify_websub_signature("secret", "not-a-signature", body),
            Err(WebSubSignatureError::Malformed)
        );
        assert_eq!(
            verify_websub_signature("secret", "md5=0000", body),
            Err(WebSubSignatureError::UnsupportedAlgorithm)
        );
        assert_eq!(
            verify_websub_signature(
                "secret",
                "sha256=0000000000000000000000000000000000000000000000000000000000000000",
                body,
            ),
            Err(WebSubSignatureError::Mismatch)
        );
    }
}
