use std::time::Duration;

use chrono::{TimeDelta, Utc};
use rill3_db::{Database, DatabaseOptions, PublicRegistration, WalletChallenge};
use rill3_domain::{ProviderKind, VerificationState};
use uuid::Uuid;

async fn database() -> Option<Database> {
    let Ok(url) = std::env::var("RILL3_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL ownership tests: RILL3_TEST_DATABASE_URL is unset");
        return None;
    };
    let db = Database::connect(
        &DatabaseOptions::new(url, 4).with_acquire_timeout(Duration::from_secs(10)),
    )
    .await
    .expect("PostgreSQL test database must be reachable");
    db.migrate().await.expect("migrations must apply");
    Some(db)
}

async fn registration(db: &Database) -> PublicRegistration {
    db.register_public_channel(
        ProviderKind::Chzzk,
        &Uuid::new_v4().simple().to_string(),
        "방송 등록",
    )
    .await
    .unwrap()
}

async fn session(db: &Database, channel: &PublicRegistration) -> Vec<u8> {
    let hash = hash();
    db.create_owner_session(
        &hash,
        channel.channel_id,
        channel.provider,
        &channel.provider_channel_id,
        Utc::now() + TimeDelta::minutes(15),
    )
    .await
    .unwrap();
    hash
}

fn hash() -> Vec<u8> {
    [
        Uuid::new_v4().as_bytes().as_slice(),
        Uuid::new_v4().as_bytes().as_slice(),
    ]
    .concat()
}

fn challenge(channel: &PublicRegistration, session: &[u8], wallet_digit: char) -> WalletChallenge {
    WalletChallenge {
        nonce_hash: hash(),
        session_hash: session.to_vec(),
        channel_id: channel.channel_id,
        wallet_address: format!("0x{}", wallet_digit.to_string().repeat(40)),
        message: "A specific wallet, channel, chain, origin and nonce".to_owned(),
        expires_at: Utc::now() + TimeDelta::minutes(5),
    }
}

#[tokio::test]
async fn registration_is_concurrent_idempotent_and_cannot_overwrite() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4().simple().to_string();
    let (first, second) = tokio::join!(
        db.register_public_channel(ProviderKind::Chzzk, &id, "Original"),
        db.register_public_channel(ProviderKind::Chzzk, &id, "Untrusted rename"),
    );
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert_eq!(first.verification_state, VerificationState::Pending);
    let returned = db
        .register_public_channel(ProviderKind::Chzzk, &id, "Hijack")
        .await
        .unwrap();
    assert_eq!(returned, first);
    sqlx::query("UPDATE external_channels SET enabled = false WHERE id = $1")
        .bind(first.channel_id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.register_public_channel(ProviderKind::Chzzk, &id, "Restore")
            .await
            .is_err()
    );
    assert!(db.web3_channel(first.channel_id).await.unwrap().is_none());
}

#[tokio::test]
async fn registration_rejects_bad_ids_and_preserves_youtube_case() {
    let Some(db) = database().await else {
        return;
    };
    for (provider, id) in [
        (ProviderKind::Twitch, "username"),
        (ProviderKind::Twitch, "0123"),
        (ProviderKind::YouTube, "@channel"),
        (ProviderKind::Chzzk, "A123"),
        (ProviderKind::LinkOnly, "example.com"),
    ] {
        assert!(
            db.register_public_channel(provider, id, "Example")
                .await
                .is_err()
        );
    }
    let suffix = &Uuid::new_v4().simple().to_string()[..21];
    let upper = format!("UCA{suffix}");
    let lower = format!("UCa{suffix}");
    let a = db
        .register_public_channel(ProviderKind::YouTube, &upper, "A")
        .await
        .unwrap();
    let b = db
        .register_public_channel(ProviderKind::YouTube, &lower, "a")
        .await
        .unwrap();
    assert_ne!(a.creator_slug, b.creator_slug);
    assert_ne!(a.channel_id, b.channel_id);
}

#[tokio::test]
async fn oauth_state_requires_browser_is_single_use_and_expires() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let state = hash();
    let browser = hash();
    db.save_oauth_state(
        &state,
        &browser,
        channel.channel_id,
        Utc::now() + TimeDelta::minutes(5),
    )
    .await
    .unwrap();
    assert!(
        db.consume_oauth_state(&state, &hash())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.consume_oauth_state(&state, &browser)
            .await
            .unwrap()
            .unwrap()
            .channel_id,
        channel.channel_id
    );
    assert!(
        db.consume_oauth_state(&state, &browser)
            .await
            .unwrap()
            .is_none()
    );
    db.save_oauth_state(
        &state,
        &browser,
        channel.channel_id,
        Utc::now() + TimeDelta::minutes(5),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE creator_oauth_states SET expires_at = now() - interval '1 second' WHERE state_hash = $1")
        .bind(&state).execute(db.pool()).await.unwrap();
    assert!(
        db.consume_oauth_state(&state, &browser)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn owner_sessions_require_exact_identity_and_ignore_legacy_verification() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let unrelated = registration(&db).await;
    let token = hash();
    sqlx::query("UPDATE external_channels SET verification_state = 'verified' WHERE id = $1")
        .bind(channel.channel_id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(!db.owner_session(&token, channel.channel_id).await.unwrap());
    for (provider, identity) in [
        (ProviderKind::Twitch, channel.provider_channel_id.as_str()),
        (channel.provider, unrelated.provider_channel_id.as_str()),
    ] {
        assert!(
            db.create_owner_session(
                &token,
                channel.channel_id,
                provider,
                identity,
                Utc::now() + TimeDelta::minutes(15)
            )
            .await
            .is_err()
        );
    }
    let token = session(&db, &channel).await;
    assert!(db.owner_session(&token, channel.channel_id).await.unwrap());
    assert!(
        !db.owner_session(&token, unrelated.channel_id)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE creator_owner_sessions SET expires_at = now() - interval '1 second' WHERE session_hash = $1")
        .bind(&token).execute(db.pool()).await.unwrap();
    assert!(!db.owner_session(&token, channel.channel_id).await.unwrap());
}

#[tokio::test]
async fn challenge_cannot_cross_sessions_channels_or_wallets_and_replay_fails() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let unrelated = registration(&db).await;
    let token = session(&db, &channel).await;
    let wrong_token = session(&db, &unrelated).await;
    let challenge = challenge(&channel, &token, '1');
    db.save_wallet_challenge(&challenge).await.unwrap();
    assert!(
        db.wallet_challenge(&challenge.nonce_hash, &wrong_token, channel.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.wallet_challenge(&challenge.nonce_hash, &token, unrelated.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.reserve_verified_wallet(
            &challenge.nonce_hash,
            &token,
            channel.channel_id,
            &format!("0x{}", "2".repeat(40))
        )
        .await
        .unwrap()
    );
    let (first, replay) = tokio::join!(
        db.reserve_verified_wallet(
            &challenge.nonce_hash,
            &token,
            channel.channel_id,
            &challenge.wallet_address
        ),
        db.reserve_verified_wallet(
            &challenge.nonce_hash,
            &token,
            channel.channel_id,
            &challenge.wallet_address
        ),
    );
    assert_ne!(first.unwrap(), replay.unwrap());
    assert_eq!(
        db.web3_channel(channel.channel_id)
            .await
            .unwrap()
            .unwrap()
            .wallet_address
            .as_deref(),
        Some(challenge.wallet_address.as_str())
    );
}

#[tokio::test]
async fn beneficiary_is_locked_but_same_wallet_retry_is_allowed() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let token = session(&db, &channel).await;
    for (digit, accepted) in [('1', true), ('2', false), ('1', true)] {
        let challenge = challenge(&channel, &token, digit);
        db.save_wallet_challenge(&challenge).await.unwrap();
        assert_eq!(
            db.reserve_verified_wallet(
                &challenge.nonce_hash,
                &token,
                channel.channel_id,
                &challenge.wallet_address
            )
            .await
            .unwrap(),
            accepted
        );
        assert!(
            db.wallet_challenge(&challenge.nonce_hash, &token, channel.channel_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM creator_ownership_audit WHERE channel_id = $1 AND action = 'wallet_reserved'")
        .bind(channel.channel_id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(events, 2);
}

#[tokio::test]
async fn logout_and_expiry_revoke_outstanding_wallet_challenges() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let token = session(&db, &channel).await;
    let expired = challenge(&channel, &token, '1');
    db.save_wallet_challenge(&expired).await.unwrap();
    sqlx::query("UPDATE creator_wallet_challenges SET expires_at = now() - interval '1 second' WHERE nonce_hash = $1")
        .bind(&expired.nonce_hash).execute(db.pool()).await.unwrap();
    assert!(
        db.wallet_challenge(&expired.nonce_hash, &token, channel.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.reserve_verified_wallet(
            &expired.nonce_hash,
            &token,
            channel.channel_id,
            &expired.wallet_address
        )
        .await
        .unwrap()
    );
    let active = challenge(&channel, &token, '1');
    db.save_wallet_challenge(&active).await.unwrap();
    db.logout(&token).await.unwrap();
    assert!(!db.owner_session(&token, channel.channel_id).await.unwrap());
    assert!(
        db.wallet_challenge(&active.nonce_hash, &token, channel.channel_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.reserve_verified_wallet(
            &active.nonce_hash,
            &token,
            channel.channel_id,
            &active.wallet_address
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn deployment_binding_is_immutable_even_after_configuration_changes() {
    let Some(db) = database().await else {
        return;
    };
    let channel = registration(&db).await;
    let factory = format!("0x{}", "a".repeat(40));
    let receive = format!("0x{}", "b".repeat(40));
    assert!(
        db.bind_channel_vault(channel.channel_id, 84532, &factory, &receive)
            .await
            .unwrap()
    );
    assert!(
        db.bind_channel_vault(channel.channel_id, 84532, &factory.to_uppercase(), &receive)
            .await
            .unwrap()
    );
    assert!(
        !db.bind_channel_vault(channel.channel_id, 8453, &factory, &receive)
            .await
            .unwrap()
    );
    assert!(
        !db.bind_channel_vault(channel.channel_id, 84532, &receive, &factory)
            .await
            .unwrap()
    );
}
