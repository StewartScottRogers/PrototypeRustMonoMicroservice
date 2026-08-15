//! The names every service agrees on.
//!
//! NATS subjects are dotted strings, and `>` is a wildcard matching one or
//! more trailing tokens. Keeping them in one file means a typo is a compile
//! error in the crate that owns the name, rather than a message that silently
//! goes nowhere — which is the classic way to lose an afternoon with a broker.

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

/// Used by the `dlq-replay` tool to drain the dead-letter stream. Durable so a
/// half-finished drain resumes where it stopped rather than starting over.
pub const CONSUMER_DLQ_REPLAY: &str = "order-dlq-replay";

/// The consumer a *dry run* reads through, kept separate from the one above.
///
/// Looking must not disturb the position of the thing that acts. Sharing one
/// consumer forced the tool to delete it before every run — which threw away
/// the acknowledgements of every message already replayed, so each run
/// replayed the entire history again. On a queue of poison messages that
/// doubles it: 1,714 became 3,427 in a single run.
///
/// This one is disposable and is deleted at the start of each dry run. The
/// replay consumer is not, and its acknowledgement floor is what makes
/// "how many are still waiting" a question with an answer.
pub const CONSUMER_DLQ_INSPECT: &str = "order-dlq-inspect";
