//! Structured, secret-safe process telemetry.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

/// Installs the process-wide JSON tracing subscriber.
///
/// # Errors
///
/// Returns an error when the filter is invalid or another subscriber is active.
pub fn init(filter: &str) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter)?;
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()?;
    Ok(())
}
