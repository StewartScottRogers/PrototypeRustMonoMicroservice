//! The live plane: counters fed by the tap, and the rule that merges them with
//! what Prometheus knows.
//!
//! # Three planes, arbitrated per reading
//!
//! The panel now has three sources, and each owns the readings only it can be
//! right about:
//!
//! - **The tap** sees every message the instant it is published. It owns the
//!   readings that are counts of messages, and every edge animation.
//! - **Prometheus** samples on a timer. It owns everything that needs history —
//!   percentiles — and everything the tap cannot see at all, such as how many
//!   instances of a service are answering.
//! - **The NATS monitoring endpoint** owns queue depth, which is a length
//!   rather than a rate and so cannot be counted from a message stream.
//!
//! # The rule, in one sentence
//!
//! The tap may drive a reading only if the tap can actually observe the thing
//! that reading claims to measure.
//!
//! That sentence is doing real work. It is why `g-accepted` stays on
//! Prometheus: the gateway writes an outbox row inside its transaction and the
//! relay publishes it up to a quarter of a second later, so the tap observes
//! the *relay's* publish and never the gateway's accept. An order that has
//! committed but not yet been relayed is invisible to the tap — and that gap is
//! precisely what an outbox exists to make visible. It is also why the two
//! subscriber readings stay on Prometheus: the tap sees an event *published*,
//! not the notifier and the audit service *handling* it.
//!
//! # Reading this file as a Rust newcomer
//!
//! Everything here is either a plain data structure or a function with no
//! network in it. That is deliberate: it means the interesting decisions can be
//! unit tested with no broker, no database and no browser, which is what the
//! tests at the bottom do.

use crate::collect::{Gauge, Snapshot};
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far behind a message may be before the panel refuses to animate it.
const STALENESS_LIMIT_SECONDS: i64 = 10;

/// The window the live rates are averaged over.
///
/// Fifteen seconds to match the `rate(...[15s])` windows the Prometheus-derived
/// readings use, so the two kinds of number sit on the same scale and a reader
/// can compare them without being quietly misled.
const RATE_WINDOW: Duration = Duration::from_secs(15);

/// What the tap saw. Nothing more.
///
/// Deliberately carries no order identifier, no item and no payload. The panel
/// counts and animates; it does not inspect orders. This is the first place
/// someone will reach to add "just the order number", and the moment it carries
/// one, the feed stops being a mimic and starts being an audit log that nobody
/// has made durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapEvent {
    Command,
    Event,
    DeadLetter,
}

/// The edges a command pulse lights, in the order a message travels them.
///
/// # The lesson, written down in code
///
/// Six edges ending at **exactly one** line into the worker, because one worker
/// takes each command. Compare [`EVENT_EDGES`].
///
/// # Where this is inference rather than observation
///
/// The tap observes the relay's publish, so the first three hops here already
/// happened — up to a quarter of a second earlier. The panel staggers the flash
/// left to right to show that ordering honestly rather than lighting all six at
/// once as though they were simultaneous.
pub const COMMAND_EDGES: [&str; 6] = [
    "edge-clients-gateway",
    "edge-gateway-postgres",
    "edge-postgres-relay",
    "edge-relay-tag",
    "edge-tag-commands",
    "edge-commands-worker",
];

/// The edges an event pulse lights.
///
/// # The other half of the lesson
///
/// One publish lights **both** subscriber lines, and the panel starts them
/// together rather than in sequence, because that simultaneity is the whole
/// difference between eventing and messaging.
///
/// # Where this is inference rather than observation
///
/// The tap sees the event published; it does not see the notifier or the audit
/// service handle it. These two lines therefore say "a copy was sent here", not
/// "a copy arrived here" — which is exactly why the readings beside them stay
/// on Prometheus, which does observe the handling.
pub const EVENT_EDGES: [&str; 3] = [
    "edge-worker-events",
    "edge-events-notifier",
    "edge-events-audit",
];

/// The edge a dead letter lights.
pub const DEAD_LETTER_EDGES: [&str; 1] = ["edge-worker-deadletter"];

/// Decides whether a message is too old to animate.
///
/// # Why this guard is permanent
///
/// It is tempting to read this as a stopgap for a consumer that starts at the
/// beginning of the stream. It is not, and it must not be removed when the
/// consumer starts from the end instead. A tap that *stalls* — a slow
/// aggregator, a paused container, a broker reconnect — and then catches up
/// would otherwise paint a burst of motion for messages that crossed the system
/// a minute ago. Starting from the end does nothing about that case.
///
/// # The failure mode this creates, stated plainly
///
/// A publisher whose clock runs more than ten seconds behind this container has
/// every message dropped, and the panel then shows a connected flow plane with
/// nothing ever arriving. That is the trade, and it is why the tap logs a
/// warning when it drops.
///
/// A timestamp in the *future* — a publisher whose clock runs ahead — is never
/// stale. The comparison is signed for that reason: taking an absolute
/// difference would silence the panel whenever a clock drifted forward.
pub fn is_stale(occurred_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (now - occurred_at).num_seconds() > STALENESS_LIMIT_SECONDS
}

/// Counts of recent events, used to produce a per-second rate.
///
/// # Why a `VecDeque` rather than a `Vec`
///
/// Old entries are removed from the *front*. Removing from the front of a `Vec`
/// shuffles every remaining element down; a `VecDeque` is a ring buffer, so it
/// is a pointer move. This is the standard shape for a sliding window.
///
/// # Why `Instant` rather than `DateTime<Utc>`
///
/// `Instant` is monotonic: it measures elapsed time and cannot go backwards
/// when the machine's clock is corrected. A wall-clock adjustment during a
/// leap second or a time sync would otherwise make a duration negative.
#[derive(Debug, Default)]
pub struct SlidingCount {
    seen: VecDeque<Instant>,
}

impl SlidingCount {
    /// Records one occurrence.
    pub fn push(&mut self, now: Instant) {
        self.seen.push_back(now);
    }

    /// Events per second, averaged over the whole window.
    ///
    /// Always divided by the full fifteen seconds, including in the first
    /// fifteen seconds after start when the window is not yet full. That is
    /// exactly what Prometheus does, and matching it is what stops the live
    /// readings spiking on startup while the sampled ones beside them do not.
    ///
    /// Memory is bounded by throughput times the window: at five hundred
    /// messages a second that is about seven and a half thousand timestamps,
    /// which is nothing — but the bound is worth stating out loud, because an
    /// unbounded queue is the obvious worry with this shape.
    pub fn rate(&mut self, now: Instant) -> f64 {
        while let Some(oldest) = self.seen.front() {
            if now.duration_since(*oldest) > RATE_WINDOW {
                self.seen.pop_front();
            } else {
                break;
            }
        }

        self.seen.len() as f64 / RATE_WINDOW.as_secs_f64()
    }
}

/// The readings the tap owns, ready to be merged into a snapshot.
#[derive(Debug, Default)]
pub struct LiveReadings {
    pub relayed: SlidingCount,
    pub processed: SlidingCount,
    pub dead_letters: SlidingCount,
}

impl LiveReadings {
    /// Records one observed message.
    pub fn record(&mut self, event: TapEvent, now: Instant) {
        match event {
            TapEvent::Command => self.relayed.push(now),
            TapEvent::Event => self.processed.push(now),
            TapEvent::DeadLetter => self.dead_letters.push(now),
        }
    }

    /// The three readings, formatted exactly as the collector formats its own.
    pub fn gauges(&mut self, now: Instant) -> Vec<Gauge> {
        vec![
            Gauge {
                id: "g-relayed".to_owned(),
                value: format!("{:.2}", self.relayed.rate(now)),
                warn: false,
            },
            Gauge {
                id: "g-processed".to_owned(),
                value: format!("{:.2}", self.processed.rate(now)),
                warn: false,
            },
        ]
    }
}

/// The readings the tap is allowed to overwrite.
///
/// Named as data rather than expressed as branching prose, so the rule can be
/// read at a glance and asserted in a test.
const LIVE_OWNED: [&str; 2] = ["g-relayed", "g-processed"];

/// Replaces the sampled value of a tap-owned reading with the live one.
///
/// When the flow plane is not connected, every sampled value survives
/// untouched — including a missing one. A reading Prometheus could not supply
/// stays as the "no reading" mark and is never replaced by a confident zero.
///
/// Node lamps are never touched by this function, and a test asserts it. That
/// assertion is what stops a later change from quietly letting the flow plane
/// influence the health plane, which would make a broker outage look like six
/// dead services.
pub fn merge(mut sampled: Snapshot, live: Vec<Gauge>, live_ok: bool) -> Snapshot {
    if !live_ok {
        return sampled;
    }

    for replacement in live {
        if !LIVE_OWNED.contains(&replacement.id.as_str()) {
            continue;
        }

        if let Some(existing) = sampled
            .gauges
            .iter_mut()
            .find(|gauge| gauge.id == replacement.id)
        {
            *existing = replacement;
        }
    }

    sampled
}

/// Whether two snapshots would draw the same screen.
///
/// # Why this is hand-written rather than derived
///
/// `generated_at` changes on every reconciliation by definition, so deriving
/// equality on [`Snapshot`] would return false forever — which is exactly the
/// bug this function exists to avoid. Ignoring that one field turns "the
/// picture changed" into something a test can assert, and lets an idle system
/// send a heartbeat instead of a full redraw.
pub fn same_picture(left: &Snapshot, right: &Snapshot) -> bool {
    left.sources_ok == right.sources_ok
        && left.nodes == right.nodes
        && left.gauges == right.gauges
        && left.alarms == right.alarms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{Alarm, Node, Status};

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_786_000_000 + seconds, 0).expect("a valid timestamp")
    }

    // ---- the staleness guard ----

    #[test]
    fn a_message_from_this_moment_is_not_stale() {
        assert!(!is_stale(at(0), at(0)));
    }

    #[test]
    fn a_message_from_just_inside_the_limit_is_not_stale() {
        assert!(!is_stale(at(0), at(9)));
    }

    #[test]
    fn a_message_from_beyond_the_limit_is_stale() {
        assert!(is_stale(at(0), at(11)));
    }

    #[test]
    fn the_limit_is_exclusive() {
        // Exactly at the limit is still fine; only past it is a problem. Same
        // convention as the thresholds in the collector.
        assert!(!is_stale(at(0), at(STALENESS_LIMIT_SECONDS)));
        assert!(is_stale(at(0), at(STALENESS_LIMIT_SECONDS + 1)));
    }

    #[test]
    fn a_timestamp_from_the_future_is_never_stale() {
        // A publisher whose clock runs ahead must not silence the panel. This
        // is why the comparison is signed rather than absolute.
        assert!(!is_stale(at(30), at(0)));
    }

    // ---- the sliding window ----

    #[test]
    fn a_rate_divides_by_the_whole_window_even_before_it_is_full() {
        let start = Instant::now();
        let mut count = SlidingCount::default();

        for _ in 0..15 {
            count.push(start);
        }

        // Fifteen events in a fifteen-second window is one per second, even
        // though they all arrived at once and the window has only just opened.
        // Matching Prometheus here is what stops the live readings spiking on
        // startup while the sampled ones beside them do not.
        assert!((count.rate(start) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn events_older_than_the_window_stop_counting() {
        let start = Instant::now();
        let mut count = SlidingCount::default();

        count.push(start);
        assert!(count.rate(start) > 0.0);

        let later = start + RATE_WINDOW + Duration::from_secs(1);
        assert_eq!(count.rate(later), 0.0);
    }

    #[test]
    fn each_kind_of_event_feeds_its_own_reading() {
        let now = Instant::now();
        let mut readings = LiveReadings::default();

        readings.record(TapEvent::Command, now);
        readings.record(TapEvent::Event, now);
        readings.record(TapEvent::Event, now);

        let gauges = readings.gauges(now);
        let relayed = gauges
            .iter()
            .find(|g| g.id == "g-relayed")
            .expect("present");
        let processed = gauges
            .iter()
            .find(|g| g.id == "g-processed")
            .expect("present");

        // Two events against one command, so the processed rate is double.
        assert_eq!(relayed.value, "0.07");
        assert_eq!(processed.value, "0.13");
    }

    // ---- the merge rule ----

    fn sampled_snapshot() -> Snapshot {
        Snapshot {
            generated_at: at(0),
            nodes: vec![Node {
                id: "worker".to_owned(),
                status: Status::Healthy,
                detail: "2 instances".to_owned(),
            }],
            gauges: vec![
                Gauge {
                    id: "g-relayed".to_owned(),
                    value: "9.99".to_owned(),
                    warn: false,
                },
                Gauge {
                    id: "g-accepted".to_owned(),
                    value: "1.00".to_owned(),
                    warn: false,
                },
            ],
            alarms: vec![],
            sources_ok: true,
        }
    }

    fn live_gauges() -> Vec<Gauge> {
        vec![
            Gauge {
                id: "g-relayed".to_owned(),
                value: "3.33".to_owned(),
                warn: false,
            },
            // Not owned by the tap. Present here on purpose: the merge must
            // refuse it even when it is offered.
            Gauge {
                id: "g-accepted".to_owned(),
                value: "0.00".to_owned(),
                warn: false,
            },
        ]
    }

    #[test]
    fn the_live_reading_wins_when_the_flow_plane_is_connected() {
        let merged = merge(sampled_snapshot(), live_gauges(), true);

        let relayed = merged
            .gauges
            .iter()
            .find(|g| g.id == "g-relayed")
            .expect("present");
        assert_eq!(relayed.value, "3.33");
    }

    #[test]
    fn a_reading_the_tap_does_not_own_is_never_overwritten() {
        // The tap observes the relay's publish, not the gateway's accept, so it
        // cannot see an order sitting committed in the outbox. Letting it drive
        // this reading would show a confident number for a thing it cannot see.
        let merged = merge(sampled_snapshot(), live_gauges(), true);

        let accepted = merged
            .gauges
            .iter()
            .find(|g| g.id == "g-accepted")
            .expect("present");
        assert_eq!(accepted.value, "1.00");
    }

    #[test]
    fn the_sampled_reading_survives_when_the_flow_plane_is_down() {
        let merged = merge(sampled_snapshot(), live_gauges(), false);

        let relayed = merged
            .gauges
            .iter()
            .find(|g| g.id == "g-relayed")
            .expect("present");
        assert_eq!(relayed.value, "9.99");
    }

    #[test]
    fn a_missing_sampled_reading_is_not_replaced_by_a_zero() {
        // "No reading" and "a reading of zero" mean different things. A panel
        // that shows 0.00 for something it could not measure is lying quietly.
        let mut sampled = sampled_snapshot();
        sampled.gauges[0].value = "—".to_owned();

        let merged = merge(sampled, live_gauges(), false);

        assert_eq!(merged.gauges[0].value, "—");
    }

    #[test]
    fn merging_never_touches_a_node_lamp() {
        // The health plane and the flow plane are separate claims. If a broker
        // outage could change a lamp, it would look like six dead services.
        let before = sampled_snapshot();
        let after = merge(sampled_snapshot(), live_gauges(), true);

        assert_eq!(before.nodes, after.nodes);
    }

    // ---- change detection ----

    #[test]
    fn two_snapshots_differing_only_in_their_timestamp_are_the_same_picture() {
        let first = sampled_snapshot();
        let mut second = sampled_snapshot();
        second.generated_at = at(3600);

        assert!(same_picture(&first, &second));
    }

    #[test]
    fn a_changed_lamp_is_a_different_picture() {
        let first = sampled_snapshot();
        let mut second = sampled_snapshot();
        second.nodes[0].status = Status::Down;

        assert!(!same_picture(&first, &second));
    }

    #[test]
    fn a_changed_reading_is_a_different_picture() {
        let first = sampled_snapshot();
        let mut second = sampled_snapshot();
        second.gauges[0].value = "0.01".to_owned();

        assert!(!same_picture(&first, &second));
    }

    #[test]
    fn a_new_alarm_is_a_different_picture() {
        let first = sampled_snapshot();
        let mut second = sampled_snapshot();
        second.alarms.push(Alarm {
            text: "something broke".to_owned(),
            severity: Status::Down,
        });

        assert!(!same_picture(&first, &second));
    }

    // ---- the animated edge vocabulary ----

    #[test]
    fn a_command_ends_at_exactly_one_worker_edge() {
        // One command goes to one worker. That is messaging, and it is half the
        // reason this repository exists.
        let into_worker = COMMAND_EDGES
            .iter()
            .filter(|edge| edge.ends_with("-worker"))
            .count();

        assert_eq!(into_worker, 1);
    }

    #[test]
    fn an_event_lights_both_subscriber_edges() {
        // One event reaches every subscriber. That is eventing, and it is the
        // other half.
        assert!(EVENT_EDGES.contains(&"edge-events-notifier"));
        assert!(EVENT_EDGES.contains(&"edge-events-audit"));
    }

    #[test]
    fn the_three_edge_sets_do_not_overlap() {
        for edge in COMMAND_EDGES {
            assert!(!EVENT_EDGES.contains(&edge));
            assert!(!DEAD_LETTER_EDGES.contains(&edge));
        }
        for edge in EVENT_EDGES {
            assert!(!DEAD_LETTER_EDGES.contains(&edge));
        }
    }
}
