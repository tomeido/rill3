//! Provider-neutral domain values for RILL3.

mod embed;
mod manual;
mod models;
mod provider;
mod state;

pub use embed::{
    EmbedDescriptor, EmbedValidationError, TwitchEmbedDescriptor, YouTubeEmbedDescriptor,
    validate_twitch_channel, validate_twitch_parent, validate_youtube_origin,
    validate_youtube_video_id,
};
pub use manual::{effective_live_status, manual_live_status};
pub use models::{
    Creator, CreatorStatus, ExternalChannel, LiveSession, ProviderEvent, ProviderEventStatus,
    VerificationState,
};
pub use provider::{
    EmbedCapability, ProviderAvailability, ProviderKind, ProviderKindParseError, VerifiedChannel,
    VerifyChannelInput,
};
pub use state::{
    LiveState, LiveStatus, LiveStatusParseError, StateApplyOutcome, StateTransitionError,
};
