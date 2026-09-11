use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// An external streaming platform understood by the M0/M1 application.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Twitch,
    YouTube,
    Chzzk,
    LinkOnly,
}

impl ProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Twitch => "twitch",
            Self::YouTube => "youtube",
            Self::Chzzk => "chzzk",
            Self::LinkOnly => "link_only",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderKind {
    type Err = ProviderKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "twitch" => Ok(Self::Twitch),
            "youtube" => Ok(Self::YouTube),
            "chzzk" => Ok(Self::Chzzk),
            "link_only" => Ok(Self::LinkOnly),
            _ => Err(ProviderKindParseError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unsupported provider kind: {0}")]
pub struct ProviderKindParseError(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedCapability {
    OfficialEmbed,
    LinkOnly,
    Manual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAvailability {
    Enabled,
    Disabled,
}

/// Untrusted channel-registration input. Provider implementations must validate
/// every populated field before returning [`VerifiedChannel`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifyChannelInput {
    pub provider_channel_id: String,
    pub handle: Option<String>,
    pub external_url: Option<String>,
    pub current_live_url: Option<String>,
}

impl VerifyChannelInput {
    #[must_use]
    pub fn new(provider_channel_id: impl Into<String>) -> Self {
        Self {
            provider_channel_id: provider_channel_id.into(),
            handle: None,
            external_url: None,
            current_live_url: None,
        }
    }
}

/// Canonical data returned only after a provider has validated an input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifiedChannel {
    pub provider: ProviderKind,
    pub provider_channel_id: String,
    pub handle: Option<String>,
    pub display_name: Option<String>,
    pub canonical_url: Url,
    pub current_live_url: Option<Url>,
    pub thumbnail_url: Option<Url>,
    pub embed_capability: EmbedCapability,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kind_has_stable_database_text() {
        for (kind, value) in [
            (ProviderKind::Twitch, "twitch"),
            (ProviderKind::YouTube, "youtube"),
            (ProviderKind::Chzzk, "chzzk"),
            (ProviderKind::LinkOnly, "link_only"),
        ] {
            assert_eq!(kind.as_str(), value);
            assert_eq!(value.parse::<ProviderKind>(), Ok(kind));
        }
    }
}
