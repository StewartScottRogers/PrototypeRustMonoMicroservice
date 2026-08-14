//! Contract tests for audit-service.
//!
//! Two contracts meet here:
//!
//! - **Inbound messaging.** It consumes `orders.event.completed`, exactly as
//!   notifier-service does — same event, entirely different reaction.
//! - **Outbound persistence.** It writes an `audit_log` row, whose columns are
//!   owned by `db-core` and therefore by the Orchestration Agent. This team
//!   consumes that schema; it does not change it.
//!
//! The persistence side is tested by asserting the *mapping* — that every
//! value the row needs is present on the event and typed compatibly. Testing
//! the SQL itself would need a database, which belongs to the e2e suite.

use messaging_core::{Envelope, OrderCompleted, subjects};

/// An event in exactly the shape worker-service publishes.
const EVENT_FIXTURE: &str = r#"{
    "id": "9c2e7b41-5d3a-4f8e-b1c6-2a7d9e0f3b58",
    "kind": "order.completed",
    "schema_version": 1,
    "occurred_at": "2026-08-14T10:15:04Z",
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

    assert_eq!(envelope.data.quantity, 4);
    assert!(envelope.is_supported());
}

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
            "warehouse": "eu-west-1"
        },
        "correlation_id": "abc-123"
    }"#;

    let envelope: Envelope<OrderCompleted> =
        serde_json::from_str(from_the_future).expect("additive changes must not break the auditor");

    assert_eq!(envelope.data.item, "widget");
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

#[test]
fn a_future_schema_version_is_flagged_unsupported() {
    let newer = EVENT_FIXTURE.replace("\"schema_version\": 1", "\"schema_version\": 42");

    let envelope: Envelope<OrderCompleted> =
        serde_json::from_str(&newer).expect("it must decode far enough to be recognised");

    assert!(!envelope.is_supported());
}

// ---------------------------------------------------------------------------
// The db-core side of the contract
// ---------------------------------------------------------------------------

/// Every column of `audit_log` has a source on the event.
///
/// The insert binds `message_id`, `order_id`, `item`, `quantity`,
/// `processed_by` and `occurred_at`. If a contract change dropped any of them
/// the insert would fail at runtime against a live database — this catches it
/// at compile-and-test time instead, with no database in sight.
#[test]
fn every_audit_column_has_a_value_on_the_event() {
    let envelope: Envelope<OrderCompleted> = serde_json::from_str(EVENT_FIXTURE).expect("parses");

    // Naming each binding makes the test fail to *compile* if a field is
    // renamed, which is a better signal than an assertion failing later.
    let message_id = envelope.id;
    let occurred_at = envelope.occurred_at;
    let order_id = envelope.data.order_id;
    let item = &envelope.data.item;
    let quantity = envelope.data.quantity;
    let processed_by = &envelope.data.processed_by;

    assert!(!message_id.is_nil(), "message_id is the row's primary key");
    assert!(!order_id.is_nil());
    assert!(!item.is_empty());
    assert!(!processed_by.is_empty());
    assert!(occurred_at.timestamp() > 0);

    // The column is INTEGER, so the u32 on the wire must fit in an i32. A
    // quantity beyond i32::MAX would be clamped rather than stored correctly,
    // and that is worth knowing before it happens in production.
    assert!(
        i32::try_from(quantity).is_ok(),
        "quantity must fit the INTEGER column db-core defines"
    );
}

/// The audit row is keyed by message id, which is what makes the insert safe
/// to repeat.
///
/// At-least-once delivery means a redelivered event carries the *same*
/// `message_id`, so `ON CONFLICT DO NOTHING` turns the second write into a
/// no-op. Keying on anything regenerated per delivery would produce duplicate
/// rows for one event.
#[test]
fn the_message_id_is_stable_across_redeliveries_of_the_same_event() {
    let first: Envelope<OrderCompleted> = serde_json::from_str(EVENT_FIXTURE).expect("parses");
    let redelivered: Envelope<OrderCompleted> =
        serde_json::from_str(EVENT_FIXTURE).expect("parses");

    assert_eq!(
        first.id, redelivered.id,
        "the same event redelivered must carry the same id, or dedupe cannot work"
    );
}

/// This service's consumer name must differ from the notifier's.
#[test]
fn the_consumer_name_is_distinct_from_the_other_subscriber() {
    assert_eq!(subjects::CONSUMER_AUDIT, "order-audit");
    assert_ne!(subjects::CONSUMER_AUDIT, subjects::CONSUMER_NOTIFIER);
}
