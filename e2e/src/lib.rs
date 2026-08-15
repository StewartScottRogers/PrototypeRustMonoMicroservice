//! The harness the end-to-end tests are built on.
//!
//! # What lives here and what does not
//!
//! This crate belongs to the **Orchestration Agent**. It tests choreography
//! *between* services — the part no single team owns and therefore no single
//! team can test. A service team's own tests never start a sibling service;
//! that is precisely what this crate is for.
//!
//! Nothing here asserts. The assertions live in `tests/`; this file is the
//! plumbing they share: where the stack is, how to talk to it, and how to wait
//! for something asynchronous to become true.
//!
//! # Reading this crate as a Rust newcomer
//!
//! This is a *library* crate — no `main`, nothing to run. Rust's `tests/`
//! directory can only import a crate's library, which is the whole reason this
//! file exists rather than the helpers being copied into each test.

use anyhow::{Context as _, Result};
use std::time::{Duration, Instant};

/// How long a poll helper keeps trying before giving up.
///
/// Generous, because these tests cross a broker: a message may sit in a queue
/// for a moment before anything reacts to it. Too short and the suite fails on
/// a slow machine, which teaches people to re-run it rather than read it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(45);

/// Gap between attempts while polling.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Where the composed stack is listening.
///
/// Every field has a default matching `compose.yaml`, overridable by
/// environment variable so the suite can run against a stack published on
/// different ports — which is what a `.env` file does.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub gateway: String,
    pub nats_monitor: String,
    pub prometheus: String,
    pub database_url: String,
    /// The mimic panel, which serves its own merged snapshot at `/api/state`.
    pub mimic: String,
}

impl Endpoints {
    /// Reads the endpoints from the environment, falling back to the compose
    /// defaults.
    ///
    /// `unwrap_or_else` builds the fallback string only when the variable is
    /// missing, rather than allocating one on every call.
    pub fn from_env() -> Self {
        Self {
            gateway: std::env::var("E2E_GATEWAY_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_owned()),
            nats_monitor: std::env::var("E2E_NATS_MONITOR_URL")
                .unwrap_or_else(|_| "http://localhost:8222".to_owned()),
            prometheus: std::env::var("E2E_PROMETHEUS_URL")
                .unwrap_or_else(|_| "http://localhost:9090".to_owned()),
            database_url: std::env::var("E2E_DATABASE_URL").unwrap_or_else(|_| {
                "postgres://devuser:devpassword@localhost:5432/devdb".to_owned()
            }),
            mimic: std::env::var("E2E_MIMIC_URL")
                .unwrap_or_else(|_| "http://localhost:8090".to_owned()),
        }
    }
}

// `Default` is the conventional trait for "give me the usual one".
// Implementing it in terms of `from_env` means `Endpoints::default()` and
// `Endpoints::from_env()` can never drift apart.
impl Default for Endpoints {
    fn default() -> Self {
        Self::from_env()
    }
}

/// A client for driving the stack.
///
/// Holds one `reqwest::Client` rather than building one per request: it owns a
/// connection pool, and a fresh client each time would throw that away.
pub struct Harness {
    pub endpoints: Endpoints,
    client: reqwest::Client,
}

impl Harness {
    /// Builds a harness pointing at wherever the environment says the stack
    /// is.
    pub fn new() -> Self {
        Self {
            endpoints: Endpoints::from_env(),
            client: reqwest::Client::new(),
        }
    }

    /// Places an order and returns the id the gateway assigned.
    ///
    /// Passing `order_id` re-submits an existing order, which is how the
    /// idempotency test proves the worker does the work only once.
    ///
    /// The gateway answers `202 Accepted`, never `200 OK` — the order is
    /// stored but nothing has processed it yet, and this asserts that
    /// distinction rather than accepting any success code.
    pub async fn place_order(
        &self,
        item: &str,
        quantity: u32,
        order_id: Option<uuid::Uuid>,
    ) -> Result<uuid::Uuid> {
        // `serde_json::json!` builds a value without needing a struct for a
        // one-off request shape.
        let mut body = serde_json::json!({ "item": item, "quantity": quantity });
        if let Some(id) = order_id {
            body["order_id"] = serde_json::json!(id);
        }

        let response = self
            .client
            .post(format!("{}/order", self.endpoints.gateway))
            .json(&body)
            .send()
            .await
            .context("the gateway did not answer — is the stack running?")?;

        anyhow::ensure!(
            response.status() == reqwest::StatusCode::ACCEPTED,
            "expected 202 Accepted from the gateway, got {}",
            response.status()
        );

        let accepted: serde_json::Value = response
            .json()
            .await
            .context("the gateway's response was not JavaScript Object Notation")?;

        let id = accepted
            .get("order_id")
            .and_then(serde_json::Value::as_str)
            .context("the gateway's response carried no order_id")?;

        uuid::Uuid::parse_str(id)
            .context("the gateway returned an order_id that is not a universally unique identifier")
    }

    /// Runs one Prometheus instant query and returns the first sample.
    ///
    /// `Ok(None)` means the query is valid but matched nothing yet, which is a
    /// normal state early in a test — distinct from `Err`, which means the
    /// query or Prometheus itself is broken.
    pub async fn prometheus_scalar(&self, query: &str) -> Result<Option<f64>> {
        // Encoded by hand because reqwest's `.query()` needs features this
        // workspace switched off. `service-core` owns the helper; see its
        // `url` module for why.
        let url = format!(
            "{}/api/v1/query?query={}",
            self.endpoints.prometheus,
            service_core::url::percent_encode(query)
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Prometheus did not answer")?;

        let body: serde_json::Value = response
            .json()
            .await
            .context("Prometheus sent no JavaScript Object Notation")?;

        let sample = body
            .get("data")
            .and_then(|data| data.get("result"))
            .and_then(serde_json::Value::as_array)
            .and_then(|results| results.first())
            .and_then(|first| first.get("value"))
            .and_then(serde_json::Value::as_array)
            .and_then(|pair| pair.get(1))
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| raw.parse().ok());

        Ok(sample)
    }

    /// Total messages held by a JetStream stream, from the NATS monitoring
    /// port.
    ///
    /// Nothing exports queue depth to Prometheus, so this reads NATS directly.
    pub async fn stream_messages(&self, stream: &str) -> Result<u64> {
        let body: serde_json::Value = self
            .client
            .get(format!("{}/jsz?streams=1", self.endpoints.nats_monitor))
            .send()
            .await
            .context("the NATS monitoring port did not answer")?
            .json()
            .await
            .context("NATS sent no JavaScript Object Notation")?;

        let count = body
            .get("account_details")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|account| account.get("stream_detail"))
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .find(|detail| detail.get("name").and_then(serde_json::Value::as_str) == Some(stream))
            .and_then(|detail| detail.get("state"))
            .and_then(|state| state.get("messages"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);

        Ok(count)
    }

    /// Opens a Postgres connection pool for the assertions that need one.
    pub async fn database(&self) -> Result<sqlx::PgPool> {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&self.endpoints.database_url)
            .await
            .context("could not reach Postgres — is the stack running?")
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// Retries `check` until it returns `true` or the timeout expires.
///
/// # Why every assertion goes through this
///
/// Nothing downstream of the gateway is synchronous. An order is *accepted*
/// long before it is processed, so asserting immediately after the POST tests
/// nothing but the speed of the machine. Polling turns "eventually true" into
/// something a test can state.
///
/// # The signature, piece by piece
///
/// - `F: Fn() -> Fut` — a closure producing a fresh future on each attempt.
///   Futures are consumed by awaiting, so the closure must be able to make a
///   new one rather than the caller passing one in.
/// - `Fut: Future<Output = Result<bool>>` — each attempt may fail outright,
///   which is different from returning `false` (not true *yet*).
pub async fn wait_until<F, Fut>(description: &str, check: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    wait_until_within(description, DEFAULT_TIMEOUT, check).await
}

/// [`wait_until`] with the deadline supplied by the caller.
///
/// Exists so the timeout behaviour itself can be unit tested in milliseconds
/// rather than by making the suite sit through the real 45 seconds. Tests
/// needing an unusually slow condition can also use it directly.
pub async fn wait_until_within<F, Fut>(description: &str, timeout: Duration, check: F) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let deadline = Instant::now() + timeout;
    let mut last_error = None;

    while Instant::now() < deadline {
        match check().await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            // Transient failures are expected while the stack settles, so keep
            // the most recent one and report it only if time runs out.
            Err(error) => last_error = Some(error),
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }

    match last_error {
        Some(error) => Err(error.context(format!(
            "timed out after {:?} waiting for: {description}",
            timeout
        ))),
        None => anyhow::bail!("timed out after {timeout:?} waiting for: {description}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These cover the pure parts of the harness. The parts that need a stack
    // are in tests/, behind #[ignore].

    #[test]
    fn endpoints_fall_back_to_the_compose_defaults() {
        // No E2E_* variables are set in a normal test run, so this exercises
        // every fallback branch at once.
        let endpoints = Endpoints::from_env();

        assert!(endpoints.gateway.starts_with("http://"));
        assert!(endpoints.nats_monitor.contains("8222"));
        assert!(endpoints.prometheus.contains("9090"));
        assert!(endpoints.database_url.starts_with("postgres://"));
    }

    #[test]
    fn default_and_from_env_agree() {
        // Implementing Default in terms of from_env is what guarantees this;
        // the test stops someone "simplifying" one of them later.
        assert_eq!(Endpoints::default().gateway, Endpoints::from_env().gateway);
    }

    #[tokio::test]
    async fn wait_until_returns_as_soon_as_the_check_passes() {
        let result = wait_until("an immediately true condition", || async { Ok(true) }).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn a_condition_that_never_holds_times_out_and_says_what_it_wanted() {
        let result = wait_until_within(
            "something that never happens",
            Duration::from_millis(60),
            || async { Ok(false) },
        )
        .await;

        let message = result.unwrap_err().to_string();
        assert!(
            message.contains("something that never happens"),
            "the timeout must name the condition, got: {message}"
        );
    }

    #[tokio::test]
    async fn a_failing_check_is_retried_and_its_error_survives_to_the_timeout() {
        let attempts = std::sync::atomic::AtomicUsize::new(0);

        // The deadline must exceed POLL_INTERVAL or only one attempt fits, and
        // "it retried" becomes untestable. Two intervals plus a margin.
        let timeout = POLL_INTERVAL * 2 + Duration::from_millis(200);

        let result = wait_until_within("an upstream that is down", timeout, || {
            attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Err rather than Ok(false), so the branch that carries the last
            // error into the timeout message is the one exercised. A transient
            // failure while the stack settles must not end the wait.
            async { anyhow::bail!("upstream not ready") }
        })
        .await;

        assert!(result.is_err());
        assert!(
            attempts.load(std::sync::atomic::Ordering::Relaxed) > 1,
            "one failed attempt must not end the wait"
        );
        // The cause is preserved, not replaced by a bare "timed out".
        assert!(format!("{:#}", result.unwrap_err()).contains("upstream not ready"));
    }
}
