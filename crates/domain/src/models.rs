use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::{EmbedCapability, LiveState, LiveStatus, ProviderKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatorStatus {
    Active,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Creator {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub bio: Option<String>,
    pub locale: String,
    pub status: CreatorStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationState {
    Pending,
    Verified,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalChannel {
    pub id: Uuid,
    pub creator_id: Uuid,
    pub provider: ProviderKind,
    pub provider_channel_id: String,
    pub handle: Option<String>,
    pub canonical_url: Url,
    pub current_live_url: Option<Url>,
    pub manual_live_status: Option<LiveStatus>,
    pub manual_state_expires_at: Option<DateTime<Utc>>,
    pub embed_capability: EmbedCapability,
    pub verification_state: VerificationState,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExternalChannel {
    #[must_use]
    pub fn from_verified(
        id: Uuid,
        creator_id: Uuid,
        verified: crate::VerifiedChannel,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            creator_id,
            provider: verified.provider,
            provider_channel_id: verified.provider_channel_id,
            handle: verified.handle,
            canonical_url: verified.canonical_url,
            current_live_url: verified.current_live_url,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: verified.embed_capability,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }
}

/// Persisted session-shaped view of normalized live state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveSession {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub provider_session_id: Option<String>,
    pub state: LiveStatus,
    pub title: Option<String>,
    pub category: Option<String>,
    pub thumbnail_url: Option<Url>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub stale_since: Option<DateTime<Utc>>,
}

impl LiveSession {
    #[must_use]
    pub fn from_state(id: Uuid, state: &LiveState) -> Self {
        Self {
            id,
            channel_id: state.channel_id,
            provider_session_id: state.provider_session_id.clone(),
            state: state.status,
            title: state.title.clone(),
            category: state.category.clone(),
            thumbnail_url: state.thumbnail_url.clone(),
            started_at: state.started_at,
            ended_at: (state.status == LiveStatus::Offline).then_some(state.observed_at),
            observed_at: state.observed_at,
            last_success_at: state.last_success_at,
            stale_since: state.stale_since,
        }
    }

    /// Treats explicit provider failures and observations older than the
    /// caller's freshness window as stale.
    #[must_use]
    pub fn is_stale_at(&self, now: DateTime<Utc>, max_age: chrono::TimeDelta) -> bool {
        self.stale_since.is_some()
            || self
                .last_success_at
                .is_none_or(|last_success| now.signed_duration_since(last_success) > max_age)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEventStatus {
    Received,
    Applied,
    Duplicate,
    IgnoredOutOfOrder,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderEvent {
    pub provider: ProviderKind,
    pub external_event_id: String,
    pub channel_id: Option<Uuid>,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    /// Lower-case hexadecimal SHA-256 of the raw payload. Raw payloads are not retained.
    pub payload_hash: String,
    pub handled_at: Option<DateTime<Utc>>,
    pub status: ProviderEventStatus,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, TimeZone};

    use super::*;

    #[test]
    fn session_freshness_includes_explicit_and_age_based_staleness() {
        let observed_at = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
        let mut session = LiveSession {
            id: Uuid::nil(),
            channel_id: Uuid::nil(),
            provider_session_id: None,
            state: LiveStatus::Online,
            title: None,
            category: None,
            thumbnail_url: None,
            started_at: None,
            ended_at: None,
            observed_at,
            last_success_at: Some(observed_at),
            stale_since: None,
        };
        let freshness = TimeDelta::minutes(10);

        assert!(!session.is_stale_at(observed_at + TimeDelta::minutes(5), freshness));
        assert!(session.is_stale_at(observed_at + TimeDelta::minutes(11), freshness));

        session.stale_since = Some(observed_at + TimeDelta::minutes(1));
        assert!(session.is_stale_at(observed_at + TimeDelta::minutes(2), freshness));
    }
}
