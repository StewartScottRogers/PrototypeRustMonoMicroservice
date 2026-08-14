//! Contract tests for notifier-service.
//!
//! This service is a pure consumer: it reads `orders.event.completed` and
//! reacts. It publishes nothing, so there is no provider side — the only
//! contract it owns is "what shapes must I survive".
//!
//! That makes unknown-field tolerance the central test rather than a footnote.
//! The worker will gain fields before this service is redeployed, and during
//! that window every event carries something unrecognised.

use messaging_core::{Envelope, OrderCompleted, subjects};

/// An event in exactly the shape worker-service publishes.
///
/// Deliberately raw JavaScript Object Notation rather than built from our own
/// types: a fixture made with `Envelope::new` would keep passing even if the
/// type drifted away from the contract, because both sides would drift
/// together.
const EVENT_FIXTURE: &str = r#"{
    "id": "9c2e7b41-5d3a-4f8e-b1c6-2a7d9e0f3b58",
    "kind": "order.completed",
    "schema_version": 1,
    "occurred_at": "2026-08-14T10:15:04Z",
    "trace": { "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01" },
    "data": {
        "order_id": "11111111-2222-3333-4444-555555555555",
        "item": "widget",
        "quantity": 4,
        "processed_by": "worker-abc123"
    }
}"#;

#[test]
fn a_contract_shaped_event_is_accepted() {
    let envelope: Envelope<OrderCompleted> =
        serde_json::from_str(EVENT_FIXTURE).expect("the worker's shape must deserialise");

    assert_eq!(envelope.kind, "order.completed");
    assert_eq!(envelope.data.item, "widget");
    assert_eq!(envelope.data.processed_by, "worker-abc123");
    assert!(envelope.is_supported());
}

/// The test that keeps an additive contract change from becoming an outage.
#[test]
fn unknown_fields_are_tolerated() {
    let from_the_future = r#"{
        "id": "9c2e7b41-5d3a-4f8e-b1c6-2a7d9e0f3b58",
        "kind": "order.completed",
        "schema_version": 1,
        "occurred_at": "2026-08-14T10:15:04Z",
        "data": {
            "order_id": "11111111-2222-3333-4444-555555555555",
            "item": "widget",
            "quantity": 4,
            "processed_by": "worker-abc123",
            "warehouse": "eu-west-1",
            "picked_at": "2026-08-14T10:15:03Z"
        },
        "correlation_id": "abc-123"
    }"#;

    let envelope: Envelope<OrderCompleted> = serde_json::from_str(from_the_future)
        .expect("a producer upgraded ahead of this service must not break it");

    assert_eq!(envelope.data.processed_by, "worker-abc123");
}

#[test]
fn a_message_without_a_schema_version_is_read_as_version_one() {
    let legacy = r#"{
        "id": "9c2e7b41-5d3a-4f8e-b1c6-2a7d9e0f3b58",
        "kind": "order.completed",
        "occurred_at": "2026-08-14T10:15:04Z",
        "data": {
            "order_id": "11111111-2222-3333-4444-555555555555",
            "item": "widget",
            "quantity": 1,
            "processed_by": "worker-1"
        }
    }"#;

    let envelope: Envelope<OrderCompleted> =
        serde_json::from_str(legacy).expect("pre-versioning events must still decode");

    assert_eq!(envelope.schema_version, 1);
}

/// An unsupported version decodes but is recognised as unreadable.
///
/// A subscriber has no dead-letter queue of its own — a missed event is a fact
/// it did not learn, not work left undone — so it logs loudly and moves on.
/// Decoding must still succeed far enough to make that decision.
#[test]
fn a_future_schema_version_is_flagged_unsupported() {
    let newer = EVENT_FIXTURE.replace("\"schema_version\": 1", "\"schema_version\": 42");

    let envelope: Envelope<OrderCompleted> =
        serde_json::from_str(&newer).expect("it must decode far enough to be recognised");

    assert!(!envelope.is_supported());
}

#[test]
fn an_event_missing_a_required_field_is_rejected() {
    let broken = r#"{
        "id": "9c2e7b41-5d3a-4f8e-b1c6-2a7d9e0f3b58",
        "kind": "order.completed",
        "occurred_at": "2026-08-14T10:15:04Z",
        "data": { "order_id": "11111111-2222-3333-4444-555555555555", "item": "widget", "quantity": 1 }
    }"#;

    assert!(
        serde_json::from_str::<Envelope<OrderCompleted>>(broken).is_err(),
        "processed_by is required; defaulting it would invent an attribution"
    );
}

/// This service's durable consumer name must differ from the auditor's.
///
/// Sharing a name would turn eventing into messaging: the two subscribers
/// would split events between them instead of each receiving every one, and
/// roughly half the notifications would silently stop being sent.
#[test]
fn the_consumer_name_is_distinct_from_the_other_subscriber() {
    assert_eq!(subjects::CONSUMER_NOTIFIER, "order-notifier");
    assert_ne!(
        subjects::CONSUMER_NOTIFIER,
        subjects::CONSUMER_AUDIT,
        "separate names are what make both subscribers see every event"
    );
    assert_eq!(subjects::ORDER_EVENT_COMPLETED, "orders.event.completed");
}
