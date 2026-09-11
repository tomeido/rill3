use rill3_domain::{Creator, CreatorStatus, ExternalChannel, VerificationState};
use uuid::Uuid;

use crate::{
    ChannelRow, CreatorListing, CreatorRow, Database, DbError, ListingRow, MAX_QUERY_RESULTS,
    records::{embed_capability_as_str, live_status_as_str},
};

impl Database {
    /// Atomically registers a verified channel under a stable creator slug.
    ///
    /// Existing creator profiles and IDs are preserved. Re-registering a provider
    /// identity refreshes its channel fields while preserving its owner and UUID.
    /// Changed manual fields invalidate cached session data until reconciliation.
    /// The channel's input `creator_id` is replaced with the persisted creator ID.
    /// Provider validation must be performed before calling this method.
    ///
    /// # Errors
    ///
    /// Rejects disabled creators/channels, unverified input, and provider identities
    /// already owned by another creator. Every failure rolls back both writes.
    #[allow(clippy::too_many_lines)]
    pub async fn register_channel(
        &self,
        creator: &Creator,
        channel: &ExternalChannel,
    ) -> Result<(Creator, ExternalChannel), DbError> {
        if creator.status != CreatorStatus::Active
            || !channel.enabled
            || channel.verification_state != VerificationState::Verified
        {
            return Err(DbError::RegistrationConflict(
                "registration requires an active creator and an enabled, verified channel",
            ));
        }

        let mut transaction = self.pool.begin().await?;
        sqlx::query!(
            r"
            INSERT INTO creators (
                id, slug, display_name, bio, locale, status, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, 'active', $6, $7)
            ON CONFLICT (slug) DO NOTHING
            ",
            creator.id,
            &creator.slug,
            &creator.display_name,
            creator.bio.as_deref(),
            &creator.locale,
            creator.created_at,
            creator.updated_at
        )
        .execute(&mut *transaction)
        .await?;

        // Lock the actual row after INSERT so concurrent registrations also see
        // the committed winner of the unique-slug conflict.
        let creator_row = sqlx::query_as!(
            CreatorRow,
            r"
            SELECT id, slug, display_name, bio, locale, status, created_at, updated_at
            FROM creators
            WHERE slug = $1
            FOR UPDATE
            ",
            &creator.slug
        )
        .fetch_one(&mut *transaction)
        .await?;
        let stored_creator = Creator::try_from(creator_row)?;
        if stored_creator.status != CreatorStatus::Active {
            return Err(DbError::RegistrationConflict("creator is disabled"));
        }

        let manual_changed = sqlx::query_scalar!(
            r#"
            SELECT (current_live_url IS DISTINCT FROM $3
                    OR manual_live_status IS DISTINCT FROM $4
                    OR manual_state_expires_at IS DISTINCT FROM $5) AS "changed!"
            FROM external_channels
            WHERE provider = $1 AND provider_channel_id = $2
            FOR UPDATE
            "#,
            channel.provider.as_str(),
            &channel.provider_channel_id,
            channel.current_live_url.as_ref().map(url::Url::as_str),
            channel.manual_live_status.map(live_status_as_str),
            channel.manual_state_expires_at
        )
        .fetch_optional(&mut *transaction)
        .await?
        .unwrap_or(false);

        let channel_row = sqlx::query_as!(
            ChannelRow,
            r"
            INSERT INTO external_channels (
                id, creator_id, provider, provider_channel_id, handle, canonical_url,
                current_live_url, manual_live_status, manual_state_expires_at,
                embed_capability, verification_state, enabled, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'verified', true, $11, $12)
            ON CONFLICT (provider, provider_channel_id) DO UPDATE SET
                handle = EXCLUDED.handle,
                canonical_url = EXCLUDED.canonical_url,
                current_live_url = EXCLUDED.current_live_url,
                manual_live_status = EXCLUDED.manual_live_status,
                manual_state_expires_at = EXCLUDED.manual_state_expires_at,
                embed_capability = EXCLUDED.embed_capability,
                verification_state = EXCLUDED.verification_state
            WHERE external_channels.creator_id = EXCLUDED.creator_id
              AND external_channels.enabled
              AND external_channels.verification_state <> 'disabled'
            RETURNING
                id, creator_id, provider, provider_channel_id, handle, canonical_url,
                current_live_url, manual_live_status, manual_state_expires_at,
                embed_capability, verification_state, enabled, created_at, updated_at
            ",
            channel.id,
            stored_creator.id,
            channel.provider.as_str(),
            &channel.provider_channel_id,
            channel.handle.as_deref(),
            channel.canonical_url.as_str(),
            channel.current_live_url.as_ref().map(url::Url::as_str),
            channel.manual_live_status.map(live_status_as_str),
            channel.manual_state_expires_at,
            embed_capability_as_str(channel.embed_capability),
            channel.created_at,
            channel.updated_at
        )
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(DbError::RegistrationConflict(
            "provider identity belongs to another creator or the channel is disabled",
        ))?;
        let stored_channel = ExternalChannel::try_from(channel_row)?;
        if manual_changed {
            sqlx::query!(
                "DELETE FROM live_sessions WHERE channel_id = $1",
                stored_channel.id
            )
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok((stored_creator, stored_channel))
    }

    /// Lists registered channels, including offline and not-yet-observed channels.
    ///
    /// Results are ordered by channel UUID with a hard limit of 100. Pass the
    /// previous page's last channel ID as `after` to continue. Public callers must
    /// pass `false` for `include_disabled`; operator listings may pass `true`.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn list_registered_channels_after(
        &self,
        after: Option<Uuid>,
        limit: u32,
        include_disabled: bool,
    ) -> Result<Vec<CreatorListing>, DbError> {
        self.registered_channels_filtered(None, after, limit, include_disabled)
            .await
    }

    /// Lists one creator's enabled channels, including channels without live state.
    ///
    /// The creator must be active. Results are keyset-paginated by channel UUID
    /// and hard-capped at 100 so even large connected-channel lists stay bounded.
    ///
    /// # Errors
    ///
    /// Returns an error when `PostgreSQL` fails or a persisted domain value is invalid.
    pub async fn connected_channels_after(
        &self,
        slug: &str,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<CreatorListing>, DbError> {
        self.registered_channels_filtered(Some(slug), after, limit, false)
            .await
    }

    async fn registered_channels_filtered(
        &self,
        slug: Option<&str>,
        after: Option<Uuid>,
        limit: u32,
        include_disabled: bool,
    ) -> Result<Vec<CreatorListing>, DbError> {
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
            WHERE ($1::text IS NULL OR c.slug = $1)
              AND ($2::uuid IS NULL OR ch.id > $2)
              AND ($3 OR (c.status = 'active' AND ch.enabled
                          AND ch.verification_state <> 'disabled'))
            ORDER BY ch.id
            LIMIT $4
            "#,
            slug,
            after,
            include_disabled,
            i64::from(limit.min(MAX_QUERY_RESULTS))
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(ListingRow::try_into_creator_listing)
            .collect()
    }
}
