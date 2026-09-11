use std::time::Duration;

use rill3_domain::ProviderKind;
use thiserror::Error;

use crate::{BackoffPolicy, UnsafeUrlReason};

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{provider} provider is disabled: {reason}")]
    Disabled {
        provider: ProviderKind,
        reason: &'static str,
    },
    #[error("invalid {field} for {provider}: {reason}")]
    InvalidInput {
        provider: ProviderKind,
        field: &'static str,
        reason: &'static str,
    },
    #[error("unsafe external URL: {0}")]
    UnsafeUrl(#[from] UnsafeUrlReason),
    #[error("{provider} channel was not found")]
    NotFound { provider: ProviderKind },
    #[error("{provider} rate limit exceeded")]
    RateLimited {
        provider: ProviderKind,
        retry_after: Option<Duration>,
    },
    #[error("{provider} returned transient HTTP status {status}")]
    TransientHttp {
        provider: ProviderKind,
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("{provider} returned HTTP status {status}")]
    RejectedHttp { provider: ProviderKind, status: u16 },
    #[error("{provider} returned an invalid response: {reason}")]
    InvalidResponse {
        provider: ProviderKind,
        reason: &'static str,
    },
    #[error("{provider} request failed")]
    Transport {
        provider: ProviderKind,
        #[source]
        source: reqwest::Error,
    },
    #[error("invalid provider configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("provider state lock is poisoned")]
    StateLockPoisoned,
}

impl ProviderError {
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::TransientHttp { .. } | Self::Transport { .. }
        )
    }

    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } | Self::TransientHttp { retry_after, .. } => {
                *retry_after
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn backoff_delay(&self, policy: BackoffPolicy, attempt: u32) -> Option<Duration> {
        self.is_transient()
            .then(|| policy.delay(attempt, self.retry_after()))
    }

    /// Returns a jittered retry delay for transient failures.
    ///
    /// The caller owns the random sample so production can use its normal random
    /// source while tests remain deterministic. Provider-supplied retry hints are
    /// preserved as a lower bound by [`BackoffPolicy::delay_with_jitter`].
    #[must_use]
    pub fn backoff_delay_with_jitter(
        &self,
        policy: BackoffPolicy,
        attempt: u32,
        sample: u64,
    ) -> Option<Duration> {
        self.is_transient()
            .then(|| policy.delay_with_jitter(attempt, self.retry_after(), sample))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittered_provider_backoff_preserves_long_retry_after() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(1), Duration::from_secs(30), 20).unwrap();
        let error = ProviderError::RateLimited {
            provider: ProviderKind::Twitch,
            retry_after: Some(Duration::from_mins(5)),
        };

        assert_eq!(
            error.backoff_delay_with_jitter(policy, 0, 0),
            Some(Duration::from_mins(5))
        );
    }

    #[test]
    fn permanent_failure_has_no_provider_retry_delay() {
        let policy = BackoffPolicy::default();
        let error = ProviderError::RejectedHttp {
            provider: ProviderKind::Twitch,
            status: 401,
        };

        assert_eq!(error.backoff_delay_with_jitter(policy, 0, 5_000), None);
    }
}
