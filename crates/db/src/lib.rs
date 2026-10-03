//! `PostgreSQL` persistence for RILL3's normalized creator and live-state data.

mod discovery;
mod error;
mod lock;
mod pool;
mod records;
mod repository;
mod web3;

use sqlx::PgPool;

pub use error::DbError;
pub use lock::AdvisoryLockGuard;
pub use pool::{DatabaseOptions, MAX_POOL_CONNECTIONS};
pub use records::{CreatorListing, LiveListing};
pub use repository::MAX_QUERY_RESULTS;
pub use web3::{PublicRegistration, WalletChallenge, Web3Binding};

pub(crate) use records::{ChannelRow, CreatorRow, ListingRow};

/// Versioned discovery and ownership migrations embedded into the application binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Cloneable `PostgreSQL` repository handle.
#[derive(Clone, Debug)]
pub struct Database {
    pub(crate) pool: PgPool,
}

impl Database {
    /// Connects to `PostgreSQL` with a pool that can never exceed 20 connections.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero-sized pool or when `PostgreSQL` cannot be
    /// reached before the configured acquire timeout.
    pub async fn connect(options: &DatabaseOptions) -> Result<Self, DbError> {
        Ok(Self {
            pool: pool::connect(options).await?,
        })
    }

    /// Wraps an existing pool. Intended for dependency injection and tests.
    #[must_use]
    pub const fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the underlying pool for transaction composition at app edges.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies all embedded migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if a migration cannot be acquired or applied.
    pub async fn migrate(&self) -> Result<(), DbError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Checks `PostgreSQL` readiness without mutating application data.
    ///
    /// # Errors
    ///
    /// Returns an error if the pool cannot run a trivial query.
    pub async fn readiness(&self) -> Result<(), DbError> {
        sqlx::query_scalar!(r#"SELECT 1 AS "ready!""#)
            .fetch_one(&self.pool)
            .await?;
        Ok(())
    }
}
