//! Process-wide tracing setup.

use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Installs a JSON tracing subscriber for `service`.
///
/// Level comes from `RUST_LOG`, defaulting to `info`. JSON output is what makes
/// the logs queryable once the service runs in a container.
///
/// Call this once, from `main`; a second call panics inside `tracing`.
pub fn init_tracing(service: &'static str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().flatten_event(true))
        .init();

    tracing::info!(service, "tracing initialised");
}
