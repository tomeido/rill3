use rill3_domain::ProviderKind;
use rill3_providers::{ChzzkCredentials, TwitchCredentials};

/// The same credential boundary drives server labels and worker availability.
/// Values and constructor errors are deliberately never included in diagnostics.
pub(crate) enum CredentialState<T> {
    Ready(T),
    Disabled(&'static str),
}

impl<T> CredentialState<T> {
    pub(crate) const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    pub(crate) const fn disabled_reason(&self) -> Option<&'static str> {
        match self {
            Self::Ready(_) => None,
            Self::Disabled(reason) => Some(reason),
        }
    }

    pub(crate) fn into_credentials(self) -> Option<T> {
        match self {
            Self::Ready(credentials) => Some(credentials),
            Self::Disabled(_) => None,
        }
    }
}

pub(crate) struct ProviderCredentials {
    pub(crate) twitch: CredentialState<TwitchCredentials>,
    pub(crate) chzzk: CredentialState<ChzzkCredentials>,
}

impl ProviderCredentials {
    pub(crate) fn new(
        twitch_id: Option<&str>,
        twitch_secret: Option<&str>,
        eventsub_secret: Option<&str>,
        chzzk_id: Option<&str>,
        chzzk_secret: Option<&str>,
    ) -> Self {
        let twitch = match credential_problem(&[twitch_id, twitch_secret, eventsub_secret]) {
            Some(reason) => CredentialState::Disabled(reason),
            None => match (twitch_id, twitch_secret, eventsub_secret) {
                (Some(id), Some(secret), Some(eventsub)) => {
                    TwitchCredentials::new(id, secret, eventsub).map_or_else(
                        |_| {
                            CredentialState::Disabled(
                                "Twitch EventSub secret must be 10..=100 ASCII characters",
                            )
                        },
                        CredentialState::Ready,
                    )
                }
                _ => CredentialState::Disabled("credential set is incomplete"),
            },
        };
        let chzzk = match credential_problem(&[chzzk_id, chzzk_secret]) {
            Some(reason) => CredentialState::Disabled(reason),
            None => match (chzzk_id, chzzk_secret) {
                (Some(id), Some(secret)) => ChzzkCredentials::new(id, secret).map_or_else(
                    |_| CredentialState::Disabled("CHZZK credentials are invalid"),
                    CredentialState::Ready,
                ),
                _ => CredentialState::Disabled("credential set is incomplete"),
            },
        };
        Self { twitch, chzzk }
    }

    pub(crate) fn log_disabled(&self) {
        for (provider, reason) in [
            (ProviderKind::Twitch, self.twitch.disabled_reason()),
            (ProviderKind::Chzzk, self.chzzk.disabled_reason()),
        ] {
            if let Some(reason) = reason {
                tracing::warn!(%provider, reason, "provider disabled; other providers remain available");
            }
        }
    }
}

fn credential_problem(values: &[Option<&str>]) -> Option<&'static str> {
    if values.iter().all(|value| value.is_none_or(str::is_empty)) {
        return Some("credentials are not configured");
    }
    if values.iter().any(|value| value.is_none_or(str::is_empty)) {
        return Some("credential set is incomplete");
    }
    if values
        .iter()
        .flatten()
        .any(|value| !value.bytes().all(|byte| byte.is_ascii_graphic()))
    {
        return Some("credentials must be ASCII without whitespace or control characters");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_partial_credential_permutation_only_disables_its_provider() {
        for twitch_mask in 0..8 {
            for chzzk_mask in 0..4 {
                let credentials = ProviderCredentials::new(
                    (twitch_mask & 1 != 0).then_some("twitch-id"),
                    (twitch_mask & 2 != 0).then_some("twitch-secret"),
                    (twitch_mask & 4 != 0).then_some("eventsub-secret"),
                    (chzzk_mask & 1 != 0).then_some("chzzk-id"),
                    (chzzk_mask & 2 != 0).then_some("chzzk-secret"),
                );
                assert_eq!(credentials.twitch.is_ready(), twitch_mask == 7);
                assert_eq!(credentials.chzzk.is_ready(), chzzk_mask == 3);
                assert_eq!(
                    credentials.twitch.disabled_reason().is_none(),
                    twitch_mask == 7
                );
                assert_eq!(
                    credentials.chzzk.disabled_reason().is_none(),
                    chzzk_mask == 3
                );
            }
        }
    }

    #[test]
    fn malformed_values_do_not_disable_the_other_provider() {
        for malformed in ["", " ", " id", "secret\n", "value\r\nHeader:x", "한글"] {
            for invalid_index in 0..5 {
                let mut values = [
                    "twitch-id",
                    "twitch-secret",
                    "eventsub-secret",
                    "chzzk-id",
                    "chzzk-secret",
                ];
                values[invalid_index] = malformed;
                let credentials = ProviderCredentials::new(
                    Some(values[0]),
                    Some(values[1]),
                    Some(values[2]),
                    Some(values[3]),
                    Some(values[4]),
                );
                assert_eq!(credentials.twitch.is_ready(), invalid_index >= 3);
                assert_eq!(credentials.chzzk.is_ready(), invalid_index < 3);
            }
        }
    }

    #[test]
    fn eventsub_secret_length_is_checked_before_readiness() {
        for secret in ["short".to_owned(), "x".repeat(101)] {
            let credentials = ProviderCredentials::new(
                Some("twitch-id"),
                Some("twitch-secret"),
                Some(&secret),
                Some("chzzk-id"),
                Some("chzzk-secret"),
            );
            assert!(!credentials.twitch.is_ready());
            assert!(credentials.chzzk.is_ready());
            assert!(credentials.twitch.into_credentials().is_none());
            assert!(credentials.chzzk.into_credentials().is_some());
        }
    }
}
