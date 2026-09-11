use askama::Template;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use rill3_domain::{
    Creator, EmbedCapability, ExternalChannel, LiveSession, LiveStatus, ProviderKind,
    TwitchEmbedDescriptor, YouTubeEmbedDescriptor, effective_live_status,
};
use rill3_providers::parse_youtube_video_id;
use url::Url;

use crate::{assets::stylesheet_path, watch_url::watch_url};

#[derive(Clone, Debug)]
pub(crate) struct EmbedConfig {
    pub(crate) twitch_parent: String,
    pub(crate) public_origin: Url,
}

#[derive(Clone, Debug)]
pub(crate) struct PagePaths {
    pub(crate) base: String,
    pub(crate) home: String,
    pub(crate) stylesheet: String,
    pub(crate) channels: String,
}

impl PagePaths {
    pub(crate) fn new(base: &str) -> Self {
        Self {
            base: base.to_owned(),
            home: if base.is_empty() { "/" } else { base }.to_owned(),
            stylesheet: stylesheet_path(base),
            channels: format!("{base}/channels"),
        }
    }
}

const TWITCH_EMBED_MIN_WIDTH: u16 = 400;
const TWITCH_EMBED_MIN_HEIGHT: u16 = 300;
const YOUTUBE_EMBED_MIN_WIDTH: u16 = 200;
const YOUTUBE_EMBED_MIN_HEIGHT: u16 = 200;

struct EmbedView {
    url: Url,
    provider_class: &'static str,
    min_width: u16,
    min_height: u16,
}

#[derive(Clone, Debug)]
// Askama conditionals intentionally mirror these independent presentation flags.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct LiveCard {
    pub(crate) creator_slug: String,
    pub(crate) channel_id: String,
    pub(crate) creator_name: String,
    pub(crate) initial: String,
    pub(crate) provider: String,
    pub(crate) external_url: String,
    pub(crate) has_external_url: bool,
    pub(crate) title: String,
    pub(crate) category: String,
    pub(crate) has_category: bool,
    pub(crate) thumbnail_url: String,
    pub(crate) has_thumbnail: bool,
    pub(crate) started_label: String,
    pub(crate) started_at: String,
    pub(crate) observed_label: String,
    pub(crate) observed_at: String,
    pub(crate) is_manual: bool,
    pub(crate) is_live: bool,
    pub(crate) is_stale: bool,
}

impl LiveCard {
    pub(crate) fn new(
        creator: &Creator,
        channel: &ExternalChannel,
        session: &LiveSession,
        now: DateTime<Utc>,
    ) -> Self {
        let category = session.category.clone().unwrap_or_default();
        let status = effective_live_status(channel, Some(session), now);
        let external_url = watch_url(channel, (status == LiveStatus::Online).then_some(session));
        let thumbnail_url = session
            .thumbnail_url
            .as_ref()
            .map_or_else(String::new, ToString::to_string);
        Self {
            creator_slug: creator.slug.clone(),
            channel_id: channel.id.to_string(),
            creator_name: creator.display_name.clone(),
            initial: first_grapheme_fallback(&creator.display_name),
            provider: provider_label(channel.provider).to_owned(),
            has_external_url: external_url.is_some(),
            external_url: external_url.map_or_else(String::new, |url| url.to_string()),
            title: session
                .title
                .clone()
                .unwrap_or_else(|| "제목 없는 방송".to_owned()),
            has_category: !category.is_empty(),
            category,
            has_thumbnail: !thumbnail_url.is_empty(),
            thumbnail_url,
            started_label: session.started_at.map_or_else(
                || "시작 시각 미확인".to_owned(),
                |time| format!("{} 시작", relative_time(time, now)),
            ),
            started_at: session.started_at.map_or_else(String::new, format_time),
            observed_label: session.last_success_at.map_or_else(
                || "확인 기록 없음".to_owned(),
                |time| format!("마지막 확인 {}", relative_time(time, now)),
            ),
            observed_at: session
                .last_success_at
                .map_or_else(String::new, format_time),
            is_manual: is_manual_provider(channel.provider),
            is_live: status == LiveStatus::Online,
            is_stale: session_is_stale(channel.provider, session, now),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChannelCard {
    pub(crate) id: String,
    pub(crate) creator_slug: String,
    pub(crate) creator_name: String,
    pub(crate) initial: String,
    pub(crate) provider: &'static str,
    pub(crate) status: &'static str,
    pub(crate) external_url: String,
    pub(crate) has_external_url: bool,
}

impl ChannelCard {
    pub(crate) fn new(
        creator: &Creator,
        channel: &ExternalChannel,
        session: Option<&LiveSession>,
        now: DateTime<Utc>,
    ) -> Self {
        let effective = effective_live_status(channel, session, now);
        let external_url = watch_url(channel, session.filter(|_| effective == LiveStatus::Online));
        let status = match session {
            _ if effective == LiveStatus::Unknown => "방송 확인 대기",
            _ if effective == LiveStatus::Offline && is_manual_provider(channel.provider) => {
                "방송 종료"
            }
            Some(value) if session_is_stale(channel.provider, value, now) => "확인 지연",
            Some(_) => match effective {
                LiveStatus::Online if is_manual_provider(channel.provider) => {
                    "방송 중 · 등록 정보 기준"
                }
                LiveStatus::Online => "방송 중",
                LiveStatus::Offline => "방송 종료",
                LiveStatus::Unknown => "방송 확인 대기",
            },
            None => "방송 확인 대기",
        };
        Self {
            id: channel.id.to_string(),
            creator_slug: creator.slug.clone(),
            creator_name: creator.display_name.clone(),
            initial: first_grapheme_fallback(&creator.display_name),
            provider: provider_label(channel.provider),
            status,
            has_external_url: external_url.is_some(),
            external_url: external_url.map_or_else(String::new, |url| url.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderStatusView {
    pub(crate) kind: ProviderKind,
    pub(crate) name: String,
    pub(crate) css_status: &'static str,
    pub(crate) label: &'static str,
}

impl ProviderStatusView {
    pub(crate) fn enabled(kind: ProviderKind) -> Self {
        Self {
            kind,
            name: provider_label(kind).to_owned(),
            css_status: "enabled",
            label: match kind {
                ProviderKind::Twitch | ProviderKind::Chzzk => "자동 확인 설정됨",
                ProviderKind::YouTube => "등록된 영상 기준",
                ProviderKind::LinkOnly => "외부 링크 제공",
            },
        }
    }

    pub(crate) fn disabled(kind: ProviderKind) -> Self {
        Self {
            kind,
            name: provider_label(kind).to_owned(),
            css_status: "disabled",
            label: "방송 정보 미연동",
        }
    }

    pub(crate) fn degraded(kind: ProviderKind) -> Self {
        Self {
            kind,
            name: provider_label(kind).to_owned(),
            css_status: "degraded",
            label: "상태 확인 지연",
        }
    }
}

#[derive(Template)]
#[template(path = "home.html")]
pub(crate) struct HomeTemplate {
    pub(crate) paths: PagePaths,
    pub(crate) lives: Vec<LiveCard>,
    pub(crate) channels: Vec<ChannelCard>,
    pub(crate) providers: Vec<ProviderStatusView>,
    pub(crate) has_degraded_providers: bool,
}

#[derive(Template)]
#[template(path = "channels.html")]
pub(crate) struct ChannelsTemplate {
    pub(crate) paths: PagePaths,
    pub(crate) channels: Vec<ChannelCard>,
    pub(crate) next_url: String,
    pub(crate) has_previous: bool,
}

#[derive(Template)]
#[template(path = "creator.html")]
// Askama conditionals intentionally mirror these independent presentation flags.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CreatorTemplate {
    pub(crate) paths: PagePaths,
    pub(crate) creator_name: String,
    pub(crate) initial: String,
    pub(crate) bio: String,
    pub(crate) has_bio: bool,
    pub(crate) provider: String,
    pub(crate) channel_id: String,
    pub(crate) channels: Vec<ChannelCard>,
    pub(crate) next_channels_url: String,
    pub(crate) is_manual: bool,
    pub(crate) embed_url: String,
    pub(crate) embed_provider_class: &'static str,
    pub(crate) embed_min_width: u16,
    pub(crate) embed_min_height: u16,
    pub(crate) has_embed: bool,
    pub(crate) external_url: String,
    pub(crate) has_external_url: bool,
    pub(crate) player_message: &'static str,
    pub(crate) title: String,
    pub(crate) category: String,
    pub(crate) has_category: bool,
    pub(crate) is_stale: bool,
}

impl CreatorTemplate {
    pub(crate) fn new(
        creator: &Creator,
        channel: &ExternalChannel,
        session: Option<&LiveSession>,
        embed_config: &EmbedConfig,
        paths: &PagePaths,
        now: DateTime<Utc>,
    ) -> Self {
        let bio = creator.bio.clone().unwrap_or_default();
        let status = effective_live_status(channel, session, now);
        let external_url = watch_url(channel, session.filter(|_| status == LiveStatus::Online));
        let category = session
            .and_then(|value| value.category.clone())
            .unwrap_or_default();
        let embed = session
            .filter(|_| status == LiveStatus::Online)
            .and_then(|value| embed_view(channel, value, embed_config));
        let has_embed = embed.is_some();
        let (embed_url, embed_provider_class, embed_min_width, embed_min_height) =
            embed.map_or((String::new(), "none", 0, 0), |view| {
                (
                    view.url.to_string(),
                    view.provider_class,
                    view.min_width,
                    view.min_height,
                )
            });

        Self {
            paths: paths.clone(),
            creator_name: creator.display_name.clone(),
            initial: first_grapheme_fallback(&creator.display_name),
            has_bio: !bio.is_empty(),
            bio,
            provider: provider_label(channel.provider).to_owned(),
            channel_id: channel.id.to_string(),
            channels: Vec::new(),
            next_channels_url: String::new(),
            is_manual: is_manual_provider(channel.provider),
            has_embed,
            embed_url,
            embed_provider_class,
            embed_min_width,
            embed_min_height,
            has_external_url: external_url.is_some(),
            external_url: external_url.map_or_else(String::new, |url| url.to_string()),
            player_message: match status {
                LiveStatus::Online => "원래 플랫폼에서 방송을 시청할 수 있습니다.",
                LiveStatus::Offline => "방송이 종료되었습니다. 플랫폼에서 채널을 둘러보세요.",
                LiveStatus::Unknown => {
                    "현재 방송 여부를 확인하지 못했습니다. 플랫폼에서 직접 확인해 보세요."
                }
            },
            title: session
                .and_then(|value| value.title.clone())
                .unwrap_or_else(|| "현재 방송 정보가 없습니다".to_owned()),
            has_category: !category.is_empty(),
            category,
            is_stale: session.is_some_and(|value| session_is_stale(channel.provider, value, now)),
        }
    }
}

#[derive(Template)]
#[template(path = "not_found.html")]
pub(crate) struct NotFoundTemplate {
    pub(crate) paths: PagePaths,
}

fn embed_view(
    channel: &ExternalChannel,
    session: &LiveSession,
    config: &EmbedConfig,
) -> Option<EmbedView> {
    if channel.embed_capability != EmbedCapability::OfficialEmbed {
        return None;
    }

    match channel.provider {
        ProviderKind::Twitch => {
            let login = channel.handle.as_deref()?;
            TwitchEmbedDescriptor::new(login, &config.twitch_parent)
                .ok()
                .map(|descriptor| EmbedView {
                    url: descriptor.src().clone(),
                    provider_class: "twitch",
                    min_width: TWITCH_EMBED_MIN_WIDTH,
                    min_height: TWITCH_EMBED_MIN_HEIGHT,
                })
        }
        ProviderKind::YouTube => {
            let video_id = channel
                .current_live_url
                .as_ref()
                .and_then(|url| parse_youtube_video_id(url).ok())
                .or_else(|| session.provider_session_id.clone())?;
            YouTubeEmbedDescriptor::new(&video_id, &config.public_origin)
                .ok()
                .map(|descriptor| EmbedView {
                    url: descriptor.src().clone(),
                    provider_class: "youtube",
                    min_width: YOUTUBE_EMBED_MIN_WIDTH,
                    min_height: YOUTUBE_EMBED_MIN_HEIGHT,
                })
        }
        ProviderKind::Chzzk | ProviderKind::LinkOnly => None,
    }
}

fn provider_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Twitch => "Twitch",
        ProviderKind::YouTube => "YouTube",
        ProviderKind::Chzzk => "치지직",
        ProviderKind::LinkOnly => "외부 플랫폼",
    }
}

pub(crate) fn session_is_stale(
    provider: ProviderKind,
    session: &LiveSession,
    now: DateTime<Utc>,
) -> bool {
    let max_age = match provider {
        ProviderKind::Twitch => TimeDelta::minutes(11),
        ProviderKind::YouTube | ProviderKind::Chzzk | ProviderKind::LinkOnly => {
            TimeDelta::minutes(3)
        }
    };
    session.is_stale_at(now, max_age)
}

fn first_grapheme_fallback(value: &str) -> String {
    value
        .trim()
        .chars()
        .next()
        .map_or_else(|| "R".to_owned(), |character| character.to_string())
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn is_manual_provider(provider: ProviderKind) -> bool {
    matches!(provider, ProviderKind::YouTube | ProviderKind::LinkOnly)
}

fn relative_time(value: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = now.signed_duration_since(value).num_minutes().max(0);
    match minutes {
        0 => "방금".to_owned(),
        1..60 => format!("{minutes}분 전"),
        60..1440 => format!("{}시간 전", minutes / 60),
        _ => format!("{}일 전", minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rill3_domain::{CreatorStatus, VerificationState};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn youtube_parser_accepts_only_known_hosts_and_ids() {
        let valid = Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        let attacker = Url::parse("https://www.youtube.com.evil.test/watch?v=dQw4w9WgXcQ").unwrap();
        let encoded = Url::parse("https://youtu.be/..%2Fprivate").unwrap();

        assert_eq!(parse_youtube_video_id(&valid).unwrap(), "dQw4w9WgXcQ");
        assert!(parse_youtube_video_id(&attacker).is_err());
        assert!(parse_youtube_video_id(&encoded).is_err());
    }

    #[test]
    fn relative_times_handle_future_minutes_hours_and_days() {
        let now = Utc::now();
        for (minutes, expected) in [
            (-5, "방금"),
            (0, "방금"),
            (35, "35분 전"),
            (60, "1시간 전"),
            (1439, "23시간 전"),
            (1440, "1일 전"),
        ] {
            assert_eq!(
                relative_time(now - TimeDelta::minutes(minutes), now),
                expected
            );
        }
    }

    #[test]
    fn twitch_embed_has_a_policy_sized_viewport_and_narrow_fallback() {
        let rendered = render_embed_fixture(ProviderKind::Twitch, "");

        assert!(rendered.contains("player-frame--twitch"));
        assert!(rendered.contains("width=\"400\" height=\"300\""));
        assert!(rendered.contains("player-fallback--twitch"));
        assert!(rendered.contains("href=\"https://www.twitch.tv/safe_channel\""));
        assert!(rendered.contains("class=\"stream-actions\""));
        assert!(rendered.contains("Twitch에서 직접 보기"));
        assert_eq!(rendered.matches("<iframe").count(), 1);
    }

    #[test]
    fn youtube_embed_has_trusted_origin_policy_size_and_narrow_fallback() {
        let rendered = render_embed_fixture(ProviderKind::YouTube, "");

        assert!(rendered.contains("player-frame--youtube"));
        assert!(rendered.contains("width=\"200\" height=\"200\""));
        assert!(rendered.contains("player-fallback--youtube"));
        assert!(rendered.contains("origin=https%3A%2F%2Frill3.example"));
        assert!(!rendered.contains("origin=https%3A%2F%2Fevil.example"));
        assert!(rendered.contains("href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ\""));
        assert!(rendered.contains("class=\"stream-actions\""));
        assert!(rendered.contains("YouTube에서 직접 보기"));
        assert_eq!(rendered.matches("<iframe").count(), 1);
    }

    #[test]
    fn subpath_navigation_does_not_change_youtube_embed_origin() {
        let rendered = render_embed_fixture(ProviderKind::YouTube, "/rill3");
        assert!(rendered.contains("href=\"/rill3\""));
        assert!(rendered.contains(&format!("href=\"{}\"", stylesheet_path("/rill3"))));
        assert!(rendered.contains("origin=https%3A%2F%2Frill3.example"));
        assert!(!rendered.contains("rill3.example%2Frill3"));
    }

    fn render_embed_fixture(provider: ProviderKind, base_path: &str) -> String {
        let now = Utc::now();
        let creator = Creator {
            id: Uuid::new_v4(),
            slug: "safe".to_owned(),
            display_name: "Safe".to_owned(),
            bio: None,
            locale: "ko-KR".to_owned(),
            status: CreatorStatus::Active,
            created_at: now,
            updated_at: now,
        };
        let (provider_channel_id, handle, canonical_url, current_live_url) = match provider {
            ProviderKind::Twitch => (
                "42".to_owned(),
                Some("safe_channel".to_owned()),
                Url::parse("https://www.twitch.tv/safe_channel").unwrap(),
                None,
            ),
            ProviderKind::YouTube => (
                "UC_x5XG1OV2P6uZZ5FSM9Ttw".to_owned(),
                None,
                Url::parse("https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw").unwrap(),
                Some(Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap()),
            ),
            ProviderKind::Chzzk | ProviderKind::LinkOnly => {
                panic!("fixture only supports official embed providers")
            }
        };
        let channel = ExternalChannel {
            id: Uuid::new_v4(),
            creator_id: creator.id,
            provider,
            provider_channel_id,
            handle,
            canonical_url,
            current_live_url,
            manual_live_status: (provider == ProviderKind::YouTube).then_some(LiveStatus::Online),
            manual_state_expires_at: (provider == ProviderKind::YouTube)
                .then_some(now + TimeDelta::hours(2)),
            embed_capability: EmbedCapability::OfficialEmbed,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let state = rill3_domain::LiveState::new(&channel, LiveStatus::Online, now);
        let session = LiveSession::from_state(Uuid::new_v4(), &state);

        CreatorTemplate::new(
            &creator,
            &channel,
            Some(&session),
            &EmbedConfig {
                twitch_parent: "rill3.example".to_owned(),
                public_origin: Url::parse("https://rill3.example").unwrap(),
            },
            &PagePaths::new(base_path),
            now,
        )
        .render()
        .unwrap()
    }
}
