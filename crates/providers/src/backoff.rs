use std::time::Duration;

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackoffPolicy {
    initial: Duration,
    maximum: Duration,
    jitter_percent: u8,
}

impl BackoffPolicy {
    pub fn new(
        initial: Duration,
        maximum: Duration,
        jitter_percent: u8,
    ) -> Result<Self, BackoffPolicyError> {
        if initial.is_zero() || maximum < initial {
            return Err(BackoffPolicyError::InvalidBounds);
        }
        if jitter_percent > 100 {
            return Err(BackoffPolicyError::InvalidJitter);
        }
        Ok(Self {
            initial,
            maximum,
            jitter_percent,
        })
    }

    #[must_use]
    pub fn delay(self, attempt: u32, server_hint: Option<Duration>) -> Duration {
        let exponential = self.exponential_delay(attempt);
        server_hint.map_or(exponential, |hint| hint.max(exponential))
    }

    /// Adds deterministic symmetric jitter to the policy-controlled exponential delay.
    ///
    /// `sample` is folded into `0..=10_000`, making this usable with any
    /// caller-owned random source and easy to test. A provider-supplied delay is
    /// always treated as a lower bound: jitter can never schedule a retry before
    /// `Retry-After` or an equivalent rate-limit reset hint.
    #[must_use]
    pub fn delay_with_jitter(
        self,
        attempt: u32,
        server_hint: Option<Duration>,
        sample: u64,
    ) -> Duration {
        let base = self.exponential_delay(attempt);
        if self.jitter_percent == 0 {
            return server_hint.map_or(base, |hint| hint.max(base));
        }

        let spread_millis = base
            .as_millis()
            .saturating_mul(u128::from(self.jitter_percent))
            / 100;
        let position = u128::from(sample % 10_001);
        let range = spread_millis.saturating_mul(2);
        let offset = range.saturating_mul(position) / 10_000;
        let base_millis = base.as_millis();
        let jittered = base_millis
            .saturating_sub(spread_millis)
            .saturating_add(offset)
            .min(self.maximum.as_millis());
        let jittered = Duration::from_millis(u64::try_from(jittered).unwrap_or(u64::MAX));
        server_hint.map_or(jittered, |hint| hint.max(jittered))
    }

    fn exponential_delay(self, attempt: u32) -> Duration {
        let multiplier = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.initial.saturating_mul(multiplier).min(self.maximum)
    }
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            maximum: Duration::from_mins(1),
            jitter_percent: 20,
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BackoffPolicyError {
    #[error("initial delay must be non-zero and no greater than maximum")]
    InvalidBounds,
    #[error("jitter percentage must be at most 100")]
    InvalidJitter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_delay_is_bounded_and_honors_server_hint() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(1), Duration::from_secs(30), 20).unwrap();
        assert_eq!(policy.delay(0, None), Duration::from_secs(1));
        assert_eq!(policy.delay(4, None), Duration::from_secs(16));
        assert_eq!(policy.delay(20, None), Duration::from_secs(30));
        assert_eq!(
            policy.delay(1, Some(Duration::from_secs(10))),
            Duration::from_secs(10)
        );
        assert_eq!(
            policy.delay(20, Some(Duration::from_mins(5))),
            Duration::from_mins(5),
            "the local exponential ceiling must not truncate Retry-After"
        );
    }

    #[test]
    fn jitter_stays_inside_configured_window() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(10), Duration::from_mins(1), 20).unwrap();
        assert_eq!(policy.delay_with_jitter(0, None, 0), Duration::from_secs(8));
        assert_eq!(
            policy.delay_with_jitter(0, None, 10_000),
            Duration::from_secs(12)
        );
    }

    #[test]
    fn jitter_never_schedules_before_server_hint() {
        let policy =
            BackoffPolicy::new(Duration::from_secs(10), Duration::from_secs(30), 20).unwrap();

        assert_eq!(
            policy.delay_with_jitter(0, Some(Duration::from_secs(10)), 0),
            Duration::from_secs(10)
        );
        assert_eq!(
            policy.delay_with_jitter(0, Some(Duration::from_secs(10)), 10_000),
            Duration::from_secs(12)
        );
        assert_eq!(
            policy.delay_with_jitter(0, Some(Duration::from_mins(5)), 10_000),
            Duration::from_mins(5),
            "a server hint larger than the policy ceiling must remain intact"
        );
    }
}
