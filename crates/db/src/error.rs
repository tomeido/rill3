use thiserror::Error;
use uuid::Uuid;

/// Errors raised by the `PostgreSQL` persistence boundary.
#[derive(Debug, Error)]
pub enum DbError {
    /// The configured pool size is not usable.
    #[error("database max_connections must be at least one")]
    InvalidPoolSize,
    /// Registration would change channel ownership or reactivate disabled data.
    #[error("channel registration rejected: {0}")]
    RegistrationConflict(&'static str),
    /// A persisted string could not be converted to a domain value.
    #[error("invalid persisted {field}: {value}")]
    InvalidPersistedValue {
        /// Name of the invalid database field.
        field: &'static str,
        /// Value read from the database.
        value: String,
    },
    /// A normalized state referred to a channel that does not exist.
    #[error("external channel does not exist: {0}")]
    ChannelNotFound(Uuid),
    /// The event/state identity does not match the locked channel row.
    #[error("provider event or live state does not match channel {0}")]
    ChannelIdentityMismatch(Uuid),
    /// A provider event did not contain a lower-case hexadecimal SHA-256 hash.
    #[error("provider event payload_hash must be 64 lower-case hexadecimal characters")]
    InvalidPayloadHash,
    /// An event ID was reused for different immutable event data.
    #[error("provider event idempotency conflict for {provider}/{external_event_id}")]
    IdempotencyConflict {
        /// Provider owning the event identifier.
        provider: String,
        /// Provider-scoped delivery/event identifier.
        external_event_id: String,
    },
    /// Domain state transition validation failed.
    #[error(transparent)]
    StateTransition(#[from] rill3_domain::StateTransitionError),
    /// A SQL query or connection failed.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// A schema migration failed.
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
}
