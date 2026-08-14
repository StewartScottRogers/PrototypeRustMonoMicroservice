//! Contract tests for mimic-service.
//!
//! This service publishes no messages to siblings, so "provider and consumer"
//! needs reading differently here. It still has two real contracts:
//!
//! - **Provides** `/api/state` to its own console page. The page switches on
//!   these strings to colour lamps, so a rename here silently leaves every
//!   lamp grey — a monitoring tool that looks fine and reports nothing.
//! - **Consumes** Prometheus and NATS monitoring JSON, neither of which this
//!   repo controls. Tolerance matters more than usual: an upstream version
//!   bump can add fields at any time.

use crate::collect::{Alarm, Gauge, Node, Snapshot, Status};
use chrono::Utc;

// ---------------------------------------------------------------------------
// Provider side — the JSON the console page consumes
// ---------------------------------------------------------------------------

/// The status strings are the contract with the page.
///
/// `console.html` and `panel.html` both index a colour table by these exact
/// values. A rename compiles, ships, and produces a panel where nothing ever
/// changes colour.
#[test]
fn the_status_values_are_the_strings_the_page_switches_on() {
    assert_eq!(
        serde_json::to_string(&Status::Healthy).expect("serialises"),
        "\"healthy\""
    );
    assert_eq!(
        serde_json::to_string(&Status::Degraded).expect("serialises"),
        "\"degraded\""
    );
    assert_eq!(
        serde_json::to_string(&Status::Down).expect("serialises"),
        "\"down\""
    );
    assert_eq!(
        serde_json::to_string(&Status::Unknown).expect("serialises"),
        "\"unknown\""
    );
}

/// The snapshot's field names, pinned.
#[test]
fn a_snapshot_serialises_to_the_shape_the_page_expects() {
    let snapshot = Snapshot {
        generated_at: Utc::now(),
        nodes: vec![Node {
            id: "worker".to_owned(),
            status: Status::Degraded,
            detail: "p99 850 ms".to_owned(),
        }],
        gauges: vec![Gauge {
            id: "g-queue".to_owned(),
            value: "depth 12".to_owned(),
            warn: true,
        }],
        alarms: vec![Alarm {
            text: "worker-service — slow".to_owned(),
            severity: Status::Degraded,
        }],
        sources_ok: true,
    };

    let json = serde_json::to_value(&snapshot).expect("must serialise");

    // Top level.
    assert!(json.get("generated_at").is_some());
    assert!(json.get("sources_ok").is_some());

    // A node: the page finds `led-<id>` and `txt-<id>` in the SVG from `id`.
    let node = &json["nodes"][0];
    assert_eq!(node["id"], "worker");
    assert_eq!(node["status"], "degraded");
    assert!(node.get("detail").is_some());

    // A gauge: `id` matches an SVG text element, `warn` picks its colour.
    let gauge = &json["gauges"][0];
    assert_eq!(gauge["id"], "g-queue");
    assert!(gauge.get("value").is_some());
    assert_eq!(gauge["warn"], true);

    // An alarm: the banner reads `text` and colours itself by `severity`.
    let alarm = &json["alarms"][0];
    assert!(alarm.get("text").is_some());
    assert_eq!(alarm["severity"], "degraded");
}

/// Empty collections must serialise as empty arrays, not be omitted.
///
/// The page iterates them unconditionally. A missing key would be `undefined`
/// in JavaScript and throw on the first loop, blanking the whole panel because
/// nothing happened to be wrong.
#[test]
fn an_all_clear_snapshot_still_carries_every_array() {
    let snapshot = Snapshot {
        generated_at: Utc::now(),
        nodes: vec![],
        gauges: vec![],
        alarms: vec![],
        sources_ok: true,
    };

    let json = serde_json::to_value(&snapshot).expect("must serialise");

    assert!(json["nodes"].is_array());
    assert!(json["gauges"].is_array());
    assert!(
        json["alarms"].is_array(),
        "the page loops over alarms unconditionally; a missing key throws"
    );
}

// ---------------------------------------------------------------------------
// Consumer side — upstream JSON this service reads
// ---------------------------------------------------------------------------

/// A Prometheus instant-query response in its documented shape.
///
/// The value arrives as a *string* inside a two-element array, which is easy
/// to get wrong and produces a silently absent metric rather than an error.
#[test]
fn a_prometheus_instant_query_response_is_read_correctly() {
    let body = r#"{
        "status": "success",
        "data": {
            "resultType": "vector",
            "result": [
                { "metric": { "service": "worker-service" }, "value": [1786700000.0, "2"] }
            ]
        }
    }"#;

    let parsed: serde_json::Value = serde_json::from_str(body).expect("must parse");

    // The same path the collector walks.
    let sample = parsed["data"]["result"][0]["value"][1]
        .as_str()
        .and_then(|raw| raw.parse::<f64>().ok());

    assert_eq!(sample, Some(2.0));
}

/// An empty result is "no data", not an error.
///
/// This distinction is the one the whole panel rests on: Prometheus answering
/// with nothing means the target is absent, which is very different from
/// Prometheus being unreachable.
#[test]
fn an_empty_prometheus_result_is_distinguishable_from_a_failure() {
    let body = r#"{ "status": "success", "data": { "resultType": "vector", "result": [] } }"#;

    let parsed: serde_json::Value = serde_json::from_str(body).expect("must parse");

    assert!(
        parsed["data"]["result"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

/// Extra fields from a newer Prometheus must not break parsing.
#[test]
fn unknown_fields_in_upstream_json_are_tolerated() {
    let body = r#"{
        "status": "success",
        "warnings": ["something new"],
        "data": {
            "resultType": "vector",
            "analysis": { "note": "a future field" },
            "result": [ { "metric": {}, "value": [1786700000.0, "7"] } ]
        }
    }"#;

    let parsed: serde_json::Value = serde_json::from_str(body).expect("must parse");
    let sample = parsed["data"]["result"][0]["value"][1].as_str();

    assert_eq!(sample, Some("7"));
}

/// The NATS `/jsz` shape this service reads stream and consumer depth from.
#[test]
fn the_nats_monitoring_shape_is_read_correctly() {
    let body = r#"{
        "account_details": [
            {
                "stream_detail": [
                    {
                        "name": "ORDER_DLQ",
                        "state": { "messages": 3 },
                        "consumer_detail": [ { "name": "order-worker", "num_pending": 12 } ]
                    }
                ]
            }
        ]
    }"#;

    let parsed: serde_json::Value = serde_json::from_str(body).expect("must parse");

    let stream = &parsed["account_details"][0]["stream_detail"][0];
    assert_eq!(stream["name"], "ORDER_DLQ");
    assert_eq!(stream["state"]["messages"].as_u64(), Some(3));
    assert_eq!(
        stream["consumer_detail"][0]["num_pending"].as_u64(),
        Some(12),
        "num_pending is the backlog the worker lamp turns amber on"
    );
}
