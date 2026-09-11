use async_trait::async_trait;
use chrono::Utc;
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, LiveStatus, ProviderKind,
    VerifiedChannel, VerifyChannelInput, manual_live_status,
};
use url::Url;

use crate::{ProviderError, StreamProvider, validate_external_https_url};

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkOnlyProvider;

fn validate_identifier(identifier: &str) -> Result<(), ProviderError> {
    if identifier.is_empty() || identifier.len() > 128 || identifier.chars().any(char::is_control) {
        return Err(ProviderError::InvalidInput {
            provider: ProviderKind::LinkOnly,
            field: "provider_channel_id",
            reason: "must be 1..=128 non-control characters",
        });
    }
    Ok(())
}

#[async_trait]
impl StreamProvider for LinkOnlyProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::LinkOnly
    }

    fn embed_capability(&self) -> EmbedCapability {
        EmbedCapability::LinkOnly
    }

    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError> {
        validate_identifier(&input.provider_channel_id)?;
        let raw_url = input.external_url.ok_or(ProviderError::InvalidInput {
            provider: ProviderKind::LinkOnly,
            field: "external_url",
            reason: "is required",
        })?;
        let canonical_url = validate_external_https_url(&raw_url)?;
        let current_live_url = input
            .current_live_url
            .map(|raw| validate_external_https_url(&raw))
            .transpose()?;

        Ok(VerifiedChannel {
            provider: ProviderKind::LinkOnly,
            provider_channel_id: input.provider_channel_id,
            handle: input.handle,
            display_name: None,
            canonical_url,
            current_live_url,
            thumbnail_url: None,
            embed_capability: EmbedCapability::LinkOnly,
        })
    }

    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError> {
        if channel.provider != ProviderKind::LinkOnly {
            return Err(ProviderError::InvalidInput {
                provider: ProviderKind::LinkOnly,
                field: "provider",
                reason: "channel belongs to a different provider",
            });
        }
        let now = Utc::now();
        let status = manual_live_status(channel, now);
        if status == LiveStatus::Online
            && let Some(live_url) = channel.current_live_url.as_ref()
        {
            validate_external_https_url(live_url.as_str())?;
        }
        Ok(LiveState::new(channel, status, now))
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

    fn embed_descriptor(&self, _live: &LiveState) -> Option<EmbedDescriptor> {
        None
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn manual_online_requires_an_unexpired_state_and_safe_broadcast_url() {
        let provider = LinkOnlyProvider;
        let mut input = VerifyChannelInput::new("manual-channel");
        input.external_url = Some("https://video.example/channel".to_owned());
        input.current_live_url = Some("https://video.example/live".to_owned());
        let mut channel = ExternalChannel::from_verified(
            Uuid::new_v4(),
            Uuid::new_v4(),
            provider.verify_channel(input).await.unwrap(),
            Utc::now(),
        );
        channel.manual_live_status = Some(LiveStatus::Online);
        assert_eq!(
            provider.fetch_live_state(&channel).await.unwrap().status,
            LiveStatus::Unknown
        );
        channel.manual_state_expires_at = Some(Utc::now() + TimeDelta::hours(1));
        assert_eq!(
            provider.fetch_live_state(&channel).await.unwrap().status,
            LiveStatus::Online
        );
        channel.current_live_url = Some(Url::parse("https://127.0.0.1/live").unwrap());
        assert!(provider.fetch_live_state(&channel).await.is_err());
        channel.manual_live_status = Some(LiveStatus::Offline);
        assert_eq!(
            provider.fetch_live_state(&channel).await.unwrap().status,
            LiveStatus::Offline
        );
        channel.manual_state_expires_at = Some(Utc::now() - TimeDelta::seconds(1));
        assert_eq!(
            provider.fetch_live_state(&channel).await.unwrap().status,
            LiveStatus::Unknown
        );
    }

    #[tokio::test]
    async fn rejects_javascript_file_and_private_ip_urls() {
        let provider = LinkOnlyProvider;
        for raw in [
            "javascript:alert(1)",
            "file:///tmp/video",
            "https://192.168.1.2/watch",
        ] {
            let mut input = VerifyChannelInput::new("manual-channel");
            input.external_url = Some(raw.to_owned());
            assert!(
                provider.verify_channel(input).await.is_err(),
                "accepted {raw}"
            );
        }
    }
}
