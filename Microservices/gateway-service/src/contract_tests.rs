//! Contract tests for gateway-service.
//!
//! This service sits on two contracts at once:
//!
//! - **Inbound, over Hypertext Transfer Protocol.** It consumes `POST /order`
//!   bodies from clients.
//! - **Messaging, outbound.** It writes an `Envelope<OrderCommand>` into the
//!   outbox, which `outbox-relay` later publishes verbatim.
//!
//! The outbox one is the sharper of the two, because the message is *stored*
//! before it is sent. A shape that no longer matches `messaging-core` is not
//! caught at publish time — it is caught by a consumer, minutes later, on a
//! message that has already been committed and cannot be un-sent.
//!
//! See `worker-service/src/contract_tests.rs` for why unknown fields must be
//! tolerated on every consumer side.

use crate::orders::PlaceOrder;
use messaging_core::envelope::SCHEMA_VERSION;
use messaging_core::{Envelope, OrderCommand, subjects};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Provider side — what this service stores and publishes
// ---------------------------------------------------------------------------

/// The outbox payload must deserialise into the shared type.
///
/// `serde_json::to_value` is what the real code stores, because the column is
/// `JSONB` — so this exercises the same conversion the gateway performs.
#[test]
fn the_outbox_payload_round_trips_through_the_contract() {
    let order_id = Uuid::new_v4();
    let envelope = Envelope::new(
        "order.created",
        OrderCommand {
            order_id,
            item: "widget".to_owned(),
            quantity: 2,
        },
    );

    let stored = serde_json::to_value(&envelope)
        .expect("the outbox column takes JavaScript Object Notation");
    let decoded: Envelope<OrderCommand> =
        serde_json::from_value(stored).expect("the relay must be able to read this back");

    assert_eq!(decoded, envelope);
    assert_eq!(decoded.data.order_id, order_id);
    assert_eq!(decoded.schema_version, SCHEMA_VERSION);
}

/// The stored JavaScript Object Notation uses the key names every downstream
/// consumer looks for.
#[test]
fn the_stored_command_uses_the_agreed_field_names() {
    let envelope = Envelope::new(
        "order.created",
        OrderCommand {
            order_id: Uuid::nil(),
            item: "widget".to_owned(),
            quantity: 1,
        },
    );

    let json = serde_json::to_value(&envelope).expect("must serialise");

    assert!(json.get("id").is_some());
    assert!(json.get("kind").is_some());
    assert!(json.get("occurred_at").is_some());
    assert_eq!(json["schema_version"], SCHEMA_VERSION);
    assert!(json["data"].get("order_id").is_some());
    assert!(json["data"].get("item").is_some());
    assert!(json["data"].get("quantity").is_some());
}

/// Trace context is carried inside the envelope, not only in broker headers.
///
/// This is what lets a trace survive the outbox: the row sits in Postgres for
/// however long, and the relay restores the originating context when it
/// publishes. Headers would be long gone.
#[test]
fn the_envelope_carries_a_trace_field_the_relay_can_restore() {
    let envelope = Envelope::new(
        "order.created",
        OrderCommand {
            order_id: Uuid::nil(),
            item: "widget".to_owned(),
            quantity: 1,
        },
    );

    // With no tracing subscriber installed the map is empty, and an empty map
    // is skipped in the JavaScript Object Notation. What matters for the
    // contract is that the field exists as a concept and survives a round trip
    // when populated.
    let mut populated = envelope.clone();
    populated.trace.insert(
        "traceparent".to_owned(),
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned(),
    );

    let json = serde_json::to_value(&populated).expect("must serialise");
    assert!(
        json["trace"].get("traceparent").is_some(),
        "the relay reads trace context back out of this field"
    );

    let decoded: Envelope<OrderCommand> = serde_json::from_value(json).expect("must deserialise");
    assert_eq!(decoded.trace.len(), 1);
}

/// The subject written into the outbox row is the one the relay publishes to.
#[test]
fn the_outbox_subject_matches_the_shared_constant() {
    assert_eq!(subjects::ORDER_COMMAND_CREATED, "orders.command.created");
}

// ---------------------------------------------------------------------------
// Consumer side — the Hypertext Transfer Protocol body this service accepts
// ---------------------------------------------------------------------------

#[test]
fn a_minimal_order_request_is_accepted() {
    // order_id is optional; the gateway generates one when it is absent.
    let body = r#"{ "item": "widget", "quantity": 2 }"#;

    let request: PlaceOrder = serde_json::from_str(body).expect("the documented shape must parse");

    assert_eq!(request.item, "widget");
    assert_eq!(request.quantity, 2);
    assert!(
        request.order_id.is_none(),
        "an absent order_id must mean 'generate one', not fail"
    );
}

#[test]
fn a_caller_supplied_order_id_is_honoured() {
    // Supplying the id is how a caller makes a retry idempotent.
    let body = r#"{
        "item": "widget",
        "quantity": 1,
        "order_id": "11111111-2222-3333-4444-555555555555"
    }"#;

    let request: PlaceOrder = serde_json::from_str(body).expect("must parse");

    assert_eq!(
        request.order_id.expect("the id was supplied").to_string(),
        "11111111-2222-3333-4444-555555555555"
    );
}

#[test]
fn unknown_fields_in_a_request_are_tolerated() {
    // An older gateway must not reject a newer client's request outright.
    let body = r#"{
        "item": "widget",
        "quantity": 1,
        "gift_wrap": true,
        "coupon": "SUMMER"
    }"#;

    let request: PlaceOrder =
        serde_json::from_str(body).expect("unknown fields must be ignored, not rejected");

    assert_eq!(request.item, "widget");
}

#[test]
fn a_request_missing_a_required_field_is_rejected() {
    // The bound on tolerance: extra is fine, absent is not.
    let body = r#"{ "item": "widget" }"#;

    assert!(
        serde_json::from_str::<PlaceOrder>(body).is_err(),
        "a missing quantity must not silently default to zero"
    );
}

#[test]
fn a_request_with_the_wrong_type_is_rejected() {
    let body = r#"{ "item": "widget", "quantity": "two" }"#;

    assert!(
        serde_json::from_str::<PlaceOrder>(body).is_err(),
        "quantity is a number; a string must not be coerced"
    );
}
