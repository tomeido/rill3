use serde::Serialize;
use thiserror::Error;
use url::{Host, Url};

use crate::ProviderKind;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum EmbedDescriptor {
    Twitch(TwitchEmbedDescriptor),
    YouTube(YouTubeEmbedDescriptor),
}

impl EmbedDescriptor {
    #[must_use]
    pub const fn provider(&self) -> ProviderKind {
        match self {
            Self::Twitch(_) => ProviderKind::Twitch,
            Self::YouTube(_) => ProviderKind::YouTube,
        }
    }

    #[must_use]
    pub fn src(&self) -> &Url {
        match self {
            Self::Twitch(descriptor) => descriptor.src(),
            Self::YouTube(descriptor) => descriptor.src(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TwitchEmbedDescriptor {
    channel: String,
    parent: String,
    src: Url,
}

impl TwitchEmbedDescriptor {
    /// Builds an official Twitch player descriptor from validated scalar values.
    ///
    /// # Errors
    ///
    /// Returns an error when `channel` is not a Twitch login or `parent` is not
    /// a bare DNS hostname.
    pub fn new(channel: &str, parent: &str) -> Result<Self, EmbedValidationError> {
        validate_twitch_channel(channel)?;
        validate_twitch_parent(parent)?;

        let mut src = Url::parse("https://player.twitch.tv/")
            .map_err(|_| EmbedValidationError::InternalBaseUrl)?;
        src.query_pairs_mut()
            .append_pair("channel", channel)
            .append_pair("parent", parent);

        Ok(Self {
            channel: channel.to_owned(),
            parent: parent.to_ascii_lowercase(),
            src,
        })
    }

    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    #[must_use]
    pub fn parent(&self) -> &str {
        &self.parent
    }

    #[must_use]
    pub fn src(&self) -> &Url {
        &self.src
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct YouTubeEmbedDescriptor {
    video_id: String,
    origin: Url,
    src: Url,
}

impl YouTubeEmbedDescriptor {
    /// Builds an official `YouTube` iframe descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid video ID or an origin outside the HTTPS
    /// and explicit local-loopback policy.
    pub fn new(video_id: &str, origin: &Url) -> Result<Self, EmbedValidationError> {
        validate_youtube_video_id(video_id)?;
        validate_youtube_origin(origin)?;

        let mut src = Url::parse("https://www.youtube.com/embed/")
            .map_err(|_| EmbedValidationError::InternalBaseUrl)?;
        src.path_segments_mut()
            .map_err(|()| EmbedValidationError::InternalBaseUrl)?
            .push(video_id);
        src.query_pairs_mut()
            .append_pair("enablejsapi", "1")
            .append_pair("origin", origin.as_str().trim_end_matches('/'));

        Ok(Self {
            video_id: video_id.to_owned(),
            origin: origin.clone(),
            src,
        })
    }

    #[must_use]
    pub fn video_id(&self) -> &str {
        &self.video_id
    }

    #[must_use]
    pub fn origin(&self) -> &Url {
        &self.origin
    }

    #[must_use]
    pub fn src(&self) -> &Url {
        &self.src
    }
}

/// Validates the restricted character set accepted for Twitch logins.
///
/// # Errors
///
/// Returns [`EmbedValidationError::InvalidTwitchChannel`] for an invalid login.
pub fn validate_twitch_channel(channel: &str) -> Result<(), EmbedValidationError> {
    let valid = (1..=25).contains(&channel.len())
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(EmbedValidationError::InvalidTwitchChannel)
    }
}

/// Validates the trusted Twitch `parent` configuration as a bare DNS host.
///
/// # Errors
///
/// Returns [`EmbedValidationError::InvalidTwitchParent`] for a URL, port,
/// credentials, malformed label, or non-DNS host.
pub fn validate_twitch_parent(parent: &str) -> Result<(), EmbedValidationError> {
    if parent.is_empty()
        || parent.len() > 253
        || !parent.is_ascii()
        || parent.contains(['/', ':', '@'])
    {
        return Err(EmbedValidationError::InvalidTwitchParent);
    }

    let parsed = Url::parse(&format!("https://{parent}/"))
        .map_err(|_| EmbedValidationError::InvalidTwitchParent)?;
    let Some(Host::Domain(host)) = parsed.host() else {
        return Err(EmbedValidationError::InvalidTwitchParent);
    };
    if !host.eq_ignore_ascii_case(parent)
        || host.split('.').any(|label| {
            label.is_empty()
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(EmbedValidationError::InvalidTwitchParent);
    }
    Ok(())
}

/// Validates a `YouTube` video identifier without accepting URL syntax.
///
/// # Errors
///
/// Returns [`EmbedValidationError::InvalidYouTubeVideoId`] unless the ID is 11
/// URL-safe ASCII characters.
pub fn validate_youtube_video_id(video_id: &str) -> Result<(), EmbedValidationError> {
    if video_id.len() == 11
        && video_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(EmbedValidationError::InvalidYouTubeVideoId)
    }
}

/// Validates an application origin used by the `YouTube` `IFrame` Player API.
///
/// # Errors
///
/// Returns [`EmbedValidationError::InvalidYouTubeOrigin`] unless the value is a
/// path-free HTTPS origin, or HTTP on an explicit loopback host for development.
pub fn validate_youtube_origin(origin: &Url) -> Result<(), EmbedValidationError> {
    let secure_origin = origin.scheme() == "https";
    let local_development_origin = origin.scheme() == "http"
        && match origin.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
    if (!secure_origin && !local_development_origin)
        || origin.host().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || !matches!(origin.path(), "" | "/")
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(EmbedValidationError::InvalidYouTubeOrigin);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EmbedValidationError {
    #[error("invalid Twitch channel")]
    InvalidTwitchChannel,
    #[error("invalid Twitch parent host")]
    InvalidTwitchParent,
    #[error("invalid YouTube video ID")]
    InvalidYouTubeVideoId,
    #[error(
        "YouTube origin must be HTTPS (or loopback HTTP for local development) without credentials or a path"
    )]
    InvalidYouTubeOrigin,
    #[error("internal embed base URL is invalid")]
    InternalBaseUrl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twitch_descriptor_uses_only_validated_channel_and_parent() {
        let descriptor = TwitchEmbedDescriptor::new("safe_channel", "rill3.example").unwrap();
        assert_eq!(descriptor.src().scheme(), "https");
        assert_eq!(descriptor.src().host_str(), Some("player.twitch.tv"));
        assert!(
            descriptor
                .src()
                .query_pairs()
                .any(|pair| { pair.0 == "parent" && pair.1 == "rill3.example" })
        );
        assert!(TwitchEmbedDescriptor::new("safe/channel", "rill3.example").is_err());
        assert!(TwitchEmbedDescriptor::new("safe_channel", "evil.test/path").is_err());
    }

    #[test]
    fn youtube_descriptor_rejects_untrusted_origin_shapes() {
        let origin = Url::parse("https://rill3.example").unwrap();
        let descriptor = YouTubeEmbedDescriptor::new("dQw4w9WgXcQ", &origin).unwrap();
        assert_eq!(descriptor.src().host_str(), Some("www.youtube.com"));
        assert_eq!(descriptor.video_id(), "dQw4w9WgXcQ");

        let origin_with_path = Url::parse("https://rill3.example/attacker").unwrap();
        assert!(YouTubeEmbedDescriptor::new("dQw4w9WgXcQ", &origin_with_path).is_err());
        assert!(YouTubeEmbedDescriptor::new("../../escape", &origin).is_err());

        for local in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(
                YouTubeEmbedDescriptor::new("dQw4w9WgXcQ", &Url::parse(local).unwrap()).is_ok()
            );
        }
        assert!(
            YouTubeEmbedDescriptor::new(
                "dQw4w9WgXcQ",
                &Url::parse("http://rill3.example").unwrap()
            )
            .is_err()
        );
    }
}
