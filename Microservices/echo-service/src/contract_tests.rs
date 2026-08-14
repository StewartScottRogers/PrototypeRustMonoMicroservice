//! Contract tests for echo-service.
//!
//! This service has a **Hypertext Transfer Protocol** contract rather than a
//! messaging one — it publishes nothing and consumes nothing from NATS. Its
//! neighbour is gateway-service, which calls `POST /echo` synchronously.
//!
//! The obligations are the same shape as everywhere else, just over Hypertext
//! Transfer Protocol: prove the response body matches what the caller parses,
//! and prove the request body survives anything contract-shaped, including
//! fields this build has never seen.

use crate::{EchoRequest, EchoResponse};

// ---------------------------------------------------------------------------
// Consumer side — the request body this service accepts
// ---------------------------------------------------------------------------

#[test]
fn a_contract_shaped_request_is_accepted() {
    let body = r#"{ "message": "hello" }"#;

    let request: EchoRequest = serde_json::from_str(body).expect("the gateway's shape must parse");

    assert_eq!(request.message, "hello");
}

#[test]
fn unknown_fields_in_a_request_are_tolerated() {
    // The gateway may be upgraded first and start sending more than this build
    // knows about. Rejecting that would make an additive change an outage.
    let body = r#"{ "message": "hello", "locale": "en-GB", "attempt": 2 }"#;

    let request: EchoRequest =
        serde_json::from_str(body).expect("unknown fields must be ignored, not rejected");

    assert_eq!(request.message, "hello");
}

#[test]
fn a_request_missing_the_message_is_rejected() {
    // The bound on tolerance: extra is fine, absent is not. Defaulting to an
    // empty string would echo silence and look like success.
    assert!(serde_json::from_str::<EchoRequest>(r#"{}"#).is_err());
    assert!(serde_json::from_str::<EchoRequest>(r#"{ "msg": "hello" }"#).is_err());
}

#[test]
fn a_request_with_the_wrong_type_is_rejected() {
    assert!(
        serde_json::from_str::<EchoRequest>(r#"{ "message": 42 }"#).is_err(),
        "message is a string; a number must not be coerced"
    );
}

// ---------------------------------------------------------------------------
// Provider side — the response body this service emits
// ---------------------------------------------------------------------------

/// The response uses the field names the gateway reads.
///
/// The gateway deserialises only `echo` and ignores the rest, so that field
/// name is the load-bearing part of this contract. Renaming it would give the
/// gateway a 502 with no obvious cause.
#[test]
fn the_response_uses_the_field_names_the_gateway_reads() {
    let response = EchoResponse {
        echo: "hello".to_owned(),
        service: "echo-service",
    };

    let json = serde_json::to_value(&response).expect("must serialise");

    assert_eq!(json["echo"], "hello");
    assert!(
        json.get("service").is_some(),
        "service names the responder, which makes a misrouted call obvious"
    );
}

/// The exact wire shape, pinned.
///
/// A golden string catches key renames that a round trip through our own types
/// would not, because both sides would rename together.
#[test]
fn the_response_serialises_to_the_agreed_json() {
    let response = EchoResponse {
        echo: "hello".to_owned(),
        service: "echo-service",
    };

    let json = serde_json::to_string(&response).expect("must serialise");

    assert_eq!(json, r#"{"echo":"hello","service":"echo-service"}"#);
}

/// What the gateway does with our response must keep working.
///
/// The gateway declares only the field it needs and lets serde ignore the
/// rest, so adding a field here is safe — this proves it, and would fail if
/// `echo` were ever renamed or removed.
#[test]
fn the_gateway_can_still_read_the_response_when_we_add_a_field() {
    // Stands in for the gateway's own `UpstreamResponse`, declared here rather
    // than imported because this service must not depend on a sibling.
    #[derive(serde::Deserialize)]
    struct AsTheGatewaySeesIt {
        echo: String,
    }

    let with_a_new_field = r#"{ "echo": "hello", "service": "echo-service", "latency_ms": 3 }"#;

    let seen: AsTheGatewaySeesIt = serde_json::from_str(with_a_new_field)
        .expect("an additive change must not break the caller");

    assert_eq!(seen.echo, "hello");
}
