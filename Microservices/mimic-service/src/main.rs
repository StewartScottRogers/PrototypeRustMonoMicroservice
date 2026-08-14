//! A live control-room mimic of this stack.
//!
//! Serves one page at `/` showing the real topology. Open it at
//! <http://localhost:8090>.
//!
//! # Three sources, and which is right about what
//!
//! - **The tap** ([`tap`]) is an independent subscriber on the message bus. It
//!   sees every command and every event the instant it is published, which is
//!   what makes the flow on the panel immediate rather than sampled.
//! - **Prometheus** is read every two seconds for the things only it knows:
//!   how many instances answer, and how slow the slowest orders are.
//! - **The NATS monitoring endpoint** supplies queue depth, which is a length
//!   rather than a rate and so cannot be counted from a stream of messages.
//!
//! [`live`] holds the rule deciding which source wins for each reading, and
//! [`push`] sends the result to every open browser over a socket.
//!
//! # What this is for
//!
//! Grafana already shows the numbers, and shows them better — history, zoom,
//! ad-hoc queries. What it cannot show is *shape*: which component is amber,
//! and what sits downstream of it. A mimic panel trades analytical depth for
//! one glance that answers "what is wrong and what does it affect".
//!
//! # Reading this file as a Rust newcomer
//!
//! The page is embedded with `include_str!`, which reads the file **at compile
//! time** and bakes the contents into the binary as a `&'static str`. That is
//! why the runtime image needs no assets directory — which matters, because a
//! distroless image has no filesystem to speak of.

/// Contract tests: the shapes this service emits and accepts, checked against
/// the shared messaging-core types with no broker and no sibling running.
///
/// They live in src/ rather than tests/ because this is a binary-only crate:
/// Rust's tests/ directory can import a crate's lib target, and there isn't
/// one. See .claude/skills/microservice-agent-team/SKILL.md.
#[cfg(test)]
mod contract_tests;

mod collect;
mod live;
mod push;
mod tap;

use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::{Json, Router, routing::get};
use collect::Collector;
use push::Live;
use service_core::{health_routes, init_tracing, port_from_env, self_check};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};

const SERVICE: &str = "mimic-service";
const DEFAULT_PORT: u16 = 8080;

/// The mimic panel, baked into the binary at compile time.
const PANEL_HTML: &str = include_str!("../assets/panel.html");

/// The console shell that frames every tool.
const CONSOLE_HTML: &str = include_str!("../assets/console.html");

/// The written walkthrough, served so the console can frame it.
///
/// A `file://` link would work on this machine only, and could not be framed
/// by a page served over Hypertext Transfer Protocol. Serving it makes the
/// console self-contained and reachable from any machine on the network.
const DOCS_HTML: &str = include_str!("../../../docs/messaging-and-eventing.html");

/// How many reports the tap may queue before it starts dropping them.
///
/// Bounded on purpose. A stalled panel must never be able to slow down the
/// message bus, so a full channel drops the report and logs it rather than
/// applying backpressure to the thing it is watching.
const REPORT_QUEUE: usize = 1024;

/// How many messages a browser may fall behind before it is sent a fresh
/// picture instead.
const BROWSER_BACKLOG: usize = 256;

#[tokio::main]
async fn main() -> Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);
    service_core::init_metrics(SERVICE);

    let prometheus_url =
        std::env::var("PROMETHEUS_URL").unwrap_or_else(|_| "http://prometheus:9090".to_owned());
    let nats_monitor_url =
        std::env::var("NATS_MONITOR_URL").unwrap_or_else(|_| "http://nats:8222".to_owned());

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://nats:4222".to_owned());

    let collector = Collector::new(prometheus_url.clone(), nats_monitor_url.clone());

    // Take one reading before serving, so the first visitor sees real data
    // rather than an empty panel that fills in a couple of seconds later.
    let primed = Arc::new(collector.snapshot().await);

    // A `watch` channel is the old lock plus the one thing polling could never
    // have: readers are *told* when the value changes.
    let (latest_tx, latest_rx) = watch::channel(primed);
    let (reports_tx, reports_rx) = mpsc::channel(REPORT_QUEUE);
    let (hub, _) = broadcast::channel(BROWSER_BACKLOG);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Stamped once per process. A browser carrying a sequence number from a
    // previous run can tell "the server restarted, my numbering means nothing"
    // apart from "I missed a few messages" — the recovery is the same either
    // way, but only one of them deserves a log line.
    let epoch = uuid::Uuid::new_v4();

    let health = tap::Health::new();

    let mut tasks = tap::spawn(
        nats_url.clone(),
        reports_tx,
        health.clone(),
        shutdown_rx.clone(),
    );

    tasks.push(tokio::spawn(push::aggregate(
        collector.clone(),
        reports_rx,
        health,
        latest_tx,
        hub.clone(),
        shutdown_rx,
        epoch,
    )));

    let live = Live {
        latest: latest_rx,
        hub,
    };

    // Where the browser should go for each external tool. Read from the
    // environment because these are *published host ports*, which compose
    // decides and this process cannot discover.
    let links = Links {
        grafana: std::env::var("GRAFANA_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_owned()),
        jaeger: std::env::var("JAEGER_URL").unwrap_or_else(|_| "http://localhost:16686".to_owned()),
        prometheus: std::env::var("PROMETHEUS_UI_URL")
            .unwrap_or_else(|_| "http://localhost:9090".to_owned()),
    };

    let app = Router::new()
        // The console is the front door; the mimic is its first tab.
        .route("/", get(console))
        .route("/mimic", get(panel))
        .route("/docs", get(docs))
        .route("/api/state", get(state))
        // Registered *before* `.with_state`. Adding a stateful route after that
        // line produces one of axum's long "the trait Handler is not
        // implemented" errors that never mentions route ordering as the cause.
        .route("/api/live", get(push::live_socket))
        .with_state(live)
        .route(
            "/api/links",
            get({
                let links = links.clone();
                move || {
                    let links = links.clone();
                    async move { Json(links) }
                }
            }),
        )
        .merge(health_routes(SERVICE))
        .merge(service_core::metrics_routes());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("could not bind the mimic port")?;

    tracing::info!(
        %addr,
        prometheus = %prometheus_url,
        nats = %nats_url,
        "mimic panel listening"
    );

    // Serve, but stop if a background task dies.
    //
    // Every *expected* failure — a broker restart, a dropped connection — is
    // already retried in place by the backoff loop in `tap`. A task that ends
    // anyway therefore panicked, and the honest response is to exit so the
    // container restarts clean. Without this the process would keep serving
    // happily with a dead tap, and every browser would sit watching a flow
    // plane that reported itself connected and delivered nothing.
    //
    // In-process backoff for expected failures, process exit for unexpected
    // ones, and never a silent freeze.
    tokio::select! {
        served = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()) => {
            served.context("the mimic server stopped")?;
        }
        (finished, index, _rest) = futures::future::select_all(&mut tasks) => {
            tracing::error!(
                task = index,
                outcome = ?finished,
                "a background task ended unexpectedly; exiting so the container restarts"
            );
            shutdown_tx.send_replace(true);
            anyhow::bail!("a background task ended unexpectedly");
        }
    }

    // Tell the tap and the aggregator to stop, then give the tap a moment to
    // remove its consumers. Nothing on the server expires an abandoned one, so
    // this is the only cleanup there is.
    shutdown_tx.send_replace(true);

    for task in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .ok();
    }

    Ok(())
}

/// Browser-facing URLs for the tools this service does not host.
///
/// `Clone` because each route handler needs its own copy; the struct is three
/// short strings, so copying it per request costs nothing worth measuring.
#[derive(Clone, serde::Serialize)]
struct Links {
    grafana: String,
    jaeger: String,
    prometheus: String,
}

/// Serves the console shell that frames everything.
async fn console() -> Html<&'static str> {
    Html(CONSOLE_HTML)
}

/// Serves the mimic panel itself.
async fn panel() -> Html<&'static str> {
    Html(PANEL_HTML)
}

/// Serves the written walkthrough.
async fn docs() -> Html<&'static str> {
    Html(DOCS_HTML)
}

/// Serves the current snapshot as JavaScript Object Notation.
///
/// # Kept deliberately, not deprecated
///
/// The panel is pushed to over a socket now, so nothing polls this in normal
/// use. It stays for three reasons, each of which has bitten a real system: a
/// proxy that strips the upgrade header leaves the browser with nothing else to
/// fall back to; the end-to-end tests assert through it rather than opening a
/// socket, which keeps a WebSocket client out of the test crate entirely; and
/// it is the one endpoint a person can read with `curl` while debugging.
async fn state(State(live): State<Live>) -> impl IntoResponse {
    // `borrow()` returns a guard that must not be held across an await, so the
    // value is cloned out and the guard dropped on this line. The inner
    // snapshot is cloned rather than serialising the `Arc` directly: serde can
    // serialise an `Arc`, but only with a feature this workspace does not
    // deliberately enable.
    let snapshot = Arc::clone(&live.latest.borrow());
    Json(collect::Snapshot::clone(&snapshot))
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        tracing::info!("shutdown signal received");
    }
}
