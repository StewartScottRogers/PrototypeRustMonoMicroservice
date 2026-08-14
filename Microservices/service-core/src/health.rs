//! Liveness and readiness probes.
//!
//! Two shapes of the same idea live here:
//!
//! - [`health_routes`] — the endpoints a service *serves* over Hypertext
//!   Transfer Protocol.
//! - [`self_check`] — a tiny client the container's `HEALTHCHECK` runs to *call*
//!   those endpoints from inside the container.

use axum::{Json, Router, routing::get};
use serde::Serialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// How long [`self_check`] waits before deciding the service is not answering.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Body returned by both probes.
///
/// `#[derive(...)]` asks the compiler to write trait implementations for us:
///
/// - `Debug` enables the `{:?}` formatting used in logs and test failures.
/// - `Serialize` comes from the `serde` crate and is what lets axum turn this
///   struct into JavaScript Object Notation. Without it, `Json(Probe { .. })`
///   would not compile.
/// - `PartialEq`/`Eq` allow `==`, which `assert_eq!` needs in the tests below.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Probe {
    /// `ok` for liveness, `ready` for readiness.
    ///
    /// `&'static str` is borrowed text with a `'static` *lifetime*, meaning it
    /// lives for the whole program. String literals in the source qualify, so
    /// no allocation happens for this field.
    pub status: &'static str,
    /// Which service answered, so a probe against the wrong port is obvious.
    ///
    /// This one is an owned `String` because the value is only known at
    /// runtime and the struct must keep it alive independently of the caller.
    pub service: String,
}

/// Builds a router exposing `/healthz` and `/readyz` for `service`.
///
/// A `Router` maps paths to handler functions. It is a value like any other:
/// you build it, pass it around, and merge routers together (see the services'
/// `main.rs`, which merges this one into its own).
pub fn health_routes(service: &str) -> Router {
    // Each route needs its own copy of the name. `to_owned()` allocates a
    // `String` from the borrowed `&str`, because the closures below outlive
    // this function and cannot hold a borrow of the caller's data.
    let live = service.to_owned();
    let ready = service.to_owned();

    Router::new()
        // `move` transfers ownership of `live` into the closure, so the
        // closure still owns valid data after `health_routes` returns.
        //
        // The extra `.clone()` inside is because axum may call a handler many
        // times concurrently, and each call consumes a `String`. Cloning a
        // short name per request is far cheaper than the alternatives.
        .route("/healthz", get(move || probe("ok", live.clone())))
        .route("/readyz", get(move || probe("ready", ready.clone())))
}

/// The actual handler.
///
/// `async fn` returns a *future*: a value describing work that has not run
/// yet. Nothing happens until something `.await`s it — here, the axum server
/// does. Wrapping the return value in `Json(..)` sets the `content-type`
/// header and serialises the body.
async fn probe(status: &'static str, service: String) -> Json<Probe> {
    Json(Probe { status, service })
}

/// Runs a Hypertext Transfer Protocol server exposing only the health probes,
/// until the process ends.
///
/// The consumer services (worker, notifier, audit) have no programming
/// interface of their own — they read from NATS. They still need this, because
/// Docker's `HEALTHCHECK` has nothing else to ask, and `depends_on: condition:
/// service_healthy` in compose would never be satisfied.
///
/// Returns `std::io::Result<()>`, so a port already in use surfaces as an
/// error rather than a panic.
pub async fn serve(service: &'static str, port: u16) -> std::io::Result<()> {
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "health and metrics endpoints listening");
    axum::serve(listener, health_routes(service).merge(metrics_routes())).await
}

/// Mounts `/metrics` for Prometheus to scrape.
///
/// Merged into every service's router, so adding a service means Prometheus
/// finds it without anyone remembering to wire an endpoint up.
pub fn metrics_routes() -> Router {
    Router::new().route("/metrics", get(scrape))
}

/// Renders the metrics in the text format Prometheus expects.
///
/// The content type matters: without it Prometheus refuses the response.
async fn scrape() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        crate::metrics::render(),
    )
}

/// Asks `http://127.0.0.1:{port}/healthz` whether the service is alive.
///
/// The container image is `distroless`: no shell, no `curl`, no `wget`. So the
/// service binary doubles as its own health probe — `service healthcheck` runs
/// this and exits 0 or 1, and Docker reads that exit code.
///
/// This deliberately speaks Hypertext Transfer Protocol by hand over a raw
/// Transmission Control Protocol socket rather than pulling in a Hypertext
/// Transfer Protocol client crate. The request is three fixed lines, and the
/// only answer needed is whether the status line says `200`.
///
/// Everything here is *blocking* (it waits rather than yielding), which is
/// fine because the probe process does nothing else and exits immediately
/// after.
pub fn self_check(port: u16) -> bool {
    // `match` on a `Result` is the explicit form of error handling: handle
    // both arms or the code does not compile. Returning `false` on any failure
    // is right here — a probe that cannot connect *is* a failed probe.
    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };

    // Without timeouts a hung service would hang the probe forever, and Docker
    // would never mark the container unhealthy.
    if stream.set_read_timeout(Some(PROBE_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(PROBE_TIMEOUT)).is_err()
    {
        return false;
    }

    // `\r\n` line endings and the blank line at the end are required by
    // Hypertext Transfer Protocol. `Connection: close` tells the server to
    // hang up after replying, so the read below ends on its own instead of
    // waiting for more data.
    let request =
        format!("GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");

    // `b"..."` would be a byte string; `.as_bytes()` does the same for a
    // `String`. Sockets deal in bytes, not text.
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    // Deliberately no `stream.shutdown(Shutdown::Write)` here. Half-closing
    // the socket looks like a disconnect to hyper, which then drops the
    // connection without replying. `Connection: close` above already tells the
    // server to hang up once it has answered, which is all this needs.

    // A fixed buffer is enough: only the first line of the response matters,
    // and it is far shorter than this.
    let mut response = [0_u8; 128];
    let read = match stream.read(&mut response) {
        Ok(count) => count,
        Err(_) => return false,
    };

    // Bytes off a socket are not guaranteed to be valid UTF-8, so this
    // conversion replaces anything invalid rather than failing.
    let status_line = String::from_utf8_lossy(&response[..read]);
    status_line.starts_with("HTTP/1.1 200")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Drives the router directly, with no network involved.
    ///
    /// `oneshot` comes from `tower::ServiceExt` and feeds a single request
    /// through, which is why that trait is imported above: in Rust a trait's
    /// methods are only callable when the trait itself is in scope.
    async fn get_body(path: &str) -> (StatusCode, String) {
        let response = health_routes("test-service")
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            // `.await` suspends until the future produces its value. It is
            // only legal inside an `async fn`.
            .await
            .unwrap();

        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    // `#[tokio::test]` replaces `#[test]` for async tests: it starts a small
    // async runtime, runs the test body on it, then shuts it down.
    #[tokio::test]
    async fn healthz_reports_ok() {
        let (status, body) = get_body("/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"status":"ok","service":"test-service"}"#);
    }

    #[tokio::test]
    async fn readyz_reports_ready() {
        let (status, body) = get_body("/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"status":"ready","service":"test-service"}"#);
    }

    #[tokio::test]
    async fn unknown_path_is_not_found() {
        let (status, _) = get_body("/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn self_check_succeeds_against_a_running_service() {
        // Port 0 asks the operating system for any free port, which keeps
        // parallel tests from colliding. We then ask the listener which port
        // it actually got.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // `tokio::spawn` runs the server concurrently. Without this the test
        // would block here forever, since `serve` only returns on shutdown.
        tokio::spawn(async move {
            let _ = axum::serve(listener, health_routes("probe-target")).await;
        });

        // `self_check` blocks its thread, so it must not run on the async
        // runtime's thread — `spawn_blocking` moves it to a thread pool meant
        // for exactly this.
        let alive = tokio::task::spawn_blocking(move || self_check(port))
            .await
            .unwrap();

        assert!(alive, "probe should see the running service");
    }

    #[tokio::test]
    async fn self_check_fails_when_nothing_is_listening() {
        // Bind a port, record it, then drop the listener so the port is free.
        // This is the reliable way to name a port nothing is using.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let alive = tokio::task::spawn_blocking(move || self_check(port))
            .await
            .unwrap();

        assert!(!alive, "probe should fail with no service listening");
    }
}
