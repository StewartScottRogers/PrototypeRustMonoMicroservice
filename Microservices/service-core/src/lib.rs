//! Building blocks every service in this workspace shares.
//!
//! # Reading this crate as a Rust newcomer
//!
//! A **crate** is Rust's unit of compilation — roughly "one library or one
//! program". This file, `lib.rs`, is the root of a *library* crate: it produces
//! no executable, it is linked into other crates that depend on it.
//!
//! Comments starting with `//!` (like these) document the *thing they are
//! inside* — here, the whole crate. Comments starting with `///` document the
//! *item that follows them*. Both are picked up by `cargo doc`, which builds a
//! browsable HTML manual:
//!
//! ```text
//! cargo doc --open
//! ```
//!
//! Anything that belongs to more than one service goes here, so a change lands
//! once and CI's affected-crate detection fans it out to every dependent.

// `pub mod` declares a public **module** and tells the compiler to look for it
// in a sibling file of the same name (`config.rs`, `health.rs`, ...). Without
// these lines those files are not part of the crate at all — Rust never scans
// the directory for stray files, everything is declared explicitly.
pub mod config;
pub mod health;
pub mod metrics;
pub mod telemetry;

// `pub use` re-exports an item under a shorter path. Callers can write
// `service_core::health_routes(...)` instead of
// `service_core::health::health_routes(...)`. The long path keeps working too.
//
// Note the crate is named `service-core` with a hyphen in Cargo.toml, but Rust
// identifiers cannot contain hyphens, so in code it becomes `service_core`.
pub use config::port_from_env;
pub use health::{Probe, health_routes, metrics_routes, self_check};
pub use metrics::init_metrics;
pub use telemetry::init_tracing;
