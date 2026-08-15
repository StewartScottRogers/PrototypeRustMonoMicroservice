//! Pushing changes to the browser, and the one task that decides what to push.
//!
//! # What replaced what
//!
//! The browser used to ask "anything new?" once a second. Now it is told. A
//! `watch` channel is the same "many readers, rare writer" shape the old lock
//! had, plus the one thing polling could never have: a notification.
//!
//! # The promise this feed refuses to make
//!
//! **This is a mimic feed, not an audit log. No count may be derived from it.**
//!
//! Reports are dropped when the tap's channel is full, dropped when a slow
//! browser falls behind, and dropped by the staleness guard — and none of those
//! drops are reported as gaps. Someone will eventually want to total the
//! numbers that cross this wire and say how many orders were processed. That
//! number would be wrong, quietly, under exactly the conditions where it
//! mattered. The totals live in Prometheus, which is built for the question.
//!
//! A promise not made beats a promise kept badly.

use crate::collect::{Collector, Gauge, Snapshot};
use crate::live::{self, LiveReadings, TapEvent};
use crate::tap::Health;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

/// How often reports are gathered into one message.
///
/// The trade this buys, stated plainly: five hundred orders in a second become
/// ten messages each saying "fifty crossed here", not five hundred messages
/// each saying "one". Nothing is sent when nothing happened, so an idle system
/// costs nothing at all.
const COALESCE_WINDOW: Duration = Duration::from_millis(100);

/// How often a heartbeat goes out, whether or not anything changed.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// How often the sampled plane is re-read.
///
/// Two seconds, because Prometheus scrapes every two seconds and reading faster
/// only re-reads the same sample.
const RECONCILE: Duration = Duration::from_secs(2);

/// How long a single write to one browser may take before that browser is cut.
///
/// A laptop whose lid closed on a flaky connection can otherwise pin a task and
/// a growing backlog of bytes indefinitely.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest one reading of the sampled sources may take.
///
/// Shorter than the interval between readings on purpose: a read that has not
/// finished before the next one is due is not going to be useful.
const SNAPSHOT_BUDGET: Duration = Duration::from_millis(1800);

/// One line on the drawing that just carried something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub count: u32,
}

/// Everything this service can say to a browser.
///
/// # Why readings are absolute and never increments
///
/// `gauges` carries current values, formatted exactly as the collector formats
/// its own, so applying a message twice is the same as applying it once.
/// `edges` is the only "since last time" field, and losing it loses only
/// motion — never a number.
///
/// That single choice collapses every way this can go wrong into one recovery
/// rule: *apply a message only if its sequence number is greater than the last
/// one seen, and the picture converges.* A browser that slept, lagged,
/// reconnected, or met a restarted server all recover the same way. The
/// alternative — increments — would need a replay contract, a cursor per
/// browser, and server-side sessions to match.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Push {
    /// The whole picture. Sent on connect, and whenever it changes.
    Frame {
        seq: u64,
        epoch: Uuid,
        state: Snapshot,
    },
    /// Traffic crossed the bus. Readings are absolute; edges are motion.
    Pulse {
        seq: u64,
        epoch: Uuid,
        gauges: Vec<Gauge>,
        edges: Vec<Edge>,
    },
    /// Nothing changed, and here is how fresh each claim is.
    ///
    /// The ages are the point. A per-socket timer would prove the network path
    /// is alive, which is a *different claim* from the data being alive. Two
    /// claims need two sources, and this one originates in the aggregator so it
    /// cannot outlive the thing it reports on.
    ///
    /// If someone later "tidies" this into a timer inside the socket handler, a
    /// dead collector will report itself healthy to every open browser and
    /// nothing in the test suite will notice. That is the whole reason this
    /// paragraph exists.
    Tick {
        seq: u64,
        epoch: Uuid,
        at: DateTime<Utc>,
        live_ok: bool,
        prometheus_age_milliseconds: u64,
        last_message_age_milliseconds: Option<u64>,
    },
}

/// The two ends of the push machinery, cheap to clone into axum's state.
#[derive(Clone)]
pub struct Live {
    /// The most recent merged snapshot, for `/api/state` and for a new socket.
    pub latest: watch::Receiver<Arc<Snapshot>>,
    /// Every message, already serialised once.
    pub hub: broadcast::Sender<Arc<str>>,
}

/// Runs the one task that owns the live counters.
///
/// # One owner, no lock
///
/// The counters are owned outright by this function — no `Arc`, no mutex.
/// Anything wanting to change them sends a message instead. That is both
/// simpler and faster than sharing, and it is why the interesting logic could
/// be moved into [`crate::live`] where a test can reach it without a broker.
#[allow(clippy::too_many_arguments)]
pub async fn aggregate(
    collector: Collector,
    mut reports: mpsc::Receiver<TapEvent>,
    health: Health,
    latest: watch::Sender<Arc<Snapshot>>,
    hub: broadcast::Sender<Arc<str>>,
    mut shutdown: watch::Receiver<bool>,
    epoch: Uuid,
) {
    let mut readings = LiveReadings::default();
    let mut pending: Vec<(TapEvent, u32)> = Vec::new();
    let mut seq: u64 = 0;
    let mut last_message_at: Option<Instant> = None;
    let mut last_sample_at = Instant::now();
    let mut published = Arc::clone(&latest.borrow());
    let mut counting_since_connected = false;
    let mut dropped_since_last_reconcile = health.dropped_total();

    let mut coalesce = tokio::time::interval(COALESCE_WINDOW);
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut reconcile = tokio::time::interval(RECONCILE);

    // Skip missed ticks rather than firing them back to back.
    //
    // The default is to catch up: an interval that could not run for two
    // seconds then fires every missed tick with no gap between them. For the
    // heartbeat that means a burst of identical messages the moment a stall
    // clears; for the reconcile it means several Prometheus reads at once,
    // right after the thing that caused the stall. Neither is useful, and
    // "delay" is what a person means by "every second".
    coalesce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // The first tick of an interval fires immediately, which would reconcile
    // before the priming read had aged at all.
    reconcile.tick().await;

    loop {
        tokio::select! {
            _ = shutdown.changed() => return,

            Some(event) = reports.recv() => {
                let now = Instant::now();
                readings.record(event, now);
                last_message_at = Some(now);

                match pending.iter_mut().find(|(kind, _)| *kind == event) {
                    Some((_, count)) => *count += 1,
                    None => pending.push((event, 1)),
                }
            }

            _ = coalesce.tick() => {
                if pending.is_empty() {
                    continue;
                }

                let now = Instant::now();
                let edges = edges_for(&pending);
                pending.clear();

                seq += 1;
                send(&hub, &Push::Pulse {
                    seq,
                    epoch,
                    gauges: readings.gauges(now),
                    edges,
                });
            }

            _ = heartbeat.tick() => {
                // Start the warm-up clock when the tap connects, not when the
                // first message lands. An idle stack would otherwise never warm
                // up, and its first message would arrive into a cold window.
                if !counting_since_connected && health.all_connected() {
                    readings.begin(Instant::now());
                    counting_since_connected = true;
                }

                seq += 1;
                send(&hub, &Push::Tick {
                    seq,
                    epoch,
                    at: Utc::now(),
                    live_ok: health.all_connected(),
                    prometheus_age_milliseconds: last_sample_at.elapsed().as_millis() as u64,
                    last_message_age_milliseconds: last_message_at
                        .map(|at| at.elapsed().as_millis() as u64),
                });
            }

            _ = reconcile.tick() => {
                // Bounded, because everything else in this loop waits on it.
                //
                // The collector's own client has a timeout, but one snapshot is
                // a sequence of queries and a pathological source could still
                // hold this arm far longer than a tick. While it is held the
                // heartbeat does not go out, pulses do not go out, and reports
                // pile up in the channel until it drops them — a Prometheus
                // problem would present as a dead panel.
                let sampled = match tokio::time::timeout(
                    SNAPSHOT_BUDGET,
                    collector.snapshot(),
                ).await {
                    Ok(snapshot) => {
                        // Only a reading that actually reached Prometheus
                        // counts as fresh. A snapshot that came back promptly
                        // saying "I could not reach anything" is a *fast*
                        // failure, not a recent measurement, and treating it as
                        // one would let the age read "live" forever while
                        // Prometheus was down.
                        if snapshot.sources_ok {
                            last_sample_at = Instant::now();
                        }
                        snapshot
                    }
                    Err(_) => {
                        tracing::warn!(
                            "reading the sampled sources took longer than the budget; \
                             keeping the previous picture"
                        );
                        // Deliberately not updating last_sample_at, so the age
                        // on the next heartbeat climbs and the panel says the
                        // lamps are old. That is the honest report.
                        continue;
                    }
                };

                let now = Instant::now();

                // Being connected is not the same as being able to observe.
                //
                // Two ways a connected tap can still be blind: the staleness
                // guard rejects everything (a publisher whose clock runs
                // behind), or this aggregator fell behind and the report
                // channel filled. Either way the window contains a hole, and a
                // rate counted from a window with a hole is not a measurement.
                //
                // Letting it through would put a confident zero over a correct
                // sampled reading, with every lamp green and the flow indicator
                // saying live — which reads as "the relay has stopped" at the
                // very moment the relay may be working hardest.
                let dropped_now = health.dropped_total();
                let observed_everything = dropped_now == dropped_since_last_reconcile;
                dropped_since_last_reconcile = dropped_now;

                // Live readings also only win once their window has actually
                // been open for a full window. Before that they under-read, and
                // an under-reading live value replacing a correct sampled one
                // is a confident lie told on every restart.
                let live_ok =
                    health.all_connected() && readings.warmed_up(now) && observed_everything;
                let merged = live::merge(
                    sampled,
                    readings.gauges(now),
                    live_ok,
                );

                let changed = !live::same_picture(&published, &merged);
                let merged = Arc::new(merged);

                // Published either way, so `/api/state` never goes stale even
                // when the picture is unchanged.
                published = Arc::clone(&merged);
                latest.send_replace(Arc::clone(&merged));

                if changed {
                    seq += 1;
                    send(&hub, &Push::Frame {
                        seq,
                        epoch,
                        state: Snapshot::clone(&merged),
                    });
                }
            }
        }
    }
}

/// Turns the coalesced reports into the lines that should light.
fn edges_for(pending: &[(TapEvent, u32)]) -> Vec<Edge> {
    let mut edges = Vec::new();

    for (kind, count) in pending {
        let ids: &[&str] = match kind {
            TapEvent::Command => &live::COMMAND_EDGES,
            TapEvent::Event => &live::EVENT_EDGES,
            TapEvent::DeadLetter => &live::DEAD_LETTER_EDGES,
        };

        for id in ids {
            edges.push(Edge {
                id: (*id).to_owned(),
                count: *count,
            });
        }
    }

    edges
}

/// Serialises once and hands the same bytes to every browser.
///
/// `Arc<str>` rather than `Arc<String>`: one heap allocation and one pointer,
/// where `Arc<String>` would be a pointer to a `String` which is itself a
/// pointer to the text. Twenty open tabs then cost one serialisation and twenty
/// pointer copies.
fn send(hub: &broadcast::Sender<Arc<str>>, push: &Push) {
    match serde_json::to_string(push) {
        Ok(json) => {
            // An error here means nobody is listening, which is normal.
            hub.send(Arc::from(json.into_boxed_str())).ok();
        }
        Err(error) => tracing::error!(%error, "could not encode a push message"),
    }
}

/// The `/api/live` route.
///
/// `on_upgrade` is marked `#[must_use]` and spawns the connection internally.
/// Dropping the returned response rather than returning it never establishes
/// the socket, and the browser sees a request that hangs instead of an error —
/// a genuinely confusing half hour if it happens.
pub async fn live_socket(upgrade: WebSocketUpgrade, State(live): State<Live>) -> Response {
    upgrade
        // The browser never sends anything on this socket, so the generous
        // defaults — tens of megabytes — are pure exposure. A kilobyte is more
        // than a client that says nothing could ever legitimately need, and it
        // turns "send a huge frame at the panel" from a memory question into an
        // immediate disconnection.
        .max_message_size(1024)
        .max_frame_size(1024)
        .on_upgrade(move |socket| serve(socket, live))
}

/// Serves one browser until it goes away.
async fn serve(mut socket: WebSocket, live: Live) {
    let mut feed = live.hub.subscribe();

    // The current picture immediately, so a browser that connects during a
    // quiet spell is not staring at an empty panel waiting for something to
    // happen.
    let opening = Push::Frame {
        seq: 0,
        epoch: Uuid::nil(),
        state: Snapshot::clone(&live.latest.borrow()),
    };

    if let Ok(json) = serde_json::to_string(&opening)
        && !write(&mut socket, &json).await
    {
        return;
    }

    loop {
        tokio::select! {
            received = feed.recv() => match received {
                Ok(json) => {
                    if !write(&mut socket, &json).await {
                        return;
                    }
                }
                // This browser fell behind and messages were discarded. It does
                // not try to catch up: one fresh whole picture puts it right,
                // which is the entire reason readings are absolute.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(missed, "a browser fell behind; sending a fresh picture");

                    let recovery = Push::Frame {
                        seq: 0,
                        epoch: Uuid::nil(),
                        state: Snapshot::clone(&live.latest.borrow()),
                    };

                    match serde_json::to_string(&recovery) {
                        Ok(json) if write(&mut socket, &json).await => {}
                        _ => return,
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },

            // The browser never speaks, but reading matters anyway: it is how a
            // close is noticed promptly, and how the library answers a ping.
            // Without it a half-open socket leaks this task until some later
            // write finally fails.
            incoming = socket.recv() => match incoming {
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

/// Writes one message, giving up on a browser that will not take it.
///
/// Returns false when the connection should be abandoned.
async fn write(socket: &mut WebSocket, json: &str) -> bool {
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, socket.send(Message::Text(json.into()))).await,
        Ok(Ok(()))
    )
}
