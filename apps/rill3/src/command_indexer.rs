use anyhow::{Result, ensure};

use crate::config::IndexerArgs;

pub(crate) fn run(arguments: &IndexerArgs) -> Result<()> {
    arguments.common.validated_pool_size()?;
    ensure!(
        !arguments.common.database_url.trim().is_empty(),
        "DATABASE_URL cannot be empty"
    );
    tracing::info!(
        milestone = "M3",
        "chain indexer boundary is intentionally inactive in M0/M1"
    );
    Ok(())
}
