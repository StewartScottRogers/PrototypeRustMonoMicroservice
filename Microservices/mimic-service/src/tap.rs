//! The tap: three observer consumers on the message bus.
//!
//! # Why this exists
//!
//! Everything the panel used to show came from Prometheus, which *samples* on a
//! timer. Sampling can say "roughly this much throughput"; it can never say
//! "that order just moved". This module watches the message bus itself, so the
//! panel learns about an order in the same instant every other service does.
//!
//! # This is the repository's own lesson, applied to itself
//!
//! The one line that decides messaging from eventing is the durable consumer
//! name. Share it and a second process splits the load; vary it and a second
//! process adds a reaction. The three names below are new, so this tap is a
//! *third subscriber* that changes nothing about the worker, the notifier or
//! the audit service. Every one of them still receives exactly what it did
//! before.
//!
//! # Reading this file as a Rust newcomer
//!
//! Almost nothing here is testable without a running broker, which is why it is
//! kept deliberately thin: every decision that *can* be made without the
//! network lives in [`crate::live`] instead, where a unit test can reach it.

use crate::live::{TapEvent, is_stale};
use anyhow::Result;
use futures::StreamExt as _;
use messaging_core::{Envelope, Messaging, subjects};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// The durable consumer name for the command stream.
///
/// # Read this before changing any of the three names below
///
/// [`Messaging::durable_consumer_from_now`] accepts *any* `&'static str`.
/// Passing `"order-worker"` here — by a copy and paste, by a well-meant
/// refactor that moves these constants into `messaging-core/src/subjects.rs`
/// beside the real ones, or by an editor completing the wrong name — would
/// silently enlist this panel in the worker's queue group. The panel would then
/// start *taking* production commands, and the symptom would be orders that
/// quietly vanish: no error, no failed test, no log line anywhere saying why.
///
/// There is no registry, no compiler check and no runtime guard anywhere in
/// this repository that would catch it. The tests in `contract_tests.rs`
/// asserting these three names stay disjoint from the four real consumer names
/// are the only protection that can exist. Do not delete them.
///
/// They are declared here rather than in `subjects.rs` for the same reason: the
/// names in that file are shared contracts between services, while these three
/// are private to one observer. Keeping them apart is what makes the
/// disjointness assertion meaningful rather than circular.
pub const TAP_COMMANDS: &str = "mimic-tap-commands";

/// The durable consumer name for the event stream. See [`TAP_COMMANDS`].
pub const TAP_EVENTS: &str = "mimic-tap-events";

/// The durable consumer name for the dead-letter stream. See [`TAP_COMMANDS`].
pub const TAP_DEAD_LETTERS: &str = "mimic-tap-dead-letters";

/// All three, for the tests that prove they cannot collide.
pub const TAP_NAMES: [&str; 3] = [TAP_COMMANDS, TAP_EVENTS, TAP_DEAD_LETTERS];

/// The shortest wait before a failed consumer tries again.
const BACKOFF_FLOOR: Duration = Duration::from_millis(500);

/// The longest wait between attempts.
const BACKOFF_CEILING: Duration = Duration::from_secs(10);

/// One stream this tap watches, and what to report when a message arrives on it.
struct Watched {
    stream: &'static str,
    durable_name: &'static str,
    filter_subject: &'static str,
    reports: TapEvent,
}

/// The three streams, in the order a message travels them.
const WATCHED: [Watched; 3] = [
    Watched {
        stream: subjects::STREAM_COMMANDS,
        durable_name: TAP_COMMANDS,
        // The wildcard rather than the leaf subject, so a command kind added
        // later lights the panel with no change here.
        filter_subject: subjects::STREAM_COMMANDS_SUBJECTS,
        reports: TapEvent::Command,
    },
    Watched {
        stream: subjects::STREAM_EVENTS,
        durable_name: TAP_EVENTS,
        filter_subject: subjects::STREAM_EVENTS_SUBJECTS,
        reports: TapEvent::Event,
    },
    Watched {
        stream: subjects::STREAM_DEAD_LETTER,
        durable_name: TAP_DEAD_LETTERS,
        filter_subject: subjects::STREAM_DEAD_LETTER_SUBJECTS,
        reports: TapEvent::DeadLetter,
    },
];

/// Reports whether every consumer is currently connected, and whether any of
/// them has had to throw a message away.
///
/// A `watch` channel is "one value, many readers, and the readers are told when
/// it changes" — the right shape for a flag several tasks read and one writes.
#[derive(Clone)]
pub struct Health {
    connected: watch::Sender<[bool; 3]>,
    /// Every message the tap saw but could not report, for any reason.
    ///
    /// # Why a count and not a flag
    ///
    /// The reader needs to ask "did anything get thrown away *since I last
    /// looked*", which a flag cannot answer without the reader clearing it and
    /// racing the writer. A monotonic count lets the reader compare two
    /// readings and decide for itself.
    dropped: Arc<AtomicU64>,
}

impl Health {
    pub fn new() -> Self {
        Self {
            connected: watch::Sender::new([false; 3]),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// True only when all three consumers are reading.
    ///
    /// Deliberately *not* derived from whether messages are arriving: an idle
    /// system is not a broken one, and conflating the two would paint a healthy
    /// quiet stack as a failure.
    pub fn all_connected(&self) -> bool {
        self.connected.borrow().iter().all(|connected| *connected)
    }

    /// How many messages have been thrown away since the process started.
    ///
    /// # What this is for
    ///
    /// Being *connected* is not the same as being *able to observe*. A tap can
    /// hold all three consumers open and still discard everything — because the
    /// staleness guard rejected it, or because the aggregator fell behind and
    /// the channel filled. A rate counted from a window in which messages were
    /// discarded is not a measurement, and must not be allowed to overwrite a
    /// sampled value that is correct.
    ///
    /// Without this, the panel would show a confident zero over the top of a
    /// true reading, with every lamp green and the flow indicator saying live —
    /// which reads as "the relay has stopped" at the exact moment it may be
    /// working hardest.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn note_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn set(&self, index: usize, connected: bool) {
        self.connected.send_modify(|state| state[index] = connected);
    }
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

/// Starts one supervised task per stream and returns their handles.
///
/// The handles are returned rather than dropped so the caller can notice a task
/// ending. Expected failures — a broker restart, a dropped connection — are
/// retried in place by the backoff loop below and never reach the caller. A
/// task that *ends* therefore means it panicked, which is not a condition to
/// paper over: the caller logs it and lets the process exit so the container
/// restarts clean.
pub fn spawn(
    nats_url: String,
    sender: mpsc::Sender<TapEvent>,
    health: Health,
    shutdown: watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    // Named at startup so that anyone looking at the broker's consumer list and
    // wondering where three extra consumers came from can find the answer in
    // this service's log rather than by reading the source.
    tracing::info!(
        consumers = ?TAP_NAMES,
        "the panel is subscribing under its own names; no other service is affected"
    );

    WATCHED
        .iter()
        .enumerate()
        .map(|(index, watched)| {
            let nats_url = nats_url.clone();
            let sender = sender.clone();
            let health = health.clone();
            let shutdown = shutdown.clone();

            tokio::spawn(async move {
                supervise(index, watched, nats_url, sender, health, shutdown).await;
            })
        })
        .collect()
}

/// Runs one consumer forever, restarting it with a widening pause after a fault.
async fn supervise(
    index: usize,
    watched: &'static Watched,
    nats_url: String,
    sender: mpsc::Sender<TapEvent>,
    health: Health,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut backoff = BACKOFF_FLOOR;

    loop {
        if *shutdown.borrow() {
            return;
        }

        match follow(watched, &nats_url, &sender, &health, index, &mut shutdown).await {
            Ok(()) => return, // asked to stop
            Err(error) => {
                health.set(index, false);
                tracing::warn!(
                    %error,
                    consumer = watched.durable_name,
                    retry_in_milliseconds = backoff.as_millis() as u64,
                    "the tap lost its consumer and will reconnect"
                );
            }
        }

        // `select!` races the pause against the shutdown flag, so a stopping
        // process does not have to sit through the whole backoff.
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.changed() => return,
        }

        backoff = (backoff * 2).min(BACKOFF_CEILING);
    }
}

/// Connects, opens one consumer, and forwards messages until something breaks.
async fn follow(
    watched: &'static Watched,
    nats_url: &str,
    sender: &mpsc::Sender<TapEvent>,
    health: &Health,
    index: usize,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
    let messaging = Messaging::connect(nats_url).await?;

    // The panel may legitimately start before any publisher has run, and
    // opening a consumer on a stream that does not exist is an error rather
    // than an empty read.
    messaging.ensure_streams().await?;

    // Not tidiness. `get_or_create_consumer` returns an existing consumer *as
    // it is* and never reconciles a changed configuration, so without this the
    // very first deployment's settings would run forever — with no compile
    // error and no failing test to say so. Deleting also pairs exactly with
    // starting from now: a fresh position every restart is what "show me what
    // is happening" means.
    messaging
        .delete_consumer(watched.stream, watched.durable_name)
        .await
        .ok();

    let mut messages = messaging
        .durable_consumer_from_now(watched.stream, watched.durable_name, watched.filter_subject)
        .await?;

    health.set(index, true);

    let mut dropped_stale: u64 = 0;
    let mut dropped_full: u64 = 0;
    let mut last_complaint = std::time::Instant::now();

    // Distinguishes "we were told to stop" from "the stream ended". Both leave
    // the loop, and treating them the same would be a real fault: a broker
    // restart ends the stream, and reporting that as a clean stop would retire
    // this consumer permanently and silently. The panel would keep saying the
    // flow plane was connected while receiving nothing ever again.
    let mut asked_to_stop = false;

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                asked_to_stop = true;
                break;
            }
            next = messages.next() => {
                let Some(message) = next else {
                    tracing::warn!(
                        consumer = watched.durable_name,
                        "the message stream ended; reconnecting"
                    );
                    break;
                };
                let message = message?;

                // Decoded as a generic value on purpose. The tap needs only the
                // envelope's timestamp, so reading the payload as `Value`
                // instead of a concrete type means a message kind added later
                // is *counted* rather than dropped as unparseable.
                match serde_json::from_slice::<Envelope<serde_json::Value>>(&message.payload) {
                    Ok(envelope) if is_stale(envelope.occurred_at, chrono::Utc::now()) => {
                        dropped_stale += 1;
                        health.note_dropped();
                    }
                    Ok(_) => {
                        // `try_send` never waits. A stalled panel must not be
                        // able to apply backpressure to the message bus, so a
                        // full channel drops the report and says so in a log
                        // rather than slowing anything down.
                        if sender.try_send(watched.reports).is_err() {
                            dropped_full += 1;
                            health.note_dropped();
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "the tap could not read a message envelope");
                    }
                }

                // Always, and whatever happened above. The acknowledgement
                // deadline is fixed at ten seconds, and an unacknowledged
                // message comes back regardless of what this observer intended.
                message.ack().await.ok();

                if last_complaint.elapsed() >= Duration::from_secs(60)
                    && (dropped_stale > 0 || dropped_full > 0)
                {
                    tracing::warn!(
                        consumer = watched.durable_name,
                        dropped_stale,
                        dropped_full,
                        "the tap dropped reports in the last minute"
                    );
                    dropped_stale = 0;
                    dropped_full = 0;
                    last_complaint = std::time::Instant::now();
                }
            }
        }
    }

    health.set(index, false);

    if !asked_to_stop {
        // The stream ended by itself. Say so as an error, which sends the
        // supervisor into its backoff and reconnects, rather than returning
        // Ok and retiring this consumer for the life of the process.
        anyhow::bail!("the message stream for {} ended", watched.durable_name);
    }

    // Nothing expires an abandoned consumer on the server, so removing it on
    // the way out is the only cleanup there is.
    messaging
        .delete_consumer(watched.stream, watched.durable_name)
        .await
        .ok();

    Ok(())
}
