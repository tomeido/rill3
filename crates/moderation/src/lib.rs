#![doc = include_str!("../README.md")]

/// Milestone in which content moderation becomes an active capability.
pub const TARGET_MILESTONE: &str = "M4";

/// Donation-content moderation is intentionally unavailable during M0/M1.
#[must_use]
pub const fn is_enabled() -> bool {
    false
}
