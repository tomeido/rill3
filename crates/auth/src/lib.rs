#![doc = include_str!("../README.md")]

use std::{fmt, time::Duration};

use reqwest::{Client, RequestBuilder, redirect::Policy};
use rill3_domain::ProviderKind;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

/// Server-side credentials with a deployment-configured, exact redirect URI.
#[derive(Clone, Debug)]
pub struct OAuthProviderConfig {
    pub provider: ProviderKind,
    pub client_id: String,
    pub client_secret: SecretString,
    pub redirect_uri: Url,
}

/// Immutable channel identities proven by an official authenticated provider API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedIdentity {
    pub provider: ProviderKind,
    pub provider_channel_ids: Vec<String>,
}

/// Sanitized authentication failures; never includes credentials or remote bodies.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("OAuth is not configured for this provider")]
    NotConfigured,
    #[error("invalid OAuth configuration")]
    InvalidConfiguration,
    #[error("OAuth provider request failed")]
    ProviderRequest,
    #[error("OAuth provider returned invalid identity information")]
    InvalidIdentity,
    #[error("invalid OAuth callback input")]
    InvalidCallback,
}

#[derive(Clone)]
pub struct OAuthClient {
    client: Client,
    providers: Vec<OAuthProviderConfig>,
    endpoints: Endpoints,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthClient")
            .field(
                "providers",
                &self
                    .providers
                    .iter()
                    .map(|p| p.provider)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct Endpoints {
    twitch_token: String,
    twitch_identity: String,
    youtube_token: String,
    youtube_identity: String,
    chzzk_token: String,
    chzzk_identity: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            twitch_token: "https://id.twitch.tv/oauth2/token".to_owned(),
            twitch_identity: "https://id.twitch.tv/oauth2/validate".to_owned(),
            youtube_token: "https://oauth2.googleapis.com/token".to_owned(),
            youtube_identity: "https://www.googleapis.com/youtube/v3/channels".to_owned(),
            chzzk_token: "https://openapi.chzzk.naver.com/auth/v1/token".to_owned(),
            chzzk_identity: "https://openapi.chzzk.naver.com/open/v1/users/me".to_owned(),
        }
    }
}

impl OAuthClient {
    /// Builds an OAuth client with fixed official endpoints and strict network bounds.
    ///
    /// # Errors
    /// Rejects unsupported/duplicate providers, empty credentials or unsafe redirect URIs.
    pub fn new(providers: Vec<OAuthProviderConfig>) -> Result<Self, AuthError> {
        for (index, provider) in providers.iter().enumerate() {
            if provider.provider == ProviderKind::LinkOnly
                || provider.client_id.trim().is_empty()
                || provider.client_secret.expose_secret().trim().is_empty()
                || providers[..index]
                    .iter()
                    .any(|p| p.provider == provider.provider)
                || !safe_redirect(&provider.redirect_uri)
            {
                return Err(AuthError::InvalidConfiguration);
            }
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(10))
            .redirect(Policy::none())
            .user_agent("rill3-owner-verification/0.1")
            .build()
            .map_err(|_| AuthError::InvalidConfiguration)?;
        Ok(Self {
            client,
            providers,
            endpoints: Endpoints::default(),
        })
    }

    /// Whether this deployment has enabled a provider's ownership flow.
    pub fn is_configured(&self, provider: ProviderKind) -> bool {
        self.providers.iter().any(|p| p.provider == provider)
    }

    /// Returns the official provider consent URL bound to a caller-generated state.
    ///
    /// # Errors
    /// Rejects unknown providers and missing or oversized state values.
    pub fn authorization_url(&self, provider: ProviderKind, state: &str) -> Result<Url, AuthError> {
        validate_callback_value(state)?;
        let config = self.config(provider)?;
        let endpoint = match provider {
            ProviderKind::Twitch => "https://id.twitch.tv/oauth2/authorize",
            ProviderKind::YouTube => "https://accounts.google.com/o/oauth2/v2/auth",
            ProviderKind::Chzzk => "https://chzzk.naver.com/account-interlock",
            ProviderKind::LinkOnly => return Err(AuthError::NotConfigured),
        };
        let mut url = Url::parse(endpoint).map_err(|_| AuthError::InvalidConfiguration)?;
        {
            let mut query = url.query_pairs_mut();
            if provider == ProviderKind::Chzzk {
                query
                    .append_pair("clientId", &config.client_id)
                    .append_pair("redirectUri", config.redirect_uri.as_str())
                    .append_pair("state", state);
            } else {
                query
                    .append_pair("client_id", &config.client_id)
                    .append_pair("redirect_uri", config.redirect_uri.as_str())
                    .append_pair("response_type", "code")
                    .append_pair("state", state);
                if provider == ProviderKind::YouTube {
                    query
                        .append_pair("scope", "https://www.googleapis.com/auth/youtube.readonly")
                        .append_pair("access_type", "online")
                        .append_pair("prompt", "select_account");
                } else {
                    // Token validation identifies the user without reading their email.
                    query
                        .append_pair("scope", "")
                        .append_pair("force_verify", "true");
                }
            }
        }
        Ok(url)
    }

    /// Exchanges a single-use code and proves channel ownership through the provider.
    /// The caller must atomically consume browser-bound state before invoking this.
    /// Access/refresh tokens remain transient and are never returned or persisted.
    ///
    /// # Errors
    /// Rejects failed exchanges, malformed responses, app-only Twitch tokens, and empty channels.
    pub async fn verify_code(
        &self,
        provider: ProviderKind,
        code: &str,
        state: &str,
    ) -> Result<VerifiedIdentity, AuthError> {
        validate_callback_value(code)?;
        validate_callback_value(state)?;
        let config = self.config(provider)?;
        let provider_channel_ids = match provider {
            ProviderKind::Twitch => self.verify_twitch(config, code).await?,
            ProviderKind::YouTube => self.verify_youtube(config, code).await?,
            ProviderKind::Chzzk => self.verify_chzzk(config, code, state).await?,
            ProviderKind::LinkOnly => return Err(AuthError::NotConfigured),
        };
        if provider_channel_ids.is_empty()
            || provider_channel_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 255)
        {
            return Err(AuthError::InvalidIdentity);
        }
        Ok(VerifiedIdentity {
            provider,
            provider_channel_ids,
        })
    }

    fn config(&self, provider: ProviderKind) -> Result<&OAuthProviderConfig, AuthError> {
        self.providers
            .iter()
            .find(|p| p.provider == provider)
            .ok_or(AuthError::NotConfigured)
    }

    async fn token(
        &self,
        endpoint: &str,
        config: &OAuthProviderConfig,
        code: &str,
    ) -> Result<SecretString, AuthError> {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &config.client_id)
            .append_pair("client_secret", config.client_secret.expose_secret())
            .append_pair("code", code)
            .append_pair("grant_type", "authorization_code")
            .append_pair("redirect_uri", config.redirect_uri.as_str())
            .finish();
        let token: TokenResponse = bounded_json(
            self.client
                .post(endpoint)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body),
        )
        .await?;
        if token.access_token.is_empty() || !token.token_type.eq_ignore_ascii_case("bearer") {
            return Err(AuthError::InvalidIdentity);
        }
        Ok(token.access_token.into())
    }

    async fn verify_twitch(
        &self,
        config: &OAuthProviderConfig,
        code: &str,
    ) -> Result<Vec<String>, AuthError> {
        let token = self
            .token(&self.endpoints.twitch_token, config, code)
            .await?;
        let identity: TwitchIdentity = bounded_json(
            self.client
                .get(&self.endpoints.twitch_identity)
                .bearer_auth(token.expose_secret()),
        )
        .await?;
        if identity.client_id != config.client_id || identity.expires_in == 0 {
            return Err(AuthError::InvalidIdentity);
        }
        let user_id = identity
            .user_id
            .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            .ok_or(AuthError::InvalidIdentity)?;
        Ok(vec![user_id])
    }

    async fn verify_youtube(
        &self,
        config: &OAuthProviderConfig,
        code: &str,
    ) -> Result<Vec<String>, AuthError> {
        let token = self
            .token(&self.endpoints.youtube_token, config, code)
            .await?;
        let identity: YouTubeIdentity = bounded_json(
            self.client
                .get(&self.endpoints.youtube_identity)
                .bearer_auth(token.expose_secret())
                .query(&[("part", "id"), ("mine", "true"), ("maxResults", "50")]),
        )
        .await?;
        Ok(identity
            .items
            .into_iter()
            .map(|channel| channel.id)
            .collect())
    }

    async fn verify_chzzk(
        &self,
        config: &OAuthProviderConfig,
        code: &str,
        state: &str,
    ) -> Result<Vec<String>, AuthError> {
        let token: ChzzkResponse<ChzzkToken> = bounded_json(self.client.post(&self.endpoints.chzzk_token)
            .json(&serde_json::json!({
                "grantType": "authorization_code", "clientId": config.client_id,
                "clientSecret": config.client_secret.expose_secret(), "code": code, "state": state,
            }))).await?;
        if token.code != 200
            || token.content.access_token.is_empty()
            || !token.content.token_type.eq_ignore_ascii_case("bearer")
        {
            return Err(AuthError::InvalidIdentity);
        }
        let secret = SecretString::from(token.content.access_token);
        let identity: ChzzkResponse<ChzzkIdentity> = bounded_json(
            self.client
                .get(&self.endpoints.chzzk_identity)
                .bearer_auth(secret.expose_secret()),
        )
        .await?;
        if identity.code != 200 {
            return Err(AuthError::InvalidIdentity);
        }
        Ok(vec![identity.content.channel_id])
    }
}

/// Generates a 256-bit opaque token from more than 256 bits of OS-backed UUID randomness.
/// Only its SHA-256 hash should be stored in a database.
pub fn random_token() -> String {
    let mut hasher = Sha256::new();
    for _ in 0..3 {
        hasher.update(Uuid::new_v4().as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Hashes a bearer token before persistence; the raw token stays in an `HttpOnly` cookie.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn safe_redirect(uri: &Url) -> bool {
    let loopback = matches!(uri.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    (uri.scheme() == "https" || uri.scheme() == "http" && loopback)
        && uri.host_str().is_some()
        && uri.username().is_empty()
        && uri.password().is_none()
        && uri.query().is_none()
        && uri.fragment().is_none()
}

fn validate_callback_value(value: &str) -> Result<(), AuthError> {
    if value.is_empty() || value.len() > 2048 || value.chars().any(char::is_control) {
        Err(AuthError::InvalidCallback)
    } else {
        Ok(())
    }
}

async fn bounded_json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, AuthError> {
    let mut response = request
        .send()
        .await
        .map_err(|_| AuthError::ProviderRequest)?;
    if !response.status().is_success() {
        return Err(AuthError::ProviderRequest);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AuthError::ProviderRequest)?
    {
        if body.len().saturating_add(chunk.len()) > 65_536 {
            return Err(AuthError::InvalidIdentity);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| AuthError::InvalidIdentity)
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
}
#[derive(serde::Deserialize)]
struct TwitchIdentity {
    client_id: String,
    user_id: Option<String>,
    expires_in: u64,
}
#[derive(serde::Deserialize)]
struct YouTubeIdentity {
    items: Vec<YouTubeChannel>,
}
#[derive(serde::Deserialize)]
struct YouTubeChannel {
    id: String,
}
#[derive(serde::Deserialize)]
struct ChzzkResponse<T> {
    code: u16,
    content: T,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChzzkToken {
    access_token: String,
    token_type: String,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChzzkIdentity {
    channel_id: String,
}

#[cfg(test)]
mod tests;
