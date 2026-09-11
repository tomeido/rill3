use std::{fmt, time::Duration};

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::DbError;

/// Maximum number of connections RILL3 will allocate to one application pool.
pub const MAX_POOL_CONNECTIONS: u32 = 20;

/// `PostgreSQL` connection settings with a mandatory pool bound.
#[derive(Clone)]
pub struct DatabaseOptions {
    database_url: String,
    requested_max_connections: u32,
    acquire_timeout: Duration,
}

impl DatabaseOptions {
    /// Builds database options. Values above 20 are safely capped when connecting.
    pub fn new(database_url: impl Into<String>, requested_max_connections: u32) -> Self {
        Self {
            database_url: database_url.into(),
            requested_max_connections,
            acquire_timeout: Duration::from_secs(5),
        }
    }

    /// Overrides how long callers wait for a pooled connection.
    #[must_use]
    pub const fn with_acquire_timeout(mut self, acquire_timeout: Duration) -> Self {
        self.acquire_timeout = acquire_timeout;
        self
    }

    pub(crate) fn max_connections(&self) -> Result<u32, DbError> {
        if self.requested_max_connections == 0 {
            return Err(DbError::InvalidPoolSize);
        }

        Ok(self.requested_max_connections.min(MAX_POOL_CONNECTIONS))
    }
}

impl fmt::Debug for DatabaseOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DatabaseOptions")
            .field("database_url", &"<redacted>")
            .field("requested_max_connections", &self.requested_max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .finish()
    }
}

pub(crate) async fn connect(options: &DatabaseOptions) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(options.max_connections()?)
        .acquire_timeout(options.acquire_timeout)
        .connect(&options.database_url)
        .await?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::{DatabaseOptions, MAX_POOL_CONNECTIONS};

    #[test]
    fn pool_size_is_hard_capped() {
        let options = DatabaseOptions::new("postgres://unused", u32::MAX);

        assert_eq!(options.max_connections().unwrap(), MAX_POOL_CONNECTIONS);
    }

    #[test]
    fn zero_pool_size_is_rejected() {
        let options = DatabaseOptions::new("postgres://unused", 0);

        assert!(options.max_connections().is_err());
    }

    #[test]
    fn debug_output_redacts_database_url() {
        let options = DatabaseOptions::new("postgres://user:secret@example/db", 5);
        let debug = format!("{options:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }
}
