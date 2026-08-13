//! Process-wide tracing setup.
//!
//! "Tracing" here means structured logging: instead of printing a sentence, a
//! service emits a record with named fields, which a log system can index and
//! query. The `tracing` crate is the de facto standard for this in Rust.

// A *prelude* is a module of traits a crate expects you to import wholesale.
// The `.with(...)` calls below are trait methods, and Rust only lets you call a
// trait method when that trait is in scope — hence this import.
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Installs a JSON tracing subscriber for `service`.
///
/// A *subscriber* is the thing that decides what happens to log records. Until
/// one is installed, `tracing::info!` and friends are silently discarded.
///
/// Level comes from the `RUST_LOG` environment variable, defaulting to `info`.
/// JSON output is what makes the logs queryable once the service runs in a
/// container.
///
/// Call this once, from `main`. A second call panics inside `tracing`, because
/// the global subscriber can only be set one time.
///
/// The `&'static str` parameter means the name must live for the whole program
/// — a string literal such as `"echo-service"` does.
pub fn init_tracing(service: &'static str) {
    // `try_from_default_env()` reads `RUST_LOG` and returns a `Result`.
    // `.unwrap_or_else(|_| ...)` supplies a fallback when it fails; the `|_|`
    // closure ignores the error value. Unlike `.unwrap_or(...)`, the fallback
    // is only built if it is actually needed.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // This is the *builder pattern*: each call returns a modified value, so the
    // steps chain together and the final `.init()` consumes the whole thing.
    tracing_subscriber::registry()
        .with(filter)
        // `.json()` switches the output format; `.flatten_event(true)` lifts
        // the record's fields to the top level of the JSON object instead of
        // nesting them under "fields".
        .with(fmt::layer().json().flatten_event(true))
        .init();

    // `tracing::info!` is a *macro* (the `!` gives it away). Macros run at
    // compile time and can do things functions cannot — here, capturing
    // `service` as a named field rather than pasting it into a message string.
    tracing::info!(service, "tracing initialised");
}
