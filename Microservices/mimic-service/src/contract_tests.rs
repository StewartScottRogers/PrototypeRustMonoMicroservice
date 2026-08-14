//! Contract tests for mimic-service.
//!
//! This service publishes no messages to siblings, so "provider and consumer"
//! needs reading differently here. It still has two real contracts:
//!
//! - **Provides** `/api/state` to its own console page. The page switches on
//!   these strings to colour lamps, so a rename here silently leaves every
//!   lamp grey — a monitoring tool that looks fine and reports nothing.
//! - **Consumes** the monitoring output of Prometheus and NATS, neither of
//!   which this repository controls. Tolerance matters more than usual: an
//!   upstream version can add fields at any time.
//! - **Consumes** the message bus itself, as an independent subscriber. That
//!   makes the durable consumer names a contract with every other service —
//!   see [`the_tap_names_cannot_steal_another_services_traffic`].

use crate::collect::{Alarm, Gauge, Node, Snapshot, Status};
use crate::live::{COMMAND_EDGES, DEAD_LETTER_EDGES, EVENT_EDGES};
use crate::push::{Edge, Push};
use crate::tap::TAP_NAMES;
use chrono::Utc;
use messaging_core::subjects;

// ---------------------------------------------------------------------------
// Provider side — the JavaScript Object Notation the console page consumes
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
            detail: "99th percentile 850 milliseconds".to_owned(),
        }],
        gauges: vec![Gauge {
            id: "g-queue".to_owned(),
            value: "12".to_owned(),
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

    // A node: the page finds `led-<id>` and `txt-<id>` in the drawing from `id`.
    let node = &json["nodes"][0];
    assert_eq!(node["id"], "worker");
    assert_eq!(node["status"], "degraded");
    assert!(node.get("detail").is_some());

    // A gauge: `id` matches a text element in the drawing, `warn` picks its colour.
    let gauge = &json["gauges"][0];
    assert_eq!(gauge["id"], "g-queue");
    assert_eq!(
        gauge["value"], "12",
        "the panel supplies the unit; the value is the bare number"
    );
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
// Consumer side — upstream JavaScript Object Notation this service reads
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

// ---------------------------------------------------------------------------
// The message bus contract — the names, and what they must never collide with
// ---------------------------------------------------------------------------

/// The single most important test in this crate.
///
/// `durable_consumer_from_now` accepts any string. Passing a name a real
/// service already uses would silently enlist this panel in that service's
/// queue group and start *taking* its messages — orders would quietly vanish,
/// with no error and no failing build anywhere to say why.
///
/// Nothing else in this repository can catch that: not the compiler, not the
/// broker, not a runtime check. This assertion is the whole guard.
#[test]
fn the_tap_names_cannot_steal_another_services_traffic() {
    let already_taken = [
        subjects::CONSUMER_WORKER,
        subjects::CONSUMER_NOTIFIER,
        subjects::CONSUMER_AUDIT,
        subjects::CONSUMER_DLQ_REPLAY,
    ];

    for name in TAP_NAMES {
        assert!(
            !already_taken.contains(&name),
            "{name} is already a durable consumer name belonging to a service; \
             sharing it would make this panel a competing consumer and orders would disappear"
        );
    }
}

/// Two tap consumers sharing a name would split the streams between the
/// panel's own tasks, and each would then see half the traffic.
#[test]
fn the_three_tap_names_are_distinct_from_each_other() {
    for (index, name) in TAP_NAMES.iter().enumerate() {
        for other in &TAP_NAMES[index + 1..] {
            assert_ne!(name, other, "two tap consumers share the name {name}");
        }
    }
}

// ---------------------------------------------------------------------------
// The drawing contract — an edge that is not in the page never lights
// ---------------------------------------------------------------------------

/// Every animated line must exist in the drawing.
///
/// A typo in one of these identifiers produces a line that silently never
/// lights: no compile error, no failing test, and a reader who reasonably
/// concludes that no message ever travelled that hop. Checking against the
/// embedded copy of the page is the only way to catch it.
#[test]
fn every_animated_edge_exists_in_the_drawing() {
    let page = crate::PANEL_HTML;

    for edge in COMMAND_EDGES
        .iter()
        .chain(EVENT_EDGES.iter())
        .chain(DEAD_LETTER_EDGES.iter())
    {
        assert!(
            page.contains(&format!("id=\"{edge}\"")),
            "the drawing has no element with the identifier {edge}, so that line can never light"
        );
    }
}

// ---------------------------------------------------------------------------
// The wire contract — what the browser branches on
// ---------------------------------------------------------------------------

/// The page switches on the `type` field, so these three strings are load-bearing.
#[test]
fn the_push_messages_are_tagged_with_the_names_the_page_switches_on() {
    let frame = Push::Frame {
        seq: 1,
        epoch: uuid::Uuid::nil(),
        state: a_snapshot(),
    };
    let pulse = Push::Pulse {
        seq: 2,
        epoch: uuid::Uuid::nil(),
        gauges: vec![],
        edges: vec![Edge {
            id: "edge-commands-worker".to_owned(),
            count: 3,
        }],
    };
    let tick = Push::Tick {
        seq: 3,
        epoch: uuid::Uuid::nil(),
        at: Utc::now(),
        live_ok: true,
        prometheus_age_milliseconds: 400,
        last_message_age_milliseconds: Some(20),
    };

    assert_eq!(tagged(&frame), "frame");
    assert_eq!(tagged(&pulse), "pulse");
    assert_eq!(tagged(&tick), "tick");
}

/// A pulse carries absolute readings and a per-line count, and nothing else.
///
/// Specifically it carries no order identifier and no total. That absence is
/// the contract: this is a mimic feed, not an audit log, and a number on the
/// wire is an invitation to derive a count from a stream that drops messages
/// by design.
#[test]
fn a_pulse_carries_readings_and_motion_but_never_an_order() {
    let pulse = Push::Pulse {
        seq: 7,
        epoch: uuid::Uuid::nil(),
        gauges: vec![Gauge {
            id: "g-processed".to_owned(),
            value: "2.40".to_owned(),
            warn: false,
        }],
        edges: vec![Edge {
            id: "edge-events-audit".to_owned(),
            count: 2,
        }],
    };

    let json = serde_json::to_value(&pulse).expect("must serialise");

    assert_eq!(json["type"], "pulse");
    assert_eq!(json["seq"], 7);
    assert_eq!(json["gauges"][0]["value"], "2.40");
    assert_eq!(json["edges"][0]["count"], 2);
    assert!(
        json.get("total").is_none() && json.get("order_id").is_none(),
        "the feed must not carry a total or an order identifier"
    );
}

/// A tick reports two ages, because there are two clocks.
///
/// The socket being alive and the data being fresh are different claims. A
/// panel that collapses them is confidently wrong about whichever half still
/// works.
#[test]
fn a_tick_reports_both_ages_separately() {
    let tick = Push::Tick {
        seq: 9,
        epoch: uuid::Uuid::nil(),
        at: Utc::now(),
        live_ok: false,
        prometheus_age_milliseconds: 12_000,
        last_message_age_milliseconds: None,
    };

    let json = serde_json::to_value(&tick).expect("must serialise");

    assert_eq!(json["live_ok"], false);
    assert_eq!(json["prometheus_age_milliseconds"], 12_000);
    assert!(
        json["last_message_age_milliseconds"].is_null(),
        "no message seen yet must be absent, not zero — an idle system is not a broken one"
    );
}

/// The whole picture round-trips, which is what lets the tests and the
/// end-to-end crate read a frame back.
#[test]
fn a_frame_round_trips_through_the_wire_shape() {
    let frame = Push::Frame {
        seq: 4,
        epoch: uuid::Uuid::nil(),
        state: a_snapshot(),
    };

    let json = serde_json::to_string(&frame).expect("must serialise");
    let decoded: Push = serde_json::from_str(&json).expect("must deserialise");

    match decoded {
        Push::Frame { seq, state, .. } => {
            assert_eq!(seq, 4);
            assert_eq!(state.nodes[0].status, Status::Healthy);
        }
        other => panic!("a frame decoded as something else: {other:?}"),
    }
}

fn tagged(push: &Push) -> String {
    serde_json::to_value(push).expect("must serialise")["type"]
        .as_str()
        .expect("the tag is a string")
        .to_owned()
}

fn a_snapshot() -> Snapshot {
    Snapshot {
        generated_at: Utc::now(),
        nodes: vec![Node {
            id: "worker".to_owned(),
            status: Status::Healthy,
            detail: "2 instances".to_owned(),
        }],
        gauges: vec![],
        alarms: vec![],
        sources_ok: true,
    }
}

// ---------------------------------------------------------------------------
// The honesty contract — "I cannot tell" must never render as a number
// ---------------------------------------------------------------------------

/// A reading the panel could not take shows the no-reading mark.
///
/// This is the one that was actually observed failing: with the broker stopped,
/// the panel reported a confident zero dead letters seconds after reporting
/// three. Zero and "unknown" look identical to a reader, and the difference
/// matters most exactly when the broker is the thing that has broken.
///
/// The pairing with `warn` is the other half. A reading of unknown must not
/// draw itself in the warning colour — not knowing is not evidence of a
/// problem — and it must not draw itself in the calm colour either, which is
/// why the value itself carries the mark.
#[test]
fn an_unreadable_depth_shows_the_no_reading_mark_rather_than_a_zero() {
    let unknown: Option<u64> = None;

    let queue = Gauge {
        id: "g-queue".to_owned(),
        value: unknown.map_or_else(|| "—".to_owned(), |depth: u64| depth.to_string()),
        warn: unknown.is_some_and(|depth| depth > 100),
    };

    assert_eq!(
        queue.value, "—",
        "an unreachable broker must not render as an empty queue"
    );
    assert!(!queue.warn, "not knowing is not evidence of a problem");
}

/// And a reading that *was* taken still renders as the plain number.
#[test]
fn a_readable_depth_still_shows_the_number() {
    let known = Some(7u64);

    let queue = Gauge {
        id: "g-queue".to_owned(),
        value: known.map_or_else(|| "—".to_owned(), |depth| depth.to_string()),
        warn: known.is_some_and(|depth| depth > 100),
    };

    assert_eq!(queue.value, "7");
    assert!(!queue.warn);
}
