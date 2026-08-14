//! Contract tests for dlq-replay.
//!
//! This tool publishes nothing to a sibling in normal operation, so "provider
//! and consumer" reads oddly for it. It still has a real contract, and it sits
//! at both ends of the same one:
//!
//! - **Consumes** dead letters — which worker-service writes as
//!   `Envelope<OrderCommand>`, not a bespoke shape.
//! - **Provides** command envelopes back onto `orders.command.created`, where
//!   the worker will pick them up as though they were new.
//!
//! The interesting obligation is the deliberate *asymmetry*: unlike the relay,
//! this tool must **not** forward the original envelope untouched. It mints a
//! fresh one, so a replayed message is distinguishable from the failure that
//! produced it.

use messaging_core::{Envelope, OrderCommand, subjects};

/// A dead letter exactly as worker-service writes it.
const DEAD_LETTER: &str = r#"{
    "id": "3d8f1c07-9a4b-4e2d-8f61-7b0c5e9a2d13",
    "kind": "order.dead-letter",
    "schema_version": 1,
    "occurred_at": "2026-08-14T10:20:00Z",
    "data": { "order_id": "11111111-2222-3333-4444-555555555555", "item": "poison", "quantity": 1 }
}"#;

// ---------------------------------------------------------------------------
// Consumer side
// ---------------------------------------------------------------------------

#[test]
fn a_dead_letter_is_read_as_a_command_envelope() {
    // The tool's whole purpose depends on this: if the worker wrote a bespoke
    // shape instead of the original command, nothing could replay it.
    let envelope: Envelope<OrderCommand> =
        serde_json::from_str(DEAD_LETTER).expect("the worker's dead-letter shape must deserialise");

    assert_eq!(envelope.kind, "order.dead-letter");
    assert_eq!(envelope.data.item, "poison");
}

#[test]
fn unknown_fields_are_tolerated() {
    let from_the_future = r#"{
        "id": "3d8f1c07-9a4b-4e2d-8f61-7b0c5e9a2d13",
        "kind": "order.dead-letter",
        "schema_version": 1,
        "occurred_at": "2026-08-14T10:20:00Z",
        "data": {
            "order_id": "11111111-2222-3333-4444-555555555555",
            "item": "poison",
            "quantity": 1,
            "failure_reason": "refused"
        },
        "attempts": 3
    }"#;

    let envelope: Envelope<OrderCommand> = serde_json::from_str(from_the_future)
        .expect("a dead letter from a newer worker must still be replayable");

    assert_eq!(envelope.data.quantity, 1);
}

/// A dead letter nobody can decode must stay put.
///
/// The tool leaves undecodable messages in the queue rather than acking them:
/// a message that cannot be read is exactly the kind that needs a human, and
/// acking it would destroy the only copy.
#[test]
fn an_undecodable_dead_letter_is_recognised_as_such() {
    let corrupt =
        r#"{ "id": "3d8f1c07-9a4b-4e2d-8f61-7b0c5e9a2d13", "kind": "order.dead-letter" }"#;

    assert!(
        serde_json::from_str::<Envelope<OrderCommand>>(corrupt).is_err(),
        "an undecodable dead letter must be detectable so it can be left in place"
    );
}

// ---------------------------------------------------------------------------
// Provider side
// ---------------------------------------------------------------------------

/// A replayed message is a *new* envelope carrying the original payload.
///
/// The opposite rule to outbox-relay, and deliberately so. The relay forwards
/// untouched because it is a conduit; this tool re-submits, and a fresh id
/// keeps the replay distinguishable from the original failure in logs and
/// traces. The payload is what must survive, not the envelope.
#[test]
fn a_replayed_message_carries_the_original_payload_under_a_new_envelope() {
    let dead: Envelope<OrderCommand> = serde_json::from_str(DEAD_LETTER).expect("must deserialise");

    // Exactly what the tool builds before publishing.
    let replayed = Envelope::new("order.created", dead.data.clone());

    assert_eq!(
        replayed.data, dead.data,
        "the order itself must survive the replay unchanged"
    );
    assert_ne!(
        replayed.id, dead.id,
        "a fresh id keeps the replay distinguishable from the failure"
    );
    assert_eq!(
        replayed.kind, "order.created",
        "it is republished as a command, not as another dead letter"
    );
}

/// The replayed message deserialises as a command the worker will accept.
#[test]
fn a_replayed_message_round_trips_as_a_command() {
    let dead: Envelope<OrderCommand> = serde_json::from_str(DEAD_LETTER).expect("must deserialise");

    let replayed = Envelope::new("order.created", dead.data);
    let bytes = serde_json::to_vec(&replayed).expect("must serialise");

    let decoded: Envelope<OrderCommand> =
        serde_json::from_slice(&bytes).expect("the worker must be able to read this");

    assert_eq!(decoded, replayed);
    assert!(decoded.is_supported());
}

/// The subjects and stream this tool works across.
#[test]
fn the_subjects_match_the_shared_constants() {
    assert_eq!(subjects::ORDER_DEAD_LETTER, "orders.dlq");
    assert_eq!(subjects::STREAM_DEAD_LETTER, "ORDER_DLQ");
    assert_eq!(subjects::ORDER_COMMAND_CREATED, "orders.command.created");
    // Its own durable name, so draining never competes with the worker.
    assert_eq!(subjects::CONSUMER_DLQ_REPLAY, "order-dlq-replay");
    assert_ne!(subjects::CONSUMER_DLQ_REPLAY, subjects::CONSUMER_WORKER);
}

/// Replaying is safe because the worker deduplicates on the business key.
///
/// Two replays of one dead letter produce two envelopes with different ids but
/// the *same* `order_id` — and the worker dedupes on `order_id`, so the work
/// happens once. That is what makes an accidental double-replay harmless.
#[test]
fn two_replays_of_one_dead_letter_share_a_business_key() {
    let dead: Envelope<OrderCommand> = serde_json::from_str(DEAD_LETTER).expect("must deserialise");

    let first = Envelope::new("order.created", dead.data.clone());
    let second = Envelope::new("order.created", dead.data);

    assert_ne!(first.id, second.id);
    assert_eq!(
        first.data.order_id, second.data.order_id,
        "the worker dedupes on order_id, which is what makes a double replay safe"
    );
}
