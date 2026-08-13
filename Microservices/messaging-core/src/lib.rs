//! Everything the services need to talk to each other asynchronously.
//!
//! # The two patterns this crate exists to demonstrate
//!
//! They are easy to confuse, and the difference is the whole point:
//!
//! **Messaging** is a *command*: "process this order." Exactly one worker
//! should do it. Adding a second worker splits the load — it does not double
//! the work. In JetStream this is one durable consumer shared by every worker
//! process; the server hands each message to one of them.
//!
//! **Eventing** is a *fact*: "this order completed." Anyone interested gets a
//! copy. Adding a subscriber adds a reaction — it does not split anything. In
//! JetStream this is one durable consumer *per subscriber*, each with its own
//! independent position in the stream.
//!
//! Same broker, same stream technology. The only difference is whether the
//! consumers share a name.
//!
//! ```text
//!   gateway ──orders.command.created──▶ [ORDER_COMMANDS] ──▶ worker  (shared consumer)
//!                                                              │
//!   worker  ──orders.event.completed──▶ [ORDER_EVENTS] ────────┤
//!                                            ├── consumer "notifier" ──▶ notifier
//!                                            └── consumer "audit"    ──▶ audit
//! ```
//!
//! # Reading this crate as a Rust newcomer
//!
//! This is a *library* crate, like `service-core`. The modules below are
//! declared with `pub mod`, which tells the compiler to look for a file of the
//! same name next to this one.

pub mod client;
pub mod contract;
pub mod envelope;
pub mod idempotency;
pub mod subjects;
pub mod trace;

pub use client::Messaging;
pub use contract::{OrderCommand, OrderCompleted};
pub use envelope::Envelope;
pub use idempotency::IdempotencyGuard;
