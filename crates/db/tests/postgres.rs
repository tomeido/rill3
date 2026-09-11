use std::time::Duration;

use chrono::{TimeDelta, Utc};
use rill3_db::{Database, DatabaseOptions, DbError};
use rill3_domain::{
    Creator, CreatorStatus, EmbedCapability, ExternalChannel, LiveState, LiveStatus, ProviderEvent,
    ProviderEventStatus, ProviderKind, StateApplyOutcome, VerificationState,
};
use url::Url;
use uuid::Uuid;

async fn test_database() -> Option<Database> {
    let Ok(database_url) = std::env::var("RILL3_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration assertion: RILL3_TEST_DATABASE_URL is unset");
        return None;
    };
    let options =
        DatabaseOptions::new(database_url, 4).with_acquire_timeout(Duration::from_secs(10));
    let database = Database::connect(&options)
        .await
        .expect("test PostgreSQL must be reachable");
    database
        .migrate()
        .await
        .expect("test migrations must apply");
    Some(database)
}

fn creator_and_channel() -> (Creator, ExternalChannel) {
    let now = Utc::now();
    let creator_id = Uuid::new_v4();
    let channel_id = Uuid::new_v4();
    let suffix = channel_id.simple().to_string();
    let creator = Creator {
        id: creator_id,
        slug: format!("creator-{suffix}"),
        display_name: "RILL3 database test".to_owned(),
        bio: None,
        locale: "ko-KR".to_owned(),
        status: CreatorStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let channel = ExternalChannel {
        id: channel_id,
        creator_id,
        provider: ProviderKind::Twitch,
        provider_channel_id: format!("channel-{suffix}"),
        handle: Some(format!("handle_{suffix}")),
        canonical_url: Url::parse(&format!("https://www.twitch.tv/handle_{suffix}"))
            .expect("fixture URL is valid"),
        current_live_url: None,
        manual_live_status: None,
        manual_state_expires_at: None,
        embed_capability: EmbedCapability::OfficialEmbed,
        verification_state: VerificationState::Verified,
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    (creator, channel)
}

async fn insert_fixture(database: &Database) -> (Creator, ExternalChannel) {
    let (creator, channel) = creator_and_channel();
    let creator = database
        .upsert_creator(&creator)
        .await
        .expect("creator insert succeeds");
    let channel = database
        .upsert_channel(&channel)
        .await
        .expect("channel insert succeeds");
    (creator, channel)
}

fn live_state(channel: &ExternalChannel, status: LiveStatus, seconds_ago: i64) -> LiveState {
    let observed_at = Utc::now() - TimeDelta::seconds(seconds_ago);
    let mut state = LiveState::new(channel, status, observed_at);
    state.provider_session_id = (status == LiveStatus::Online).then(|| "session-1".to_owned());
    state.title = Some("A normalized stream".to_owned());
    state.last_success_at = Some(observed_at);
    state
}

fn provider_event(
    channel: &ExternalChannel,
    external_event_id: impl Into<String>,
    occurred_at: chrono::DateTime<Utc>,
) -> ProviderEvent {
    ProviderEvent {
        provider: channel.provider,
        external_event_id: external_event_id.into(),
        channel_id: Some(channel.id),
        event_type: "stream.state".to_owned(),
        occurred_at,
        received_at: Utc::now(),
        payload_hash: "ab".repeat(32),
        handled_at: None,
        status: ProviderEventStatus::Received,
    }
}

#[tokio::test]
async fn duplicate_event_and_late_offline_do_not_replace_newer_online() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, channel) = insert_fixture(&database).await;
    let online = live_state(&channel, LiveStatus::Online, 30);
    let online_event = provider_event(&channel, Uuid::new_v4().to_string(), online.observed_at);

    assert_eq!(
        database
            .apply_provider_event(&online_event, &online)
            .await
            .expect("first event applies"),
        StateApplyOutcome::AppliedChanged
    );
    assert_eq!(
        database
            .apply_provider_event(&online_event, &online)
            .await
            .expect("identical retry is accepted"),
        StateApplyOutcome::Duplicate
    );

    let offline = live_state(&channel, LiveStatus::Offline, 60);
    let offline_event = provider_event(&channel, Uuid::new_v4().to_string(), offline.observed_at);
    assert_eq!(
        database
            .apply_provider_event(&offline_event, &offline)
            .await
            .expect("late event is recorded but ignored"),
        StateApplyOutcome::IgnoredOutOfOrder
    );

    let listing = database
        .creator_by_slug(&creator.slug)
        .await
        .expect("creator query succeeds")
        .expect("creator exists");
    let session = listing.session.expect("live state exists");
    assert_eq!(session.state, LiveStatus::Online);
    assert_eq!(
        session.observed_at.timestamp_micros(),
        online.observed_at.timestamp_micros()
    );

    let duplicate_count = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!"
        FROM provider_events
        WHERE provider = $1 AND external_event_id = $2
        "#,
        channel.provider.as_str(),
        &online_event.external_event_id
    )
    .fetch_one(database.pool())
    .await
    .expect("event count succeeds");
    assert_eq!(duplicate_count, 1);
}

#[tokio::test]
async fn stale_marker_preserves_state_and_success_clears_it() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, mut channel) = creator_and_channel();
    channel.provider = ProviderKind::LinkOnly;
    channel.embed_capability = EmbedCapability::LinkOnly;
    channel.canonical_url = Url::parse("https://example.com/live").unwrap();
    channel.current_live_url = Some(channel.canonical_url.clone());
    channel.manual_live_status = Some(LiveStatus::Online);
    channel.manual_state_expires_at = Some(Utc::now() + TimeDelta::hours(1));
    channel.created_at = Utc::now() - TimeDelta::minutes(2);
    channel.updated_at = channel.created_at;
    database.upsert_creator(&creator).await.unwrap();
    let channel = database
        .upsert_channel(&channel)
        .await
        .expect("manual channel insert succeeds");
    let online = live_state(&channel, LiveStatus::Online, 60);
    database
        .apply_reconciled_state(&online)
        .await
        .expect("initial state applies");

    let stale_at = Utc::now() - TimeDelta::seconds(30);
    assert_eq!(
        database
            .mark_provider_stale(channel.provider, stale_at)
            .await
            .expect("stale mark succeeds"),
        1
    );
    assert_eq!(
        database
            .mark_provider_stale(channel.provider, stale_at + TimeDelta::seconds(1))
            .await
            .expect("repeat stale mark succeeds"),
        0
    );
    let stale_listing = database
        .list_live(100)
        .await
        .expect("live listing succeeds")
        .into_iter()
        .find(|listing| listing.channel.id == channel.id)
        .expect("stale live state remains visible");
    assert_eq!(stale_listing.session.state, LiveStatus::Online);
    assert_eq!(
        stale_listing
            .session
            .stale_since
            .map(|value| value.timestamp_micros()),
        Some(stale_at.timestamp_micros())
    );

    let refreshed = live_state(&channel, LiveStatus::Online, 0);
    assert_eq!(
        database
            .apply_reconciled_state(&refreshed)
            .await
            .expect("fresh observation applies"),
        StateApplyOutcome::AppliedChanged
    );
    let refreshed_listing = database
        .creator_by_slug(&stale_listing.creator.slug)
        .await
        .expect("creator query succeeds")
        .expect("creator exists");
    assert_eq!(
        refreshed_listing
            .session
            .expect("session exists")
            .stale_since,
        None
    );
}

#[tokio::test]
async fn lookup_signal_and_advisory_lock_are_idempotent() {
    let Some(database) = test_database().await else {
        return;
    };
    let (_, channel) = insert_fixture(&database).await;

    let found = database
        .channel_by_provider_id(channel.provider, &channel.provider_channel_id)
        .await
        .expect("channel lookup succeeds")
        .expect("channel exists");
    assert_eq!(found.id, channel.id);

    let signal = provider_event(&channel, Uuid::new_v4().to_string(), Utc::now());
    assert!(
        database
            .record_provider_signal(&signal)
            .await
            .expect("first signal is recorded")
    );
    assert!(
        !database
            .record_provider_signal(&signal)
            .await
            .expect("signal retry is idempotent")
    );

    let lock_key = i64::from_le_bytes(channel.id.as_bytes()[..8].try_into().unwrap());
    let mut guard = database
        .try_advisory_lock(lock_key)
        .await
        .expect("first lock query succeeds")
        .expect("first lock is acquired");
    guard
        .ensure_held()
        .await
        .expect("the dedicated lock connection is healthy");
    assert!(
        database
            .try_advisory_lock(lock_key)
            .await
            .expect("contended lock query succeeds")
            .is_none()
    );
    guard.release().await.expect("lock releases");
    assert!(
        database
            .try_advisory_lock(lock_key)
            .await
            .expect("lock can be reacquired")
            .is_some()
    );
}

#[tokio::test]
async fn invalid_hash_is_rejected_before_database_mutation() {
    let Some(database) = test_database().await else {
        return;
    };
    let (_, channel) = insert_fixture(&database).await;
    let mut signal = provider_event(&channel, Uuid::new_v4().to_string(), Utc::now());
    signal.payload_hash = "NOT-A-SHA256".to_owned();

    assert!(matches!(
        database.record_provider_signal(&signal).await,
        Err(DbError::InvalidPayloadHash)
    ));
}

#[tokio::test]
async fn migration_enforces_keys_without_native_enums_or_raw_payload() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, channel) = insert_fixture(&database).await;

    let mut duplicate_slug = creator.clone();
    duplicate_slug.id = Uuid::new_v4();
    assert!(database.upsert_creator(&duplicate_slug).await.is_err());

    let mut duplicate_provider_channel = channel;
    duplicate_provider_channel.id = Uuid::new_v4();
    assert!(
        database
            .upsert_channel(&duplicate_provider_channel)
            .await
            .is_err()
    );

    let native_enum_count = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!"
        FROM pg_type AS t
        JOIN pg_namespace AS n ON n.oid = t.typnamespace
        WHERE n.nspname = current_schema() AND t.typtype = 'e'
        "#,
    )
    .fetch_one(database.pool())
    .await
    .expect("enum catalog query succeeds");
    assert_eq!(native_enum_count, 0);

    let raw_payload_column_count = sqlx::query_scalar!(
        r#"
        SELECT count(*) AS "count!"
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'provider_events'
          AND column_name IN ('payload', 'raw_payload')
        "#,
    )
    .fetch_one(database.pool())
    .await
    .expect("column catalog query succeeds");
    assert_eq!(raw_payload_column_count, 0);
}

#[tokio::test]
async fn registration_preserves_identity_profile_and_unchanged_live_state() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, channel) = creator_and_channel();
    let (stored_creator, stored_channel) = database
        .register_channel(&creator, &channel)
        .await
        .expect("registration succeeds");
    database
        .apply_reconciled_state(&live_state(&stored_channel, LiveStatus::Online, 1))
        .await
        .unwrap();

    let mut retry_creator = creator.clone();
    retry_creator.id = Uuid::new_v4();
    retry_creator.display_name = "must not replace existing creator profile".to_owned();
    retry_creator.bio = Some("not a profile update".to_owned());
    let mut retry_channel = channel.clone();
    retry_channel.id = Uuid::new_v4();
    retry_channel.creator_id = retry_creator.id;
    let (again_creator, again_channel) = database
        .register_channel(&retry_creator, &retry_channel)
        .await
        .expect("same slug and provider identity can be retried");

    assert_eq!(again_creator, stored_creator);
    assert_eq!(again_channel.id, stored_channel.id);
    assert_eq!(again_channel.creator_id, stored_creator.id);
    assert_eq!(again_channel.created_at, stored_channel.created_at);
    let page = database
        .creator_by_slug(&creator.slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.session.unwrap().state, LiveStatus::Online);
}

#[tokio::test]
async fn registration_rejects_ownership_conflicts_without_partial_creator_writes() {
    let Some(database) = test_database().await else {
        return;
    };
    let (owner, channel) = insert_fixture(&database).await;
    let (other_creator, _) = creator_and_channel();
    let mut claimed_channel = channel.clone();
    claimed_channel.id = Uuid::new_v4();
    claimed_channel.creator_id = other_creator.id;
    assert!(matches!(
        database
            .register_channel(&other_creator, &claimed_channel)
            .await,
        Err(DbError::RegistrationConflict(_))
    ));
    let orphan_count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM creators WHERE slug = $1"#,
        &other_creator.slug
    )
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(orphan_count, 0, "conflicts must roll back new creators");
    let untouched = database
        .channel_by_provider_id(channel.provider, &channel.provider_channel_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched, channel);
    assert_eq!(untouched.creator_id, owner.id);

    let (invalid_creator, mut invalid_channel) = creator_and_channel();
    invalid_channel.canonical_url = Url::parse("http://www.twitch.tv/invalid").unwrap();
    assert!(
        database
            .register_channel(&invalid_creator, &invalid_channel)
            .await
            .is_err()
    );
    let invalid_orphan_count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM creators WHERE slug = $1"#,
        &invalid_creator.slug
    )
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(
        invalid_orphan_count, 0,
        "SQL errors also roll back creators"
    );
}

#[tokio::test]
async fn registration_never_reactivates_disabled_creators_or_channels() {
    let Some(database) = test_database().await else {
        return;
    };
    let (mut creator, mut channel) = insert_fixture(&database).await;
    let registration_creator = creator.clone();
    let registration_channel = channel.clone();
    for (enabled, verification_state) in [
        (false, VerificationState::Verified),
        (true, VerificationState::Disabled),
    ] {
        channel.enabled = enabled;
        channel.verification_state = verification_state;
        database.upsert_channel(&channel).await.unwrap();
        assert!(matches!(
            database
                .register_channel(&registration_creator, &registration_channel)
                .await,
            Err(DbError::RegistrationConflict(_))
        ));
        let persisted = database
            .channel_by_provider_id(channel.provider, &channel.provider_channel_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.enabled, enabled);
        assert_eq!(persisted.verification_state, verification_state);
    }

    database
        .upsert_channel(&registration_channel)
        .await
        .unwrap();
    creator.status = CreatorStatus::Disabled;
    database.upsert_creator(&creator).await.unwrap();
    assert!(matches!(
        database
            .register_channel(&registration_creator, &registration_channel)
            .await,
        Err(DbError::RegistrationConflict(_))
    ));
    assert!(
        database
            .creator_by_slug(&creator.slug)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn concurrent_registration_converges_on_one_creator_and_channel() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, channel) = creator_and_channel();
    let mut other_creator = creator.clone();
    other_creator.id = Uuid::new_v4();
    let mut other_channel = channel.clone();
    other_channel.id = Uuid::new_v4();
    other_channel.creator_id = other_creator.id;
    let (first, second) = tokio::join!(
        database.register_channel(&creator, &channel),
        database.register_channel(&other_creator, &other_channel)
    );
    let (first_creator, first_channel) = first.unwrap();
    let (second_creator, second_channel) = second.unwrap();
    assert_eq!(first_creator.id, second_creator.id);
    assert_eq!(first_channel.id, second_channel.id);
    assert_eq!(first_channel.creator_id, second_channel.creator_id);
}

#[tokio::test]
async fn manual_registration_refresh_invalidates_old_session_atomically() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, mut channel) = creator_and_channel();
    channel.provider = ProviderKind::YouTube;
    channel.current_live_url =
        Some(Url::parse("https://www.youtube.com/watch?v=oldvideo123").unwrap());
    channel.manual_live_status = Some(LiveStatus::Online);
    channel.manual_state_expires_at = Some(Utc::now() + TimeDelta::hours(1));
    let (_, mut channel) = database.register_channel(&creator, &channel).await.unwrap();
    database
        .apply_reconciled_state(&live_state(&channel, LiveStatus::Online, 1))
        .await
        .unwrap();

    channel.current_live_url =
        Some(Url::parse("https://www.youtube.com/watch?v=newvideo123").unwrap());
    channel.manual_live_status = Some(LiveStatus::Offline);
    database.register_channel(&creator, &channel).await.unwrap();
    let page = database
        .creator_by_slug(&creator.slug)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.channel.manual_live_status, Some(LiveStatus::Offline));
    assert!(
        page.session.is_none(),
        "old title and Online state must not survive refresh"
    );
}

#[tokio::test]
async fn obsolete_manual_source_cannot_restore_online_even_with_a_later_observation() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, mut channel) = creator_and_channel();
    channel.provider = ProviderKind::YouTube;
    channel.current_live_url =
        Some(Url::parse("https://www.youtube.com/watch?v=oldvideo123").unwrap());
    channel.manual_live_status = Some(LiveStatus::Online);
    channel.manual_state_expires_at = Some(Utc::now() + TimeDelta::hours(1));
    let (_, old_source) = database.register_channel(&creator, &channel).await.unwrap();
    database
        .apply_reconciled_state(&live_state(&old_source, LiveStatus::Online, 1))
        .await
        .unwrap();

    for status in [LiveStatus::Offline, LiveStatus::Unknown] {
        channel.manual_live_status = Some(status);
        let (_, current_source) = database.register_channel(&creator, &channel).await.unwrap();
        assert_ne!(old_source.updated_at, current_source.updated_at);
        // The old worker completes after the registration and timestamps its
        // observation later. Timestamp ordering alone cannot reject this state.
        let observed_at = current_source.updated_at + TimeDelta::minutes(1);
        let stale_worker = LiveState::new(&old_source, LiveStatus::Online, observed_at);
        assert_eq!(
            database
                .apply_reconciled_state(&stale_worker)
                .await
                .unwrap(),
            StateApplyOutcome::IgnoredOutOfOrder
        );
        let mut missing_revision = stale_worker;
        missing_revision.channel_updated_at = None;
        assert_eq!(
            database
                .apply_reconciled_state(&missing_revision)
                .await
                .unwrap(),
            StateApplyOutcome::IgnoredOutOfOrder
        );
        assert!(
            database
                .creator_by_slug(&creator.slug)
                .await
                .unwrap()
                .unwrap()
                .session
                .is_none(),
            "obsolete worker must not recreate the discarded session"
        );

        let mut current_worker = LiveState::new(&current_source, status, observed_at);
        // PostgreSQL stores microseconds; sub-microsecond precision in a source
        // timestamp must not incorrectly invalidate an otherwise matching revision.
        current_worker.channel_updated_at =
            Some(current_source.updated_at + TimeDelta::nanoseconds(999));
        assert_eq!(
            database
                .apply_reconciled_state(&current_worker)
                .await
                .unwrap(),
            StateApplyOutcome::AppliedChanged
        );
        let selected = database
            .creator_by_slug(&creator.slug)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.session.unwrap().state, status);
    }
}

#[tokio::test]
async fn invalid_manual_online_rows_are_filtered_before_the_live_directory_limit() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, mut channel) = creator_and_channel();
    channel.provider = ProviderKind::YouTube;
    channel.current_live_url =
        Some(Url::parse("https://www.youtube.com/watch?v=livevideo12").unwrap());
    channel.manual_live_status = Some(LiveStatus::Online);
    channel.manual_state_expires_at = Some(Utc::now() + TimeDelta::days(10));
    let (_, valid) = database.register_channel(&creator, &channel).await.unwrap();
    let valid_observed_at = Utc::now() + TimeDelta::days(1);
    database
        .apply_reconciled_state(&LiveState::new(
            &valid,
            LiveStatus::Online,
            valid_observed_at,
        ))
        .await
        .unwrap();

    for invalid_case in [
        "offline",
        "unknown",
        "unbounded",
        "expired",
        "no_url",
        "old_session",
    ] {
        let (_, mut invalid) = creator_and_channel();
        invalid.provider = ProviderKind::YouTube;
        invalid.current_live_url = valid.current_live_url.clone();
        invalid.manual_live_status = Some(LiveStatus::Online);
        invalid.manual_state_expires_at = valid.manual_state_expires_at;
        match invalid_case {
            "offline" => invalid.manual_live_status = Some(LiveStatus::Offline),
            "unknown" => invalid.manual_live_status = Some(LiveStatus::Unknown),
            "unbounded" => {
                invalid.manual_live_status = None;
                invalid.manual_state_expires_at = None;
            }
            "expired" => invalid.manual_state_expires_at = Some(Utc::now() - TimeDelta::minutes(1)),
            "no_url" => invalid.current_live_url = None,
            "old_session" => (),
            _ => unreachable!(),
        }
        let (_, invalid) = database.register_channel(&creator, &invalid).await.unwrap();
        let observed_at = if invalid_case == "old_session" {
            invalid.updated_at - TimeDelta::microseconds(1)
        } else {
            valid_observed_at + TimeDelta::days(1)
        };
        database
            .apply_reconciled_state(&LiveState::new(&invalid, LiveStatus::Online, observed_at))
            .await
            .unwrap();
    }

    let page = database.list_live(1).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].channel.id, valid.id,
        "invalid rows must not consume the result limit"
    );
}

#[tokio::test]
async fn default_creator_channel_prefers_effective_online_state_but_preserves_selection() {
    let Some(database) = test_database().await else {
        return;
    };
    for provider in [ProviderKind::YouTube, ProviderKind::LinkOnly] {
        let (creator, automatic) = insert_fixture(&database).await;
        database
            .apply_reconciled_state(&live_state(&automatic, LiveStatus::Online, 60))
            .await
            .unwrap();
        let (_, mut manual) = creator_and_channel();
        manual.provider = provider;
        manual.creator_id = creator.id;
        manual.current_live_url =
            Some(Url::parse("https://www.youtube.com/watch?v=livevideo12").unwrap());
        manual.manual_live_status = Some(LiveStatus::Online);
        manual.manual_state_expires_at = Some(Utc::now() + TimeDelta::hours(1));
        let (_, valid) = database.register_channel(&creator, &manual).await.unwrap();
        database
            .apply_reconciled_state(&live_state(&valid, LiveStatus::Online, 0))
            .await
            .unwrap();
        assert_eq!(
            database
                .creator_by_slug(&creator.slug)
                .await
                .unwrap()
                .unwrap()
                .channel
                .id,
            valid.id,
            "a current manual online observation remains eligible for default selection"
        );

        for invalid_case in ["expired", "offline", "unknown", "no_url", "old_session"] {
            let mut invalid = valid.clone();
            match invalid_case {
                "expired" => {
                    invalid.manual_state_expires_at = Some(Utc::now() - TimeDelta::seconds(1));
                }
                "offline" => invalid.manual_live_status = Some(LiveStatus::Offline),
                "unknown" => invalid.manual_live_status = Some(LiveStatus::Unknown),
                "no_url" => invalid.current_live_url = None,
                "old_session" => (),
                _ => unreachable!(),
            }
            let (_, invalid) = database.register_channel(&creator, &invalid).await.unwrap();
            let observed_at = if invalid_case == "old_session" {
                invalid.updated_at - TimeDelta::microseconds(1)
            } else {
                Utc::now()
            };
            database
                .apply_reconciled_state(&LiveState::new(&invalid, LiveStatus::Online, observed_at))
                .await
                .unwrap();

            let default = database
                .creator_by_slug(&creator.slug)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                default.channel.id, automatic.id,
                "{provider} {invalid_case} must not outrank a current automatic broadcast"
            );
            let explicit = database
                .creator_by_slug_and_channel(&creator.slug, Some(invalid.id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                explicit.channel.id, invalid.id,
                "an explicit {provider} selection stays selected even for {invalid_case}"
            );
        }
        // Do not leave this provider-wide fixture for the stale-marker test.
        sqlx::query("DELETE FROM creators WHERE id = $1")
            .bind(creator.id)
            .execute(database.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn registered_directory_exposes_offline_and_unobserved_channels_with_keyset_pages() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, channel) = insert_fixture(&database).await;
    let before_channel = Uuid::from_u128(channel.id.as_u128() - 1);
    let initial = database
        .list_registered_channels_after(Some(before_channel), 1, false)
        .await
        .unwrap();
    assert_eq!(initial[0].channel.id, channel.id);
    assert!(initial[0].session.is_none());
    database
        .apply_reconciled_state(&live_state(&channel, LiveStatus::Offline, 1))
        .await
        .unwrap();
    let offline = database
        .list_registered_channels_after(Some(before_channel), 1, false)
        .await
        .unwrap();
    assert_eq!(offline[0].channel.id, channel.id);
    assert_eq!(
        offline[0].session.as_ref().unwrap().state,
        LiveStatus::Offline
    );

    let mut ids = vec![channel.id];
    for _ in 0..2 {
        let (_, mut next_channel) = creator_and_channel();
        next_channel.creator_id = creator.id;
        let (_, registered) = database
            .register_channel(&creator, &next_channel)
            .await
            .unwrap();
        ids.push(registered.id);
    }
    ids.sort_unstable();
    let mut cursor = None;
    let mut observed = Vec::new();
    for _ in 0..3 {
        let page = database
            .connected_channels_after(&creator.slug, cursor, 1)
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        cursor = Some(page[0].channel.id);
        observed.push(page[0].channel.id);
    }
    assert_eq!(observed, ids);
    assert!(
        database
            .connected_channels_after(&creator.slug, cursor, 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        database
            .list_registered_channels_after(None, 0, false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn channel_selection_preserves_platform_and_rejects_foreign_or_disabled_channels() {
    let Some(database) = test_database().await else {
        return;
    };
    let (creator, first) = insert_fixture(&database).await;
    let (_, mut second) = creator_and_channel();
    second.creator_id = creator.id;
    second.provider = ProviderKind::YouTube;
    let (_, second) = database.register_channel(&creator, &second).await.unwrap();
    for channel in [&first, &second] {
        database
            .apply_reconciled_state(&live_state(channel, LiveStatus::Online, 1))
            .await
            .unwrap();
        let selected = database
            .creator_by_slug_and_channel(&creator.slug, Some(channel.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.channel.id, channel.id);
        assert_eq!(selected.channel.provider, channel.provider);
    }
    let (_, foreign_channel) = insert_fixture(&database).await;
    for rejected_id in [foreign_channel.id, Uuid::new_v4()] {
        assert!(
            database
                .creator_by_slug_and_channel(&creator.slug, Some(rejected_id))
                .await
                .unwrap()
                .is_none()
        );
    }

    let mut disabled = second;
    for (enabled, verification_state) in [
        (false, VerificationState::Verified),
        (true, VerificationState::Disabled),
    ] {
        disabled.enabled = enabled;
        disabled.verification_state = verification_state;
        database.upsert_channel(&disabled).await.unwrap();
        assert!(
            database
                .creator_by_slug_and_channel(&creator.slug, Some(disabled.id))
                .await
                .unwrap()
                .is_none()
        );
        let connected = database
            .connected_channels_after(&creator.slug, None, 100)
            .await
            .unwrap();
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].channel.id, first.id);
        let before_disabled = Uuid::from_u128(disabled.id.as_u128() - 1);
        let public = database
            .list_registered_channels_after(Some(before_disabled), 1, false)
            .await
            .unwrap();
        assert_ne!(public.first().map(|row| row.channel.id), Some(disabled.id));
        let operator = database
            .list_registered_channels_after(Some(before_disabled), 1, true)
            .await
            .unwrap();
        assert_eq!(operator[0].channel.id, disabled.id);
        assert_eq!(operator[0].channel.verification_state, verification_state);
        assert!(
            !database
                .list_live(100)
                .await
                .unwrap()
                .iter()
                .any(|row| row.channel.id == disabled.id)
        );
    }
    let mut disabled_creator = creator;
    disabled_creator.status = CreatorStatus::Disabled;
    database.upsert_creator(&disabled_creator).await.unwrap();
    assert!(
        database
            .creator_by_slug_and_channel(&disabled_creator.slug, Some(first.id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        database
            .connected_channels_after(&disabled_creator.slug, None, 100)
            .await
            .unwrap()
            .is_empty()
    );
}
