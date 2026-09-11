#![doc = include_str!("../README.md")]

/// Milestone in which interactive payments become an active capability.
pub const TARGET_MILESTONE: &str = "M3";

/// Interactive payments are intentionally unavailable during M0/M1.
#[must_use]
pub const fn is_enabled() -> bool {
    false
}
