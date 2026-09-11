//! Official and manual stream-provider adapters.

// Errors are represented by the shared, typed `ProviderError`/webhook enums.
// Individual adapter methods intentionally keep their rustdoc compact.
#![allow(clippy::missing_errors_doc)]

mod backoff;
mod chzzk;
mod error;
mod link_only;
mod mock;
mod provider;
mod twitch;
mod url_policy;
mod youtube;

pub use backoff::{BackoffPolicy, BackoffPolicyError};
pub use chzzk::{ChzzkConfig, ChzzkCredentials, ChzzkProvider};
pub use error::ProviderError;
pub use link_only::LinkOnlyProvider;
pub use mock::MockProvider;
pub use provider::StreamProvider;
pub use twitch::{
    EventSubStreamEvent, TwitchConfig, TwitchCredentials, TwitchEventSubHeaders, TwitchProvider,
    TwitchWebhookError, verify_eventsub_signature,
};
pub use url_policy::{UnsafeUrlReason, validate_external_https_url};
pub use youtube::{
    WebSubChallenge, WebSubError, WebSubSignatureError, YOUTUBE_WEBSUB_TOPIC_PREFIX, YouTubeConfig,
    YouTubeOAuthLiveApi, YouTubeProvider, YouTubeWebSubVerifier, parse_youtube_video_id,
    verify_websub_signature,
};
