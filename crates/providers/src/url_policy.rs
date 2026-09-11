use std::net::{Ipv4Addr, Ipv6Addr};

use thiserror::Error;
use url::{Host, Url};

/// Validates a stored link. RILL3 still never fetches this URL server-side.
pub fn validate_external_https_url(raw: &str) -> Result<Url, UnsafeUrlReason> {
    let url = Url::parse(raw).map_err(|_| UnsafeUrlReason::Malformed)?;
    if url.scheme() != "https" {
        return Err(UnsafeUrlReason::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnsafeUrlReason::Credentials);
    }
    let host = url.host().ok_or(UnsafeUrlReason::MissingHost)?;
    match host {
        Host::Domain(domain) => validate_domain(domain)?,
        Host::Ipv4(address) => {
            if !is_public_ipv4(address) {
                return Err(UnsafeUrlReason::NonPublicHost);
            }
        }
        Host::Ipv6(address) => {
            if !is_public_ipv6(address) {
                return Err(UnsafeUrlReason::NonPublicHost);
            }
        }
    }
    Ok(url)
}

fn validate_domain(domain: &str) -> Result<(), UnsafeUrlReason> {
    let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || has_dns_suffix(&normalized, "local")
        || has_dns_suffix(&normalized, "internal")
        || normalized.is_empty()
    {
        return Err(UnsafeUrlReason::NonPublicHost);
    }
    Ok(())
}

fn has_dns_suffix(domain: &str, suffix: &str) -> bool {
    domain
        .rsplit_once('.')
        .is_some_and(|(_, final_label)| final_label == suffix)
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    if address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
    {
        return false;
    }
    address.to_ipv4_mapped().is_none_or(is_public_ipv4)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum UnsafeUrlReason {
    #[error("URL is malformed")]
    Malformed,
    #[error("only HTTPS links are accepted")]
    Scheme,
    #[error("URL must not contain credentials")]
    Credentials,
    #[error("URL has no host")]
    MissingHost,
    #[error("localhost, private, link-local, reserved, and unspecified hosts are rejected")]
    NonPublicHost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_active_and_local_url_schemes() {
        for raw in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "http://example.com",
            "https://127.0.0.1/live",
            "https://2130706433/live",
            "https://0x7f000001/live",
            "https://10.2.3.4/live",
            "https://[::1]/live",
            "https://[::ffff:127.0.0.1]/live",
            "https://localhost/live",
            "https://metadata.internal/live",
        ] {
            assert!(validate_external_https_url(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn accepts_public_https_without_fetching_it() {
        let url = validate_external_https_url("https://video.example/watch/42").unwrap();
        assert_eq!(url.host_str(), Some("video.example"));
    }
}
