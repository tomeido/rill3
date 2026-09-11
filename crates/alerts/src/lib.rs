#![doc = include_str!("../README.md")]

/// Milestone in which OBS alerts become an active capability.
pub const TARGET_MILESTONE: &str = "M4";

/// OBS alerts are intentionally unavailable during M0/M1.
#[must_use]
pub const fn is_enabled() -> bool {
    false
}
