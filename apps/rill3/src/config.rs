use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use rill3_providers::YOUTUBE_WEBSUB_TOPIC_PREFIX;
use url::Url;

const DEFAULT_DATABASE_CONNECTIONS: u32 = 10;
const MAX_DATABASE_CONNECTIONS: u32 = 20;

#[derive(Parser)]
#[command(
    name = "rill3",
    version,
    about = "RILL3 external-stream discovery service"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Serve public SSR pages, APIs, health checks, and provider webhooks.
    Server(Box<ServerArgs>),
    /// Reconcile registered external channels into `PostgreSQL`.
    Worker(Box<WorkerArgs>),
    /// Placeholder for the M3 chain indexer boundary.
    Indexer(IndexerArgs),
    /// Insert deterministic local-development fixtures.
    SeedDemo(SeedArgs),
    /// Register and inspect real channels using operator database access.
    Channels(crate::command_channels::ChannelsArgs),
}

#[derive(Clone, Args)]
pub struct CommonArgs {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    #[arg(
        long,
        env = "RILL3_DB_MAX_CONNECTIONS",
        default_value_t = DEFAULT_DATABASE_CONNECTIONS
    )]
    pub database_max_connections: u32,

    #[arg(long, env = "RILL3_ENVIRONMENT", default_value = "development")]
    pub environment: String,
}

#[derive(Clone, Args)]
pub struct ServerArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    #[command(flatten)]
    pub web3: crate::web3::Web3Args,

    #[arg(long, env = "RILL3_BIND_ADDR", default_value = "127.0.0.1:3000")]
    pub bind_addr: SocketAddr,

    #[arg(
        long,
        env = "RILL3_PUBLIC_ORIGIN",
        default_value = "http://localhost:3000"
    )]
    pub public_origin: Url,

    /// URL path prefix without a trailing slash, for example /rill3.
    #[arg(long, env = "RILL3_BASE_PATH", default_value = "")]
    pub base_path: String,

    #[arg(long, env = "RILL3_TWITCH_PARENT", default_value = "localhost")]
    pub twitch_parent: String,

    #[arg(long, env = "TWITCH_EVENTSUB_SECRET", hide_env_values = true)]
    pub twitch_eventsub_secret: Option<String>,

    #[arg(long, env = "TWITCH_CLIENT_ID", hide_env_values = true)]
    pub twitch_client_id: Option<String>,

    #[arg(long, env = "TWITCH_CLIENT_SECRET", hide_env_values = true)]
    pub twitch_client_secret: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_ID", hide_env_values = true)]
    pub chzzk_client_id: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_SECRET", hide_env_values = true)]
    pub chzzk_client_secret: Option<String>,

    #[arg(long, env = "YOUTUBE_WEBSUB_TOKEN", hide_env_values = true)]
    pub youtube_websub_token: Option<String>,

    #[arg(long, env = "YOUTUBE_WEBSUB_SECRET", hide_env_values = true)]
    pub youtube_websub_secret: Option<String>,

    #[arg(
        long,
        env = "YOUTUBE_WEBSUB_TOPIC_PREFIX",
        default_value = YOUTUBE_WEBSUB_TOPIC_PREFIX
    )]
    pub youtube_websub_topic_prefix: String,

    #[arg(long, env = "RILL3_REQUEST_TIMEOUT_SECONDS", default_value_t = 10)]
    pub request_timeout_seconds: u64,

    #[arg(long, env = "RILL3_WEBHOOK_MAX_BYTES", default_value_t = 65_536)]
    pub webhook_max_bytes: usize,
}

#[derive(Clone, Args)]
pub struct WorkerArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Reconcile one batch and exit; useful for smoke tests and scheduled jobs.
    #[arg(long)]
    pub once: bool,

    #[arg(long, env = "RILL3_WORKER_INTERVAL_SECONDS", default_value_t = 60)]
    pub interval_seconds: u64,

    #[arg(long, env = "RILL3_PROVIDER_TIMEOUT_SECONDS", default_value_t = 12)]
    pub provider_timeout_seconds: u64,

    #[arg(long, env = "TWITCH_CLIENT_ID", hide_env_values = true)]
    pub twitch_client_id: Option<String>,

    #[arg(long, env = "TWITCH_CLIENT_SECRET", hide_env_values = true)]
    pub twitch_client_secret: Option<String>,

    #[arg(long, env = "TWITCH_EVENTSUB_SECRET", hide_env_values = true)]
    pub twitch_eventsub_secret: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_ID", hide_env_values = true)]
    pub chzzk_client_id: Option<String>,

    #[arg(long, env = "CHZZK_CLIENT_SECRET", hide_env_values = true)]
    pub chzzk_client_secret: Option<String>,

    #[arg(
        long,
        env = "RILL3_PUBLIC_ORIGIN",
        default_value = "http://localhost:3000"
    )]
    pub public_origin: Url,

    #[arg(long, env = "RILL3_TWITCH_PARENT", default_value = "localhost")]
    pub twitch_parent: String,
}

#[derive(Clone, Args)]
pub struct IndexerArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Clone, Args)]
pub struct SeedArgs {
    #[command(flatten)]
    pub common: CommonArgs,
}

impl CommonArgs {
    pub fn validated_pool_size(&self) -> Result<u32> {
        if self.database_max_connections == 0 {
            bail!("RILL3_DB_MAX_CONNECTIONS must be at least 1");
        }
        if self.database_max_connections > MAX_DATABASE_CONNECTIONS {
            bail!("RILL3_DB_MAX_CONNECTIONS cannot exceed {MAX_DATABASE_CONNECTIONS}");
        }
        Ok(self.database_max_connections)
    }

    pub fn is_production(&self) -> bool {
        self.environment.eq_ignore_ascii_case("production")
    }
}

impl ServerArgs {
    pub fn validate(&self) -> Result<()> {
        self.common.validated_pool_size()?;
        validate_origin(&self.public_origin, self.common.is_production())?;
        validate_base_path(&self.base_path)?;
        validate_hostname(&self.twitch_parent)?;
        if self.request_timeout_seconds == 0 {
            bail!("RILL3_REQUEST_TIMEOUT_SECONDS must be at least 1");
        }
        if !(1_024..=1_048_576).contains(&self.webhook_max_bytes) {
            bail!("RILL3_WEBHOOK_MAX_BYTES must be between 1024 and 1048576");
        }
        validate_optional_secret(
            self.youtube_websub_token.as_deref(),
            "YOUTUBE_WEBSUB_TOKEN",
            16,
            255,
        )?;
        validate_optional_secret(
            self.youtube_websub_secret.as_deref(),
            "YOUTUBE_WEBSUB_SECRET",
            16,
            255,
        )?;
        validate_youtube_websub_topic_prefix(&self.youtube_websub_topic_prefix)?;
        self.web3.validate()?;
        Ok(())
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }
}

impl WorkerArgs {
    pub fn validate(&self) -> Result<()> {
        if self.common.validated_pool_size()? < 2 {
            bail!(
                "worker RILL3_DB_MAX_CONNECTIONS must be at least 2 because its advisory lock holds one connection"
            );
        }
        validate_origin(&self.public_origin, self.common.is_production())?;
        validate_hostname(&self.twitch_parent)?;
        if self.interval_seconds < 30 {
            bail!("RILL3_WORKER_INTERVAL_SECONDS must be at least 30");
        }
        if self.provider_timeout_seconds == 0 {
            bail!("RILL3_PROVIDER_TIMEOUT_SECONDS must be at least 1");
        }
        Ok(())
    }

    pub fn provider_timeout(&self) -> Duration {
        Duration::from_secs(self.provider_timeout_seconds)
    }
}

pub(crate) fn validate_origin(origin: &Url, production: bool) -> Result<()> {
    if origin.cannot_be_a_base() || origin.host_str().is_none() {
        bail!("RILL3_PUBLIC_ORIGIN must be an absolute origin URL");
    }
    if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
        bail!("RILL3_PUBLIC_ORIGIN cannot contain a path, query, or fragment");
    }
    if production && origin.scheme() != "https" {
        bail!("RILL3_PUBLIC_ORIGIN must use https in production");
    }
    if !matches!(origin.scheme(), "http" | "https") {
        bail!("RILL3_PUBLIC_ORIGIN must use http or https");
    }
    Ok(())
}

fn validate_hostname(host: &str) -> Result<()> {
    let parsed = Url::parse(&format!("https://{host}/"))
        .with_context(|| "RILL3_TWITCH_PARENT must be a hostname")?;
    if parsed.host_str() != Some(host)
        || parsed.port().is_some()
        || host.contains('/')
        || host.contains('@')
    {
        bail!("RILL3_TWITCH_PARENT must be a hostname without scheme, port, or path");
    }
    Ok(())
}

fn validate_base_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    if !path.starts_with('/')
        || path[1..].split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || !segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
                })
        })
        || matches!(path, "/health/live" | "/health/ready")
    {
        bail!(
            "RILL3_BASE_PATH must be empty or an absolute path with URL-safe segments, no trailing slash, and no health-check route collision"
        );
    }
    Ok(())
}

fn validate_youtube_websub_topic_prefix(prefix: &str) -> Result<()> {
    if prefix != YOUTUBE_WEBSUB_TOPIC_PREFIX {
        bail!("YOUTUBE_WEBSUB_TOPIC_PREFIX must be the official YouTube feed URL");
    }
    Ok(())
}

fn validate_optional_secret(
    value: Option<&str>,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<()> {
    if let Some(value) = value.filter(|value| !value.is_empty())
        && (!(minimum..=maximum).contains(&value.len()) || !value.is_ascii())
    {
        bail!("{name} must be {minimum}..={maximum} ASCII characters when configured");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_requires_https_origin() {
        let result = validate_origin(&Url::parse("http://rill3.example").unwrap(), true);
        assert!(result.is_err());
    }

    #[test]
    fn twitch_parent_cannot_smuggle_a_path() {
        assert!(validate_hostname("rill3.example/evil").is_err());
        assert!(validate_hostname("rill3.example").is_ok());
    }

    #[test]
    fn base_path_requires_canonical_url_safe_segments() {
        for path in ["", "/rill3", "/apps/rill3-v1.0"] {
            assert!(validate_base_path(path).is_ok(), "{path}");
        }
        for path in [
            "/",
            "rill3",
            "/rill3/",
            "//evil.example",
            "/apps//rill3",
            "/apps/../rill3",
            "/apps/./rill3",
            "/rill3?query",
            "/rill3#fragment",
            "/rill3%2f",
            "/{slug}",
            "/health/live",
            "/health/ready",
        ] {
            assert!(validate_base_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn base_path_is_separate_from_the_embed_origin() {
        let mut arguments = valid_production_server_args();
        arguments.base_path = "/rill3".to_owned();
        assert!(arguments.validate().is_ok());

        arguments.public_origin = Url::parse("https://rill3.example/rill3").unwrap();
        assert!(arguments.validate().is_err());
    }

    #[test]
    fn youtube_websub_requires_the_official_topic_prefix() {
        assert!(validate_youtube_websub_topic_prefix(YOUTUBE_WEBSUB_TOPIC_PREFIX).is_ok());
        assert!(
            validate_youtube_websub_topic_prefix(
                "https://www.youtube.com/xml/feeds/videos.xml?channel_id="
            )
            .is_err()
        );
    }

    #[test]
    fn production_server_validation_rejects_the_legacy_youtube_topic() {
        let mut arguments = valid_production_server_args();
        assert!(arguments.validate().is_ok());

        arguments.youtube_websub_topic_prefix =
            "https://www.youtube.com/xml/feeds/videos.xml?channel_id=".to_owned();
        assert!(arguments.validate().is_err());
    }

    fn valid_production_server_args() -> ServerArgs {
        ServerArgs {
            web3: crate::web3::Web3Args::default(),
            common: CommonArgs {
                database_url: "postgres://rill3:secret@postgres/rill3".to_owned(),
                database_max_connections: DEFAULT_DATABASE_CONNECTIONS,
                environment: "production".to_owned(),
            },
            bind_addr: "127.0.0.1:3000".parse().unwrap(),
            public_origin: Url::parse("https://rill3.example").unwrap(),
            base_path: String::new(),
            twitch_parent: "rill3.example".to_owned(),
            twitch_eventsub_secret: None,
            twitch_client_id: None,
            twitch_client_secret: None,
            chzzk_client_id: None,
            chzzk_client_secret: None,
            youtube_websub_token: None,
            youtube_websub_secret: None,
            youtube_websub_topic_prefix: YOUTUBE_WEBSUB_TOPIC_PREFIX.to_owned(),
            request_timeout_seconds: 10,
            webhook_max_bytes: 65_536,
        }
    }
}
