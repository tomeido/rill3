use anyhow::Result;
use async_trait::async_trait;
use rill3_db::Database;
use rill3_domain::{ExternalChannel, LiveState, ProviderEvent, ProviderKind, StateApplyOutcome};
use uuid::Uuid;

use crate::http::{CreatorRecord, LiveRecord, PublicStore};

#[async_trait]
impl PublicStore for Database {
    async fn readiness(&self) -> Result<()> {
        Database::readiness(self).await.map_err(Into::into)
    }

    async fn list_live(&self, limit: u32) -> Result<Vec<LiveRecord>> {
        Ok(Database::list_live(self, limit)
            .await?
            .into_iter()
            .map(|listing| LiveRecord {
                creator: listing.creator,
                channel: listing.channel,
                session: listing.session,
            })
            .collect())
    }

    async fn creator_by_slug(&self, slug: &str) -> Result<Option<CreatorRecord>> {
        Ok(Database::creator_by_slug(self, slug)
            .await?
            .map(|listing| CreatorRecord {
                creator: listing.creator,
                channel: listing.channel,
                session: listing.session,
            }))
    }

    async fn creator_channel(
        &self,
        slug: &str,
        channel: Option<Uuid>,
    ) -> Result<Option<CreatorRecord>> {
        Ok(Database::creator_by_slug_and_channel(self, slug, channel)
            .await?
            .map(creator_record))
    }

    async fn registered_channels(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<CreatorRecord>> {
        Ok(
            Database::list_registered_channels_after(self, after, limit, false)
                .await?
                .into_iter()
                .map(creator_record)
                .collect(),
        )
    }

    async fn connected_channels(
        &self,
        slug: &str,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<CreatorRecord>> {
        Ok(Database::connected_channels_after(self, slug, after, limit)
            .await?
            .into_iter()
            .map(creator_record)
            .collect())
    }

    async fn channel_by_provider_id(
        &self,
        provider: ProviderKind,
        provider_channel_id: &str,
    ) -> Result<Option<ExternalChannel>> {
        Database::channel_by_provider_id(self, provider, provider_channel_id)
            .await
            .map_err(Into::into)
    }

    async fn apply_provider_event(
        &self,
        event: &ProviderEvent,
        state: &LiveState,
    ) -> Result<StateApplyOutcome> {
        Database::apply_provider_event(self, event, state)
            .await
            .map_err(Into::into)
    }

    async fn record_provider_signal(&self, event: &ProviderEvent) -> Result<bool> {
        Database::record_provider_signal(self, event)
            .await
            .map_err(Into::into)
    }
}

fn creator_record(listing: rill3_db::CreatorListing) -> CreatorRecord {
    CreatorRecord {
        creator: listing.creator,
        channel: listing.channel,
        session: listing.session,
    }
}
