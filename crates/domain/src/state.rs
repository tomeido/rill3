use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{ExternalChannel, ProviderKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    Online,
    Offline,
    Unknown,
}

impl LiveStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for LiveStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LiveStatus {
    type Err = LiveStatusParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "online" => Ok(Self::Online),
            "offline" => Ok(Self::Offline),
            "unknown" => Ok(Self::Unknown),
            _ => Err(LiveStatusParseError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unsupported live status: {0}")]
pub struct LiveStatusParseError(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveState {
    pub channel_id: Uuid,
    /// Source registration revision for manual observations. This is checked at
    /// persistence time and is not stored as part of the live session.
    #[serde(default)]
    pub channel_updated_at: Option<DateTime<Utc>>,
    pub provider: ProviderKind,
    pub provider_channel_id: String,
    pub handle: Option<String>,
    pub provider_session_id: Option<String>,
    pub status: LiveStatus,
    pub title: Option<String>,
    pub category: Option<String>,
    pub thumbnail_url: Option<Url>,
    pub started_at: Option<DateTime<Utc>>,
    /// Ordering authority for provider observations.
    pub observed_at: DateTime<Utc>,
    /// Most recent successful provider/manual observation.
    pub last_success_at: Option<DateTime<Utc>>,
    /// Set without erasing the last known live/offline state after freshness expires.
    pub stale_since: Option<DateTime<Utc>>,
}

impl LiveState {
    #[must_use]
    pub fn new(channel: &ExternalChannel, status: LiveStatus, observed_at: DateTime<Utc>) -> Self {
        Self {
            channel_id: channel.id,
            channel_updated_at: matches!(
                channel.provider,
                ProviderKind::YouTube | ProviderKind::LinkOnly
            )
            .then_some(channel.updated_at),
            provider: channel.provider,
            provider_channel_id: channel.provider_channel_id.clone(),
            handle: channel.handle.clone(),
            provider_session_id: None,
            status,
            title: None,
            category: None,
            thumbnail_url: None,
            started_at: None,
            observed_at,
            last_success_at: Some(observed_at),
            stale_since: None,
        }
    }

    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.stale_since.is_some()
    }

    /// Marks a last-known-good observation stale without fabricating an offline state.
    /// Returns `true` only when this call changes the state.
    pub fn mark_stale(&mut self, stale_at: DateTime<Utc>) -> bool {
        if self
            .stale_since
            .is_some_and(|existing| existing <= stale_at)
        {
            return false;
        }
        self.stale_since = Some(stale_at);
        true
    }

    /// Applies an observation only when it is strictly newer than the current one.
    ///
    /// # Errors
    ///
    /// Returns [`StateTransitionError::ChannelMismatch`] if the provider or
    /// channel identity differs from the stored state.
    pub fn apply_observation(
        &mut self,
        incoming: Self,
    ) -> Result<StateApplyOutcome, StateTransitionError> {
        if self.channel_id != incoming.channel_id
            || self.provider != incoming.provider
            || self.provider_channel_id != incoming.provider_channel_id
        {
            return Err(StateTransitionError::ChannelMismatch);
        }

        if incoming.observed_at < self.observed_at {
            return Ok(StateApplyOutcome::IgnoredOutOfOrder);
        }

        if incoming.observed_at == self.observed_at {
            return Ok(if *self == incoming {
                StateApplyOutcome::Duplicate
            } else {
                StateApplyOutcome::IgnoredOutOfOrder
            });
        }

        let materially_changed = !self.materially_matches(&incoming);
        *self = incoming;
        Ok(if materially_changed {
            StateApplyOutcome::AppliedChanged
        } else {
            StateApplyOutcome::AppliedUnchanged
        })
    }

    fn materially_matches(&self, other: &Self) -> bool {
        self.handle == other.handle
            && self.provider_session_id == other.provider_session_id
            && self.status == other.status
            && self.title == other.title
            && self.category == other.category
            && self.thumbnail_url == other.thumbnail_url
            && self.started_at == other.started_at
            && self.stale_since.is_some() == other.stale_since.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateApplyOutcome {
    AppliedChanged,
    AppliedUnchanged,
    Duplicate,
    IgnoredOutOfOrder,
}

impl StateApplyOutcome {
    #[must_use]
    pub const fn should_persist(self) -> bool {
        matches!(self, Self::AppliedChanged | Self::AppliedUnchanged)
    }

    #[must_use]
    pub const fn changed(self) -> bool {
        matches!(self, Self::AppliedChanged)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum StateTransitionError {
    #[error("live observations belong to different channels")]
    ChannelMismatch,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::{EmbedCapability, VerificationState};

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
    fn live_status_has_stable_database_text() {
        for (status, value) in [
            (LiveStatus::Online, "online"),
            (LiveStatus::Offline, "offline"),
            (LiveStatus::Unknown, "unknown"),
        ] {
            assert_eq!(status.as_str(), value);
            assert_eq!(value.parse::<LiveStatus>(), Ok(status));
        }
    }

    #[test]
    fn manual_observations_capture_the_source_registration_revision() {
        let mut channel = channel();
        for provider in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
            channel.provider = provider;
            let state = LiveState::new(&channel, LiveStatus::Unknown, channel.updated_at);
            assert_eq!(state.channel_updated_at, Some(channel.updated_at));
        }
        for provider in [ProviderKind::Twitch, ProviderKind::Chzzk] {
            channel.provider = provider;
            let state = LiveState::new(&channel, LiveStatus::Unknown, channel.updated_at);
            assert_eq!(state.channel_updated_at, None);
        }
    }

    #[test]
    fn online_can_transition_to_newer_offline() {
        let channel = channel();
        let first = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let second = Utc.timestamp_opt(1_700_000_060, 0).single().unwrap();
        let mut current = LiveState::new(&channel, LiveStatus::Online, first);
        current.provider_session_id = Some("session-1".to_owned());
        let incoming = LiveState::new(&channel, LiveStatus::Offline, second);

        assert_eq!(
            current.apply_observation(incoming),
            Ok(StateApplyOutcome::AppliedChanged)
        );
        assert_eq!(current.status, LiveStatus::Offline);
    }

    #[test]
    fn duplicate_is_idempotent() {
        let channel = channel();
        let observed_at = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let mut current = LiveState::new(&channel, LiveStatus::Online, observed_at);

        assert_eq!(
            current.apply_observation(current.clone()),
            Ok(StateApplyOutcome::Duplicate)
        );
    }

    #[test]
    fn late_offline_cannot_overwrite_newer_online() {
        let channel = channel();
        let newer = Utc.timestamp_opt(1_700_000_060, 0).single().unwrap();
        let older = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let mut current = LiveState::new(&channel, LiveStatus::Online, newer);
        let incoming = LiveState::new(&channel, LiveStatus::Offline, older);

        assert_eq!(
            current.apply_observation(incoming),
            Ok(StateApplyOutcome::IgnoredOutOfOrder)
        );
        assert_eq!(current.status, LiveStatus::Online);
        assert_eq!(current.observed_at, newer);
    }

    #[test]
    fn stale_marker_preserves_last_known_status() {
        let channel = channel();
        let observed_at = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let stale_at = Utc.timestamp_opt(1_700_000_060, 0).single().unwrap();
        let mut current = LiveState::new(&channel, LiveStatus::Online, observed_at);

        assert!(current.mark_stale(stale_at));
        assert_eq!(current.status, LiveStatus::Online);
        assert!(current.is_stale());
        assert!(!current.mark_stale(stale_at));
    }
}
