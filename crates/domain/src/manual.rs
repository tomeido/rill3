use chrono::{DateTime, Utc};

use crate::{ExternalChannel, LiveSession, LiveStatus, ProviderKind};

/// Resolves an explicitly supplied manual state at the requested time.
///
/// Only `YouTube` and link-only channels support manual state. Online requires a
/// broadcast URL and a future expiry; URL validation remains the provider's job.
/// Expired observations and missing state are unknown. Explicit offline state
/// without an expiry remains offline because it does not claim a live broadcast.
#[must_use]
pub fn manual_live_status(channel: &ExternalChannel, now: DateTime<Utc>) -> LiveStatus {
    if !matches!(
        channel.provider,
        ProviderKind::YouTube | ProviderKind::LinkOnly
    ) {
        return LiveStatus::Unknown;
    }
    match (channel.manual_live_status, channel.manual_state_expires_at) {
        (Some(LiveStatus::Online), Some(expires_at))
            if expires_at > now && channel.current_live_url.is_some() =>
        {
            LiveStatus::Online
        }
        (Some(LiveStatus::Offline), Some(expires_at)) if expires_at > now => LiveStatus::Offline,
        (Some(LiveStatus::Offline), None) => LiveStatus::Offline,
        _ => LiveStatus::Unknown,
    }
}

/// Resolves a public status without allowing cached state to outlive manual input.
///
/// Manual online state must also have an online worker observation of the current
/// channel revision. Other manual statuses take effect immediately. API-backed
/// providers retain their observed status. A mismatched session is always unknown.
#[must_use]
pub fn effective_live_status(
    channel: &ExternalChannel,
    session: Option<&LiveSession>,
    now: DateTime<Utc>,
) -> LiveStatus {
    if session.is_some_and(|session| session.channel_id != channel.id) {
        return LiveStatus::Unknown;
    }
    if !matches!(
        channel.provider,
        ProviderKind::YouTube | ProviderKind::LinkOnly
    ) {
        return session.map_or(LiveStatus::Unknown, |session| session.state);
    }
    match manual_live_status(channel, now) {
        LiveStatus::Online => session
            .filter(|session| {
                session.state == LiveStatus::Online && session.observed_at >= channel.updated_at
            })
            .map_or(LiveStatus::Unknown, |_| LiveStatus::Online),
        status => status,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, TimeZone};
    use url::Url;
    use uuid::Uuid;

    use crate::{EmbedCapability, LiveState, VerifiedChannel};

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000, 0).single().unwrap()
    }

    fn channel(provider: ProviderKind) -> ExternalChannel {
        let mut channel = ExternalChannel::from_verified(
            Uuid::new_v4(),
            Uuid::new_v4(),
            VerifiedChannel {
                provider,
                provider_channel_id: "fixture".to_owned(),
                handle: None,
                display_name: None,
                canonical_url: Url::parse("https://example.com/channel").unwrap(),
                current_live_url: Some(Url::parse("https://example.com/live").unwrap()),
                thumbnail_url: None,
                embed_capability: EmbedCapability::LinkOnly,
            },
            now(),
        );
        channel.manual_live_status = Some(LiveStatus::Online);
        channel.manual_state_expires_at = Some(now() + TimeDelta::hours(1));
        channel
    }

    fn session(channel: &ExternalChannel, status: LiveStatus) -> LiveSession {
        LiveSession::from_state(Uuid::new_v4(), &LiveState::new(channel, status, now()))
    }

    #[test]
    fn expiry_invalidates_a_cached_online_session_without_a_worker_update() {
        for provider in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
            let channel = channel(provider);
            let cached = session(&channel, LiveStatus::Online);
            let expiry = channel.manual_state_expires_at.unwrap();
            assert_eq!(
                effective_live_status(&channel, Some(&cached), expiry - TimeDelta::seconds(1)),
                LiveStatus::Online
            );
            assert_eq!(
                effective_live_status(&channel, Some(&cached), expiry),
                LiveStatus::Unknown
            );
            assert_eq!(
                effective_live_status(&channel, Some(&cached), expiry + TimeDelta::hours(1)),
                LiveStatus::Unknown
            );
        }
    }

    #[test]
    fn explicit_offline_unknown_and_missing_state_override_cached_online() {
        for provider in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
            let mut channel = channel(provider);
            let cached = session(&channel, LiveStatus::Online);
            for status in [None, Some(LiveStatus::Unknown), Some(LiveStatus::Offline)] {
                channel.manual_live_status = status;
                assert_eq!(
                    effective_live_status(&channel, Some(&cached), now()),
                    status.unwrap_or(LiveStatus::Unknown)
                );
                assert_eq!(
                    effective_live_status(&channel, None, now()),
                    status.unwrap_or(LiveStatus::Unknown)
                );
            }
            channel.manual_state_expires_at = Some(now());
            assert_eq!(
                effective_live_status(&channel, Some(&cached), now()),
                LiveStatus::Unknown
            );
            channel.manual_state_expires_at = None;
            assert_eq!(
                effective_live_status(&channel, Some(&cached), now()),
                LiveStatus::Offline
            );
        }
    }

    #[test]
    fn online_requires_a_bounded_url_and_current_worker_observation() {
        for provider in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
            let mut channel = channel(provider);
            let online = session(&channel, LiveStatus::Online);
            let offline = session(&channel, LiveStatus::Offline);
            let unknown = session(&channel, LiveStatus::Unknown);
            assert_eq!(manual_live_status(&channel, now()), LiveStatus::Online);
            for missing_or_inactive in [None, Some(&offline), Some(&unknown)] {
                assert_eq!(
                    effective_live_status(&channel, missing_or_inactive, now()),
                    LiveStatus::Unknown
                );
            }
            channel.updated_at = now() + TimeDelta::seconds(1);
            assert_eq!(
                effective_live_status(&channel, Some(&online), now() + TimeDelta::seconds(2)),
                LiveStatus::Unknown
            );
            channel.updated_at = now();
            channel.manual_state_expires_at = None;
            assert_eq!(
                effective_live_status(&channel, Some(&online), now()),
                LiveStatus::Unknown
            );
            channel.manual_state_expires_at = Some(now() + TimeDelta::hours(1));
            channel.current_live_url = None;
            assert_eq!(
                effective_live_status(&channel, Some(&online), now()),
                LiveStatus::Unknown
            );
        }
    }

    #[test]
    fn automatic_providers_ignore_manual_fields_and_preserve_observed_state() {
        for provider in [ProviderKind::Twitch, ProviderKind::Chzzk] {
            let mut channel = channel(provider);
            channel.manual_state_expires_at = Some(now());
            channel.manual_live_status = Some(LiveStatus::Offline);
            channel.updated_at = now() + TimeDelta::hours(1);
            assert_eq!(
                effective_live_status(&channel, None, now()),
                LiveStatus::Unknown
            );
            for status in [LiveStatus::Online, LiveStatus::Offline, LiveStatus::Unknown] {
                assert_eq!(
                    effective_live_status(&channel, Some(&session(&channel, status)), now()),
                    status
                );
            }
            assert_eq!(manual_live_status(&channel, now()), LiveStatus::Unknown);
        }
    }

    #[test]
    fn foreign_sessions_are_never_used() {
        for provider in [
            ProviderKind::Twitch,
            ProviderKind::Chzzk,
            ProviderKind::YouTube,
            ProviderKind::LinkOnly,
        ] {
            let mut channel = channel(provider);
            let mut cached = session(&channel, LiveStatus::Online);
            cached.channel_id = Uuid::new_v4();
            for status in [LiveStatus::Online, LiveStatus::Offline] {
                channel.manual_live_status = Some(status);
                assert_eq!(
                    effective_live_status(&channel, Some(&cached), now()),
                    LiveStatus::Unknown
                );
            }
        }
    }
}
