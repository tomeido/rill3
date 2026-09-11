use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use rill3_db::{Database, DatabaseOptions, MAX_QUERY_RESULTS};
use rill3_domain::{ExternalChannel, LiveState, ProviderAvailability, ProviderKind};
use rill3_providers::{
    BackoffPolicy, ChzzkConfig, ChzzkProvider, LinkOnlyProvider, StreamProvider, TwitchConfig,
    TwitchProvider, YouTubeConfig, YouTubeProvider,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::{config::WorkerArgs, provider_config::ProviderCredentials};

const WORKER_LOCK_KEY: i64 = 0x5249_4c4c_3301;
const LOCK_HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const LOCK_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROVIDER_BATCHES: usize = 100;
const CIRCUIT_BREAKER_FAILURES: u32 = 5;
const CIRCUIT_BREAKER_COOLDOWN: Duration = Duration::from_mins(15);

struct ScheduledProvider {
    provider: Arc<dyn StreamProvider>,
    interval: Duration,
}

#[async_trait::async_trait]
trait WorkerStore: Send + Sync {
    async fn channels_after(
        &self,
        provider: ProviderKind,
        after: Option<uuid::Uuid>,
        limit: u32,
    ) -> Result<Vec<ExternalChannel>>;
    async fn apply(&self, state: &LiveState) -> Result<()>;
    async fn mark_stale(&self, provider: ProviderKind) -> Result<()>;
}

#[async_trait::async_trait]
impl WorkerStore for Database {
    async fn channels_after(
        &self,
        provider: ProviderKind,
        after: Option<uuid::Uuid>,
        limit: u32,
    ) -> Result<Vec<ExternalChannel>> {
        self.channels_for_provider_after(provider, after, limit)
            .await
            .map_err(Into::into)
    }

    async fn apply(&self, state: &LiveState) -> Result<()> {
        let outcome = self.apply_reconciled_state(state).await?;
        tracing::debug!(
            provider = %state.provider,
            provider_channel_id = %state.provider_channel_id,
            ?outcome,
            "reconciliation state applied"
        );
        Ok(())
    }

    async fn mark_stale(&self, provider: ProviderKind) -> Result<()> {
        let affected = self.mark_provider_stale(provider, Utc::now()).await?;
        tracing::warn!(%provider, affected, "provider state marked stale");
        Ok(())
    }
}

pub(crate) async fn run(arguments: WorkerArgs) -> Result<()> {
    arguments.validate()?;
    let database = Arc::new(
        Database::connect(&DatabaseOptions::new(
            &arguments.common.database_url,
            arguments.common.validated_pool_size()?,
        ))
        .await
        .context("connect to PostgreSQL")?,
    );
    database
        .migrate()
        .await
        .context("apply database migrations")?;
    let Some(mut scheduler_lock) = database
        .try_advisory_lock(WORKER_LOCK_KEY)
        .await
        .context("acquire provider scheduler advisory lock")?
    else {
        tracing::info!("another provider scheduler owns the advisory lock; exiting");
        return Ok(());
    };

    let providers = build_providers(&arguments)?;
    if arguments.once {
        let outcome = run_once(database.clone(), &providers, arguments.provider_timeout()).await;
        let release = scheduler_lock
            .release()
            .await
            .context("release provider scheduler advisory lock");
        outcome?;
        release?;
        return Ok(());
    }

    let cancellation = CancellationToken::new();
    tokio::spawn(crate::shutdown::signal(cancellation.clone()));
    let mut tasks = JoinSet::new();
    for scheduled in providers {
        if scheduled.provider.availability() == ProviderAvailability::Disabled {
            let kind = scheduled.provider.kind();
            tracing::info!(
                provider = %kind,
                reason = scheduled.provider.disabled_reason().unwrap_or("disabled"),
                "provider is disabled; worker remains available"
            );
            database
                .mark_stale(kind)
                .await
                .with_context(|| format!("mark disabled {kind} provider stale"))?;
            continue;
        }
        let backoff = BackoffPolicy::new(scheduled.interval, CIRCUIT_BREAKER_COOLDOWN, 20)
            .context("build provider backoff policy")?;
        tasks.spawn(provider_loop(
            database.clone(),
            scheduled,
            arguments.provider_timeout(),
            backoff,
            cancellation.child_token(),
        ));
    }

    let mut lock_health = tokio::time::interval(LOCK_HEALTH_INTERVAL);
    lock_health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    lock_health.tick().await;
    let lock_error = loop {
        tokio::select! {
            () = cancellation.cancelled() => break None,
            _ = lock_health.tick() => {
                let result = tokio::time::timeout(
                    LOCK_HEALTH_TIMEOUT,
                    scheduler_lock.ensure_held(),
                ).await;
                let failure = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(anyhow!(error)),
                    Err(_) => Some(anyhow!(
                        "advisory lock health probe exceeded {LOCK_HEALTH_TIMEOUT:?}"
                    )),
                };
                if let Some(error) = failure {
                    tracing::error!(%error, "provider scheduler advisory lock was lost");
                    cancellation.cancel();
                    break Some(error);
                }
            }
        }
    };
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tracing::error!(%error, "provider task failed while shutting down");
        }
    }
    if let Some(error) = lock_error {
        drop(scheduler_lock);
        return Err(error.context("provider scheduler advisory lock was lost"));
    }
    scheduler_lock.release().await?;
    tracing::info!("provider worker stopped gracefully");
    Ok(())
}

async fn run_once(
    store: Arc<dyn WorkerStore>,
    providers: &[ScheduledProvider],
    timeout: Duration,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    for scheduled in providers {
        if scheduled.provider.availability() == ProviderAvailability::Disabled {
            let kind = scheduled.provider.kind();
            tracing::info!(
                provider = %kind,
                reason = scheduled.provider.disabled_reason().unwrap_or("disabled"),
                "provider is disabled; skipping one-shot reconciliation"
            );
            store
                .mark_stale(kind)
                .await
                .with_context(|| format!("mark disabled {kind} provider stale"))?;
            continue;
        }
        let store = store.clone();
        let provider = scheduled.provider.clone();
        tasks.spawn(async move {
            let kind = provider.kind();
            if let Err(error) = reconcile(store.as_ref(), provider.as_ref(), timeout).await {
                tracing::warn!(provider = %kind, %error, "provider reconciliation failed");
                store
                    .mark_stale(kind)
                    .await
                    .with_context(|| format!("mark failed {kind} reconciliation stale"))?;
                return Err(error).with_context(|| format!("reconcile {kind}"));
            }
            Ok(())
        });
    }

    let mut first_error = None;
    while let Some(result) = tasks.join_next().await {
        let result = match result {
            Ok(result) => result,
            Err(error) => Err(anyhow!(error).context("one-shot provider task failed")),
        };
        if let Err(error) = result {
            tracing::error!(%error, "one-shot provider reconciliation failed");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn provider_loop(
    store: Arc<dyn WorkerStore>,
    scheduled: ScheduledProvider,
    timeout: Duration,
    backoff: BackoffPolicy,
    cancellation: CancellationToken,
) {
    let mut attempt = 0_u32;
    loop {
        if cancellation.is_cancelled() {
            return;
        }
        let kind = scheduled.provider.kind();
        let reconciliation = tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            result = reconcile(store.as_ref(), scheduled.provider.as_ref(), timeout) => result,
        };
        let delay = match reconciliation {
            Ok(()) => {
                attempt = 0;
                jitter(scheduled.interval)
            }
            Err(error) => {
                let stale_result = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    result = store.mark_stale(kind) => result,
                };
                if let Err(mark_error) = stale_result {
                    tracing::error!(provider = %kind, error = %mark_error, "failed to mark provider stale");
                }
                attempt = attempt.saturating_add(1);
                let circuit_open = attempt >= CIRCUIT_BREAKER_FAILURES;
                let delay = retry_delay(
                    &error,
                    backoff,
                    attempt.saturating_sub(1),
                    scheduled.interval,
                    circuit_open,
                    fastrand::u64(..),
                );
                tracing::warn!(
                    provider = %kind,
                    %error,
                    ?delay,
                    consecutive_failures = attempt,
                    circuit_open,
                    "provider failed; scheduling retry"
                );
                delay
            }
        };

        tokio::select! {
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

fn retry_delay(
    error: &anyhow::Error,
    backoff: BackoffPolicy,
    attempt: u32,
    normal_interval: Duration,
    circuit_open: bool,
    jitter_sample: u64,
) -> Duration {
    let adaptive = error
        .downcast_ref::<rill3_providers::ProviderError>()
        .and_then(|provider_error| {
            provider_error.backoff_delay_with_jitter(backoff, attempt, jitter_sample)
        })
        .unwrap_or_else(|| backoff.delay_with_jitter(attempt, None, jitter_sample));
    let delay = adaptive.max(normal_interval);
    if circuit_open {
        delay.max(CIRCUIT_BREAKER_COOLDOWN)
    } else {
        delay
    }
}

async fn reconcile(
    store: &dyn WorkerStore,
    provider: &dyn StreamProvider,
    timeout: Duration,
) -> Result<()> {
    let kind = provider.kind();
    let timeout = reconciliation_timeout(kind, timeout);
    if kind == ProviderKind::Chzzk {
        let channels = all_channels(store, kind).await?;
        return reconcile_batch(store, provider, &channels, timeout).await;
    }

    let mut after = None;
    for _ in 0..MAX_PROVIDER_BATCHES {
        let channels = store.channels_after(kind, after, MAX_QUERY_RESULTS).await?;
        if channels.is_empty() {
            return Ok(());
        }
        let next_after = channels.last().map(|channel| channel.id);
        reconcile_batch(store, provider, &channels, timeout).await?;
        if channels.len() < usize::try_from(MAX_QUERY_RESULTS).unwrap_or(usize::MAX) {
            return Ok(());
        }
        after = next_after;
    }

    if store.channels_after(kind, after, 1).await?.is_empty() {
        return Ok(());
    }
    bail!(
        "{kind} has more than {} enabled channels; refusing an unbounded reconciliation pass",
        MAX_PROVIDER_BATCHES * usize::try_from(MAX_QUERY_RESULTS).unwrap_or(0)
    )
}

fn reconciliation_timeout(kind: ProviderKind, configured: Duration) -> Duration {
    if kind == ProviderKind::Chzzk {
        configured.max(Duration::from_mins(1))
    } else {
        configured
    }
}

async fn all_channels(store: &dyn WorkerStore, kind: ProviderKind) -> Result<Vec<ExternalChannel>> {
    let mut channels = Vec::new();
    let mut after = None;
    for _ in 0..MAX_PROVIDER_BATCHES {
        let mut page = store.channels_after(kind, after, MAX_QUERY_RESULTS).await?;
        if page.is_empty() {
            return Ok(channels);
        }
        after = page.last().map(|channel| channel.id);
        let is_last = page.len() < usize::try_from(MAX_QUERY_RESULTS).unwrap_or(usize::MAX);
        channels.append(&mut page);
        if is_last {
            return Ok(channels);
        }
    }
    if store.channels_after(kind, after, 1).await?.is_empty() {
        return Ok(channels);
    }
    bail!(
        "{kind} has more than {} enabled channels; refusing an unbounded reconciliation pass",
        MAX_PROVIDER_BATCHES * usize::try_from(MAX_QUERY_RESULTS).unwrap_or(0)
    )
}

async fn reconcile_batch(
    store: &dyn WorkerStore,
    provider: &dyn StreamProvider,
    channels: &[ExternalChannel],
    timeout: Duration,
) -> Result<()> {
    if channels.is_empty() {
        return Ok(());
    }
    let kind = provider.kind();
    let states = tokio::time::timeout(timeout, provider.reconcile_many(channels))
        .await
        .with_context(|| format!("{kind} reconciliation timed out"))??;
    for state in states {
        store.apply(&state).await?;
    }
    Ok(())
}

fn build_providers(arguments: &WorkerArgs) -> Result<Vec<ScheduledProvider>> {
    let credentials = ProviderCredentials::new(
        arguments.twitch_client_id.as_deref(),
        arguments.twitch_client_secret.as_deref(),
        arguments.twitch_eventsub_secret.as_deref(),
        arguments.chzzk_client_id.as_deref(),
        arguments.chzzk_client_secret.as_deref(),
    );
    credentials.log_disabled();
    let twitch = TwitchProvider::new(TwitchConfig::new(
        arguments.twitch_parent.clone(),
        credentials.twitch.into_credentials(),
    )?)?;
    let youtube = YouTubeProvider::new(YouTubeConfig::new(arguments.public_origin.clone())?);
    let chzzk = ChzzkProvider::new(ChzzkConfig::new(credentials.chzzk.into_credentials())?)?;
    let manual_interval = Duration::from_secs(arguments.interval_seconds.clamp(30, 60));

    Ok(vec![
        ScheduledProvider {
            provider: Arc::new(twitch),
            interval: Duration::from_mins(5),
        },
        ScheduledProvider {
            provider: Arc::new(youtube),
            interval: manual_interval,
        },
        ScheduledProvider {
            provider: Arc::new(chzzk),
            interval: manual_interval,
        },
        ScheduledProvider {
            provider: Arc::new(LinkOnlyProvider),
            interval: manual_interval,
        },
    ])
}

fn jitter(base: Duration) -> Duration {
    let base_millis = u64::try_from(base.as_millis()).unwrap_or(u64::MAX);
    let spread = base_millis / 10;
    let offset = fastrand::u64(0..=spread.saturating_mul(2));
    Duration::from_millis(base_millis.saturating_sub(spread).saturating_add(offset))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use rill3_domain::{
        EmbedCapability, EmbedDescriptor, LiveStatus, ProviderKind, VerificationState,
        VerifiedChannel, VerifyChannelInput,
    };
    use rill3_providers::ProviderError;
    use url::Url;
    use uuid::Uuid;

    use super::*;

    fn worker_arguments() -> WorkerArgs {
        WorkerArgs {
            common: crate::config::CommonArgs {
                database_url: "postgres://localhost/rill3_test".to_owned(),
                database_max_connections: 3,
                environment: "development".to_owned(),
            },
            once: true,
            interval_seconds: 60,
            provider_timeout_seconds: 12,
            twitch_client_id: None,
            twitch_client_secret: None,
            twitch_eventsub_secret: None,
            chzzk_client_id: None,
            chzzk_client_secret: None,
            public_origin: Url::parse("http://localhost:3000").unwrap(),
            twitch_parent: "localhost".to_owned(),
        }
    }

    #[test]
    fn incomplete_credentials_do_not_abort_provider_construction() {
        for twitch_mask in 0..8 {
            for chzzk_mask in 0..4 {
                let arguments = WorkerArgs {
                    twitch_client_id: (twitch_mask & 1 != 0).then(|| "twitch-id".to_owned()),
                    twitch_client_secret: (twitch_mask & 2 != 0)
                        .then(|| "twitch-secret".to_owned()),
                    twitch_eventsub_secret: (twitch_mask & 4 != 0)
                        .then(|| "eventsub-secret".to_owned()),
                    chzzk_client_id: (chzzk_mask & 1 != 0).then(|| "chzzk-id".to_owned()),
                    chzzk_client_secret: (chzzk_mask & 2 != 0).then(|| "chzzk-secret".to_owned()),
                    ..worker_arguments()
                };
                arguments.validate().unwrap();
                let providers = build_providers(&arguments).unwrap();
                assert_eq!(providers.len(), 4);
                for scheduled in providers {
                    let expected = match scheduled.provider.kind() {
                        ProviderKind::Twitch if twitch_mask != 7 => ProviderAvailability::Disabled,
                        ProviderKind::Chzzk if chzzk_mask != 3 => ProviderAvailability::Disabled,
                        _ => ProviderAvailability::Enabled,
                    };
                    assert_eq!(scheduled.provider.availability(), expected);
                }
            }
        }
    }

    struct ProviderStore {
        channels: Vec<ExternalChannel>,
        applied: Mutex<Vec<LiveState>>,
        stale: Mutex<Vec<ProviderKind>>,
    }

    #[async_trait::async_trait]
    impl WorkerStore for ProviderStore {
        async fn channels_after(
            &self,
            provider: ProviderKind,
            after: Option<Uuid>,
            _limit: u32,
        ) -> Result<Vec<ExternalChannel>> {
            Ok(self
                .channels
                .iter()
                .filter(|channel| {
                    channel.provider == provider && after.is_none_or(|after| channel.id > after)
                })
                .cloned()
                .collect())
        }

        async fn apply(&self, state: &LiveState) -> Result<()> {
            self.applied.lock().unwrap().push(state.clone());
            Ok(())
        }

        async fn mark_stale(&self, provider: ProviderKind) -> Result<()> {
            self.stale.lock().unwrap().push(provider);
            Ok(())
        }
    }

    fn manual_channel(provider: ProviderKind) -> ExternalChannel {
        let now = Utc::now();
        ExternalChannel {
            id: Uuid::new_v4(),
            creator_id: Uuid::new_v4(),
            provider,
            provider_channel_id: "manual-test".to_owned(),
            handle: None,
            canonical_url: Url::parse("https://example.com/channel").unwrap(),
            current_live_url: Some(
                Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            ),
            manual_live_status: Some(LiveStatus::Online),
            manual_state_expires_at: Some(now + chrono::Duration::hours(1)),
            embed_capability: EmbedCapability::LinkOnly,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn invalid_and_incomplete_credentials_leave_manual_reconciliation_running() {
        for eventsub_secret in [None, Some("short"), Some("invalid\nsecret")] {
            let arguments = WorkerArgs {
                twitch_client_id: Some("twitch-id".to_owned()),
                twitch_client_secret: Some("twitch-secret".to_owned()),
                twitch_eventsub_secret: eventsub_secret.map(str::to_owned),
                chzzk_client_id: Some("chzzk-id".to_owned()),
                chzzk_client_secret: Some("invalid\nsecret".to_owned()),
                ..worker_arguments()
            };
            arguments.validate().unwrap();
            let providers = build_providers(&arguments).unwrap();
            let store = Arc::new(ProviderStore {
                channels: vec![
                    manual_channel(ProviderKind::YouTube),
                    manual_channel(ProviderKind::LinkOnly),
                ],
                applied: Mutex::new(Vec::new()),
                stale: Mutex::new(Vec::new()),
            });

            run_once(store.clone(), &providers, Duration::from_secs(1))
                .await
                .unwrap();

            let states = store.applied.lock().unwrap();
            assert_eq!(states.len(), 2);
            for kind in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
                assert!(
                    states
                        .iter()
                        .any(|state| state.provider == kind && state.status == LiveStatus::Online)
                );
            }
            let stale = store.stale.lock().unwrap();
            assert_eq!(stale.len(), 2);
            assert!(stale.contains(&ProviderKind::Twitch));
            assert!(stale.contains(&ProviderKind::Chzzk));
        }
    }

    #[tokio::test]
    async fn invalid_twitch_credentials_allow_configured_chzzk_to_reconcile() {
        let arguments = WorkerArgs {
            twitch_client_id: Some("twitch-id".to_owned()),
            twitch_client_secret: Some("twitch-secret".to_owned()),
            twitch_eventsub_secret: Some("short".to_owned()),
            chzzk_client_id: Some("chzzk-id".to_owned()),
            chzzk_client_secret: Some("chzzk-secret".to_owned()),
            ..worker_arguments()
        };
        let mut providers = build_providers(&arguments).unwrap();
        let mock_api = axum::Router::new().route(
            "/open/v1/lives",
            axum::routing::get(|headers: http::HeaderMap| async move {
                assert_eq!(headers.get("Client-Id").unwrap(), "chzzk-id");
                assert_eq!(headers.get("Client-Secret").unwrap(), "chzzk-secret");
                axum::Json(serde_json::json!({
                    "code": 200,
                    "message": null,
                    "content": {"data": [], "page": {"next": null}}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, mock_api).await.unwrap() });
        let credentials = ProviderCredentials::new(
            arguments.twitch_client_id.as_deref(),
            arguments.twitch_client_secret.as_deref(),
            arguments.twitch_eventsub_secret.as_deref(),
            arguments.chzzk_client_id.as_deref(),
            arguments.chzzk_client_secret.as_deref(),
        );
        let chzzk_config = ChzzkConfig::new(credentials.chzzk.into_credentials())
            .unwrap()
            .with_api_base_url(Url::parse(&format!("http://{address}/open/v1/")).unwrap());
        providers
            .iter_mut()
            .find(|scheduled| scheduled.provider.kind() == ProviderKind::Chzzk)
            .unwrap()
            .provider = Arc::new(ChzzkProvider::new(chzzk_config).unwrap());
        let store = Arc::new(ProviderStore {
            channels: vec![
                ExternalChannel {
                    provider_channel_id: "0123456789abcdef0123456789abcdef".to_owned(),
                    ..manual_channel(ProviderKind::Chzzk)
                },
                manual_channel(ProviderKind::YouTube),
            ],
            applied: Mutex::new(Vec::new()),
            stale: Mutex::new(Vec::new()),
        });

        let result = run_once(store.clone(), &providers, Duration::from_secs(1)).await;
        server.abort();
        result.unwrap();

        let states = store.applied.lock().unwrap();
        assert_eq!(states.len(), 2);
        assert!(
            states
                .iter()
                .any(|state| state.provider == ProviderKind::Chzzk
                    && state.status == LiveStatus::Offline)
        );
        assert!(
            states
                .iter()
                .any(|state| state.provider == ProviderKind::YouTube
                    && state.status == LiveStatus::Online)
        );
        assert_eq!(*store.stale.lock().unwrap(), vec![ProviderKind::Twitch]);
    }

    #[test]
    fn normal_jitter_stays_within_ten_percent() {
        for _ in 0..100 {
            let delay = jitter(Duration::from_mins(1));
            assert!((Duration::from_secs(54)..=Duration::from_secs(66)).contains(&delay));
        }
    }

    #[test]
    fn retry_delay_never_beats_poll_interval_or_server_hint() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(30), Duration::from_mins(15), 20).unwrap();
        let ordinary = anyhow::Error::msg("temporary failure");
        assert_eq!(
            retry_delay(&ordinary, policy, 0, Duration::from_secs(30), false, 0,),
            Duration::from_secs(30)
        );

        let limited = anyhow::Error::new(ProviderError::RateLimited {
            provider: ProviderKind::Chzzk,
            retry_after: Some(Duration::from_mins(30)),
        });
        assert_eq!(
            retry_delay(&limited, policy, 0, Duration::from_secs(30), false, 0,),
            Duration::from_mins(30)
        );
    }

    #[test]
    fn open_circuit_uses_cooldown() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(30), Duration::from_mins(15), 20).unwrap();
        let error = anyhow::Error::msg("repeated failure");
        assert_eq!(
            retry_delay(&error, policy, 0, Duration::from_secs(30), true, 0,),
            CIRCUIT_BREAKER_COOLDOWN
        );
    }

    #[test]
    fn chzzk_scan_gets_a_bounded_total_time_budget() {
        assert_eq!(
            reconciliation_timeout(ProviderKind::Chzzk, Duration::from_secs(12)),
            Duration::from_mins(1)
        );
        assert_eq!(
            reconciliation_timeout(ProviderKind::Twitch, Duration::from_secs(12)),
            Duration::from_secs(12)
        );
    }

    struct FakeStore {
        channel: ExternalChannel,
        stale_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl WorkerStore for FakeStore {
        async fn channels_after(
            &self,
            _provider: ProviderKind,
            after: Option<Uuid>,
            _limit: u32,
        ) -> Result<Vec<ExternalChannel>> {
            Ok(after.map_or_else(|| vec![self.channel.clone()], |_| Vec::new()))
        }

        async fn apply(&self, _state: &LiveState) -> Result<()> {
            Ok(())
        }

        async fn mark_stale(&self, _provider: ProviderKind) -> Result<()> {
            self.stale_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FlakyProvider {
        calls: AtomicUsize,
        cancellation: CancellationToken,
    }

    #[async_trait::async_trait]
    impl StreamProvider for FlakyProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::LinkOnly
        }

        fn embed_capability(&self) -> EmbedCapability {
            EmbedCapability::LinkOnly
        }

        async fn verify_channel(
            &self,
            _input: VerifyChannelInput,
        ) -> Result<VerifiedChannel, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn fetch_live_state(
            &self,
            _channel: &ExternalChannel,
        ) -> Result<LiveState, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn reconcile_many(
            &self,
            channels: &[ExternalChannel],
        ) -> Result<Vec<LiveState>, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Err(ProviderError::RateLimited {
                    provider: ProviderKind::LinkOnly,
                    retry_after: Some(Duration::from_millis(1)),
                });
            }
            self.cancellation.cancel();
            Ok(vec![LiveState::new(
                &channels[0],
                LiveStatus::Unknown,
                Utc::now(),
            )])
        }

        fn external_url(&self, channel: &ExternalChannel) -> Url {
            channel.canonical_url.clone()
        }

        fn embed_descriptor(&self, _live: &LiveState) -> Option<EmbedDescriptor> {
            None
        }
    }

    #[tokio::test]
    async fn rate_limit_is_retried_without_killing_provider_loop() {
        let now = Utc::now();
        let channel = ExternalChannel {
            id: Uuid::new_v4(),
            creator_id: Uuid::new_v4(),
            provider: ProviderKind::LinkOnly,
            provider_channel_id: "fixture".to_owned(),
            handle: None,
            canonical_url: Url::parse("https://example.com/live").unwrap(),
            current_live_url: None,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: EmbedCapability::LinkOnly,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let store = Arc::new(FakeStore {
            channel,
            stale_calls: AtomicUsize::new(0),
        });
        let cancellation = CancellationToken::new();
        let provider = Arc::new(FlakyProvider {
            calls: AtomicUsize::new(0),
            cancellation: cancellation.clone(),
        });
        let scheduled = ScheduledProvider {
            provider: provider.clone(),
            interval: Duration::from_millis(1),
        };
        let backoff =
            BackoffPolicy::new(Duration::from_millis(1), Duration::from_millis(5), 0).unwrap();

        tokio::time::timeout(
            Duration::from_millis(100),
            provider_loop(
                store.clone(),
                scheduled,
                Duration::from_millis(20),
                backoff,
                cancellation,
            ),
        )
        .await
        .unwrap();

        assert!(provider.calls.load(Ordering::SeqCst) >= 2);
        assert_eq!(store.stale_calls.load(Ordering::SeqCst), 1);
    }

    struct BlockingProvider {
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl StreamProvider for BlockingProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::LinkOnly
        }

        fn embed_capability(&self) -> EmbedCapability {
            EmbedCapability::LinkOnly
        }

        async fn verify_channel(
            &self,
            _input: VerifyChannelInput,
        ) -> Result<VerifiedChannel, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn fetch_live_state(
            &self,
            _channel: &ExternalChannel,
        ) -> Result<LiveState, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn reconcile_many(
            &self,
            _channels: &[ExternalChannel],
        ) -> Result<Vec<LiveState>, ProviderError> {
            self.started.notify_one();
            std::future::pending().await
        }

        fn external_url(&self, channel: &ExternalChannel) -> Url {
            channel.canonical_url.clone()
        }

        fn embed_descriptor(&self, _live: &LiveState) -> Option<EmbedDescriptor> {
            None
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_in_flight_reconciliation() {
        let now = Utc::now();
        let store = Arc::new(FakeStore {
            channel: ExternalChannel {
                id: Uuid::new_v4(),
                creator_id: Uuid::new_v4(),
                provider: ProviderKind::LinkOnly,
                provider_channel_id: "blocking-fixture".to_owned(),
                handle: None,
                canonical_url: Url::parse("https://example.com/live").unwrap(),
                current_live_url: None,
                manual_live_status: None,
                manual_state_expires_at: None,
                embed_capability: EmbedCapability::LinkOnly,
                verification_state: VerificationState::Verified,
                enabled: true,
                created_at: now,
                updated_at: now,
            },
            stale_calls: AtomicUsize::new(0),
        });
        let started = Arc::new(tokio::sync::Notify::new());
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(provider_loop(
            store,
            ScheduledProvider {
                provider: Arc::new(BlockingProvider {
                    started: started.clone(),
                }),
                interval: Duration::from_secs(30),
            },
            Duration::from_mins(1),
            BackoffPolicy::new(Duration::from_secs(30), Duration::from_mins(15), 20).unwrap(),
            cancellation.clone(),
        ));

        tokio::time::timeout(Duration::from_millis(100), started.notified())
            .await
            .expect("reconciliation starts");
        cancellation.cancel();
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("worker stops without waiting for provider timeout")
            .expect("worker task does not panic");
    }

    #[tokio::test]
    async fn one_shot_returns_failure_after_marking_provider_stale() {
        let now = Utc::now();
        let channel = ExternalChannel {
            id: Uuid::new_v4(),
            creator_id: Uuid::new_v4(),
            provider: ProviderKind::LinkOnly,
            provider_channel_id: "one-shot-fixture".to_owned(),
            handle: None,
            canonical_url: Url::parse("https://example.com/live").unwrap(),
            current_live_url: None,
            manual_live_status: None,
            manual_state_expires_at: None,
            embed_capability: EmbedCapability::LinkOnly,
            verification_state: VerificationState::Verified,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let store = Arc::new(FakeStore {
            channel,
            stale_calls: AtomicUsize::new(0),
        });
        let provider = Arc::new(FlakyProvider {
            calls: AtomicUsize::new(0),
            cancellation: CancellationToken::new(),
        });
        let providers = vec![ScheduledProvider {
            provider,
            interval: Duration::from_secs(30),
        }];

        let error = run_once(store.clone(), &providers, Duration::from_secs(1))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("reconcile link_only"));
        assert_eq!(store.stale_calls.load(Ordering::SeqCst), 1);
    }

    struct PagedStore {
        channels: Vec<ExternalChannel>,
        applied: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl WorkerStore for PagedStore {
        async fn channels_after(
            &self,
            _provider: ProviderKind,
            after: Option<Uuid>,
            limit: u32,
        ) -> Result<Vec<ExternalChannel>> {
            Ok(self
                .channels
                .iter()
                .filter(|channel| after.is_none_or(|cursor| channel.id > cursor))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .cloned()
                .collect())
        }

        async fn apply(&self, _state: &LiveState) -> Result<()> {
            self.applied.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn mark_stale(&self, _provider: ProviderKind) -> Result<()> {
            Ok(())
        }
    }

    struct CountingChzzkProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl StreamProvider for CountingChzzkProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Chzzk
        }

        fn embed_capability(&self) -> EmbedCapability {
            EmbedCapability::LinkOnly
        }

        async fn verify_channel(
            &self,
            _input: VerifyChannelInput,
        ) -> Result<VerifiedChannel, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn fetch_live_state(
            &self,
            _channel: &ExternalChannel,
        ) -> Result<LiveState, ProviderError> {
            unreachable!("not used by the worker test")
        }

        async fn reconcile_many(
            &self,
            channels: &[ExternalChannel],
        ) -> Result<Vec<LiveState>, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(channels
                .iter()
                .map(|channel| LiveState::new(channel, LiveStatus::Unknown, Utc::now()))
                .collect())
        }

        fn external_url(&self, channel: &ExternalChannel) -> Url {
            channel.canonical_url.clone()
        }

        fn embed_descriptor(&self, _live: &LiveState) -> Option<EmbedDescriptor> {
            None
        }
    }

    #[tokio::test]
    async fn chzzk_scans_once_across_database_pages() {
        let now = Utc::now();
        let channels = (1_u128..=101)
            .map(|id| ExternalChannel {
                id: Uuid::from_u128(id),
                creator_id: Uuid::from_u128(id + 1_000),
                provider: ProviderKind::Chzzk,
                provider_channel_id: format!("{id:032x}"),
                handle: None,
                canonical_url: Url::parse(&format!("https://chzzk.naver.com/{id:032x}")).unwrap(),
                current_live_url: None,
                manual_live_status: None,
                manual_state_expires_at: None,
                embed_capability: EmbedCapability::LinkOnly,
                verification_state: VerificationState::Verified,
                enabled: true,
                created_at: now,
                updated_at: now,
            })
            .collect();
        let store = PagedStore {
            channels,
            applied: AtomicUsize::new(0),
        };
        let provider = CountingChzzkProvider {
            calls: AtomicUsize::new(0),
        };

        reconcile(&store, &provider, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.applied.load(Ordering::SeqCst), 101);
    }
}
