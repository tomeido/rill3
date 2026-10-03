//! Public channel registrations and provider-proven, single-use wallet authorization.
use chrono::{DateTime, Utc};
use rill3_domain::{ProviderKind, VerificationState};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::{Database, DbError, records::parse_provider};

/// An immutable channel identity with its current public registration details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicRegistration {
    pub channel_id: Uuid,
    pub creator_id: Uuid,
    pub creator_slug: String,
    pub provider: ProviderKind,
    pub provider_channel_id: String,
    pub display_name: String,
    pub verification_state: VerificationState,
    pub wallet_address: Option<String>,
}

/// An exact wallet-signature message bound to a provider-proven browser session.
#[derive(Clone, Debug, FromRow)]
pub struct WalletChallenge {
    pub nonce_hash: Vec<u8>,
    pub session_hash: Vec<u8>,
    pub channel_id: Uuid,
    pub wallet_address: String,
    pub message: String,
    pub expires_at: DateTime<Utc>,
}

/// The immutable deployment that receives donations for a channel.
#[derive(Clone, Debug, Eq, PartialEq, FromRow)]
pub struct Web3Binding {
    pub chain_id: i64,
    pub factory_address: String,
    pub receive_address: String,
}

#[derive(FromRow)]
struct RegistrationRow {
    channel_id: Uuid,
    creator_id: Uuid,
    creator_slug: String,
    provider: String,
    provider_channel_id: String,
    display_name: String,
    verification_state: String,
    wallet_address: Option<String>,
}

impl TryFrom<RegistrationRow> for PublicRegistration {
    type Error = DbError;

    fn try_from(row: RegistrationRow) -> Result<Self, Self::Error> {
        let verification_state = match row.verification_state.as_str() {
            "pending" => VerificationState::Pending,
            "verified" => VerificationState::Verified,
            "disabled" => VerificationState::Disabled,
            _ => {
                return Err(DbError::InvalidPersistedValue {
                    field: "verification_state",
                    value: row.verification_state,
                });
            }
        };
        Ok(Self {
            channel_id: row.channel_id,
            creator_id: row.creator_id,
            creator_slug: row.creator_slug,
            provider: parse_provider(&row.provider)?,
            provider_channel_id: row.provider_channel_id,
            display_name: row.display_name,
            verification_state,
            wallet_address: row.wallet_address,
        })
    }
}

impl Database {
    /// Registers a canonical provider identity without trusting a claim of ownership.
    /// Existing records are returned unchanged, including their original creator.
    ///
    /// # Errors
    /// Rejects malformed identities/names, disabled registrations, occupied slugs, or SQL errors.
    pub async fn register_public_channel(
        &self,
        provider: ProviderKind,
        provider_channel_id: &str,
        display_name: &str,
    ) -> Result<PublicRegistration, DbError> {
        validate_public_identity(provider, provider_channel_id)?;
        let display_name = display_name.trim();
        if display_name.is_empty()
            || display_name.chars().count() > 120
            || display_name.chars().any(char::is_control)
        {
            return Err(DbError::RegistrationConflict("invalid display name"));
        }
        let mut transaction = self.pool.begin().await?;
        // Serialize the complete create-or-return operation on the provider identity.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("public-channel:{provider}:{provider_channel_id}"))
            .execute(&mut *transaction)
            .await?;
        let existing = sqlx::query_as::<_, RegistrationRow>(
            "SELECT ch.id AS channel_id, c.id AS creator_id, c.slug AS creator_slug, ch.provider, ch.provider_channel_id, c.display_name, ch.verification_state, wc.wallet_address FROM external_channels ch JOIN creators c ON c.id = ch.creator_id LEFT JOIN creator_wallet_claims wc ON wc.channel_id = ch.id WHERE ch.provider = $1 AND ch.provider_channel_id = $2"
        )
        .bind(provider.as_str())
        .bind(provider_channel_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(existing) = existing {
            ensure_enabled(&mut transaction, existing.channel_id).await?;
            transaction.commit().await?;
            return existing.try_into();
        }
        let creator_id = Uuid::new_v4();
        let channel_id = Uuid::new_v4();
        // Hex preserves case-sensitive YouTube IDs without case-folding slug collisions.
        let slug_id = if provider == ProviderKind::YouTube {
            hex::encode(provider_channel_id)
        } else {
            provider_channel_id.to_owned()
        };
        let slug = format!("{provider}-{slug_id}");
        let inserted = sqlx::query(
            "INSERT INTO creators (id, slug, display_name)
            VALUES ($1, $2, $3) ON CONFLICT (slug) DO NOTHING",
        )
        .bind(creator_id)
        .bind(&slug)
        .bind(display_name)
        .execute(&mut *transaction)
        .await?;
        if inserted.rows_affected() != 1 {
            return Err(DbError::RegistrationConflict(
                "registration slug is already reserved",
            ));
        }
        sqlx::query("INSERT INTO external_channels
            (id, creator_id, provider, provider_channel_id, canonical_url, verification_state, embed_capability)
            VALUES ($1, $2, $3, $4, $5, 'pending', 'link_only')")
            .bind(channel_id).bind(creator_id).bind(provider.as_str())
            .bind(provider_channel_id).bind(canonical_url(provider, provider_channel_id))
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(PublicRegistration {
            channel_id,
            creator_id,
            creator_slug: slug,
            provider,
            provider_channel_id: provider_channel_id.to_owned(),
            display_name: display_name.to_owned(),
            verification_state: VerificationState::Pending,
            wallet_address: None,
        })
    }

    /// Reads a channel available for public donation and ownership verification.
    ///
    /// # Errors
    /// Returns database and persisted-value errors.
    pub async fn web3_channel(
        &self,
        channel_id: Uuid,
    ) -> Result<Option<PublicRegistration>, DbError> {
        sqlx::query_as::<_, RegistrationRow>(
            "SELECT ch.id AS channel_id, c.id AS creator_id, c.slug AS creator_slug, ch.provider, ch.provider_channel_id, c.display_name, ch.verification_state, wc.wallet_address FROM external_channels ch JOIN creators c ON c.id = ch.creator_id LEFT JOIN creator_wallet_claims wc ON wc.channel_id = ch.id WHERE ch.id = $1 AND ch.enabled AND ch.verification_state <> 'disabled' AND c.status = 'active'"
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?
        .map(TryInto::try_into)
        .transpose()
    }

    /// Binds a channel to one deployment; configuration changes cannot replace it.
    ///
    /// # Errors
    /// Returns errors for a missing/disabled channel or invalid SQL input.
    pub async fn bind_channel_vault(
        &self,
        channel_id: Uuid,
        chain_id: i64,
        factory: &str,
        address: &str,
    ) -> Result<bool, DbError> {
        let mut transaction = self.pool.begin().await?;
        ensure_enabled(&mut transaction, channel_id).await?;
        let factory = factory.to_ascii_lowercase();
        let address = address.to_ascii_lowercase();
        sqlx::query(
            "INSERT INTO creator_web3_bindings
            (channel_id, chain_id, factory_address, receive_address) VALUES ($1, $2, $3, $4)
            ON CONFLICT (channel_id) DO NOTHING",
        )
        .bind(channel_id)
        .bind(chain_id)
        .bind(&factory)
        .bind(&address)
        .execute(&mut *transaction)
        .await?;
        let binding = sqlx::query_as::<_, Web3Binding>(
            "SELECT chain_id, factory_address, receive_address
            FROM creator_web3_bindings WHERE channel_id = $1",
        )
        .bind(channel_id)
        .fetch_one(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(binding.chain_id == chain_id
            && binding.factory_address == factory
            && binding.receive_address == address)
    }

    /// Stores a hashed OAuth state bound to the initiating browser and channel.
    ///
    /// # Errors
    /// Returns errors for disabled channels, invalid expiry or malformed hash input.
    pub async fn save_oauth_state(
        &self,
        state_hash: &[u8],
        browser_hash: &[u8],
        channel_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<(), DbError> {
        require_expiry(expires_at, 600)?;
        let mut transaction = self.pool.begin().await?;
        ensure_enabled(&mut transaction, channel_id).await?;
        sqlx::query("DELETE FROM creator_oauth_states WHERE expires_at <= now()")
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "INSERT INTO creator_oauth_states (state_hash, browser_hash, channel_id, expires_at)
            VALUES ($1, $2, $3, $4)",
        )
        .bind(state_hash)
        .bind(browser_hash)
        .bind(channel_id)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Atomically consumes an unexpired state only for its initiating browser.
    ///
    /// # Errors
    /// Returns database errors. Missing, expired, or replayed states return `None`.
    pub async fn consume_oauth_state(
        &self,
        state_hash: &[u8],
        browser_hash: &[u8],
    ) -> Result<Option<PublicRegistration>, DbError> {
        let channel_id = sqlx::query_scalar::<_, Uuid>("DELETE FROM creator_oauth_states
            WHERE state_hash = $1 AND browser_hash = $2 AND expires_at > now() RETURNING channel_id")
            .bind(state_hash).bind(browser_hash).fetch_optional(&self.pool).await?;
        match channel_id {
            Some(channel_id) => self.web3_channel(channel_id).await,
            None => Ok(None),
        }
    }

    /// Creates authorization only when the provider-proven identity matches the channel.
    /// Legacy verification labels are never accepted as evidence of ownership.
    ///
    /// # Errors
    /// Rejects mismatched identity, disabled channels, invalid expiry, or SQL errors.
    pub async fn create_owner_session(
        &self,
        session_hash: &[u8],
        channel_id: Uuid,
        provider: ProviderKind,
        verified_channel_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), DbError> {
        require_expiry(expires_at, 3600)?;
        let mut transaction = self.pool.begin().await?;
        ensure_enabled(&mut transaction, channel_id).await?;
        let matched = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM external_channels
            WHERE id = $1 AND provider = $2 AND provider_channel_id = $3)",
        )
        .bind(channel_id)
        .bind(provider.as_str())
        .bind(verified_channel_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !matched || provider == ProviderKind::LinkOnly {
            return Err(DbError::ChannelIdentityMismatch(channel_id));
        }
        sqlx::query("DELETE FROM creator_owner_sessions WHERE expires_at <= now()")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO creator_owner_sessions
            (session_hash, channel_id, provider, provider_channel_id, expires_at) VALUES ($1, $2, $3, $4, $5)")
            .bind(session_hash).bind(channel_id).bind(provider.as_str())
            .bind(verified_channel_id).bind(expires_at).execute(&mut *transaction).await?;
        audit(&mut transaction, channel_id, "provider_verified", None).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Checks current, exact-channel authorization and the live channel identity.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn owner_session(
        &self,
        session_hash: &[u8],
        channel_id: Uuid,
    ) -> Result<bool, DbError> {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM creator_owner_sessions s
            JOIN external_channels ch ON ch.id = s.channel_id JOIN creators c ON c.id = ch.creator_id
            WHERE s.session_hash = $1 AND s.channel_id = $2 AND s.expires_at > now()
            AND s.provider = ch.provider AND s.provider_channel_id = ch.provider_channel_id AND ch.enabled AND ch.verification_state <> 'disabled' AND c.status = 'active')")
            .bind(session_hash).bind(channel_id).fetch_one(&self.pool).await.map_err(Into::into)
    }

    /// Invalidates a session and all of its outstanding wallet challenges.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn logout(&self, session_hash: &[u8]) -> Result<(), DbError> {
        sqlx::query("DELETE FROM creator_owner_sessions WHERE session_hash = $1")
            .bind(session_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Persists an exact signature challenge, bounded by the owner-session expiry.
    ///
    /// # Errors
    /// Rejects missing authorization, invalid expiry or malformed challenge data.
    pub async fn save_wallet_challenge(&self, challenge: &WalletChallenge) -> Result<(), DbError> {
        require_expiry(challenge.expires_at, 600)?;
        let mut transaction = self.pool.begin().await?;
        ensure_enabled(&mut transaction, challenge.channel_id).await?;
        let inserted = sqlx::query(
            "INSERT INTO creator_wallet_challenges
            (nonce_hash, session_hash, channel_id, wallet_address, message, expires_at)
            SELECT $1, s.session_hash, s.channel_id, $4, $5, LEAST($6, s.expires_at)
            FROM creator_owner_sessions s JOIN external_channels ch ON ch.id = s.channel_id
            WHERE s.session_hash = $2 AND s.channel_id = $3 AND s.expires_at > now()
            AND s.provider = ch.provider AND s.provider_channel_id = ch.provider_channel_id",
        )
        .bind(&challenge.nonce_hash)
        .bind(&challenge.session_hash)
        .bind(challenge.channel_id)
        .bind(challenge.wallet_address.to_ascii_lowercase())
        .bind(&challenge.message)
        .bind(challenge.expires_at)
        .execute(&mut *transaction)
        .await?;
        if inserted.rows_affected() != 1 {
            return Err(DbError::RegistrationConflict(
                "owner session is missing or expired",
            ));
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Reads a live challenge for the exact session and channel before signature checking.
    ///
    /// # Errors
    /// Returns database errors.
    pub async fn wallet_challenge(
        &self,
        nonce_hash: &[u8],
        session_hash: &[u8],
        channel_id: Uuid,
    ) -> Result<Option<WalletChallenge>, DbError> {
        sqlx::query_as::<_, WalletChallenge>("SELECT w.nonce_hash, w.session_hash, w.channel_id,
            w.wallet_address, w.message, w.expires_at FROM creator_wallet_challenges w
            JOIN creator_owner_sessions s ON s.session_hash = w.session_hash
            JOIN external_channels ch ON ch.id = w.channel_id JOIN creators c ON c.id = ch.creator_id
            WHERE w.nonce_hash = $1 AND w.session_hash = $2 AND w.channel_id = $3
            AND w.expires_at > now() AND s.expires_at > now() AND s.provider = ch.provider
            AND s.provider_channel_id = ch.provider_channel_id AND ch.enabled AND ch.verification_state <> 'disabled' AND c.status = 'active'")
            .bind(nonce_hash).bind(session_hash).bind(channel_id)
            .fetch_optional(&self.pool).await.map_err(Into::into)
    }

    /// Consumes a successfully verified challenge and reserves its beneficiary atomically.
    /// The caller must verify the stored message's signature before calling this method.
    /// Repeated attestations can target the same wallet; changing the wallet is rejected.
    ///
    /// # Errors
    /// Returns SQL or disabled-channel errors; replay, expiry, and conflicts return `false`.
    pub async fn reserve_verified_wallet(
        &self,
        nonce_hash: &[u8],
        session_hash: &[u8],
        channel_id: Uuid,
        wallet: &str,
    ) -> Result<bool, DbError> {
        let wallet = wallet.to_ascii_lowercase();
        let mut transaction = self.pool.begin().await?;
        ensure_enabled(&mut transaction, channel_id).await?;
        let consumed = sqlx::query("DELETE FROM creator_wallet_challenges w
            USING creator_owner_sessions s, external_channels ch
            WHERE w.nonce_hash = $1 AND w.session_hash = $2 AND w.channel_id = $3 AND w.wallet_address = $4
            AND s.session_hash = w.session_hash AND s.channel_id = w.channel_id
            AND ch.id = w.channel_id AND s.provider = ch.provider AND s.provider_channel_id = ch.provider_channel_id
            AND w.expires_at > now() AND s.expires_at > now()")
            .bind(nonce_hash).bind(session_hash).bind(channel_id).bind(&wallet)
            .execute(&mut *transaction).await?;
        if consumed.rows_affected() != 1 {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO creator_wallet_claims (channel_id, wallet_address)
            VALUES ($1, $2) ON CONFLICT (channel_id) DO NOTHING",
        )
        .bind(channel_id)
        .bind(&wallet)
        .execute(&mut *transaction)
        .await?;
        let stored = sqlx::query_scalar::<_, String>(
            "SELECT wallet_address FROM creator_wallet_claims WHERE channel_id = $1",
        )
        .bind(channel_id)
        .fetch_one(&mut *transaction)
        .await?;
        let matches = stored == wallet;
        if matches {
            audit(
                &mut transaction,
                channel_id,
                "wallet_reserved",
                Some(&wallet),
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(matches)
    }
}

async fn ensure_enabled(
    transaction: &mut Transaction<'_, Postgres>,
    channel_id: Uuid,
) -> Result<(), DbError> {
    let found = sqlx::query_scalar::<_, Uuid>("SELECT ch.id FROM external_channels ch
        JOIN creators c ON c.id = ch.creator_id WHERE ch.id = $1 AND ch.enabled AND ch.verification_state <> 'disabled' AND c.status = 'active' FOR UPDATE OF ch, c")
        .bind(channel_id).fetch_optional(&mut **transaction).await?;
    if found.is_none() {
        return Err(DbError::ChannelNotFound(channel_id));
    }
    Ok(())
}

async fn audit(
    transaction: &mut Transaction<'_, Postgres>,
    channel_id: Uuid,
    action: &str,
    wallet: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query("INSERT INTO creator_ownership_audit (id, channel_id, action, wallet_address) VALUES ($1, $2, $3, $4)")
        .bind(Uuid::new_v4()).bind(channel_id).bind(action).bind(wallet)
        .execute(&mut **transaction).await?;
    Ok(())
}

fn require_expiry(expires_at: DateTime<Utc>, max_seconds: i64) -> Result<(), DbError> {
    let remaining = expires_at.signed_duration_since(Utc::now()).num_seconds();
    if !(1..=max_seconds).contains(&remaining) {
        return Err(DbError::RegistrationConflict(
            "invalid authorization expiry",
        ));
    }
    Ok(())
}

fn validate_public_identity(provider: ProviderKind, id: &str) -> Result<(), DbError> {
    let valid = match provider {
        ProviderKind::Twitch => {
            (1..=20).contains(&id.len())
                && !id.starts_with('0')
                && id.bytes().all(|b| b.is_ascii_digit())
        }
        ProviderKind::YouTube => {
            id.len() == 24
                && id.starts_with("UC")
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }
        ProviderKind::Chzzk => {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        ProviderKind::LinkOnly => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DbError::RegistrationConflict(
            "invalid canonical provider channel ID",
        ))
    }
}

fn canonical_url(provider: ProviderKind, id: &str) -> String {
    match provider {
        ProviderKind::YouTube => format!("https://www.youtube.com/channel/{id}"),
        ProviderKind::Chzzk => format!("https://chzzk.naver.com/{id}"),
        // Twitch IDs are immutable; usernames cannot safely be inferred from them.
        ProviderKind::Twitch | ProviderKind::LinkOnly => "https://www.twitch.tv/".to_owned(),
    }
}
