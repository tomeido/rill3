use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, TimeDelta, Utc};
use clap::{Args, Subcommand, ValueEnum};
use rill3_db::{CreatorListing, Database, DatabaseOptions, DbError};
use rill3_domain::{
    Creator, CreatorStatus, ExternalChannel, LiveStatus, ProviderKind, VerificationState,
    VerifiedChannel, VerifyChannelInput,
};
use rill3_providers::{
    ChzzkConfig, ChzzkCredentials, ChzzkProvider, LinkOnlyProvider, ProviderError, StreamProvider,
    TwitchConfig, TwitchCredentials, TwitchProvider, YouTubeConfig, YouTubeProvider,
};
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::config::{CommonArgs, validate_origin};

#[derive(Args)]
pub(crate) struct ChannelsArgs {
    #[command(subcommand)]
    command: ChannelsCommand,
}

#[derive(Subcommand)]
enum ChannelsCommand {
    /// Register or refresh a channel. Existing creator profiles and ownership are preserved.
    Register(Box<RegisterArgs>),
    /// List registered channels, including offline and disabled channels.
    List(ListArgs),
}

#[derive(Args)]
struct RegisterArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Public creator URL slug; reuse a slug to attach another platform.
    #[arg(long)]
    slug: String,

    /// Display name used only when creating a new creator.
    #[arg(long)]
    display_name: String,

    /// Description used only when creating a new creator (at most 2000 characters).
    #[arg(long)]
    bio: Option<String>,

    #[arg(long, default_value = "ko-KR")]
    locale: String,

    #[arg(long, value_enum)]
    provider: ProviderArg,

    /// Twitch ID/login, CHZZK 32-character ID, `YouTube` UC channel ID, or link-only identifier.
    #[arg(long)]
    provider_channel_id: String,

    #[arg(long)]
    handle: Option<String>,

    /// Public HTTPS channel link; required only for `link_only`. It is never fetched.
    #[arg(long)]
    url: Option<String>,

    /// Public HTTPS broadcast/video link for `youtube` or `link_only`; alone it does not imply LIVE.
    #[arg(long)]
    live_url: Option<String>,

    /// Manual state for `youtube`/`link_only`. Omission resets the state to unknown.
    #[arg(long, value_enum)]
    live_status: Option<LiveStatusArg>,

    /// Manual online/offline state lifetime; re-register to renew it.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=1440))]
    live_ttl_minutes: u32,

    #[arg(
        long,
        env = "RILL3_PUBLIC_ORIGIN",
        default_value = "http://localhost:3000"
    )]
    public_origin: Url,

    #[arg(long, env = "RILL3_TWITCH_PARENT", default_value = "localhost")]
    twitch_parent: String,

    #[arg(long, env = "TWITCH_CLIENT_ID", hide_env_values = true)]
    twitch_client_id: Option<String>,

    #[arg(long, env = "TWITCH_CLIENT_SECRET", hide_env_values = true)]
    twitch_client_secret: Option<String>,

    #[arg(long, env = "TWITCH_EVENTSUB_SECRET", hide_env_values = true)]
    twitch_eventsub_secret: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_ID", hide_env_values = true)]
    chzzk_client_id: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_SECRET", hide_env_values = true)]
    chzzk_client_secret: Option<String>,

    #[arg(long, env = "RILL3_PROVIDER_TIMEOUT_SECONDS", default_value_t = 12,
        value_parser = clap::value_parser!(u64).range(1..=120))]
    provider_timeout_seconds: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProviderArg {
    Twitch,
    Youtube,
    Chzzk,
    #[value(name = "link_only")]
    LinkOnly,
}

impl ProviderArg {
    const fn kind(self) -> ProviderKind {
        match self {
            Self::Twitch => ProviderKind::Twitch,
            Self::Youtube => ProviderKind::YouTube,
            Self::Chzzk => ProviderKind::Chzzk,
            Self::LinkOnly => ProviderKind::LinkOnly,
        }
    }

    const fn is_manual(self) -> bool {
        matches!(self, Self::Youtube | Self::LinkOnly)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LiveStatusArg {
    Online,
    Offline,
    Unknown,
}

impl LiveStatusArg {
    const fn status(self) -> LiveStatus {
        match self {
            Self::Online => LiveStatus::Online,
            Self::Offline => LiveStatus::Offline,
            Self::Unknown => LiveStatus::Unknown,
        }
    }
}

#[derive(Args)]
struct ListArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Page size; a lookahead row is used to report the next cursor accurately.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=99))]
    limit: u32,

    /// Channel UUID from the previous page's `next_cursor`.
    #[arg(long)]
    after: Option<Uuid>,

    #[arg(long)]
    json: bool,
}

pub(crate) async fn run(arguments: ChannelsArgs) -> Result<()> {
    match arguments.command {
        ChannelsCommand::Register(arguments) => register(*arguments).await,
        ChannelsCommand::List(arguments) => list(arguments).await,
    }
}

async fn connect(arguments: &CommonArgs) -> Result<Database> {
    Database::connect(&DatabaseOptions::new(
        &arguments.database_url,
        arguments.validated_pool_size()?,
    ))
    .await
    .map_err(|_| {
        anyhow!("could not connect to PostgreSQL; check DATABASE_URL and database availability")
    })
}

async fn register(arguments: RegisterArgs) -> Result<()> {
    arguments.validate()?;
    let provider = arguments.build_provider()?;
    let input = VerifyChannelInput {
        provider_channel_id: arguments.provider_channel_id.clone(),
        handle: arguments.handle.clone(),
        external_url: arguments.url.clone(),
        current_live_url: arguments.live_url.clone(),
    };
    let verified = tokio::time::timeout(
        Duration::from_secs(arguments.provider_timeout_seconds),
        provider.verify_channel(input),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "{} channel verification timed out",
            arguments.provider.kind()
        )
    })?
    .map_err(|error| safe_provider_error(&error))?;
    let (creator, channel) = arguments.records(verified, Utc::now());
    let database = connect(&arguments.common).await?;
    database
        .migrate()
        .await
        .map_err(|_| anyhow!("could not apply database migrations"))?;
    let (creator, channel) = database
        .register_channel(&creator, &channel)
        .await
        .map_err(|error| safe_database_error(&error))?;
    let summary = ChannelSummary::new(&creator, &channel, None, Utc::now());
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

impl RegisterArgs {
    fn validate(&self) -> Result<()> {
        self.common.validated_pool_size()?;
        validate_origin(&self.public_origin, self.common.is_production())?;
        if !self.public_origin.username().is_empty() || self.public_origin.password().is_some() {
            bail!("RILL3_PUBLIC_ORIGIN must not contain credentials");
        }
        validate_profile(
            &self.slug,
            &self.display_name,
            self.bio.as_deref(),
            &self.locale,
        )?;
        validate_text("provider-channel-id", &self.provider_channel_id, 1, 128)?;
        if let Some(handle) = self.handle.as_deref() {
            validate_text("handle", handle, 1, 128)?;
        }
        for value in [self.url.as_deref(), self.live_url.as_deref()]
            .into_iter()
            .flatten()
        {
            validate_text("channel URL", value, 1, 2048)?;
        }
        if self.provider == ProviderArg::LinkOnly {
            if self.url.is_none() {
                bail!("--url is required for link_only channels");
            }
        } else if self.url.is_some() {
            bail!(
                "--url is supported only for link_only; official channel URLs are derived from the verified ID"
            );
        }
        if !self.provider.is_manual() && (self.live_status.is_some() || self.live_url.is_some()) {
            bail!("--live-status and --live-url are supported only for youtube and link_only");
        }
        if self.live_status == Some(LiveStatusArg::Online) && self.live_url.is_none() {
            bail!("--live-url is required with --live-status online");
        }
        if !(1..=1440).contains(&self.live_ttl_minutes) {
            bail!("--live-ttl-minutes must be between 1 and 1440");
        }
        Ok(())
    }

    fn build_provider(&self) -> Result<Box<dyn StreamProvider>> {
        let timeout = Duration::from_secs(self.provider_timeout_seconds);
        let provider: Box<dyn StreamProvider> = match self.provider {
            ProviderArg::Twitch => {
                let credentials = TwitchCredentials::new(
                    required_secret(self.twitch_client_id.as_deref(), "TWITCH_CLIENT_ID")?,
                    required_secret(self.twitch_client_secret.as_deref(), "TWITCH_CLIENT_SECRET")?,
                    required_secret(
                        self.twitch_eventsub_secret.as_deref(),
                        "TWITCH_EVENTSUB_SECRET",
                    )?,
                )
                .map_err(|error| safe_provider_error(&error))?;
                let config = TwitchConfig::new(&self.twitch_parent, Some(credentials))
                    .map_err(|error| safe_provider_error(&error))?
                    .with_request_timeout(timeout);
                Box::new(TwitchProvider::new(config).map_err(|error| safe_provider_error(&error))?)
            }
            ProviderArg::Chzzk => {
                let credentials = ChzzkCredentials::new(
                    required_secret(self.chzzk_client_id.as_deref(), "CHZZK_CLIENT_ID")?,
                    required_secret(self.chzzk_client_secret.as_deref(), "CHZZK_CLIENT_SECRET")?,
                )
                .map_err(|error| safe_provider_error(&error))?;
                let config = ChzzkConfig::new(Some(credentials))
                    .map_err(|error| safe_provider_error(&error))?
                    .with_request_timeout(timeout);
                Box::new(ChzzkProvider::new(config).map_err(|error| safe_provider_error(&error))?)
            }
            ProviderArg::Youtube => Box::new(YouTubeProvider::new(
                YouTubeConfig::new(self.public_origin.clone())
                    .map_err(|error| safe_provider_error(&error))?,
            )),
            ProviderArg::LinkOnly => Box::new(LinkOnlyProvider),
        };
        Ok(provider)
    }

    fn records(&self, verified: VerifiedChannel, now: DateTime<Utc>) -> (Creator, ExternalChannel) {
        let creator = Creator {
            id: Uuid::new_v4(),
            slug: self.slug.clone(),
            display_name: self.display_name.trim().to_owned(),
            bio: self.bio.clone(),
            locale: self.locale.clone(),
            status: CreatorStatus::Active,
            created_at: now,
            updated_at: now,
        };
        let mut channel = ExternalChannel::from_verified(Uuid::new_v4(), creator.id, verified, now);
        let status = self.live_status.unwrap_or(LiveStatusArg::Unknown).status();
        if self.provider.is_manual() && status != LiveStatus::Unknown {
            channel.manual_live_status = Some(status);
            channel.manual_state_expires_at =
                Some(now + TimeDelta::minutes(i64::from(self.live_ttl_minutes)));
        }
        (creator, channel)
    }
}

fn required_secret(value: Option<&str>, name: &str) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{name} is required to verify this provider's channel"))
}

fn validate_profile(slug: &str, display_name: &str, bio: Option<&str>, locale: &str) -> Result<()> {
    if !(1..=63).contains(&slug.len())
        || slug.starts_with('-')
        || slug.ends_with('-')
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("--slug must be 1..=63 lowercase ASCII letters, digits or interior hyphens");
    }
    validate_text("display-name", display_name.trim(), 1, 120)?;
    if let Some(bio) = bio
        && (bio.chars().count() > 2000
            || bio.chars().any(|character| {
                character.is_control() && !matches!(character, '\n' | '\r' | '\t')
            }))
    {
        bail!(
            "--bio must be at most 2000 characters with no control characters except line breaks and tabs"
        );
    }
    let mut segments = locale.split('-');
    let language = segments.next().unwrap_or_default();
    if !(2..=35).contains(&locale.len())
        || !(2..=3).contains(&language.len())
        || !language.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !segments.all(|segment| {
            (2..=8).contains(&segment.len())
                && segment.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        bail!("--locale must be a language tag such as ko-KR or en-US, at most 35 characters");
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, minimum: usize, maximum: usize) -> Result<()> {
    if !(minimum..=maximum).contains(&value.chars().count()) || value.chars().any(char::is_control)
    {
        bail!("{field} must contain {minimum}..={maximum} characters and no control characters");
    }
    Ok(())
}

// Flatten provider errors to their redacted Display text; reqwest source chains can contain URLs.
fn safe_provider_error(error: &ProviderError) -> anyhow::Error {
    anyhow!("{error}")
}

fn safe_database_error(error: &DbError) -> anyhow::Error {
    match error {
        DbError::RegistrationConflict(reason) => anyhow!("channel registration rejected: {reason}"),
        _ => anyhow!("channel database operation failed; check database availability and schema"),
    }
}

#[derive(Serialize)]
struct ChannelSummary<'a> {
    channel_id: Uuid,
    creator_slug: &'a str,
    display_name: &'a str,
    provider: &'static str,
    provider_channel_id: &'a str,
    enabled: bool,
    status: LiveStatus,
    manual_state_expires_at: Option<DateTime<Utc>>,
}

impl<'a> ChannelSummary<'a> {
    fn new(
        creator: &'a Creator,
        channel: &'a ExternalChannel,
        observed: Option<LiveStatus>,
        now: DateTime<Utc>,
    ) -> Self {
        let manual = matches!(
            channel.provider,
            ProviderKind::YouTube | ProviderKind::LinkOnly
        );
        let status = if manual {
            channel
                .manual_state_expires_at
                .filter(|expires| *expires > now)
                .and(channel.manual_live_status)
                .unwrap_or(LiveStatus::Unknown)
        } else {
            observed.unwrap_or(LiveStatus::Unknown)
        };
        Self {
            channel_id: channel.id,
            creator_slug: &creator.slug,
            display_name: &creator.display_name,
            provider: channel.provider.as_str(),
            provider_channel_id: &channel.provider_channel_id,
            enabled: creator.status == CreatorStatus::Active
                && channel.enabled
                && channel.verification_state != VerificationState::Disabled,
            status,
            manual_state_expires_at: channel.manual_state_expires_at,
        }
    }
}

#[derive(Serialize)]
struct ChannelPage<'a> {
    channels: Vec<ChannelSummary<'a>>,
    next_cursor: Option<Uuid>,
}

async fn list(arguments: ListArgs) -> Result<()> {
    let database = connect(&arguments.common).await?;
    let mut listings = database
        .list_registered_channels_after(arguments.after, arguments.limit + 1, true)
        .await
        .map_err(|error| safe_database_error(&error))?;
    let has_more = listings.len() > arguments.limit as usize;
    listings.truncate(arguments.limit as usize);
    let page = listing_page(&listings, has_more, Utc::now());
    if arguments.json {
        println!("{}", serde_json::to_string_pretty(&page)?);
    } else {
        println!("CHANNEL ID\tCREATOR\tPROVIDER\tPROVIDER CHANNEL ID\tENABLED\tSTATUS\tEXPIRES AT");
        for channel in page.channels {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                channel.channel_id,
                table_text(channel.creator_slug),
                channel.provider,
                table_text(channel.provider_channel_id),
                channel.enabled,
                status_text(channel.status),
                channel
                    .manual_state_expires_at
                    .map_or_else(|| "-".to_owned(), |value| value.to_rfc3339()),
            );
        }
        if let Some(cursor) = page.next_cursor {
            println!("next_cursor: {cursor} (continue with --after {cursor})");
        }
    }
    Ok(())
}

fn listing_page(
    listings: &[CreatorListing],
    has_more: bool,
    now: DateTime<Utc>,
) -> ChannelPage<'_> {
    ChannelPage {
        channels: listings
            .iter()
            .map(|listing| {
                ChannelSummary::new(
                    &listing.creator,
                    &listing.channel,
                    listing.session.as_ref().map(|session| session.state),
                    now,
                )
            })
            .collect(),
        next_cursor: has_more
            .then(|| listings.last().map(|listing| listing.channel.id))
            .flatten(),
    }
}

fn table_text(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}

const fn status_text(status: LiveStatus) -> &'static str {
    match status {
        LiveStatus::Online => "online",
        LiveStatus::Offline => "offline",
        LiveStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        arguments: ChannelsArgs,
    }

    fn parse_register_arguments(extra: &[&str]) -> std::result::Result<RegisterArgs, clap::Error> {
        let mut raw = vec![
            "channels",
            "register",
            "--database-url",
            "postgres://localhost/test",
            "--environment",
            "development",
            "--public-origin",
            "http://localhost:3000",
            "--slug",
            "test-creator",
            "--display-name",
            "테스트 채널",
            "--provider",
            "youtube",
            "--provider-channel-id",
            "UC1234567890123456789012",
        ];
        raw.extend_from_slice(extra);
        let ChannelsCommand::Register(arguments) = TestCli::try_parse_from(raw)?.arguments.command
        else {
            panic!("expected register");
        };
        Ok(*arguments)
    }

    fn register_arguments(extra: &[&str]) -> RegisterArgs {
        parse_register_arguments(extra).unwrap()
    }

    async fn verify(arguments: &RegisterArgs) -> Result<VerifiedChannel> {
        arguments.validate()?;
        arguments
            .build_provider()?
            .verify_channel(VerifyChannelInput {
                provider_channel_id: arguments.provider_channel_id.clone(),
                handle: arguments.handle.clone(),
                external_url: arguments.url.clone(),
                current_live_url: arguments.live_url.clone(),
            })
            .await
            .map_err(|error| safe_provider_error(&error))
    }

    #[test]
    fn rejects_invalid_profiles_before_database_access() {
        for slug in ["", "A", "-name", "name-", "../name", "name/name", "이름"] {
            assert!(validate_profile(slug, "Creator", None, "ko-KR").is_err());
        }
        assert!(validate_profile("name", "  ", None, "ko-KR").is_err());
        assert!(validate_profile("name", "Creator\u{1b}[31m", None, "ko-KR").is_err());
        assert!(validate_profile("name", "Creator", Some(&"한".repeat(2001)), "ko-KR").is_err());
        assert!(validate_profile("name", "Creator", None, "en--US").is_err());
        assert!(validate_profile("name", "한글 이름", Some("여러 줄\n소개"), "ko-KR").is_ok());
    }

    #[tokio::test]
    async fn youtube_url_without_status_is_registered_unknown() {
        let arguments = register_arguments(&["--live-url", "https://youtu.be/dQw4w9WgXcQ"]);
        let verified = verify(&arguments).await.unwrap();
        assert_eq!(
            verified.current_live_url.as_ref().unwrap().as_str(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        let (_, channel) = arguments.records(verified, Utc::now());
        assert!(channel.manual_live_status.is_none());
        assert!(channel.manual_state_expires_at.is_none());
    }

    #[tokio::test]
    async fn manual_registrations_satisfy_database_status_expiry_pair_constraint() {
        let now = Utc::now();
        for provider in [ProviderArg::Youtube, ProviderArg::LinkOnly] {
            for status in [
                None,
                Some(LiveStatusArg::Unknown),
                Some(LiveStatusArg::Online),
                Some(LiveStatusArg::Offline),
            ] {
                let mut arguments =
                    register_arguments(&["--live-url", "https://youtu.be/dQw4w9WgXcQ"]);
                arguments.provider = provider;
                arguments.live_status = status;
                if provider == ProviderArg::LinkOnly {
                    arguments.url = Some("https://video.example/channel".to_owned());
                }
                let verified = verify(&arguments).await.unwrap();
                let (_, channel) = arguments.records(verified, now);
                assert_eq!(
                    channel.manual_live_status.is_some(),
                    channel.manual_state_expires_at.is_some(),
                    "external_channels_manual_state_consistent requires both values or neither"
                );
                let expected = status.unwrap_or(LiveStatusArg::Unknown).status();
                assert_eq!(rill3_domain::manual_live_status(&channel, now), expected);
                if expected == LiveStatus::Unknown {
                    assert!(channel.manual_live_status.is_none());
                    assert!(channel.manual_state_expires_at.is_none());
                } else {
                    assert_eq!(
                        channel.manual_state_expires_at,
                        Some(now + TimeDelta::minutes(120))
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn explicit_online_requires_a_video_and_expires_after_default_ttl() {
        assert!(
            register_arguments(&["--live-status", "online"])
                .validate()
                .is_err()
        );
        let arguments = register_arguments(&[
            "--live-status",
            "online",
            "--live-url",
            "https://youtu.be/dQw4w9WgXcQ",
        ]);
        let now = Utc::now();
        let verified = verify(&arguments).await.unwrap();
        let (creator, channel) = arguments.records(verified, now);
        assert_eq!(
            channel.manual_state_expires_at,
            Some(now + TimeDelta::minutes(120))
        );
        assert_eq!(
            ChannelSummary::new(&creator, &channel, None, now).status,
            LiveStatus::Online
        );
        assert_eq!(
            ChannelSummary::new(
                &creator,
                &channel,
                Some(LiveStatus::Online),
                now + TimeDelta::minutes(120)
            )
            .status,
            LiveStatus::Unknown
        );
    }

    #[test]
    fn ttl_and_pagination_are_bounded_by_clap() {
        for ttl in ["0", "1441"] {
            assert!(parse_register_arguments(&["--live-ttl-minutes", ttl]).is_err());
        }
        for ttl in ["1", "1440"] {
            assert!(parse_register_arguments(&["--live-ttl-minutes", ttl]).is_ok());
        }
        for limit in ["0", "100"] {
            assert!(
                TestCli::try_parse_from([
                    "channels",
                    "list",
                    "--database-url",
                    "postgres://localhost/test",
                    "--limit",
                    limit
                ])
                .is_err()
            );
        }
        assert!(
            TestCli::try_parse_from([
                "channels",
                "list",
                "--database-url",
                "postgres://localhost/test",
                "--limit",
                "99",
                "--after",
                "00000000-0000-0000-0000-000000000001",
                "--json"
            ])
            .is_ok()
        );
    }

    #[tokio::test]
    async fn youtube_and_link_only_reject_unsafe_urls_without_echoing_them() {
        for raw in [
            "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
            "https://secret:password@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "javascript:secret",
        ] {
            let arguments = register_arguments(&["--live-url", raw]);
            let error = verify(&arguments).await.unwrap_err().to_string();
            assert!(!error.contains("secret"));
            assert!(!error.contains(raw));
        }
        for raw in [
            "https://127.0.0.1/live",
            "https://secret:password@video.example/live",
            "file:///secret",
        ] {
            let mut arguments = register_arguments(&[]);
            arguments.provider = ProviderArg::LinkOnly;
            arguments.url = Some(raw.to_owned());
            let error = verify(&arguments).await.unwrap_err().to_string();
            assert!(!error.contains("secret"));
            assert!(!error.contains(raw));
        }
    }

    #[test]
    fn production_origin_and_provider_option_boundaries_are_enforced() {
        let mut arguments = register_arguments(&[]);
        arguments.common.environment = "production".to_owned();
        arguments.public_origin = Url::parse("http://localhost:3000").unwrap();
        assert!(arguments.validate().is_err());
        arguments.public_origin = Url::parse("https://rill3.example").unwrap();
        assert!(arguments.validate().is_ok());
        arguments.provider = ProviderArg::Chzzk;
        arguments.live_status = Some(LiveStatusArg::Online);
        arguments.live_url = Some("https://video.example/live".to_owned());
        assert!(arguments.validate().is_err());
    }

    #[test]
    fn selected_provider_credentials_and_database_errors_do_not_expose_secrets() {
        let mut arguments = register_arguments(&[]);
        arguments.twitch_client_id = Some("unrelated-incomplete-config".to_owned());
        assert!(arguments.build_provider().is_ok());
        arguments.provider = ProviderArg::Chzzk;
        arguments.chzzk_client_id = None;
        arguments.chzzk_client_secret = Some("secret-value".to_owned());
        let error = arguments.build_provider().err().unwrap().to_string();
        assert!(error.contains("CHZZK_CLIENT_ID"));
        assert!(!error.contains("secret-value"));
        let error = safe_database_error(&DbError::InvalidPersistedValue {
            field: "canonical_url",
            value: "https://example.com/?token=secret-value".to_owned(),
        });
        assert!(!format!("{error:#}").contains("secret-value"));
    }

    #[tokio::test]
    async fn list_page_includes_offline_and_reports_only_real_continuations() {
        let arguments = register_arguments(&["--live-status", "offline"]);
        let now = Utc::now();
        let verified = verify(&arguments).await.unwrap();
        let (creator, channel) = arguments.records(verified, now);
        let id = channel.id;
        let listings = [CreatorListing {
            creator,
            channel,
            session: None,
        }];
        let page = listing_page(&listings, false, now);
        assert_eq!(page.channels[0].status, LiveStatus::Offline);
        assert!(page.next_cursor.is_none());
        assert_eq!(listing_page(&listings, true, now).next_cursor, Some(id));
        assert!(listing_page(&[], false, now).channels.is_empty());
    }

    #[tokio::test]
    async fn list_marks_moderation_disabled_channels_unavailable() {
        let arguments = register_arguments(&[]);
        let now = Utc::now();
        let verified = verify(&arguments).await.unwrap();
        let (creator, mut channel) = arguments.records(verified, now);
        assert!(ChannelSummary::new(&creator, &channel, None, now).enabled);
        channel.verification_state = VerificationState::Disabled;
        assert!(!ChannelSummary::new(&creator, &channel, None, now).enabled);
    }
}
