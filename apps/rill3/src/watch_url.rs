use rill3_domain::{
    ExternalChannel, LiveSession, LiveStatus, ProviderKind, validate_youtube_video_id,
};
use rill3_providers::{parse_youtube_video_id, validate_external_https_url};
use url::Url;

/// Resolves a current broadcast destination, falling back to the channel when
/// there is no online session or usable broadcast URL. Persisted URLs are
/// revalidated before they are exposed as outbound links.
pub(crate) fn watch_url(channel: &ExternalChannel, session: Option<&LiveSession>) -> Option<Url> {
    let live_url = session
        .filter(|session| session.channel_id == channel.id && session.state == LiveStatus::Online)
        .and_then(|session| match channel.provider {
            ProviderKind::YouTube => channel
                .current_live_url
                .as_ref()
                .and_then(|url| validate_external_https_url(url.as_str()).ok())
                .and_then(|url| parse_youtube_video_id(&url).ok())
                .and_then(|video_id| youtube_watch_url(&video_id))
                .or_else(|| {
                    session
                        .provider_session_id
                        .as_deref()
                        .and_then(youtube_watch_url)
                }),
            ProviderKind::Chzzk => {
                let channel_id = &channel.provider_channel_id;
                if channel_id.len() == 32 && channel_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    validate_external_https_url(&format!(
                        "https://chzzk.naver.com/live/{channel_id}"
                    ))
                    .ok()
                } else {
                    None
                }
            }
            ProviderKind::LinkOnly => channel
                .current_live_url
                .as_ref()
                .and_then(|url| validate_external_https_url(url.as_str()).ok()),
            ProviderKind::Twitch => None,
        });

    live_url.or_else(|| canonical_watch_url(channel))
}

fn youtube_watch_url(video_id: &str) -> Option<Url> {
    validate_youtube_video_id(video_id).ok()?;
    validate_external_https_url(&format!("https://www.youtube.com/watch?v={video_id}")).ok()
}

fn canonical_watch_url(channel: &ExternalChannel) -> Option<Url> {
    let url = validate_external_https_url(channel.canonical_url.as_str()).ok()?;
    let trusted_host = match channel.provider {
        ProviderKind::Twitch => matches!(url.host_str(), Some("twitch.tv" | "www.twitch.tv")),
        ProviderKind::YouTube => matches!(
            url.host_str(),
            Some("youtube.com" | "www.youtube.com" | "m.youtube.com")
        ),
        ProviderKind::Chzzk => url.host_str() == Some("chzzk.naver.com"),
        ProviderKind::LinkOnly => true,
    };
    trusted_host.then_some(url)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rill3_domain::{EmbedCapability, LiveState, VerifiedChannel};
    use uuid::Uuid;

    use super::*;

    const VIDEO_ID: &str = "dQw4w9WgXcQ";
    const VIDEO_URL: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";
    const CHZZK_CHANNEL_ID: &str = "0123456789abcdef0123456789abcdef";

    fn channel(provider: ProviderKind) -> ExternalChannel {
        let (channel_id, canonical_url) = match provider {
            ProviderKind::Twitch => ("42", "https://www.twitch.tv/safe_channel"),
            ProviderKind::YouTube => (
                "UC_x5XG1OV2P6uZZ5FSM9Ttw",
                "https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw",
            ),
            ProviderKind::Chzzk => (
                CHZZK_CHANNEL_ID,
                "https://chzzk.naver.com/0123456789abcdef0123456789abcdef",
            ),
            ProviderKind::LinkOnly => ("external", "https://video.example/channel"),
        };
        ExternalChannel::from_verified(
            Uuid::new_v4(),
            Uuid::new_v4(),
            VerifiedChannel {
                provider,
                provider_channel_id: channel_id.to_owned(),
                handle: None,
                display_name: None,
                canonical_url: Url::parse(canonical_url).unwrap(),
                current_live_url: None,
                thumbnail_url: None,
                embed_capability: EmbedCapability::LinkOnly,
            },
            Utc::now(),
        )
    }

    fn session(channel: &ExternalChannel, status: LiveStatus) -> LiveSession {
        LiveSession::from_state(Uuid::new_v4(), &LiveState::new(channel, status, Utc::now()))
    }

    #[test]
    fn online_youtube_urls_are_normalized_and_take_precedence_over_session_ids() {
        let mut channel = channel(ProviderKind::YouTube);
        let mut session = session(&channel, LiveStatus::Online);
        session.provider_session_id = Some("abcdefghijk".to_owned());
        for raw in [
            VIDEO_URL,
            "https://youtu.be/dQw4w9WgXcQ?t=30",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ&feature=share#player",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
        ] {
            channel.current_live_url = Some(Url::parse(raw).unwrap());
            assert_eq!(
                watch_url(&channel, Some(&session)).unwrap().as_str(),
                VIDEO_URL
            );
        }
    }

    #[test]
    fn online_youtube_uses_a_valid_session_id_without_a_usable_current_url() {
        let mut channel = channel(ProviderKind::YouTube);
        let mut session = session(&channel, LiveStatus::Online);
        session.provider_session_id = Some(VIDEO_ID.to_owned());
        assert_eq!(
            watch_url(&channel, Some(&session)).unwrap().as_str(),
            VIDEO_URL
        );

        channel.current_live_url =
            Some(Url::parse("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ").unwrap());
        assert_eq!(
            watch_url(&channel, Some(&session)).unwrap().as_str(),
            VIDEO_URL
        );
    }

    #[test]
    fn youtube_rejects_malformed_video_ids_and_untrusted_current_urls() {
        let mut channel = channel(ProviderKind::YouTube);
        let mut session = session(&channel, LiveStatus::Online);
        session.provider_session_id = Some("dQw4w9WgXcQ&redirect=evil".to_owned());
        for raw in [
            "https://www.youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://user:password@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=too_short",
            "https://youtu.be/..%2Fprivate",
            "javascript:alert(1)",
        ] {
            channel.current_live_url = Some(Url::parse(raw).unwrap());
            assert_eq!(
                watch_url(&channel, Some(&session)),
                Some(channel.canonical_url.clone())
            );
        }
    }

    #[test]
    fn offline_unknown_and_missing_sessions_use_the_channel_destination() {
        for provider in [
            ProviderKind::YouTube,
            ProviderKind::Chzzk,
            ProviderKind::LinkOnly,
        ] {
            let mut channel = channel(provider);
            channel.current_live_url = Some(Url::parse(VIDEO_URL).unwrap());
            assert_eq!(
                watch_url(&channel, None),
                Some(channel.canonical_url.clone())
            );
            for status in [LiveStatus::Offline, LiveStatus::Unknown] {
                let mut session = session(&channel, status);
                session.provider_session_id = Some(VIDEO_ID.to_owned());
                assert_eq!(
                    watch_url(&channel, Some(&session)),
                    Some(channel.canonical_url.clone())
                );
            }
        }
    }

    #[test]
    fn another_channels_session_does_not_activate_a_live_destination() {
        let mut channel = channel(ProviderKind::YouTube);
        channel.current_live_url = Some(Url::parse(VIDEO_URL).unwrap());
        let mut session = session(&channel, LiveStatus::Online);
        session.channel_id = Uuid::new_v4();
        assert_eq!(
            watch_url(&channel, Some(&session)),
            Some(channel.canonical_url.clone())
        );
    }

    #[test]
    fn online_chzzk_links_to_the_player_and_rejects_invalid_channel_ids() {
        let mut channel = channel(ProviderKind::Chzzk);
        let session = session(&channel, LiveStatus::Online);
        assert_eq!(
            watch_url(&channel, Some(&session)).unwrap().as_str(),
            format!("https://chzzk.naver.com/live/{CHZZK_CHANNEL_ID}")
        );
        for invalid_id in [
            "",
            "../other",
            "0123456789abcdef0123456789abcdef?x",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ] {
            channel.provider_channel_id = invalid_id.to_owned();
            assert_eq!(
                watch_url(&channel, Some(&session)),
                Some(channel.canonical_url.clone())
            );
        }
    }

    #[test]
    fn twitch_uses_its_canonical_channel_url_for_every_live_status() {
        let mut channel = channel(ProviderKind::Twitch);
        channel.current_live_url = Some(Url::parse("https://unrelated.example/live").unwrap());
        for status in [LiveStatus::Online, LiveStatus::Offline, LiveStatus::Unknown] {
            let session = session(&channel, status);
            assert_eq!(
                watch_url(&channel, Some(&session)),
                Some(channel.canonical_url.clone())
            );
        }
    }

    #[test]
    fn link_only_uses_a_safe_current_url_and_falls_back_for_unsafe_urls() {
        let mut channel = channel(ProviderKind::LinkOnly);
        let session = session(&channel, LiveStatus::Online);
        let current_url = Url::parse("https://video.example/live/42?quality=high").unwrap();
        channel.current_live_url = Some(current_url.clone());
        assert_eq!(watch_url(&channel, Some(&session)), Some(current_url));
        for raw in [
            "javascript:alert(1)",
            "http://video.example/live",
            "https://user:password@video.example/live",
            "https://127.0.0.1/live",
            "https://[::1]/live",
            "https://localhost/live",
            "https://metadata.internal/live",
        ] {
            channel.current_live_url = Some(Url::parse(raw).unwrap());
            assert_eq!(
                watch_url(&channel, Some(&session)),
                Some(channel.canonical_url.clone())
            );
        }
    }

    #[test]
    fn unsafe_canonical_urls_produce_no_link_when_no_live_destination_is_available() {
        for provider in [
            ProviderKind::Twitch,
            ProviderKind::YouTube,
            ProviderKind::Chzzk,
            ProviderKind::LinkOnly,
        ] {
            let mut channel = channel(provider);
            for raw in [
                "javascript:alert(1)",
                "http://video.example/channel",
                "https://127.0.0.1/channel",
                "https://user:password@video.example/channel",
            ] {
                channel.canonical_url = Url::parse(raw).unwrap();
                assert_eq!(
                    watch_url(&channel, None),
                    None,
                    "accepted {provider} URL {raw}"
                );
            }
        }
    }

    #[test]
    fn official_provider_fallbacks_reject_lookalike_hosts() {
        for (provider, raw) in [
            (
                ProviderKind::Twitch,
                "https://www.twitch.tv.evil.example/channel",
            ),
            (
                ProviderKind::YouTube,
                "https://www.youtube.com.evil.example/channel",
            ),
            (
                ProviderKind::Chzzk,
                "https://chzzk.naver.com.evil.example/channel",
            ),
        ] {
            let mut channel = channel(provider);
            channel.canonical_url = Url::parse(raw).unwrap();
            assert_eq!(watch_url(&channel, None), None);
        }
    }
}
