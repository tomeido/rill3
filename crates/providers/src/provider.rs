use async_trait::async_trait;
use rill3_domain::{
    EmbedCapability, EmbedDescriptor, ExternalChannel, LiveState, ProviderAvailability,
    ProviderKind, VerifiedChannel, VerifyChannelInput,
};
use url::Url;

use crate::ProviderError;

#[async_trait]
pub trait StreamProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn embed_capability(&self) -> EmbedCapability;

    fn availability(&self) -> ProviderAvailability {
        ProviderAvailability::Enabled
    }

    fn disabled_reason(&self) -> Option<&str> {
        None
    }

    async fn verify_channel(
        &self,
        input: VerifyChannelInput,
    ) -> Result<VerifiedChannel, ProviderError>;

    async fn fetch_live_state(&self, channel: &ExternalChannel)
    -> Result<LiveState, ProviderError>;

    async fn reconcile_many(
        &self,
        channels: &[ExternalChannel],
    ) -> Result<Vec<LiveState>, ProviderError>;

    fn external_url(&self, channel: &ExternalChannel) -> Url;

    fn embed_descriptor(&self, live: &LiveState) -> Option<EmbedDescriptor>;
}
