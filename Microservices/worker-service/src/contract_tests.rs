//! Contract tests for worker-service.
//!
//! # What a contract test is for
//!
//! This service consumes `orders.command.created` and produces
//! `orders.event.completed`. Its neighbours are the outbox relay upstream and
//! two subscribers downstream — and this team never runs any of them.
//!
//! These tests are the stand-in. Green here means the shapes this service
//! reads and writes still match `messaging-core`, so it can ship without a
//! broker, a database, or a sibling service anywhere in sight.
//!
//! - **Provider tests** assert what we *emit* matches the contract.
//! - **Consumer tests** assert what we *accept* survives contract-shaped input,
//!   including fields we have never heard of.
//!
//! # Why unknown fields must be tolerated
//!
//! Contract changes are additive by default, and a producer is upgraded before
//! its consumers. For the window between those two deploys this service will
//! receive messages carrying fields it does not know about. If that is an
//! error, every additive change becomes a coordinated outage.
//!
//! # Reading this file as a Rust newcomer
//!
//! `#[cfg(test)]` on the `mod` declaration in `main.rs` means this file is
//! compiled only by `cargo test` — it is not in the shipped binary. Being a
//! module *inside* the crate rather than a file in `tests/` is what lets it
//! reach private items through `use crate::…`.

use messaging_core::envelope::SCHEMA_VERSION;
use messaging_core::{Envelope, OrderCommand, OrderCompleted, subjects};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Provider side — what this service emits
// ---------------------------------------------------------------------------

/// The completion event must round-trip through the envelope unchanged.
///
/// A round trip catches the failure that matters: a field renamed on one side
/// only. Serialising and deserialising through the shared type means the test
/// fails the moment our shape and the contract's disagree.
#[test]
fn the_completion_event_round_trips_through_the_contract() {
    let order_id = Uuid::new_v4();
    let completed = OrderCompleted {
        order_id,
        item: "widget".to_owned(),
        quantity: 3,
        processed_by: "worker-abc123".to_owned(),
    };

    let envelope = Envelope::new("order.completed", completed);

    let bytes = serde_json::to_vec(&envelope).expect("the event must serialise");
    let decoded: Envelope<OrderCompleted> =
        serde_json::from_slice(&bytes).expect("the event must deserialise");

    assert_eq!(decoded, envelope);
    assert_eq!(decoded.data.order_id, order_id);
    assert_eq!(decoded.data.processed_by, "worker-abc123");
}

/// The wire shape is pinned field by field.
///
/// The round-trip test above would still pass if *both* sides were renamed
/// together — which would silently break every other service. This asserts the
/// actual JSON keys a consumer will look for.
#[test]
fn the_completion_event_uses_the_field_names_subscribers_expect() {
    let envelope = Envelope::new(
        "order.completed",
        OrderCompleted {
            order_id: Uuid::nil(),
            item: "widget".to_owned(),
            quantity: 1,
            processed_by: "worker-1".to_owned(),
        },
    );

    let json = serde_json::to_value(&envelope).expect("must serialise");

    // Envelope metadata every consumer relies on.
    assert!(
        json.get("id").is_some(),
        "envelope id is part of the contract"
    );
    assert!(json.get("kind").is_some());
    assert!(json.get("occurred_at").is_some());
    assert_eq!(json["schema_version"], SCHEMA_VERSION);

    // The payload's own fields.
    let data = &json["data"];
    assert!(data.get("order_id").is_some());
    assert!(data.get("item").is_some());
    assert!(data.get("quantity").is_some());
    assert!(
        data.get("processed_by").is_some(),
        "processed_by is how the audit service records which worker ran it"
    );
}

/// Dead letters are emitted as the original command, not a bespoke shape.
///
/// `dlq-replay` deserialises them straight back into `Envelope<OrderCommand>`
/// to republish, so anything else here would strand them permanently.
#[test]
fn a_dead_letter_is_still_a_command_envelope() {
    let command = OrderCommand {
        order_id: Uuid::new_v4(),
        item: messaging_core::contract::POISON_ITEM.to_owned(),
        quantity: 1,
    };

    let envelope = Envelope::new("order.dead-letter", command.clone());
    let bytes = serde_json::to_vec(&envelope).expect("must serialise");

    let decoded: Envelope<OrderCommand> =
        serde_json::from_slice(&bytes).expect("the replay tool must be able to read this back");

    assert_eq!(decoded.data, command);
}

/// The subjects this service publishes to are the ones its neighbours listen on.
#[test]
fn the_published_subjects_match_the_shared_constants() {
    assert_eq!(subjects::ORDER_EVENT_COMPLETED, "orders.event.completed");
    assert_eq!(subjects::ORDER_DEAD_LETTER, "orders.dlq");
    // Sharing one durable name is what makes command handling *messaging*:
    // each command goes to exactly one worker rather than to all of them.
    assert_eq!(subjects::CONSUMER_WORKER, "order-worker");
}

// ---------------------------------------------------------------------------
// Consumer side — what this service accepts
// ---------------------------------------------------------------------------

/// A command in exactly the shape the relay publishes.
///
/// Written as a literal rather than built with `Envelope::new`, on purpose: a
/// fixture constructed from our own types would still pass if the type drifted
/// away from the contract. Raw JSON is the neighbour's voice, not ours.
const COMMAND_FIXTURE: &str = r#"{
    "id": "6f1a9d5e-2b7c-4e1f-9a3d-8c5b0e7f2a41",
    "kind": "order.created",
    "schema_version": 1,
    "occurred_at": "2026-08-14T10:15:00Z",
    "trace": { "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01" },
    "data": { "order_id": "11111111-2222-3333-4444-555555555555", "item": "widget", "quantity": 4 }
}"#;

#[test]
fn a_contract_shaped_command_is_accepted() {
    let envelope: Envelope<OrderCommand> =
        serde_json::from_str(COMMAND_FIXTURE).expect("the relay's shape must deserialise");

    assert_eq!(envelope.kind, "order.created");
    assert_eq!(envelope.data.item, "widget");
    assert_eq!(envelope.data.quantity, 4);
    assert!(envelope.is_supported());
    assert!(
        !envelope.trace.is_empty(),
        "trace context travels in the envelope so the trace survives the outbox"
    );
}

/// The one that stops an additive change becoming an outage.
#[test]
fn unknown_fields_are_tolerated() {
    // A future producer adds fields at both levels. This service must ignore
    // them, not reject the message.
    let from_the_future = r#"{
        "id": "6f1a9d5e-2b7c-4e1f-9a3d-8c5b0e7f2a41",
        "kind": "order.created",
        "schema_version": 1,
        "occurred_at": "2026-08-14T10:15:00Z",
        "data": {
            "order_id": "11111111-2222-3333-4444-555555555555",
            "item": "widget",
            "quantity": 4,
            "gift_wrap": true,
            "promised_by": "2026-09-01T00:00:00Z"
        },
        "priority": "high",
        "region": "eu-west"
    }"#;

    let envelope: Envelope<OrderCommand> = serde_json::from_str(from_the_future)
        .expect("unknown fields must be ignored, or every additive change is a coordinated outage");

    assert_eq!(envelope.data.quantity, 4);
}

/// Messages written before the envelope carried a version still decode.
#[test]
fn a_message_without_a_schema_version_is_read_as_version_one() {
    let legacy = r#"{
        "id": "6f1a9d5e-2b7c-4e1f-9a3d-8c5b0e7f2a41",
        "kind": "order.created",
        "occurred_at": "2026-08-14T10:15:00Z",
        "data": { "order_id": "11111111-2222-3333-4444-555555555555", "item": "widget", "quantity": 1 }
    }"#;

    let envelope: Envelope<OrderCommand> =
        serde_json::from_str(legacy).expect("pre-versioning messages must still decode");

    assert_eq!(envelope.schema_version, 1);
    assert!(envelope.is_supported());
}

/// A version this build does not understand is recognised as unsupported.
///
/// The worker dead-letters these rather than guessing. Processing a payload we
/// may be misreading is worse than parking it for a human.
#[test]
fn a_future_schema_version_is_flagged_unsupported() {
    let newer = COMMAND_FIXTURE.replace("\"schema_version\": 1", "\"schema_version\": 99");

    let envelope: Envelope<OrderCommand> =
        serde_json::from_str(&newer).expect("it must still decode so we can route it to the DLQ");

    assert!(
        !envelope.is_supported(),
        "an unknown schema version must be quarantined, never guessed at"
    );
}

/// A payload missing a required field is rejected outright.
///
/// The opposite bound to unknown-field tolerance: extra is fine, *absent* is
/// not. Silently defaulting a missing quantity would process the wrong order.
#[test]
fn a_command_missing_a_required_field_is_rejected() {
    let broken = r#"{
        "id": "6f1a9d5e-2b7c-4e1f-9a3d-8c5b0e7f2a41",
        "kind": "order.created",
        "occurred_at": "2026-08-14T10:15:00Z",
        "data": { "order_id": "11111111-2222-3333-4444-555555555555", "item": "widget" }
    }"#;

    let result = serde_json::from_str::<Envelope<OrderCommand>>(broken);
    assert!(
        result.is_err(),
        "a missing quantity must not silently default"
    );
}
