use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rill3_db::{Database, DatabaseOptions};
use rill3_domain::{
    Creator, CreatorStatus, EmbedCapability, ExternalChannel, LiveState, LiveStatus, ProviderKind,
    VerificationState,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::{
    config::{SeedArgs, ServerArgs},
    http::{HttpState, default_provider_statuses},
    provider_config::ProviderCredentials,
    views::EmbedConfig,
    webhooks::WebhookConfig,
};

pub(crate) async fn run(arguments: ServerArgs) -> Result<()> {
    arguments.validate()?;
    let request_timeout = arguments.request_timeout();
    let pool_size = arguments.common.validated_pool_size()?;
    let database = Database::connect(&DatabaseOptions::new(
        &arguments.common.database_url,
        pool_size,
    ))
    .await
    .context("connect to PostgreSQL")?;
    database
        .migrate()
        .await
        .context("apply database migrations")?;

    let credentials = ProviderCredentials::new(
        arguments.twitch_client_id.as_deref(),
        arguments.twitch_client_secret.as_deref(),
        arguments.twitch_eventsub_secret.as_deref(),
        arguments.chzzk_client_id.as_deref(),
        arguments.chzzk_client_secret.as_deref(),
    );
    credentials.log_disabled();
    let twitch_enabled = credentials.twitch.is_ready();
    let chzzk_enabled = credentials.chzzk.is_ready();
    let state = HttpState::new(
        Arc::new(database),
        &arguments.base_path,
        EmbedConfig {
            twitch_parent: arguments.twitch_parent.clone(),
            public_origin: arguments.public_origin.clone(),
        },
        default_provider_statuses(twitch_enabled, chzzk_enabled),
        WebhookConfig::new(
            arguments.webhook_max_bytes,
            arguments.twitch_eventsub_secret.filter(|_| twitch_enabled),
            arguments.youtube_websub_token,
            arguments.youtube_websub_secret,
            arguments.youtube_websub_topic_prefix,
        ),
    );
    let app = crate::http::router(state, request_timeout);
    let listener = TcpListener::bind(arguments.bind_addr)
        .await
        .with_context(|| format!("bind server listener at {}", arguments.bind_addr))?;
    tracing::info!(address = %arguments.bind_addr, "server listening");

    let shutdown = CancellationToken::new();
    let signal_token = shutdown.clone();
    tokio::spawn(crate::shutdown::signal(signal_token));
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
        .context("serve HTTP")?;
    tracing::info!("server stopped gracefully");
    Ok(())
}

pub(crate) async fn seed_demo(arguments: SeedArgs) -> Result<()> {
    arguments.common.validated_pool_size()?;
    if arguments.common.is_production() {
        bail!("seed-demo is disabled in production");
    }
    let database = Database::connect(&DatabaseOptions::new(
        &arguments.common.database_url,
        arguments.common.database_max_connections,
    ))
    .await
    .context("connect to PostgreSQL")?;
    database
        .migrate()
        .await
        .context("apply database migrations")?;

    let now = Utc::now();
    let creator = Creator {
        id: Uuid::parse_str("8d84c34a-f856-498f-a2b8-1f8c6b1c53c0")?,
        slug: "rill3-demo".to_owned(),
        display_name: "RILL3 Demo".to_owned(),
        bio: Some("공식 API와 임베드 경계를 확인하는 로컬 개발 fixture입니다.".to_owned()),
        locale: "ko-KR".to_owned(),
        status: CreatorStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let channel = ExternalChannel {
        id: Uuid::parse_str("ee55f843-769b-4425-a266-2502a0e1552a")?,
        creator_id: creator.id,
        provider: ProviderKind::Twitch,
        provider_channel_id: "141981764".to_owned(),
        handle: Some("twitchdev".to_owned()),
        canonical_url: Url::parse("https://www.twitch.tv/twitchdev")?,
        current_live_url: None,
        manual_live_status: None,
        manual_state_expires_at: None,
        embed_capability: EmbedCapability::OfficialEmbed,
        verification_state: VerificationState::Verified,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    database.upsert_creator(&creator).await?;
    database.upsert_channel(&channel).await?;
    let mut live = LiveState::new(&channel, LiveStatus::Online, now);
    live.provider_session_id = Some("rill3-local-demo".to_owned());
    live.title = Some("RILL3 M0/M1 discovery vertical slice".to_owned());
    live.category = Some("Science & Technology".to_owned());
    live.started_at = Some(now);
    live.thumbnail_url = Some(Url::parse(
        "https://static-cdn.jtvnw.net/previews-ttv/live_user_twitchdev-640x360.jpg",
    )?);
    database.apply_reconciled_state(&live).await?;
    tracing::info!(creator_slug = %creator.slug, "development fixture is ready");
    Ok(())
}
