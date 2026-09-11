use sqlx::{Postgres, Transaction};

use crate::{Database, DbError};

/// An acquired transaction-scoped `PostgreSQL` advisory lock.
///
/// Dropping this guard rolls its otherwise-empty transaction back and releases
/// the lock. Calling [`Self::release`] commits it explicitly.
pub struct AdvisoryLockGuard {
    transaction: Transaction<'static, Postgres>,
}

impl AdvisoryLockGuard {
    pub(crate) const fn new(transaction: Transaction<'static, Postgres>) -> Self {
        Self { transaction }
    }

    /// Confirms that the transaction carrying the advisory lock is still alive.
    ///
    /// A database restart or connection loss ends the transaction and releases
    /// its lock server-side. Workers must stop when this probe fails so a new
    /// process cannot run alongside a lockless predecessor.
    ///
    /// # Errors
    ///
    /// Returns an error if the dedicated lock connection is no longer usable.
    pub async fn ensure_held(&mut self) -> Result<(), DbError> {
        sqlx::query_scalar!(r#"SELECT 1 AS "ready!""#)
            .fetch_one(&mut *self.transaction)
            .await?;
        Ok(())
    }

    /// Releases the advisory lock immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if `PostgreSQL` cannot finish the guard transaction.
    pub async fn release(self) -> Result<(), DbError> {
        self.transaction.commit().await?;
        Ok(())
    }
}

impl Database {
    /// Tries to acquire a transaction-scoped advisory lock without waiting.
    ///
    /// The transaction is held by the returned guard, ensuring that a pooled
    /// connection carrying a session lock is never accidentally reused.
    ///
    /// # Errors
    ///
    /// Returns an error when a connection cannot be acquired or `PostgreSQL`
    /// cannot evaluate the lock request.
    pub async fn try_advisory_lock(
        &self,
        lock_key: i64,
    ) -> Result<Option<AdvisoryLockGuard>, DbError> {
        let mut transaction = self.pool.begin().await?;
        let acquired = sqlx::query_scalar!(
            r#"SELECT pg_try_advisory_xact_lock($1) AS "acquired!""#,
            lock_key
        )
        .fetch_one(&mut *transaction)
        .await?;

        if acquired {
            Ok(Some(AdvisoryLockGuard::new(transaction)))
        } else {
            transaction.rollback().await?;
            Ok(None)
        }
    }
}
