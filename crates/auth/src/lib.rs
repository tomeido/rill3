#![doc = include_str!("../README.md")]

/// Milestone in which authentication becomes an active product capability.
pub const TARGET_MILESTONE: &str = "M2";

/// Authentication is intentionally unavailable during M0/M1.
#[must_use]
pub const fn is_enabled() -> bool {
    false
}
