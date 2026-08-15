//! Turning Prometheus and NATS into the handful of numbers the panel draws.
//!
//! # Why a service rather than a static page
//!
//! A drawing with numbers typed into it is a diagram. A drawing whose numbers
//! come from the running system is an instrument: when a light goes amber it
//! is because something is actually wrong, and you can trust it enough to act.
//!
//! Everything here degrades rather than fails. If Prometheus is unreachable
//! the panel shows every service as `unknown` (grey) instead of erroring — a
//! monitoring tool that goes down loudly when its data source blinks is worse
//! than useless, because people learn to ignore it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A 99th-percentile processing time above this is "degraded", not healthy.
const LATENCY_WARN_SECONDS: f64 = 0.4;

/// Unprocessed messages waiting on a consumer before it counts as backed up.
const QUEUE_WARN_DEPTH: u64 = 100;

/// How far below its upstream a stage may sit before it counts as behind.
///
/// Nothing measured across separate scrapes is ever exactly equal, so a small
/// shortfall is noise. Ten per cent is wide enough to ignore that and narrow
/// enough to catch a stage that is genuinely not keeping up.
const CONTINUITY_TOLERANCE: f64 = 0.9;

/// And an absolute floor, because a percentage is meaningless at low rates.
///
/// At one order a second, "ten per cent behind" is a tenth of an order — well
/// inside the rounding of two scrapes taken a moment apart. Without this the
/// panel would cry wolf on every idle system, which is the fastest way to teach
/// somebody to ignore it.
const CONTINUITY_FLOOR: f64 = 0.5;

/// How much a reading must move before it counts as moving at all.
const TREND_TOLERANCE: f64 = 0.05;

/// The chain, in order, and what each stage is called in a sentence.
///
/// This is the conservation law the whole diagnosis rests on: every order
/// accepted must be relayed, every relayed order processed, and every processed
/// order reacted to by both subscribers. In steady state these five numbers are
/// the same number. Where they stop being the same is where the fault is.
const CHAIN: [&str; 5] = [
    "the gateway",
    "the outbox relay",
    "the worker",
    "the notifier",
    "the audit service",
];

/// Finds the first stage that is not keeping up with the one before it.
///
/// Returns the index of the *downstream* stage and how far behind it is.
///
/// The **first** one, walking downstream, because gaps cascade: if the relay
/// has stalled then the worker, notifier and audit service are all starved as
/// well, and reporting four faults where there is one sends somebody to look in
/// three wrong places.
///
/// `None` when everything keeps up, when the chain is idle, or when any reading
/// is missing — an unknown is not a fault, and must not be reported as one.
fn first_stage_behind(rates: &[Option<f64>; 5]) -> Option<(usize, f64)> {
    for index in 1..rates.len() {
        let (Some(upstream), Some(downstream)) = (rates[index - 1], rates[index]) else {
            return None;
        };

        let shortfall = upstream - downstream;

        if downstream < upstream * CONTINUITY_TOLERANCE && shortfall > CONTINUITY_FLOOR {
            return Some((index, shortfall));
        }
    }

    None
}

/// Reads all the evidence and reaches one conclusion.
///
/// A pure function of plain values, deliberately: it is the only part of this
/// service where being wrong is quietly dangerous, so it is the part that has
/// to be exhaustively testable without a broker, a database or a clock.
///
/// The order of the checks is the ranking. It runs most-certain first and
/// most-specific before most-general, because an operator acts on the first
/// sentence they read:
///
/// 1. Nothing can be concluded at all.
/// 2. Something is down — no diagnosis needed, it is stated.
/// 3. A stage is behind — the most specific thing this panel can say, and it
///    names one component rather than a symptom.
/// 4. Something is degraded but the chain still balances.
/// 5. Messages are parked. Nothing is failing now, but work has been lost and
///    nothing will recover it without a person.
/// 6. Normal.
fn decide(
    sources_ok: bool,
    worst_node: Status,
    behind: Option<(usize, f64)>,
    dead_letters: Option<u64>,
) -> Verdict {
    // Rule one, and it outranks everything: if the measurements are not
    // arriving, every other conclusion on this page is about a stale picture.
    // "Everything looks fine" is the single most dangerous thing a panel can
    // say when it has stopped being told anything.
    if !sources_ok {
        return Verdict {
            level: Status::Unknown,
            headline: "Cannot tell".to_owned(),
            detail: "Prometheus is not answering, so every reading on this page \
                     is the last one that arrived rather than the current one."
                .to_owned(),
            action: "Check that Prometheus is running before trusting anything below.".to_owned(),
            runbook: "sources".to_owned(),
        };
    }

    if worst_node == Status::Down {
        return Verdict {
            level: Status::Down,
            headline: "Down".to_owned(),
            detail: "A component is not answering. Its lamp is red below.".to_owned(),
            action: "Find the red lamp, then read its container's logs.".to_owned(),
            runbook: "down".to_owned(),
        };
    }

    if let Some((index, shortfall)) = behind {
        let stage = CHAIN[index];
        let upstream = CHAIN[index - 1];

        return Verdict {
            level: Status::Degraded,
            headline: "Falling behind".to_owned(),
            detail: format!(
                "{stage} is not keeping up with {upstream} — about {shortfall:.1} orders a \
                 second are going in that are not coming out. Every stage before it is fine, \
                 so the fault is there rather than upstream."
            ),
            action: format!("Read the logs for {stage}, and check whether its queue is growing."),
            runbook: "behind".to_owned(),
        };
    }

    if worst_node == Status::Degraded {
        return Verdict {
            level: Status::Degraded,
            headline: "Degraded".to_owned(),
            detail: "Everything is still flowing and the chain balances, but a component \
                     is reporting trouble — an amber lamp below says which."
                .to_owned(),
            action: "Read the amber lamp's detail line; it says what tripped it.".to_owned(),
            runbook: "degraded".to_owned(),
        };
    }

    if let Some(parked) = dead_letters.filter(|count| *count > 0) {
        return Verdict {
            level: Status::Degraded,
            headline: "Work is parked".to_owned(),
            detail: format!(
                "The chain is flowing normally, but {parked} messages failed three times \
                 and were set aside. They are not being retried, and nothing will pick \
                 them up on its own."
            ),
            action: "Run DevReplay.cmd --dry-run to see what would come back.".to_owned(),
            runbook: "parked".to_owned(),
        };
    }

    Verdict {
        level: Status::Healthy,
        headline: "Normal".to_owned(),
        detail: "Every component is answering, and every order accepted is being relayed, \
                 processed and reacted to at the same rate."
            .to_owned(),
        action: String::new(),
        runbook: String::new(),
    }
}

/// Which way a reading moved, given the one before it.
///
/// Proportional rather than absolute, because the same rule has to serve a rate
/// of 7 a second and a store of 226,000 spans. A fixed threshold would call
/// every rate steady or every count volatile.
pub fn trend_between(previous: Option<f64>, current: f64) -> Trend {
    let Some(previous) = previous else {
        return Trend::Unknown;
    };

    // Against the larger of the two, so a fall to zero is measured against
    // where it fell from rather than against nothing.
    let scale = previous.abs().max(current.abs());
    if scale == 0.0 {
        return Trend::Steady;
    }

    let change = (current - previous) / scale;

    if change > TREND_TOLERANCE {
        Trend::Rising
    } else if change < -TREND_TOLERANCE {
        Trend::Falling
    } else {
        Trend::Steady
    }
}

/// How recently a target must have been scraped to count as present.
///
/// Comfortably longer than the 2-second scrape interval, so an ordinary missed
/// scrape does not flap the lamp, and far shorter than Prometheus' five-minute
/// default lookback, which would let a dead service read healthy for minutes.
const FRESHNESS_WINDOW: &str = "30s";

/// What a lamp on the panel can say.
///
/// `Unknown` exists deliberately: "I cannot tell" is different from "it is
/// down", and a panel that conflates them teaches people to distrust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

/// One equipment block on the panel.
///
/// `PartialEq` is derived so the aggregator can ask "would this draw the same
/// screen?" and skip a redraw when nothing moved. See [`crate::live::same_picture`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Matches an element id in the drawing, which is how the browser knows what
    /// to repaint. A typo here shows up as a lamp that never changes.
    pub id: String,
    pub status: Status,
    /// Short line under the name — replica count, or why it is amber.
    pub detail: String,
}

/// Which way a reading has moved since the one before it.
///
/// A single number cannot say whether a system is failing or recovering, and
/// that is the difference between "watch it" and "do something now". Two
/// readings can.
///
/// `Unknown` is not `Steady`. It means there is nothing to compare against —
/// the first reading after a restart, or a value that is not a number — and
/// drawing that as "steady" would be a confident claim about a measurement
/// that was never made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    Rising,
    Steady,
    Falling,
    Unknown,
}

/// One reading in the instrument column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gauge {
    pub id: String,
    pub value: String,
    /// Draws the badge in warning colour when true.
    pub warn: bool,
    /// Which way it moved since the previous reading.
    pub trend: Trend,
}

/// What an operator should conclude, right now, in one sentence.
///
/// Everything else on this panel is evidence. This is the reading of it.
///
/// It exists because a control room is not a place to do arithmetic. Five rates
/// that should match, a queue depth, a percentile and nine lamps are a puzzle;
/// somebody arriving at an alarm needs the answer, and then the evidence to
/// check it against — not the other way round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub level: Status,
    /// One or two words, large: "Normal", "Degraded", "Cannot tell".
    pub headline: String,
    /// One sentence on what is true.
    pub detail: String,
    /// One sentence on what to do about it. Empty when there is nothing to do.
    pub action: String,
    /// Anchor in the runbook page that covers this, or empty.
    pub runbook: String,
}

/// Something worth putting in the banner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub text: String,
    pub severity: Status,
}

/// Everything the panel needs for one repaint.
///
/// Deliberately *not* `PartialEq`: `generated_at` changes on every reading, so
/// a derived equality would report "different" forever — which is the very bug
/// change detection exists to avoid. [`crate::live::same_picture`] compares the
/// fields that actually draw something.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub generated_at: DateTime<Utc>,
    pub nodes: Vec<Node>,
    pub gauges: Vec<Gauge>,
    pub alarms: Vec<Alarm>,
    /// The one-sentence reading of everything above.
    pub verdict: Verdict,
    /// True when Prometheus answered. The panel says so rather than pretending
    /// a stale picture is current.
    pub sources_ok: bool,
}

/// Percent-encodes a string for use in a web address query value.
///
/// Written by hand rather than pulling in a crate: reqwest's `.query()` helper
/// needs features this workspace switched off to keep the container build
/// small, and PromQL is full of characters — `{}`, `()`, `"`, spaces — that
/// must not travel raw in a web address.
///
/// Only the Request for Comments 3986 *unreserved* set passes through
/// untouched. Everything else becomes `%XX`, which is always safe if
/// occasionally verbose.
fn percent_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len() * 2);

    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            // `{:02X}` formats one byte as two uppercase hex digits.
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }

    encoded
}

/// Decides a service's lamp from what Prometheus could tell us.
///
/// Pure, and separate from the Hypertext Transfer Protocol call, because
/// *this* is the part worth testing: the difference between "cannot tell" and
/// "gone" is the whole difference between a panel people trust and one they
/// learn to ignore.
fn classify_service(sources_ok: bool, instances_up: Option<u32>) -> (Status, String) {
    if !sources_ok {
        // Prometheus itself is unreachable, so nothing can be said about
        // anything. This is the only honest "unknown".
        return (Status::Unknown, "no data".to_owned());
    }

    match instances_up {
        // Prometheus is answering and has no recent series at all for a
        // service this panel expects. That is not uncertainty - the target has
        // vanished from discovery, which is what happens when the container
        // stops. Reading it as "unknown" would hide an outage behind a grey
        // lamp.
        None => (Status::Down, "absent".to_owned()),
        Some(0) => (Status::Down, "not responding".to_owned()),
        Some(count) => {
            let plural = if count == 1 { "instance" } else { "instances" };
            (Status::Healthy, format!("{count} {plural}"))
        }
    }
}

/// Decides whether a reachable worker is nonetheless in trouble.
///
/// Returns the replacement detail line and the alarm text, or `None` when the
/// worker is genuinely fine. Latency wins over backlog when both are breached:
/// a slow consumer is the *cause*, and a queue building up behind it is the
/// symptom, so naming the cause is more use to whoever is reading the alarm.
fn classify_worker_health(p99_seconds: Option<f64>, backlog: u64) -> Option<(String, String)> {
    if let Some(seconds) = p99_seconds.filter(|value| *value > LATENCY_WARN_SECONDS) {
        return Some((
            format!("99th percentile {:.0} milliseconds", seconds * 1000.0),
            format!(
                "worker-service — 99th percentile {:.0} milliseconds — threshold {:.0} milliseconds",
                seconds * 1000.0,
                LATENCY_WARN_SECONDS * 1000.0
            ),
        ));
    }

    if backlog > QUEUE_WARN_DEPTH {
        return Some((
            format!("{backlog} queued"),
            format!("worker-service — {backlog} messages queued — threshold {QUEUE_WARN_DEPTH}"),
        ));
    }

    None
}

/// Polls Prometheus and NATS.
#[derive(Clone)]
pub struct Collector {
    client: reqwest::Client,
    prometheus_url: String,
    nats_monitor_url: String,
    /// The previous numeric value of every reading, so each one can say which
    /// way it moved.
    ///
    /// `Arc<Mutex<…>>` rather than a plain field because this type derives
    /// `Clone` and every clone must share the same memory — a per-clone copy
    /// would mean each caller comparing against its own private history and
    /// every reading reporting `Unknown` forever.
    ///
    /// A `Mutex` and not a channel or an atomic: the lock is held for a map
    /// lookup and an insert, with no `.await` anywhere inside it, which is the
    /// condition that makes a blocking lock safe in asynchronous code.
    previous: std::sync::Arc<std::sync::Mutex<BTreeMap<String, f64>>>,
}

impl Collector {
    pub fn new(prometheus_url: String, nats_monitor_url: String) -> Self {
        Self {
            // Timeouts, not defaults.
            //
            // A default client waits forever. A Prometheus that accepts the
            // connection and then answers nothing — a black hole rather than a
            // refusal — would otherwise hang this call indefinitely, and the
            // whole picture is built from a sequence of these. The panel would
            // sit serenely showing a twenty-minute-old world, which is strictly
            // worse than saying it cannot reach anything.
            //
            // Two seconds is generous against a scrape interval of two: a query
            // that slow is a broken source, not a busy one.
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .connect_timeout(std::time::Duration::from_millis(500))
                .build()
                .unwrap_or_default(),
            prometheus_url,
            nats_monitor_url,
            previous: std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }

    /// Records a reading and says which way it moved since the last one.
    ///
    /// A poisoned lock — one whose holder panicked — is treated as "no history"
    /// rather than propagated. Losing the direction of an arrow is not worth
    /// taking the panel down for, and the next reading repairs it.
    fn trend_for(&self, id: &str, current: Option<f64>) -> Trend {
        let Some(current) = current else {
            return Trend::Unknown;
        };

        let Ok(mut previous) = self.previous.lock() else {
            return Trend::Unknown;
        };

        let trend = trend_between(previous.get(id).copied(), current);
        previous.insert(id.to_owned(), current);
        trend
    }

    /// Runs one instant query and returns the first sample's value.
    ///
    /// Returns `None` for anything that goes wrong — unreachable, malformed,
    /// or simply no data yet. The caller decides what an absent number means,
    /// which is usually "unknown" rather than "zero".
    async fn scalar(&self, query: &str) -> Option<f64> {
        let url = format!(
            "{}/api/v1/query?query={}",
            self.prometheus_url,
            percent_encode(query)
        );

        let response = self.client.get(&url).send().await.ok()?;

        let body: serde_json::Value = response.json().await.ok()?;

        body.get("data")?
            .get("result")?
            .as_array()?
            .first()?
            .get("value")?
            .as_array()?
            .get(1)?
            .as_str()?
            .parse()
            .ok()
    }

    /// How many instances of a service Prometheus has scraped *recently*.
    ///
    /// `up` is Prometheus' own per-target metric, so this reflects
    /// reachability rather than anything the service reports about itself.
    ///
    /// # Why `last_over_time` and not a bare `up`
    ///
    /// A plain instant query on `up` looks back five minutes by default. When
    /// a container disappears entirely its target leaves service discovery, no
    /// new samples are written, and that query keeps cheerfully returning the
    /// last value it saw — so a service that died two minutes ago still reads
    /// as healthy. On a monitoring panel that is the worst possible failure:
    /// confidently wrong.
    ///
    /// Restricting to a short window means "no sample in the last
    /// [`FRESHNESS_WINDOW`]" produces no result at all, which the caller reads
    /// as absent rather than fine.
    /// # Why `service_info` and not `up`
    ///
    /// `up` means "something answered at the address name resolution gave me"
    /// — it says nothing about *what* answered. Docker recycles container IP
    /// addresses, so a deleted service's address can be reassigned to a new
    /// container; Prometheus keeps scraping it, gets a valid response from a
    /// completely different service, and reports the dead one as up. Observed
    /// here: the worker's old address was handed to this very service, and the
    /// panel showed a service with zero running containers as healthy.
    ///
    /// `service_info` is emitted by each process with its own name, so this
    /// counts processes that claim to be the service rather than addresses
    /// that happen to respond.
    async fn instances_up(&self, service: &str) -> Option<u32> {
        self.scalar(&format!(
            "count(last_over_time(service_info{{service=\"{service}\"}}[{FRESHNESS_WINDOW}]))"
        ))
        .await
        .map(|value| value as u32)
    }

    /// Pending messages per JetStream consumer, from the NATS monitoring port.
    ///
    /// Prometheus does not know about queue depth — nothing exports it — so
    /// this reads NATS' own `/jsz` endpoint directly.
    async fn queue_depths(&self) -> BTreeMap<String, u64> {
        let mut depths = BTreeMap::new();

        let url = format!("{}/jsz?streams=1&consumers=1", self.nats_monitor_url);
        let Ok(response) = self.client.get(&url).send().await else {
            return depths;
        };
        let Ok(body) = response.json::<serde_json::Value>().await else {
            return depths;
        };

        let accounts = body
            .get("account_details")
            .and_then(serde_json::Value::as_array);

        for account in accounts.into_iter().flatten() {
            let streams = account
                .get("stream_detail")
                .and_then(serde_json::Value::as_array);

            for stream in streams.into_iter().flatten() {
                // Stream-level message count, used for the dead-letter lamp.
                if let (Some(name), Some(messages)) = (
                    stream.get("name").and_then(serde_json::Value::as_str),
                    stream
                        .get("state")
                        .and_then(|s| s.get("messages"))
                        .and_then(serde_json::Value::as_u64),
                ) {
                    depths.insert(format!("stream:{name}"), messages);
                }

                let consumers = stream
                    .get("consumer_detail")
                    .and_then(serde_json::Value::as_array);

                for consumer in consumers.into_iter().flatten() {
                    if let (Some(name), Some(pending)) = (
                        consumer.get("name").and_then(serde_json::Value::as_str),
                        consumer
                            .get("num_pending")
                            .and_then(serde_json::Value::as_u64),
                    ) {
                        depths.insert(format!("consumer:{name}"), pending);
                    }
                }
            }
        }

        depths
    }

    /// Builds one complete picture of the system.
    pub async fn snapshot(&self) -> Snapshot {
        let depths = self.queue_depths().await;

        // A single cheap query decides whether Prometheus is answering at all.
        let sources_ok = self.scalar("vector(1)").await.is_some();

        let p99 = self
            .scalar(
                "histogram_quantile(0.99, sum by (le) \
                 (rate(order_processing_duration_seconds_bucket[5m])))",
            )
            .await;

        // Kept as `Option`, not flattened to zero.
        //
        // These two come from the monitoring endpoint on the broker, and when
        // the broker is unreachable the honest answer is "I cannot tell". A
        // zero here would be a *confident* claim that the queue is empty and no
        // message is parked — which is exactly the reassuring lie a panel must
        // never tell, and it reads identically to a healthy idle system.
        //
        // Everything downstream now has to decide what to do about "unknown",
        // which is the point.
        let worker_backlog = depths.get("consumer:order-worker").copied();
        let dead_letters = depths.get("stream:ORDER_DLQ").copied();

        let mut nodes = Vec::new();
        let mut alarms = Vec::new();

        // ---- the services, one lamp each ----
        for (id, service) in [
            ("gateway", "gateway-service"),
            ("echo", "echo-service"),
            ("relay", "outbox-relay"),
            ("worker", "worker-service"),
            ("notifier", "notifier-service"),
            ("audit", "audit-service"),
        ] {
            let up = if sources_ok {
                self.instances_up(service).await
            } else {
                None
            };

            let (status, detail) = classify_service(sources_ok, up);

            if status == Status::Down {
                alarms.push(Alarm {
                    text: format!("{service} — not responding"),
                    severity: Status::Down,
                });
            }

            nodes.push(Node {
                id: id.to_owned(),
                status,
                detail,
            });
        }

        // ---- the worker earns amber on its own terms ----
        //
        // Reachable but slow, or reachable but falling behind, are both real
        // problems that `up` alone cannot see.
        if let Some(worker) = nodes
            .iter_mut()
            .find(|node| node.id == "worker" && node.status == Status::Healthy)
            // An unknown backlog cannot make the worker amber. Not knowing is
            // not evidence of a problem.
            && let Some((detail, alarm)) =
                classify_worker_health(p99, worker_backlog.unwrap_or(0))
        {
            worker.status = Status::Degraded;
            worker.detail = detail;
            alarms.push(Alarm {
                text: alarm,
                severity: Status::Degraded,
            });
        }

        // ---- infrastructure, inferred rather than scraped ----
        //
        // Nothing exports `up` for these, so their lamps follow the evidence
        // that they are working: NATS answered its monitoring port, and the
        // services that depend on Postgres and Redis are alive.
        let nats_ok = !depths.is_empty();
        nodes.push(Node {
            id: "nats".to_owned(),
            status: if nats_ok {
                Status::Healthy
            } else {
                Status::Unknown
            },
            detail: if nats_ok {
                format!(
                    "{} streams",
                    depths.keys().filter(|k| k.starts_with("stream:")).count()
                )
            } else {
                "no data".to_owned()
            },
        });

        let db_dependents_up = nodes
            .iter()
            .filter(|n| ["gateway", "relay", "audit"].contains(&n.id.as_str()))
            .all(|n| n.status == Status::Healthy || n.status == Status::Degraded);

        nodes.push(Node {
            id: "postgres".to_owned(),
            status: if !sources_ok {
                Status::Unknown
            } else if db_dependents_up {
                Status::Healthy
            } else {
                Status::Unknown
            },
            detail: "orders · outbox · audit".to_owned(),
        });

        nodes.push(Node {
            id: "redis".to_owned(),
            status: if !sources_ok {
                Status::Unknown
            } else {
                nodes
                    .iter()
                    .find(|n| n.id == "worker")
                    .map(|w| {
                        if w.status == Status::Down {
                            Status::Unknown
                        } else {
                            Status::Healthy
                        }
                    })
                    .unwrap_or(Status::Unknown)
            },
            detail: "idempotency keys".to_owned(),
        });

        // ---- dead letters are their own alarm ----
        if dead_letters.unwrap_or(0) > 0 {
            let dead_letters = dead_letters.unwrap_or(0);
            alarms.push(Alarm {
                text: format!(
                    "{dead_letters} message{} in the dead-letter queue — DevReplay.cmd to return them",
                    if dead_letters == 1 { "" } else { "s" }
                ),
                severity: Status::Degraded,
            });
        }

        if !sources_ok {
            alarms.push(Alarm {
                // Says what the code does, not what would be nicer. There is no
                // "last known state" anywhere: an unreachable Prometheus makes
                // every service lamp grey, which is a claim of ignorance, not a
                // stale reading. Describing it as the latter would send someone
                // looking for a cached value that does not exist.
                text: "Prometheus unreachable — service lamps show unknown, not a stale reading"
                    .to_owned(),
                severity: Status::Unknown,
            });
        }

        // ---- the readouts ----
        //
        // Each value is the bare number, and nothing here carries a unit.
        //
        // These used to be read in an instrument column down the side of the
        // panel, where each tile had room to say "orders per second" in full.
        // They are now badges attached to the thing each one measures, which is
        // a better place to read a number and a far worse place to write four
        // words. The panel resolves that with a hover: the badge shows the
        // figure, and pausing on it names the reading, its unit and what it
        // counts. So the wording still lives in the panel rather than here —
        // just in `data-title`, `data-unit` and `data-note` on the badge rather
        // than in a label beside a tile.
        // The five stages of the chain, read once. They are drawn as badges and
        // also compared against each other, and reading them twice would let
        // the picture and the diagnosis disagree about the same number.
        let chain = [
            self.scalar("sum(rate(orders_accepted_total[15s]))").await,
            self.scalar("sum(rate(outbox_relayed_total[15s]))").await,
            self.scalar("sum(rate(orders_processed_total[15s]))").await,
            self.scalar("sum(rate(events_handled_total{service=\"notifier-service\"}[15s]))")
                .await,
            self.scalar("sum(rate(events_handled_total{service=\"audit-service\"}[15s]))")
                .await,
        ];

        let gauges = vec![
            self.rate_gauge_of("g-accepted", chain[0]),
            self.rate_gauge_of("g-relayed", chain[1]),
            self.rate_gauge_of("g-processed", chain[2]),
            self.rate_gauge_of("g-notified", chain[3]),
            self.rate_gauge_of("g-audited", chain[4]),
            // The em dash is the "no reading" mark used everywhere on this
            // panel. It is not a zero, and the difference matters most exactly
            // when the broker is the thing that has gone wrong.
            Gauge {
                id: "g-queue".to_owned(),
                value: worker_backlog.map_or_else(|| "—".to_owned(), |depth| depth.to_string()),
                warn: worker_backlog.is_some_and(|depth| depth > QUEUE_WARN_DEPTH),
                trend: self.trend_for("g-queue", worker_backlog.map(|depth| depth as f64)),
            },
            Gauge {
                id: "g-dlq".to_owned(),
                value: dead_letters.map_or_else(|| "—".to_owned(), |count| count.to_string()),
                warn: dead_letters.is_some_and(|count| count > 0),
                trend: self.trend_for("g-dlq", dead_letters.map(|count| count as f64)),
            },
            Gauge {
                id: "g-p99".to_owned(),
                value: match p99 {
                    Some(seconds) => format!("{:.0}", seconds * 1000.0),
                    None => "—".to_owned(),
                },
                warn: p99.is_some_and(|seconds| seconds > LATENCY_WARN_SECONDS),
                trend: self.trend_for("g-p99", p99),
            },
            // ---- the three tools, two readings each ----
            //
            // Until now these were drawn on the panel with no numbers at all,
            // which left an operator asking "is the tracing still recording?"
            // with nowhere to look. Each gets a *stock* and a *flow*: how much
            // it holds, and whether anything is still arriving. Neither answers
            // the other's question — a large store with no inflow is a system
            // that stopped ten minutes ago, and reads as healthy if you only
            // look at the total.
            self.count_gauge("g-prom-full", "prometheus_tsdb_head_series")
                .await,
            self.rate_gauge(
                "g-prom-flow",
                "rate(prometheus_tsdb_head_samples_appended_total[1m])",
            )
            .await,
            // Not `spans_received_total`, which stays at zero forever here:
            // spans arrive over OpenTelemetry Protocol and are counted as
            // saved, not received. Measured on the running stack — the received
            // counter read 0 while 225,035 spans were sitting in storage.
            self.count_gauge(
                "g-jaeger-full",
                "sum(jaeger_collector_spans_saved_by_svc_total)",
            )
            .await,
            self.rate_gauge(
                "g-jaeger-flow",
                "sum(rate(jaeger_collector_spans_saved_by_svc_total[1m]))",
            )
            .await,
            // The synchronous path. It crosses no message bus, so the tap
            // cannot see it and this counter is the only evidence the panel can
            // have that the call still works.
            self.rate_gauge("g-echo-flow", "sum(rate(relay_calls_total[15s]))")
                .await,
            self.count_gauge("g-grafana-full", "grafana_stat_totals_dashboard")
                .await,
            self.rate_gauge(
                "g-grafana-flow",
                "sum(rate(grafana_http_request_duration_seconds_count[1m]))",
            )
            .await,
        ];

        // The reading of everything above. Computed from the worst lamp rather
        // than from any single one, so a component that is down cannot be
        // hidden by eight that are fine.
        let worst_node = nodes
            .iter()
            .map(|node| node.status)
            .max_by_key(|status| match status {
                Status::Healthy => 0,
                Status::Unknown => 1,
                Status::Degraded => 2,
                Status::Down => 3,
            })
            .unwrap_or(Status::Unknown);

        let verdict = decide(
            sources_ok,
            worst_node,
            first_stage_behind(&chain),
            dead_letters,
        );

        Snapshot {
            generated_at: Utc::now(),
            nodes,
            gauges,
            verdict,
            alarms,
            sources_ok,
        }
    }

    /// A whole-number count, shortened so it fits the badge on the drawing.
    ///
    /// 4579 becomes `4.6k` and 225035 becomes `225k`. The badge is 56 units
    /// wide and holds about five characters, so an unshortened count would
    /// either overflow its box or force every badge on the panel to be sized
    /// for the widest number any of them might ever reach.
    ///
    /// Shortening is a display decision, not a measurement one: the exact value
    /// is a Prometheus query away, and an operator reading "how full is it"
    /// wants the magnitude rather than the units digit.
    async fn count_gauge(&self, id: &str, query: &str) -> Gauge {
        let raw = self.scalar(query).await;

        Gauge {
            id: id.to_owned(),
            value: match raw {
                Some(count) => Self::compact(count),
                None => "—".to_owned(),
            },
            warn: false,
            trend: self.trend_for(id, raw),
        }
    }

    /// Shortens a count to at most five characters.
    ///
    /// `f64` rather than an integer type because that is what the Prometheus
    /// programming interface returns for every value, counts included.
    fn compact(count: f64) -> String {
        if count < 1_000.0 {
            format!("{count:.0}")
        } else if count < 10_000.0 {
            // One decimal below ten thousand, where the difference between
            // 4.6k and 4.9k is still worth seeing.
            format!("{:.1}k", count / 1_000.0)
        } else if count < 1_000_000.0 {
            format!("{:.0}k", count / 1_000.0)
        } else {
            format!("{:.1}M", count / 1_000_000.0)
        }
    }

    /// A per-second rate as a bare number, to two decimal places.
    ///
    /// An em dash rather than a zero when Prometheus has nothing: "no reading"
    /// and "a reading of zero" mean different things, and a panel that shows
    /// 0.00 for a target it cannot reach is lying quietly.
    async fn rate_gauge(&self, id: &str, query: &str) -> Gauge {
        let raw = self.scalar(query).await;
        self.rate_gauge_of(id, raw)
    }

    /// The same, from a value already measured.
    ///
    /// The five rates along the chain are read once and used twice — drawn as
    /// badges, and compared against each other for the continuity check — so
    /// they are fetched separately and handed here rather than queried again.
    /// Querying twice would risk the badge and the diagnosis disagreeing about
    /// the same number.
    fn rate_gauge_of(&self, id: &str, raw: Option<f64>) -> Gauge {
        Gauge {
            id: id.to_owned(),
            value: match raw {
                Some(rate) => format!("{rate:.2}"),
                None => "—".to_owned(),
            },
            warn: false,
            trend: self.trend_for(id, raw),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serialises_to_the_names_the_panel_expects() {
        // The browser switches on these strings, so a rename here silently
        // breaks every lamp. Pin them.
        assert_eq!(
            serde_json::to_string(&Status::Healthy).unwrap(),
            "\"healthy\""
        );
        assert_eq!(
            serde_json::to_string(&Status::Degraded).unwrap(),
            "\"degraded\""
        );
        assert_eq!(serde_json::to_string(&Status::Down).unwrap(), "\"down\"");
        assert_eq!(
            serde_json::to_string(&Status::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    // ---- the distinction the whole panel rests on ----

    #[test]
    fn an_unreachable_prometheus_makes_everything_unknown() {
        // Not down. We genuinely cannot tell, and saying "down" would raise a
        // false alarm for every service at once.
        let (status, detail) = classify_service(false, None);
        assert_eq!(status, Status::Unknown);
        assert_eq!(detail, "no data");

        // Even a stale positive reading means nothing without a live source.
        assert_eq!(classify_service(false, Some(3)).0, Status::Unknown);
    }

    #[test]
    fn a_service_missing_from_a_healthy_prometheus_is_down_not_unknown() {
        // The bug this panel was built on: a vanished target produces no
        // series at all, and reading that as "unknown" hid a dead service
        // behind a grey lamp for minutes.
        let (status, detail) = classify_service(true, None);
        assert_eq!(status, Status::Down);
        assert_eq!(detail, "absent");
    }

    #[test]
    fn zero_reachable_instances_is_down() {
        let (status, detail) = classify_service(true, Some(0));
        assert_eq!(status, Status::Down);
        assert_eq!(detail, "not responding");
    }

    #[test]
    fn instance_counts_read_naturally() {
        assert_eq!(classify_service(true, Some(1)).1, "1 instance");
        assert_eq!(classify_service(true, Some(2)).1, "2 instances");
        assert_eq!(classify_service(true, Some(7)).1, "7 instances");
        assert_eq!(classify_service(true, Some(1)).0, Status::Healthy);
    }

    // ---- amber ----

    #[test]
    fn a_quick_worker_with_an_empty_queue_is_not_degraded() {
        assert!(classify_worker_health(Some(0.05), 0).is_none());
        // No latency data yet is not a problem either.
        assert!(classify_worker_health(None, 0).is_none());
    }

    #[test]
    fn a_slow_worker_is_degraded() {
        let (detail, alarm) = classify_worker_health(Some(0.85), 0).unwrap();
        assert_eq!(detail, "99th percentile 850 milliseconds");
        assert!(alarm.contains("850 milliseconds"));
        assert!(alarm.contains("threshold 400 milliseconds"));
    }

    #[test]
    fn a_backed_up_worker_is_degraded() {
        let (detail, alarm) = classify_worker_health(Some(0.01), 500).unwrap();
        assert_eq!(detail, "500 queued");
        assert!(alarm.contains("500 messages queued"));
    }

    #[test]
    fn latency_is_reported_ahead_of_backlog() {
        // Both breached. Latency is the cause and the backlog is the symptom,
        // so naming the cause is more use to whoever reads the alarm.
        let (detail, _) = classify_worker_health(Some(0.9), 900).unwrap();
        assert_eq!(detail, "99th percentile 900 milliseconds");
    }

    #[test]
    fn the_thresholds_are_exclusive() {
        // Exactly at the threshold is still fine; only above it is a problem.
        assert!(classify_worker_health(Some(LATENCY_WARN_SECONDS), QUEUE_WARN_DEPTH).is_none());
        assert!(classify_worker_health(Some(LATENCY_WARN_SECONDS + 0.001), 0).is_some());
        assert!(classify_worker_health(None, QUEUE_WARN_DEPTH + 1).is_some());
    }

    /// Five rates that all match, at a rate worth measuring.
    fn balanced() -> [Option<f64>; 5] {
        [Some(7.0), Some(7.0), Some(7.0), Some(7.0), Some(7.0)]
    }

    #[test]
    fn a_balanced_chain_has_no_fault() {
        assert_eq!(first_stage_behind(&balanced()), None);

        // Idle is balanced too. A system doing nothing is not a system failing.
        assert_eq!(first_stage_behind(&[Some(0.0); 5]), None);
    }

    #[test]
    fn a_missing_reading_is_never_reported_as_a_fault() {
        // An unknown is not a shortfall, and saying "the worker is behind"
        // because a number failed to arrive would send somebody to read the
        // logs of a service that is working perfectly.
        let mut rates = balanced();
        rates[2] = None;
        assert_eq!(first_stage_behind(&rates), None);
    }

    #[test]
    fn the_first_stage_behind_is_the_one_reported() {
        // The relay has stalled, so everything downstream is starved as well.
        // Only the relay should be named: reporting four faults where there is
        // one sends somebody to look in three wrong places.
        let rates = [Some(7.0), Some(1.0), Some(1.0), Some(1.0), Some(1.0)];
        let (index, shortfall) = first_stage_behind(&rates).expect("a fault");
        assert_eq!(CHAIN[index], "the outbox relay");
        assert!((shortfall - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn small_gaps_at_low_rates_are_not_faults() {
        // Half an order a second between two scrapes is measurement noise, not
        // a stalled service. Without the floor the panel cries wolf whenever
        // the system is nearly idle, which is most of the time.
        assert_eq!(
            first_stage_behind(&[Some(1.0), Some(0.6), Some(0.6), Some(0.6), Some(0.6)]),
            None
        );

        // The same *proportional* gap at a rate worth measuring is real.
        assert!(
            first_stage_behind(&[Some(50.0), Some(30.0), Some(30.0), Some(30.0), Some(30.0)])
                .is_some()
        );
    }

    #[test]
    fn unreachable_sources_outrank_every_other_conclusion() {
        // Even with everything else looking catastrophic, the honest answer is
        // that nothing can be concluded — the readings are not arriving.
        let verdict = decide(false, Status::Down, Some((1, 9.0)), Some(500));
        assert_eq!(verdict.level, Status::Unknown);
        assert_eq!(verdict.headline, "Cannot tell");
    }

    #[test]
    fn a_stage_behind_is_named_ahead_of_a_general_degradation() {
        // Both are true. The specific one is more useful, so it is the one an
        // operator reads first.
        let verdict = decide(true, Status::Degraded, Some((2, 4.0)), Some(0));
        assert_eq!(verdict.headline, "Falling behind");
        assert!(verdict.detail.contains("the worker"));
        assert!(verdict.detail.contains("the outbox relay"));
        assert_eq!(verdict.runbook, "behind");
    }

    #[test]
    fn parked_work_is_reported_even_when_everything_flows() {
        let verdict = decide(true, Status::Healthy, None, Some(1_714));
        assert_eq!(verdict.level, Status::Degraded);
        assert!(verdict.detail.contains("1714"));
        assert!(!verdict.action.is_empty(), "a person has to do something");
    }

    #[test]
    fn a_healthy_system_says_so_and_asks_for_nothing() {
        let verdict = decide(true, Status::Healthy, None, Some(0));
        assert_eq!(verdict.level, Status::Healthy);
        assert_eq!(verdict.headline, "Normal");
        assert!(verdict.action.is_empty());
        assert!(verdict.runbook.is_empty());
    }

    #[test]
    fn every_verdict_that_reports_trouble_says_what_to_do() {
        // The whole point of the verdict. A conclusion with no next step is a
        // worry rather than an instruction, and a control room runs on
        // instructions.
        for verdict in [
            decide(false, Status::Healthy, None, Some(0)),
            decide(true, Status::Down, None, Some(0)),
            decide(true, Status::Healthy, Some((1, 5.0)), Some(0)),
            decide(true, Status::Degraded, None, Some(0)),
            decide(true, Status::Healthy, None, Some(1)),
        ] {
            assert_ne!(verdict.level, Status::Healthy);
            assert!(
                !verdict.action.is_empty(),
                "{} has no action",
                verdict.headline
            );
            assert!(
                !verdict.runbook.is_empty(),
                "{} has no runbook",
                verdict.headline
            );
        }
    }

    #[test]
    fn a_reading_reports_which_way_it_moved() {
        assert_eq!(trend_between(None, 7.0), Trend::Unknown);
        assert_eq!(trend_between(Some(7.0), 7.0), Trend::Steady);
        assert_eq!(trend_between(Some(7.0), 9.0), Trend::Rising);
        assert_eq!(trend_between(Some(9.0), 7.0), Trend::Falling);

        // Both zero is steady, not a division by zero.
        assert_eq!(trend_between(Some(0.0), 0.0), Trend::Steady);

        // A one per cent wobble is not a movement.
        assert_eq!(trend_between(Some(100.0), 101.0), Trend::Steady);

        // The same rule has to serve a rate of 7 and a store of 226,000.
        assert_eq!(trend_between(Some(226_000.0), 226_010.0), Trend::Steady);
        assert_eq!(trend_between(Some(226_000.0), 250_000.0), Trend::Rising);
    }

    #[test]
    fn a_count_is_shortened_to_fit_its_badge() {
        // The badge on the drawing is 56 units wide and holds about five
        // characters, so every one of these must come out at five or fewer.
        for (count, expected) in [
            (0.0, "0"),
            (7.0, "7"),
            (999.0, "999"),
            (1_000.0, "1.0k"),
            (4_579.0, "4.6k"),
            // 9_950 is deliberately absent. It divides to 9.949999… in binary
            // and formats as "9.9k" rather than the "10.0k" the arithmetic
            // suggests — true of any language using binary floating point, and
            // a trap to assert either way round.
            (9_900.0, "9.9k"),
            (10_000.0, "10k"),
            (225_035.0, "225k"),
            (999_999.0, "1000k"),
            (1_000_000.0, "1.0M"),
            (12_300_000.0, "12.3M"),
        ] {
            let shortened = Collector::compact(count);
            assert_eq!(shortened, expected, "{count} shortened wrongly");
            assert!(
                shortened.chars().count() <= 5,
                "{shortened} will not fit the badge"
            );
        }
    }

    #[test]
    fn promql_survives_url_encoding() {
        // The characters that actually appear in these queries, and would
        // otherwise break the web address or silently truncate the expression.
        assert_eq!(percent_encode("up"), "up");
        assert_eq!(
            percent_encode("count(up{service=\"worker\"} == 1)"),
            "count%28up%7Bservice%3D%22worker%22%7D%20%3D%3D%201%29"
        );
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn a_snapshot_round_trips_to_json() {
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
                warn: false,
                trend: Trend::Steady,
            }],
            alarms: vec![Alarm {
                text: "worker-service — slow".to_owned(),
                severity: Status::Degraded,
            }],
            verdict: decide(true, Status::Degraded, None, Some(0)),
            sources_ok: true,
        };

        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(json.contains("\"id\":\"worker\""));
        assert!(json.contains("\"status\":\"degraded\""));
        assert!(json.contains("\"sources_ok\":true"));
    }
}
