use std::{collections::HashMap, sync::RwLock};

use async_trait::async_trait;
use chrono::Utc;
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, LiveStatus, ProviderAvailability,
    ProviderKind, StateApplyOutcome, StateTransitionError, VerifiedChannel, VerifyChannelInput,
};
use url::Url;

use crate::{ProviderError, StreamProvider};

/// Deterministic, network-free provider used by integration tests.
pub struct MockProvider {
    kind: ProviderKind,
    capability: EmbedCapability,
    states: RwLock<HashMap<String, LiveState>>,
}

impl MockProvider {
    #[must_use]
    pub fn new(kind: ProviderKind, capability: EmbedCapability) -> Self {
        Self {
            kind,
            capability,
            states: RwLock::new(HashMap::new()),
        }
    }

    /// Inserts the first observation or applies the domain timestamp ordering rule.
    pub fn apply_observation(
        &self,
        observation: LiveState,
    ) -> Result<StateApplyOutcome, MockStateError> {
        let mut states = self
            .states
            .write()
            .map_err(|_| MockStateError::LockPoisoned)?;
        let key = observation.provider_channel_id.clone();
        let Some(current) = states.get_mut(&key) else {
            states.insert(key, observation);
            return Ok(StateApplyOutcome::AppliedChanged);
        };
        current
            .apply_observation(observation)
            .map_err(MockStateError::Transition)
    }

    pub fn clear(&self) -> Result<(), MockStateError> {
        self.states
            .write()
            .map_err(|_| MockStateError::LockPoisoned)?
            .clear();
        Ok(())
    }

    fn state_for(&self, channel: &ExternalChannel) -> Result<LiveState, ProviderError> {
        if channel.provider != self.kind {
            return Err(ProviderError::InvalidInput {
                provider: self.kind,
                field: "provider",
                reason: "channel belongs to a different provider",
            });
        }
        self.states
            .read()
            .map_err(|_| ProviderError::StateLockPoisoned)?
            .get(&channel.provider_channel_id)
            .cloned()
            .map_or_else(
                || Ok(LiveState::new(channel, LiveStatus::Unknown, Utc::now())),
                Ok,
            )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MockStateError {
    #[error("mock provider state lock is poisoned")]
    LockPoisoned,
    #[error(transparent)]
    Transition(#[from] StateTransitionError),
}

#[async_trait]
impl StreamProvider for MockProvider {
    fn kind(&self) -> ProviderKind {
        self.kind
    }

    fn embed_capability(&self) -> EmbedCapability {
        self.capability
    }

    fn availability(&self) -> ProviderAvailability {
        ProviderAvailability::Enabled
    }

    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError> {
        if input.provider_channel_id.trim().is_empty() {
            return Err(ProviderError::InvalidInput {
                provider: self.kind,
                field: "provider_channel_id",
                reason: "must not be empty",
            });
        }
        let canonical_url = input.external_url.map_or_else(
            || {
                Url::parse(&format!(
                    "https://mock.invalid/channel/{}",
                    input.provider_channel_id
                ))
                .map_err(|_| ProviderError::InvalidConfiguration("mock base URL"))
            },
            |raw| {
                Url::parse(&raw).map_err(|_| ProviderError::InvalidInput {
                    provider: self.kind,
                    field: "external_url",
                    reason: "must be an absolute URL",
                })
            },
        )?;
        Ok(VerifiedChannel {
            provider: self.kind,
            provider_channel_id: input.provider_channel_id,
            handle: input.handle,
            display_name: None,
            canonical_url,
            current_live_url: None,
            thumbnail_url: None,
            embed_capability: self.capability,
        })
    }

    async fn fetch_live_state(
        &self,
        channel: &ExternalChannel,
    ) -> Result<LiveState, ProviderError> {
        self.state_for(channel)
    }

    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError> {
        channels
            .iter()
            .map(|channel| self.state_for(channel))
            .collect()
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
    use chrono::{TimeZone, Utc};
    use rill3_domain::{ExternalChannel, VerificationState};
    use uuid::Uuid;

    use super::*;

    fn channel() -> ExternalChannel {
        let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        ExternalChannel {
            id: Uuid::nil(),
            creator_id: Uuid::nil(),
            provider: ProviderKind::Twitch,
            provider_channel_id: "42".to_owned(),
            handle: Some("rill3test".to_owned()),
            canonical_url: Url::parse("https://www.twitch.tv/rill3test").unwrap(),
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

    #[test]
    fn reproduces_duplicate_and_out_of_order_events() {
        let provider = MockProvider::new(ProviderKind::Twitch, EmbedCapability::OfficialEmbed);
        let channel = channel();
        let first = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let newer = Utc.timestamp_opt(1_700_000_060, 0).single().unwrap();
        let older = Utc.timestamp_opt(1_699_999_999, 0).single().unwrap();

        let online = LiveState::new(&channel, LiveStatus::Online, first);
        assert_eq!(
            provider.apply_observation(online.clone()).unwrap(),
            StateApplyOutcome::AppliedChanged
        );
        assert_eq!(
            provider.apply_observation(online).unwrap(),
            StateApplyOutcome::Duplicate
        );
        assert_eq!(
            provider
                .apply_observation(LiveState::new(&channel, LiveStatus::Offline, newer))
                .unwrap(),
            StateApplyOutcome::AppliedChanged
        );
        assert_eq!(
            provider
                .apply_observation(LiveState::new(&channel, LiveStatus::Online, older))
                .unwrap(),
            StateApplyOutcome::IgnoredOutOfOrder
        );
    }
}
