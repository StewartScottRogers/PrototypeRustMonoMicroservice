//! A live control-room mimic of this stack.
//!
//! Serves one page at `/` showing the real topology, with lamps and gauges
//! driven by Prometheus and the NATS monitoring endpoint. Open it at
//! <http://localhost:8090>.
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

use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::{Json, Router, routing::get};
use collect::{Collector, Snapshot};
use service_core::{health_routes, init_tracing, port_from_env, self_check};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

const SERVICE: &str = "mimic-service";
const DEFAULT_PORT: u16 = 8080;

/// How often the background poller refreshes the picture.
///
/// The browser polls this service, and this service polls Prometheus. One
/// collector serving every open tab means twenty people watching the panel
/// generate the same query load as one.
///
/// One second is the middle link of a chain that is only as quick as its
/// slowest part: Prometheus scrapes every 2 seconds, this reads every second,
/// and the browser polls every second. Making any one of them faster on its
/// own buys nothing.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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

/// The most recent snapshot, shared between the poller and every request.
///
/// `Arc<RwLock<T>>` is the standard shape for "one value, many readers, rare
/// writer": `Arc` lets several tasks own it, `RwLock` lets any number read at
/// once but only one write at a time.
type Shared = Arc<RwLock<Snapshot>>;

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

    let collector = Collector::new(prometheus_url.clone(), nats_monitor_url.clone());

    // Take one reading before serving, so the first visitor sees real data
    // rather than an empty panel that fills in three seconds later.
    let shared: Shared = Arc::new(RwLock::new(collector.snapshot().await));

    // The poller owns its own clones and runs for the life of the process.
    {
        let collector = collector.clone();
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let snapshot = collector.snapshot().await;
                *shared.write().await = snapshot;
            }
        });
    }

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
        .with_state(shared)
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

    tracing::info!(%addr, prometheus = %prometheus_url, "mimic panel listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("the mimic server stopped")?;

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

/// Serves the current snapshot as JavaScript Object Notation, which is what
/// the page polls.
///
/// Reads the shared value rather than querying Prometheus, so the cost of an
/// extra browser tab is one JavaScript Object Notation serialisation.
async fn state(State(shared): State<Shared>) -> impl IntoResponse {
    let snapshot = shared.read().await.clone();
    Json(snapshot)
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        tracing::info!("shutdown signal received");
    }
}
