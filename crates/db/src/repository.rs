use chrono::{DateTime, Utc};
use rill3_domain::{
    Creator, ExternalChannel, LiveState, LiveStatus, ProviderEvent, ProviderKind, StateApplyOutcome,
};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    ChannelRow, CreatorListing, CreatorRow, Database, DbError, ListingRow, LiveListing,
    records::{
        creator_status_as_str, embed_capability_as_str, live_status_as_str, parse_live_status,
        parse_provider, parse_url, verification_state_as_str,
    },
};

/// Hard bound for any directory or worker channel query.
pub const MAX_QUERY_RESULTS: u32 = 100;

impl Database {
    /// Returns at most 100 active online listings using one bounded join.
    ///
    /// Fresh observations sort before stale last-known-online observations.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn list_live(&self, limit: u32) -> Result<Vec<LiveListing>, DbError> {
        let rows = sqlx::query_as!(
            ListingRow,
            r#"
            SELECT
                c.id AS creator_id,
                c.slug AS creator_slug,
                c.display_name AS creator_display_name,
                c.bio AS creator_bio,
                c.locale AS creator_locale,
                c.status AS creator_status,
                c.created_at AS creator_created_at,
                c.updated_at AS creator_updated_at,
                ch.id AS channel_id,
                ch.creator_id AS channel_creator_id,
                ch.provider AS channel_provider,
                ch.provider_channel_id AS channel_provider_channel_id,
                ch.handle AS channel_handle,
                ch.canonical_url AS channel_canonical_url,
                ch.current_live_url AS channel_current_live_url,
                ch.manual_live_status AS channel_manual_live_status,
                ch.manual_state_expires_at AS channel_manual_state_expires_at,
                ch.embed_capability AS channel_embed_capability,
                ch.verification_state AS channel_verification_state,
                ch.enabled AS channel_enabled,
                ch.created_at AS channel_created_at,
                ch.updated_at AS channel_updated_at,
                ls.id AS "session_id?",
                ls.channel_id AS "session_channel_id?",
                ls.provider_session_id AS "session_provider_session_id?",
                ls.status AS "session_status?",
                ls.title AS "session_title?",
                ls.category AS "session_category?",
                ls.thumbnail_url AS "session_thumbnail_url?",
                ls.started_at AS "session_started_at?",
                ls.ended_at AS "session_ended_at?",
                ls.observed_at AS "session_observed_at?",
                ls.last_success_at AS "session_last_success_at?",
                ls.stale_since AS "session_stale_since?"
            FROM creators c
            JOIN external_channels ch ON ch.creator_id = c.id
            LEFT JOIN live_sessions ls ON ls.channel_id = ch.id
            WHERE c.status = 'active'
              AND ch.enabled
              AND ch.verification_state <> 'disabled'
              AND ls.status = 'online'
              AND (ch.provider NOT IN ('youtube', 'link_only')
                   OR (ch.manual_live_status = 'online'
                       AND ch.manual_state_expires_at > now()
                       AND ch.current_live_url IS NOT NULL
                       AND ls.observed_at >= ch.updated_at))
            ORDER BY
              (ls.stale_since IS NOT NULL),
              ls.observed_at DESC,
              c.id,
              ch.id
            LIMIT $1
            "#,
            bounded_limit(limit)
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(ListingRow::try_into_live_listing)
            .collect()
    }

    /// Finds an active creator and its best enabled channel using one bounded join.
    ///
    /// Online channels are preferred, followed by the most recently observed channel.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn creator_by_slug(&self, slug: &str) -> Result<Option<CreatorListing>, DbError> {
        self.creator_by_slug_and_channel(slug, None).await
    }

    /// Finds an active creator's enabled channel, optionally selected by its UUID.
    ///
    /// An explicit channel must belong to this creator. Invalid, foreign, or
    /// disabled selections return `None` instead of falling back to another channel.
    /// Without a selection, online and recently observed channels are preferred.
    /// Manual channels qualify as online only with a current, unexpired registration
    /// and a matching worker observation, just as they do in the live directory.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn creator_by_slug_and_channel(
        &self,
        slug: &str,
        channel_id: Option<Uuid>,
    ) -> Result<Option<CreatorListing>, DbError> {
        let row = sqlx::query_as!(
            ListingRow,
            r#"
            SELECT
                c.id AS creator_id,
                c.slug AS creator_slug,
                c.display_name AS creator_display_name,
                c.bio AS creator_bio,
                c.locale AS creator_locale,
                c.status AS creator_status,
                c.created_at AS creator_created_at,
                c.updated_at AS creator_updated_at,
                ch.id AS channel_id,
                ch.creator_id AS channel_creator_id,
                ch.provider AS channel_provider,
                ch.provider_channel_id AS channel_provider_channel_id,
                ch.handle AS channel_handle,
                ch.canonical_url AS channel_canonical_url,
                ch.current_live_url AS channel_current_live_url,
                ch.manual_live_status AS channel_manual_live_status,
                ch.manual_state_expires_at AS channel_manual_state_expires_at,
                ch.embed_capability AS channel_embed_capability,
                ch.verification_state AS channel_verification_state,
                ch.enabled AS channel_enabled,
                ch.created_at AS channel_created_at,
                ch.updated_at AS channel_updated_at,
                ls.id AS "session_id?",
                ls.channel_id AS "session_channel_id?",
                ls.provider_session_id AS "session_provider_session_id?",
                ls.status AS "session_status?",
                ls.title AS "session_title?",
                ls.category AS "session_category?",
                ls.thumbnail_url AS "session_thumbnail_url?",
                ls.started_at AS "session_started_at?",
                ls.ended_at AS "session_ended_at?",
                ls.observed_at AS "session_observed_at?",
                ls.last_success_at AS "session_last_success_at?",
                ls.stale_since AS "session_stale_since?"
            FROM creators c
            JOIN external_channels ch ON ch.creator_id = c.id
            LEFT JOIN live_sessions ls ON ls.channel_id = ch.id
            WHERE c.slug = $1
              AND c.status = 'active'
              AND ch.enabled
              AND ch.verification_state <> 'disabled'
              AND ($2::uuid IS NULL OR ch.id = $2)
            ORDER BY
              CASE
                WHEN ch.provider IN ('youtube', 'link_only') THEN
                  CASE
                    WHEN ch.manual_live_status = 'online'
                      AND ch.manual_state_expires_at > now()
                      AND ch.current_live_url IS NOT NULL
                      AND ls.status = 'online'
                      AND ls.observed_at >= ch.updated_at THEN 0
                    WHEN ch.manual_live_status = 'offline'
                      AND (ch.manual_state_expires_at IS NULL
                           OR ch.manual_state_expires_at > now()) THEN 2
                    ELSE 1
                  END
                ELSE CASE ls.status
                  WHEN 'online' THEN 0
                  WHEN 'unknown' THEN 1
                  ELSE 2
                END
              END,
              (ls.stale_since IS NOT NULL),
              ls.observed_at DESC NULLS LAST,
              ch.id
            LIMIT 1
            "#,
            slug,
            channel_id
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(ListingRow::try_into_creator_listing).transpose()
    }

    /// Lists a bounded batch of enabled channels for one provider.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn channels_for_provider(
        &self,
        provider: ProviderKind,
        limit: u32,
    ) -> Result<Vec<ExternalChannel>, DbError> {
        self.channels_for_provider_after(provider, None, limit)
            .await
    }

    /// Lists one keyset-paginated batch of enabled channels for a provider.
    ///
    /// Passing the final ID from the previous page avoids an unbounded query or
    /// the drift associated with offset pagination while channels are added.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn channels_for_provider_after(
        &self,
        provider: ProviderKind,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ExternalChannel>, DbError> {
        let rows = sqlx::query_as!(
            ChannelRow,
            r"
            SELECT
                id,
                creator_id,
                provider,
                provider_channel_id,
                handle,
                canonical_url,
                current_live_url,
                manual_live_status,
                manual_state_expires_at,
                embed_capability,
                verification_state,
                enabled,
                created_at,
                updated_at
            FROM external_channels
            WHERE provider = $1
              AND enabled
              AND ($2::uuid IS NULL OR id > $2)
            ORDER BY id
            LIMIT $3
            ",
            provider.as_str(),
            after,
            bounded_limit(limit)
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(ExternalChannel::try_from).collect()
    }

    /// Finds a channel by the provider-scoped identity used by webhook ingress.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn channel_by_provider_id(
        &self,
        provider: ProviderKind,
        provider_channel_id: &str,
    ) -> Result<Option<ExternalChannel>, DbError> {
        let row = sqlx::query_as!(
            ChannelRow,
            r"
            SELECT
                id,
                creator_id,
                provider,
                provider_channel_id,
                handle,
                canonical_url,
                current_live_url,
                manual_live_status,
                manual_state_expires_at,
                embed_capability,
                verification_state,
                enabled,
                created_at,
                updated_at
            FROM external_channels
            WHERE provider = $1 AND provider_channel_id = $2
            LIMIT 1
            ",
            provider.as_str(),
            provider_channel_id
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(ExternalChannel::try_from).transpose()
    }

    /// Inserts or updates a creator by its stable UUID.
    ///
    /// # Errors
    ///
    /// Returns an error when a constraint is violated or `PostgreSQL` fails.
    pub async fn upsert_creator(&self, creator: &Creator) -> Result<Creator, DbError> {
        let row = sqlx::query_as!(
            CreatorRow,
            r"
            INSERT INTO creators (
                id, slug, display_name, bio, locale, status, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (id) DO UPDATE SET
                slug = EXCLUDED.slug,
                display_name = EXCLUDED.display_name,
                bio = EXCLUDED.bio,
                locale = EXCLUDED.locale,
                status = EXCLUDED.status
            RETURNING id, slug, display_name, bio, locale, status, created_at, updated_at
            ",
            creator.id,
            &creator.slug,
            &creator.display_name,
            creator.bio.as_deref(),
            &creator.locale,
            creator_status_as_str(creator.status),
            creator.created_at,
            creator.updated_at
        )
        .fetch_one(&self.pool)
        .await?;

        Creator::try_from(row)
    }

    /// Inserts or updates an external channel by its stable UUID.
    ///
    /// # Errors
    ///
    /// Returns an error when a constraint is violated or `PostgreSQL` fails.
    pub async fn upsert_channel(
        &self,
        channel: &ExternalChannel,
    ) -> Result<ExternalChannel, DbError> {
        let row = sqlx::query_as!(
            ChannelRow,
            r"
            INSERT INTO external_channels (
                id,
                creator_id,
                provider,
                provider_channel_id,
                handle,
                canonical_url,
                current_live_url,
                manual_live_status,
                manual_state_expires_at,
                embed_capability,
                verification_state,
                enabled,
                created_at,
                updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14
            )
            ON CONFLICT (id) DO UPDATE SET
                creator_id = EXCLUDED.creator_id,
                provider = EXCLUDED.provider,
                provider_channel_id = EXCLUDED.provider_channel_id,
                handle = EXCLUDED.handle,
                canonical_url = EXCLUDED.canonical_url,
                current_live_url = EXCLUDED.current_live_url,
                manual_live_status = EXCLUDED.manual_live_status,
                manual_state_expires_at = EXCLUDED.manual_state_expires_at,
                embed_capability = EXCLUDED.embed_capability,
                verification_state = EXCLUDED.verification_state,
                enabled = EXCLUDED.enabled
            RETURNING
                id,
                creator_id,
                provider,
                provider_channel_id,
                handle,
                canonical_url,
                current_live_url,
                manual_live_status,
                manual_state_expires_at,
                embed_capability,
                verification_state,
                enabled,
                created_at,
                updated_at
            ",
            channel.id,
            channel.creator_id,
            channel.provider.as_str(),
            &channel.provider_channel_id,
            channel.handle.as_deref(),
            channel.canonical_url.as_str(),
            channel.current_live_url.as_ref().map(url::Url::as_str),
            channel.manual_live_status.map(live_status_as_str),
            channel.manual_state_expires_at,
            embed_capability_as_str(channel.embed_capability),
            verification_state_as_str(channel.verification_state),
            channel.enabled,
            channel.created_at,
            channel.updated_at
        )
        .fetch_one(&self.pool)
        .await?;

        ExternalChannel::try_from(row)
    }

    /// Atomically deduplicates a provider event and applies its normalized state.
    ///
    /// Event identifiers are provider-scoped. Reusing one with different immutable
    /// content produces an error instead of silently accepting an ambiguous retry.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid hash, an identity mismatch, a missing
    /// channel, a conflicting event reuse, or a `PostgreSQL` failure.
    pub async fn apply_provider_event(
        &self,
        event: &ProviderEvent,
        state: &LiveState,
    ) -> Result<StateApplyOutcome, DbError> {
        if event.provider != state.provider
            || event
                .channel_id
                .is_some_and(|channel_id| channel_id != state.channel_id)
        {
            return Err(DbError::ChannelIdentityMismatch(state.channel_id));
        }

        let payload_hash = decode_payload_hash(&event.payload_hash)?;
        let channel_id = event.channel_id.unwrap_or(state.channel_id);
        let event_row_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;
        let inserted_id = sqlx::query_scalar!(
            r"
            INSERT INTO provider_events (
                id,
                provider,
                external_event_id,
                channel_id,
                event_type,
                occurred_at,
                received_at,
                payload_hash,
                status,
                handled_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'received', NULL)
            ON CONFLICT (provider, external_event_id) DO NOTHING
            RETURNING id
            ",
            event_row_id,
            event.provider.as_str(),
            &event.external_event_id,
            channel_id,
            &event.event_type,
            postgres_timestamp(event.occurred_at),
            postgres_timestamp(event.received_at),
            &payload_hash
        )
        .fetch_optional(&mut *transaction)
        .await?;

        if inserted_id.is_none() {
            ensure_matching_existing_event(&mut transaction, event, channel_id, &payload_hash)
                .await?;
            transaction.rollback().await?;
            return Ok(StateApplyOutcome::Duplicate);
        }

        let outcome = apply_state_in_transaction(&mut transaction, state).await?;
        let event_status = match outcome {
            StateApplyOutcome::AppliedChanged | StateApplyOutcome::AppliedUnchanged => "applied",
            StateApplyOutcome::Duplicate => "duplicate",
            StateApplyOutcome::IgnoredOutOfOrder => "ignored_out_of_order",
        };
        sqlx::query!(
            r"
            UPDATE provider_events
            SET
                status = $2,
                handled_at = GREATEST(clock_timestamp(), received_at)
            WHERE id = $1
            ",
            event_row_id,
            event_status
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        Ok(outcome)
    }

    /// Records a provider signal that intentionally has no normalized live state.
    ///
    /// This is suitable for notifications such as `YouTube` `WebSub` callbacks that
    /// merely trigger reconciliation. Raw payload bytes are never persisted.
    /// Returns `true` for a new marker and `false` for an identical retry.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid hash, conflicting event-ID reuse, a foreign
    /// key violation, or another `PostgreSQL` failure.
    pub async fn record_provider_signal(&self, event: &ProviderEvent) -> Result<bool, DbError> {
        let payload_hash = decode_payload_hash(&event.payload_hash)?;
        let event_row_id = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;
        let inserted = sqlx::query_scalar!(
            r"
            INSERT INTO provider_events (
                id,
                provider,
                external_event_id,
                channel_id,
                event_type,
                occurred_at,
                received_at,
                payload_hash,
                status,
                handled_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8,
                'applied', GREATEST(clock_timestamp(), $7)
            )
            ON CONFLICT (provider, external_event_id) DO NOTHING
            RETURNING id
            ",
            event_row_id,
            event.provider.as_str(),
            &event.external_event_id,
            event.channel_id,
            &event.event_type,
            postgres_timestamp(event.occurred_at),
            postgres_timestamp(event.received_at),
            &payload_hash
        )
        .fetch_optional(&mut *transaction)
        .await?;

        if inserted.is_some() {
            transaction.commit().await?;
            return Ok(true);
        }

        ensure_matching_existing_signal(&mut transaction, event, &payload_hash).await?;
        transaction.rollback().await?;
        Ok(false)
    }

    /// Applies a normalized reconciliation result without a webhook event row.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/mismatched channel or `PostgreSQL` failure.
    pub async fn apply_reconciled_state(
        &self,
        state: &LiveState,
    ) -> Result<StateApplyOutcome, DbError> {
        let mut transaction = self.pool.begin().await?;
        let outcome = apply_state_in_transaction(&mut transaction, state).await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    /// Marks all current states for a provider stale while preserving live status.
    ///
    /// Returns the number of state rows whose first-known stale time changed.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` cannot update the rows.
    pub async fn mark_provider_stale(
        &self,
        provider: ProviderKind,
        stale_at: DateTime<Utc>,
    ) -> Result<u64, DbError> {
        let stale_at = postgres_timestamp(stale_at);
        let result = sqlx::query!(
            r"
            UPDATE live_sessions AS ls
            SET stale_since = CASE
                WHEN ls.last_success_at IS NULL THEN $2
                ELSE GREATEST($2, ls.last_success_at)
            END
            FROM external_channels AS ch
            WHERE ls.channel_id = ch.id
              AND ch.provider = $1
              AND ch.enabled
              AND (
                ls.stale_since IS NULL
                OR ls.stale_since > CASE
                    WHEN ls.last_success_at IS NULL THEN $2
                    ELSE GREATEST($2, ls.last_success_at)
                END
              )
            ",
            provider.as_str(),
            stale_at
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

fn bounded_limit(requested: u32) -> i64 {
    i64::from(requested.min(MAX_QUERY_RESULTS))
}

#[derive(Debug, FromRow)]
struct LockedStateRow {
    channel_provider: String,
    channel_provider_channel_id: String,
    channel_handle: Option<String>,
    channel_updated_at: DateTime<Utc>,
    session_id: Option<Uuid>,
    session_provider_session_id: Option<String>,
    session_status: Option<String>,
    session_title: Option<String>,
    session_category: Option<String>,
    session_thumbnail_url: Option<String>,
    session_started_at: Option<DateTime<Utc>>,
    session_ended_at: Option<DateTime<Utc>>,
    session_observed_at: Option<DateTime<Utc>>,
    session_last_success_at: Option<DateTime<Utc>>,
    session_stale_since: Option<DateTime<Utc>>,
}

impl LockedStateRow {
    fn live_state(&self, channel_id: Uuid) -> Result<Option<LiveState>, DbError> {
        if self.session_id.is_none() {
            return Ok(None);
        }
        let status = self
            .session_status
            .as_deref()
            .ok_or_else(|| DbError::InvalidPersistedValue {
                field: "live_sessions.status",
                value: "NULL for an existing session".to_owned(),
            })
            .and_then(parse_live_status)?;
        let observed_at =
            self.session_observed_at
                .ok_or_else(|| DbError::InvalidPersistedValue {
                    field: "live_sessions.observed_at",
                    value: "NULL for an existing session".to_owned(),
                })?;

        let provider = parse_provider(&self.channel_provider)?;
        Ok(Some(LiveState {
            channel_id,
            channel_updated_at: matches!(provider, ProviderKind::YouTube | ProviderKind::LinkOnly)
                .then_some(self.channel_updated_at),
            provider,
            provider_channel_id: self.channel_provider_channel_id.clone(),
            handle: self.channel_handle.clone(),
            provider_session_id: self.session_provider_session_id.clone(),
            status,
            title: self.session_title.clone(),
            category: self.session_category.clone(),
            thumbnail_url: self
                .session_thumbnail_url
                .as_deref()
                .map(|value| parse_url("thumbnail_url", value))
                .transpose()?,
            started_at: self.session_started_at,
            observed_at,
            last_success_at: self.session_last_success_at,
            stale_since: self.session_stale_since,
        }))
    }
}

#[allow(clippy::too_many_lines)]
async fn apply_state_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    incoming: &LiveState,
) -> Result<StateApplyOutcome, DbError> {
    let incoming = normalize_live_state(incoming.clone());
    let locked = sqlx::query_as!(
        LockedStateRow,
        r#"
        SELECT
            ch.provider AS channel_provider,
            ch.provider_channel_id AS channel_provider_channel_id,
            ch.handle AS channel_handle,
            ch.updated_at AS channel_updated_at,
            ls.id AS "session_id?",
            ls.provider_session_id AS "session_provider_session_id?",
            ls.status AS "session_status?",
            ls.title AS "session_title?",
            ls.category AS "session_category?",
            ls.thumbnail_url AS "session_thumbnail_url?",
            ls.started_at AS "session_started_at?",
            ls.ended_at AS "session_ended_at?",
            ls.observed_at AS "session_observed_at?",
            ls.last_success_at AS "session_last_success_at?",
            ls.stale_since AS "session_stale_since?"
        FROM external_channels AS ch
        LEFT JOIN live_sessions AS ls ON ls.channel_id = ch.id
        WHERE ch.id = $1
        FOR UPDATE OF ch
        "#,
        incoming.channel_id
    )
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(DbError::ChannelNotFound(incoming.channel_id))?;

    let persisted_provider = parse_provider(&locked.channel_provider)?;
    if persisted_provider != incoming.provider
        || locked.channel_provider_channel_id != incoming.provider_channel_id
    {
        return Err(DbError::ChannelIdentityMismatch(incoming.channel_id));
    }

    // A manual observation can finish after an operator has replaced its source
    // registration, even when observed_at is newer. The locked source revision
    // prevents an in-flight worker from restoring a discarded Online session.
    if matches!(
        persisted_provider,
        ProviderKind::YouTube | ProviderKind::LinkOnly
    ) && incoming.channel_updated_at != Some(postgres_timestamp(locked.channel_updated_at))
    {
        return Ok(StateApplyOutcome::IgnoredOutOfOrder);
    }

    let current = locked.live_state(incoming.channel_id)?;
    let outcome = if let Some(mut current) = current {
        current.apply_observation(incoming.clone())?
    } else {
        StateApplyOutcome::AppliedChanged
    };

    if outcome.should_persist() {
        let session_id = locked.session_id.unwrap_or_else(Uuid::new_v4);
        let ended_at = if incoming.status == LiveStatus::Offline {
            locked.session_ended_at.or(Some(incoming.observed_at))
        } else {
            None
        };
        sqlx::query!(
            r"
            INSERT INTO live_sessions (
                id,
                channel_id,
                provider_session_id,
                status,
                title,
                category,
                thumbnail_url,
                started_at,
                ended_at,
                observed_at,
                last_success_at,
                stale_since
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (channel_id) DO UPDATE SET
                provider_session_id = EXCLUDED.provider_session_id,
                status = EXCLUDED.status,
                title = EXCLUDED.title,
                category = EXCLUDED.category,
                thumbnail_url = EXCLUDED.thumbnail_url,
                started_at = EXCLUDED.started_at,
                ended_at = EXCLUDED.ended_at,
                observed_at = EXCLUDED.observed_at,
                last_success_at = EXCLUDED.last_success_at,
                stale_since = EXCLUDED.stale_since
            ",
            session_id,
            incoming.channel_id,
            incoming.provider_session_id.as_deref(),
            live_status_as_str(incoming.status),
            incoming.title.as_deref(),
            incoming.category.as_deref(),
            incoming.thumbnail_url.as_ref().map(url::Url::as_str),
            incoming.started_at,
            ended_at,
            incoming.observed_at,
            incoming.last_success_at,
            incoming.stale_since
        )
        .execute(&mut **transaction)
        .await?;

        sqlx::query!(
            r"
            UPDATE external_channels
            SET handle = $2
            WHERE id = $1 AND handle IS DISTINCT FROM $2
            ",
            incoming.channel_id,
            incoming.handle.as_deref()
        )
        .execute(&mut **transaction)
        .await?;
    }

    Ok(outcome)
}

#[derive(Debug, FromRow)]
struct ExistingEventRow {
    channel_id: Option<Uuid>,
    event_type: String,
    occurred_at: DateTime<Utc>,
    payload_hash: Vec<u8>,
}

async fn ensure_matching_existing_event(
    transaction: &mut Transaction<'_, Postgres>,
    incoming: &ProviderEvent,
    channel_id: Uuid,
    payload_hash: &[u8],
) -> Result<(), DbError> {
    let existing = sqlx::query_as!(
        ExistingEventRow,
        r"
        SELECT channel_id, event_type, occurred_at, payload_hash
        FROM provider_events
        WHERE provider = $1 AND external_event_id = $2
        ",
        incoming.provider.as_str(),
        &incoming.external_event_id
    )
    .fetch_one(&mut **transaction)
    .await?;

    if existing.channel_id == Some(channel_id)
        && existing.event_type == incoming.event_type
        && existing.occurred_at.timestamp_micros() == incoming.occurred_at.timestamp_micros()
        && existing.payload_hash == payload_hash
    {
        Ok(())
    } else {
        Err(DbError::IdempotencyConflict {
            provider: incoming.provider.to_string(),
            external_event_id: incoming.external_event_id.clone(),
        })
    }
}

async fn ensure_matching_existing_signal(
    transaction: &mut Transaction<'_, Postgres>,
    incoming: &ProviderEvent,
    payload_hash: &[u8],
) -> Result<(), DbError> {
    let existing = sqlx::query_as!(
        ExistingEventRow,
        r"
        SELECT channel_id, event_type, occurred_at, payload_hash
        FROM provider_events
        WHERE provider = $1 AND external_event_id = $2
        ",
        incoming.provider.as_str(),
        &incoming.external_event_id
    )
    .fetch_one(&mut **transaction)
    .await?;

    if existing.channel_id == incoming.channel_id
        && existing.event_type == incoming.event_type
        && existing.occurred_at.timestamp_micros() == incoming.occurred_at.timestamp_micros()
        && existing.payload_hash == payload_hash
    {
        Ok(())
    } else {
        Err(DbError::IdempotencyConflict {
            provider: incoming.provider.to_string(),
            external_event_id: incoming.external_event_id.clone(),
        })
    }
}

fn decode_payload_hash(value: &str) -> Result<Vec<u8>, DbError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DbError::InvalidPayloadHash);
    }

    hex::decode(value).map_err(|_| DbError::InvalidPayloadHash)
}

fn normalize_live_state(mut state: LiveState) -> LiveState {
    state.channel_updated_at = state.channel_updated_at.map(postgres_timestamp);
    state.observed_at = postgres_timestamp(state.observed_at);
    state.started_at = state.started_at.map(postgres_timestamp);
    state.last_success_at = state.last_success_at.map(postgres_timestamp);
    state.stale_since = state.stale_since.map(postgres_timestamp);
    state
}

fn postgres_timestamp(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(value.timestamp_micros()).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::{MAX_QUERY_RESULTS, bounded_limit, decode_payload_hash};

    #[test]
    fn list_limit_is_hard_capped_and_zero_stays_zero() {
        assert_eq!(bounded_limit(0), 0);
        assert_eq!(bounded_limit(12), 12);
        assert_eq!(bounded_limit(u32::MAX), i64::from(MAX_QUERY_RESULTS));
    }

    #[test]
    fn payload_hash_requires_lower_hex_sha256() {
        assert!(decode_payload_hash(&"a".repeat(64)).is_ok());
        assert!(decode_payload_hash(&"A".repeat(64)).is_err());
        assert!(decode_payload_hash(&"a".repeat(63)).is_err());
        assert!(decode_payload_hash(&"z".repeat(64)).is_err());
    }
}
