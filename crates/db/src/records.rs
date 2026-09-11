use std::str::FromStr;

use chrono::{DateTime, Utc};
use rill3_domain::{
    Creator, CreatorStatus, EmbedCapability, ExternalChannel, LiveSession, LiveStatus,
    ProviderKind, VerificationState,
};
use sqlx::FromRow;
use url::Url;
use uuid::Uuid;

use crate::DbError;

/// One live directory row assembled by a bounded join.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveListing {
    /// Public creator data.
    pub creator: Creator,
    /// External channel carrying the current live state.
    pub channel: ExternalChannel,
    /// Current normalized session state.
    pub session: LiveSession,
}

/// Creator page data assembled by a bounded join.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatorListing {
    /// Public creator data.
    pub creator: Creator,
    /// The selected enabled external channel.
    pub channel: ExternalChannel,
    /// Current normalized state, if this channel has been observed.
    pub session: Option<LiveSession>,
}

#[derive(Debug, FromRow)]
pub(crate) struct CreatorRow {
    pub(crate) id: Uuid,
    pub(crate) slug: String,
    pub(crate) display_name: String,
    pub(crate) bio: Option<String>,
    pub(crate) locale: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl TryFrom<CreatorRow> for Creator {
    type Error = DbError;

    fn try_from(row: CreatorRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            slug: row.slug,
            display_name: row.display_name,
            bio: row.bio,
            locale: row.locale,
            status: parse_creator_status(&row.status)?,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct ChannelRow {
    pub(crate) id: Uuid,
    pub(crate) creator_id: Uuid,
    pub(crate) provider: String,
    pub(crate) provider_channel_id: String,
    pub(crate) handle: Option<String>,
    pub(crate) canonical_url: String,
    pub(crate) current_live_url: Option<String>,
    pub(crate) manual_live_status: Option<String>,
    pub(crate) manual_state_expires_at: Option<DateTime<Utc>>,
    pub(crate) embed_capability: String,
    pub(crate) verification_state: String,
    pub(crate) enabled: bool,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl TryFrom<ChannelRow> for ExternalChannel {
    type Error = DbError;

    fn try_from(row: ChannelRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            creator_id: row.creator_id,
            provider: parse_provider(&row.provider)?,
            provider_channel_id: row.provider_channel_id,
            handle: row.handle,
            canonical_url: parse_url("canonical_url", &row.canonical_url)?,
            current_live_url: row
                .current_live_url
                .as_deref()
                .map(|value| parse_url("current_live_url", value))
                .transpose()?,
            manual_live_status: row
                .manual_live_status
                .as_deref()
                .map(parse_live_status)
                .transpose()?,
            manual_state_expires_at: row.manual_state_expires_at,
            embed_capability: parse_embed_capability(&row.embed_capability)?,
            verification_state: parse_verification_state(&row.verification_state)?,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct ListingRow {
    pub(crate) creator_id: Uuid,
    pub(crate) creator_slug: String,
    pub(crate) creator_display_name: String,
    pub(crate) creator_bio: Option<String>,
    pub(crate) creator_locale: String,
    pub(crate) creator_status: String,
    pub(crate) creator_created_at: DateTime<Utc>,
    pub(crate) creator_updated_at: DateTime<Utc>,
    pub(crate) channel_id: Uuid,
    pub(crate) channel_creator_id: Uuid,
    pub(crate) channel_provider: String,
    pub(crate) channel_provider_channel_id: String,
    pub(crate) channel_handle: Option<String>,
    pub(crate) channel_canonical_url: String,
    pub(crate) channel_current_live_url: Option<String>,
    pub(crate) channel_manual_live_status: Option<String>,
    pub(crate) channel_manual_state_expires_at: Option<DateTime<Utc>>,
    pub(crate) channel_embed_capability: String,
    pub(crate) channel_verification_state: String,
    pub(crate) channel_enabled: bool,
    pub(crate) channel_created_at: DateTime<Utc>,
    pub(crate) channel_updated_at: DateTime<Utc>,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) session_channel_id: Option<Uuid>,
    pub(crate) session_provider_session_id: Option<String>,
    pub(crate) session_status: Option<String>,
    pub(crate) session_title: Option<String>,
    pub(crate) session_category: Option<String>,
    pub(crate) session_thumbnail_url: Option<String>,
    pub(crate) session_started_at: Option<DateTime<Utc>>,
    pub(crate) session_ended_at: Option<DateTime<Utc>>,
    pub(crate) session_observed_at: Option<DateTime<Utc>>,
    pub(crate) session_last_success_at: Option<DateTime<Utc>>,
    pub(crate) session_stale_since: Option<DateTime<Utc>>,
}

impl ListingRow {
    pub(crate) fn try_into_creator_listing(self) -> Result<CreatorListing, DbError> {
        let session = self.session()?;
        let (creator, channel) = self.creator_and_channel()?;
        Ok(CreatorListing {
            creator,
            channel,
            session,
        })
    }

    pub(crate) fn try_into_live_listing(self) -> Result<LiveListing, DbError> {
        let session = self
            .session()?
            .ok_or_else(|| DbError::InvalidPersistedValue {
                field: "live_sessions.id",
                value: "NULL in live listing".to_owned(),
            })?;
        let (creator, channel) = self.creator_and_channel()?;
        Ok(LiveListing {
            creator,
            channel,
            session,
        })
    }

    fn creator_and_channel(&self) -> Result<(Creator, ExternalChannel), DbError> {
        let creator = Creator {
            id: self.creator_id,
            slug: self.creator_slug.clone(),
            display_name: self.creator_display_name.clone(),
            bio: self.creator_bio.clone(),
            locale: self.creator_locale.clone(),
            status: parse_creator_status(&self.creator_status)?,
            created_at: self.creator_created_at,
            updated_at: self.creator_updated_at,
        };
        let channel = ExternalChannel {
            id: self.channel_id,
            creator_id: self.channel_creator_id,
            provider: parse_provider(&self.channel_provider)?,
            provider_channel_id: self.channel_provider_channel_id.clone(),
            handle: self.channel_handle.clone(),
            canonical_url: parse_url("canonical_url", &self.channel_canonical_url)?,
            current_live_url: self
                .channel_current_live_url
                .as_deref()
                .map(|value| parse_url("current_live_url", value))
                .transpose()?,
            manual_live_status: self
                .channel_manual_live_status
                .as_deref()
                .map(parse_live_status)
                .transpose()?,
            manual_state_expires_at: self.channel_manual_state_expires_at,
            embed_capability: parse_embed_capability(&self.channel_embed_capability)?,
            verification_state: parse_verification_state(&self.channel_verification_state)?,
            enabled: self.channel_enabled,
            created_at: self.channel_created_at,
            updated_at: self.channel_updated_at,
        };
        Ok((creator, channel))
    }

    fn session(&self) -> Result<Option<LiveSession>, DbError> {
        let Some(id) = self.session_id else {
            return Ok(None);
        };
        let channel_id = self
            .session_channel_id
            .ok_or_else(|| DbError::InvalidPersistedValue {
                field: "live_sessions.channel_id",
                value: "NULL for an existing session".to_owned(),
            })?;
        let state = self
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

        Ok(Some(LiveSession {
            id,
            channel_id,
            provider_session_id: self.session_provider_session_id.clone(),
            state,
            title: self.session_title.clone(),
            category: self.session_category.clone(),
            thumbnail_url: self
                .session_thumbnail_url
                .as_deref()
                .map(|value| parse_url("thumbnail_url", value))
                .transpose()?,
            started_at: self.session_started_at,
            ended_at: self.session_ended_at,
            observed_at,
            last_success_at: self.session_last_success_at,
            stale_since: self.session_stale_since,
        }))
    }
}

pub(crate) const fn creator_status_as_str(value: CreatorStatus) -> &'static str {
    match value {
        CreatorStatus::Active => "active",
        CreatorStatus::Disabled => "disabled",
    }
}

pub(crate) const fn embed_capability_as_str(value: EmbedCapability) -> &'static str {
    match value {
        EmbedCapability::OfficialEmbed => "official_embed",
        EmbedCapability::LinkOnly => "link_only",
        EmbedCapability::Manual => "manual",
    }
}

pub(crate) const fn verification_state_as_str(value: VerificationState) -> &'static str {
    match value {
        VerificationState::Pending => "pending",
        VerificationState::Verified => "verified",
        VerificationState::Disabled => "disabled",
    }
}

pub(crate) const fn live_status_as_str(value: LiveStatus) -> &'static str {
    match value {
        LiveStatus::Online => "online",
        LiveStatus::Offline => "offline",
        LiveStatus::Unknown => "unknown",
    }
}

pub(crate) fn parse_provider(value: &str) -> Result<ProviderKind, DbError> {
    ProviderKind::from_str(value).map_err(|_| invalid_value("provider", value))
}

fn parse_creator_status(value: &str) -> Result<CreatorStatus, DbError> {
    match value {
        "active" => Ok(CreatorStatus::Active),
        "disabled" => Ok(CreatorStatus::Disabled),
        _ => Err(invalid_value("creator status", value)),
    }
}

fn parse_embed_capability(value: &str) -> Result<EmbedCapability, DbError> {
    match value {
        "official_embed" => Ok(EmbedCapability::OfficialEmbed),
        "link_only" => Ok(EmbedCapability::LinkOnly),
        "manual" => Ok(EmbedCapability::Manual),
        _ => Err(invalid_value("embed capability", value)),
    }
}

fn parse_verification_state(value: &str) -> Result<VerificationState, DbError> {
    match value {
        "pending" => Ok(VerificationState::Pending),
        "verified" => Ok(VerificationState::Verified),
        "disabled" => Ok(VerificationState::Disabled),
        _ => Err(invalid_value("verification state", value)),
    }
}

pub(crate) fn parse_live_status(value: &str) -> Result<LiveStatus, DbError> {
    match value {
        "online" => Ok(LiveStatus::Online),
        "offline" => Ok(LiveStatus::Offline),
        "unknown" => Ok(LiveStatus::Unknown),
        _ => Err(invalid_value("live status", value)),
    }
}

pub(crate) fn parse_url(field: &'static str, value: &str) -> Result<Url, DbError> {
    Url::parse(value).map_err(|_| invalid_value(field, value))
}

fn invalid_value(field: &'static str, value: &str) -> DbError {
    DbError::InvalidPersistedValue {
        field,
        value: value.to_owned(),
    }
}
