//! Building blocks every service in this workspace shares.
//!
//! Anything that belongs to more than one service goes here, so a change lands
//! once and CI's affected-crate detection fans it out to every dependent.

pub mod health;
pub mod telemetry;

pub use health::{Probe, health_routes};
pub use telemetry::init_tracing;
