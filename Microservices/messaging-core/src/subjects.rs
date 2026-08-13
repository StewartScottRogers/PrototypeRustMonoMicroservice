//! The names every service agrees on.
//!
//! NATS subjects are dotted strings, and `>` is a wildcard matching one or more
//! trailing tokens. Keeping them in one file means a typo is a compile error in
//! the crate that owns the name, rather than a message that silently goes
//! nowhere — which is the classic way to lose an afternoon with a broker.

/// Commands: "do this". One worker handles each.
pub const ORDER_COMMAND_CREATED: &str = "orders.command.created";

/// Events: "this happened". Every subscriber gets a copy.
pub const ORDER_EVENT_COMPLETED: &str = "orders.event.completed";

/// Messages that failed every delivery attempt end up here.
pub const ORDER_DEAD_LETTER: &str = "orders.dlq";

/// Stream holding commands. `orders.command.>` captures this subject and any
/// future ones, so adding `orders.command.cancelled` needs no stream change.
pub const STREAM_COMMANDS: &str = "ORDER_COMMANDS";
/// Subject filter for [`STREAM_COMMANDS`].
pub const STREAM_COMMANDS_SUBJECTS: &str = "orders.command.>";

/// Stream holding events.
pub const STREAM_EVENTS: &str = "ORDER_EVENTS";
/// Subject filter for [`STREAM_EVENTS`].
pub const STREAM_EVENTS_SUBJECTS: &str = "orders.event.>";

/// Stream holding dead letters, kept separate so it can have its own retention
/// and so a human can drain it without touching live traffic.
pub const STREAM_DEAD_LETTER: &str = "ORDER_DLQ";
/// Subject filter for [`STREAM_DEAD_LETTER`].
pub const STREAM_DEAD_LETTER_SUBJECTS: &str = "orders.dlq";

/// The one durable consumer every worker process shares. Sharing the name is
/// what makes this *messaging*: JetStream gives each message to exactly one of
/// them.
pub const CONSUMER_WORKER: &str = "order-worker";

/// One durable consumer per subscriber. Distinct names are what make this
/// *eventing*: each consumer has its own position in the stream, so both see
/// every message.
pub const CONSUMER_NOTIFIER: &str = "order-notifier";
/// See [`CONSUMER_NOTIFIER`].
pub const CONSUMER_AUDIT: &str = "order-audit";
