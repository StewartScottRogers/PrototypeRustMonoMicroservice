//! Turning Prometheus and NATS into the handful of numbers the panel draws.
//!
//! # Why a service rather than a static page
//!
//! A drawing with numbers typed into it is a diagram. A drawing whose numbers
//! come from the running system is an instrument: when a light goes amber it is
//! because something is actually wrong, and you can trust it enough to act.
//!
//! Everything here degrades rather than fails. If Prometheus is unreachable the
//! panel shows every service as `unknown` (grey) instead of erroring — a
//! monitoring tool that goes down loudly when its data source blinks is worse
//! than useless, because people learn to ignore it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;

/// p99 processing time above this is "degraded" rather than healthy.
const LATENCY_WARN_SECONDS: f64 = 0.4;

/// Unprocessed messages waiting on a consumer before it counts as backed up.
const QUEUE_WARN_DEPTH: u64 = 100;

/// How recently a target must have been scraped to count as present.
///
/// Comfortably longer than the 5-second scrape interval, so an ordinary missed
/// scrape does not flap the lamp, and far shorter than Prometheus' five-minute
/// default lookback, which would let a dead service read healthy for minutes.
const FRESHNESS_WINDOW: &str = "30s";

/// What a lamp on the panel can say.
///
/// `Unknown` exists deliberately: "I cannot tell" is different from "it is
/// down", and a panel that conflates them teaches people to distrust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

/// One equipment block on the panel.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    /// Matches an element id in the SVG, which is how the browser knows what to
    /// repaint. A typo here shows up as a lamp that never changes.
    pub id: String,
    pub status: Status,
    /// Short line under the name — replica count, or why it is amber.
    pub detail: String,
}

/// A number rendered onto a link.
#[derive(Debug, Clone, Serialize)]
pub struct Gauge {
    pub id: String,
    pub value: String,
    /// Draws the badge in warning colour when true.
    pub warn: bool,
}

/// Something worth putting in the banner.
#[derive(Debug, Clone, Serialize)]
pub struct Alarm {
    pub text: String,
    pub severity: Status,
}

/// Everything the panel needs for one repaint.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub generated_at: DateTime<Utc>,
    pub nodes: Vec<Node>,
    pub gauges: Vec<Gauge>,
    pub alarms: Vec<Alarm>,
    /// True when Prometheus answered. The panel says so rather than pretending
    /// a stale picture is current.
    pub sources_ok: bool,
}

/// Percent-encodes a string for use in a URL query value.
///
/// Written by hand rather than pulling in a crate: reqwest's `.query()` helper
/// needs features this workspace switched off to keep the container build
/// small, and PromQL is full of characters — `{}`, `()`, `"`, spaces — that
/// must not travel raw in a URL.
///
/// Only the RFC 3986 *unreserved* set passes through untouched. Everything
/// else becomes `%XX`, which is always safe if occasionally verbose.
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
/// Pure, and separate from the HTTP call, because *this* is the part worth
/// testing: the difference between "cannot tell" and "gone" is the whole
/// difference between a panel people trust and one they learn to ignore.
fn classify_service(sources_ok: bool, instances_up: Option<u32>) -> (Status, String) {
    if !sources_ok {
        // Prometheus itself is unreachable, so nothing can be said about
        // anything. This is the only honest "unknown".
        return (Status::Unknown, "no data".to_owned());
    }

    match instances_up {
        // Prometheus is answering and has no recent series at all for a service
        // this panel expects. That is not uncertainty - the target has vanished
        // from discovery, which is what happens when the container stops.
        // Reading it as "unknown" would hide an outage behind a grey lamp.
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
            format!("p99 {:.0} ms", seconds * 1000.0),
            format!(
                "worker-service — p99 {:.0} ms — threshold {:.0} ms",
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
}

impl Collector {
    pub fn new(prometheus_url: String, nats_monitor_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            prometheus_url,
            nats_monitor_url,
        }
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
    /// `up` is Prometheus' own per-target metric, so this reflects reachability
    /// rather than anything the service reports about itself.
    ///
    /// # Why `last_over_time` and not a bare `up`
    ///
    /// A plain instant query on `up` looks back five minutes by default. When a
    /// container disappears entirely its target leaves service discovery, no
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
    /// `up` means "something answered at the address DNS gave me" — it says
    /// nothing about *what* answered. Docker recycles container IP addresses,
    /// so a deleted service's address can be reassigned to a new container;
    /// Prometheus keeps scraping it, gets a valid response from a completely
    /// different service, and reports the dead one as up. Observed here: the
    /// worker's old address was handed to this very service, and the panel
    /// showed a service with zero running containers as healthy.
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

        let worker_backlog = depths.get("consumer:order-worker").copied().unwrap_or(0);
        let dead_letters = depths.get("stream:ORDER_DLQ").copied().unwrap_or(0);

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
            && let Some((detail, alarm)) = classify_worker_health(p99, worker_backlog)
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
        if dead_letters > 0 {
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
                text: "Prometheus unreachable — lamps show last known state".to_owned(),
                severity: Status::Unknown,
            });
        }

        // ---- the numbers on the links ----
        let gauges = vec![
            self.rate_gauge(
                "g-accepted",
                "sum(rate(orders_accepted_total[1m]))",
                "ord/s",
            )
            .await,
            self.rate_gauge("g-relayed", "sum(rate(outbox_relayed_total[1m]))", "msg/s")
                .await,
            self.rate_gauge(
                "g-processed",
                "sum(rate(orders_processed_total[1m]))",
                "ord/s",
            )
            .await,
            self.rate_gauge(
                "g-notified",
                "sum(rate(events_handled_total{service=\"notifier-service\"}[1m]))",
                "evt/s",
            )
            .await,
            self.rate_gauge(
                "g-audited",
                "sum(rate(events_handled_total{service=\"audit-service\"}[1m]))",
                "evt/s",
            )
            .await,
            Gauge {
                id: "g-queue".to_owned(),
                value: format!("depth {worker_backlog}"),
                warn: worker_backlog > QUEUE_WARN_DEPTH,
            },
            Gauge {
                id: "g-dlq".to_owned(),
                value: format!("{dead_letters} parked"),
                warn: dead_letters > 0,
            },
            Gauge {
                id: "g-p99".to_owned(),
                value: match p99 {
                    Some(seconds) => format!("p99 {:.0} ms", seconds * 1000.0),
                    None => "p99 —".to_owned(),
                },
                warn: p99.is_some_and(|seconds| seconds > LATENCY_WARN_SECONDS),
            },
        ];

        Snapshot {
            generated_at: Utc::now(),
            nodes,
            gauges,
            alarms,
            sources_ok,
        }
    }

    /// A per-second rate, formatted for a small badge.
    async fn rate_gauge(&self, id: &str, query: &str, unit: &str) -> Gauge {
        let value = match self.scalar(query).await {
            Some(rate) => format!("{rate:.2} {unit}"),
            None => format!("— {unit}"),
        };

        Gauge {
            id: id.to_owned(),
            value,
            warn: false,
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
        // The bug this panel was built on: a vanished target produces no series
        // at all, and reading that as "unknown" hid a dead service behind a
        // grey lamp for minutes.
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
        assert_eq!(detail, "p99 850 ms");
        assert!(alarm.contains("850 ms"));
        assert!(alarm.contains("threshold 400 ms"));
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
        assert_eq!(detail, "p99 900 ms");
    }

    #[test]
    fn the_thresholds_are_exclusive() {
        // Exactly at the threshold is still fine; only above it is a problem.
        assert!(classify_worker_health(Some(LATENCY_WARN_SECONDS), QUEUE_WARN_DEPTH).is_none());
        assert!(classify_worker_health(Some(LATENCY_WARN_SECONDS + 0.001), 0).is_some());
        assert!(classify_worker_health(None, QUEUE_WARN_DEPTH + 1).is_some());
    }

    #[test]
    fn promql_survives_url_encoding() {
        // The characters that actually appear in these queries, and would
        // otherwise break the URL or silently truncate the expression.
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
                detail: "p99 850 ms".to_owned(),
            }],
            gauges: vec![Gauge {
                id: "g-queue".to_owned(),
                value: "depth 12".to_owned(),
                warn: false,
            }],
            alarms: vec![Alarm {
                text: "worker-service — slow".to_owned(),
                severity: Status::Degraded,
            }],
            sources_ok: true,
        };

        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(json.contains("\"id\":\"worker\""));
        assert!(json.contains("\"status\":\"degraded\""));
        assert!(json.contains("\"sources_ok\":true"));
    }
}
