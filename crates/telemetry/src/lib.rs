#![doc = include_str!("../README.md")]

use std::{env, error::Error};

use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Error returned when the filter is invalid or another global subscriber is installed.
pub type InitError = Box<dyn Error + Send + Sync + 'static>;

/// Settings applied to the process-wide tracing subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// Stable process name, such as `rill3-server` or `rill3-worker`.
    pub service_name: String,
    /// Deployment environment included in the initialization event.
    pub environment: String,
    /// [`EnvFilter`] directive string.
    pub filter: String,
}

impl TelemetryConfig {
    /// Builds settings from `RILL3_ENVIRONMENT` and `RUST_LOG`.
    #[must_use]
    pub fn from_env(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            environment: env::var("RILL3_ENVIRONMENT").unwrap_or_else(|_| "development".to_owned()),
            filter: env::var("RUST_LOG").unwrap_or_else(|_| "info,rill3=debug".to_owned()),
        }
    }
}

/// Installs a JSON subscriber using environment-derived settings.
///
/// This function must be called at most once per process.
///
/// # Errors
///
/// Returns an error when `RUST_LOG` is invalid or a global subscriber has
/// already been installed.
pub fn init(service_name: &str) -> Result<(), InitError> {
    init_with_config(&TelemetryConfig::from_env(service_name))
}

/// Installs a JSON subscriber using explicit settings.
///
/// Keeping explicit configuration available makes startup behavior testable without
/// coupling command implementations directly to environment access.
///
/// # Errors
///
/// Returns an error when `config.filter` is invalid or a global subscriber has
/// already been installed.
pub fn init_with_config(config: &TelemetryConfig) -> Result<(), InitError> {
    let filter = EnvFilter::try_new(&config.filter)?;
    let formatter = tracing_subscriber::fmt::layer()
        .json()
        .with_target(true)
        .with_current_span(true)
        .with_span_list(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(formatter)
        .try_init()?;

    tracing::info!(
        service.name = %config.service_name,
        deployment.environment = %config.environment,
        "telemetry initialized"
    );

    Ok(())
}
