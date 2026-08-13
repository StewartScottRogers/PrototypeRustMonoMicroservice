//! The stack's front door.
//!
//! `POST /relay` takes a message, forwards it to `echo-service` over the
//! internal Docker network, and returns what came back. That hop is the point:
//! it makes this a real two-service system, and it is what `DevStart.cmd`
//! exercises end to end.
//!
//! ```text
//!   you ──POST /relay──▶ gateway-service ──POST /echo──▶ echo-service
//!                                        ◀──── JSON ────
//! ```
//!
//! # Reading this file as a Rust newcomer
//!
//! The new idea here compared to `echo-service` is **shared state**: the HTTP
//! client and the upstream URL are created once at startup and every request
//! handler needs to reach them. Rust makes you be explicit about how something
//! is shared, which is what [`AppState`] and axum's `State` extractor are for.

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use serde::{Deserialize, Serialize};
use service_core::{health_routes, init_tracing, port_from_env, self_check};
use std::net::SocketAddr;

const SERVICE: &str = "gateway-service";
const DEFAULT_PORT: u16 = 8080;
/// Used when `ECHO_SERVICE_URL` is unset. `echo-service` is a hostname Docker
/// Compose creates automatically from the service name in `compose.yaml`.
const DEFAULT_ECHO_URL: &str = "http://echo-service:8080";

/// Everything the handlers need that outlives a single request.
///
/// `#[derive(Clone)]` matters: axum hands each request handler its own copy of
/// the state, so the type must be cloneable. Both fields are cheap to clone —
/// `reqwest::Client` is a handle to a shared connection pool, not the pool
/// itself, so cloning it shares the pool rather than duplicating it. Creating a
/// fresh `Client` per request instead would throw away connection reuse.
#[derive(Clone)]
struct AppState {
    client: reqwest::Client,
    echo_url: String,
}

/// Request body for `POST /relay`.
#[derive(Debug, Deserialize)]
struct RelayRequest {
    message: String,
}

/// What this service sends to `echo-service`. It happens to look like
/// [`RelayRequest`], but they are separate types on purpose: one is the
/// contract with our caller, the other the contract with the upstream, and
/// either can change without dragging the other along.
#[derive(Debug, Serialize)]
struct UpstreamRequest {
    message: String,
}

/// What `echo-service` sends back. Only the field we care about is declared;
/// serde ignores unknown fields by default, so the upstream can add more
/// without breaking this.
#[derive(Debug, Deserialize)]
struct UpstreamResponse {
    echo: String,
}

/// Response body for `POST /relay`.
#[derive(Debug, Serialize)]
struct RelayResponse {
    echo: String,
    /// Proof the reply travelled through this service, which is what makes the
    /// end-to-end test meaningful.
    via: &'static str,
}

/// Body returned when the upstream call fails.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Handles `POST /relay`.
///
/// # Signature, piece by piece
///
/// - `State(state): State<AppState>` — axum's state extractor, destructured on
///   the spot. Extractors must come before the body extractor in the parameter
///   list; `Json` has to be last.
/// - The return type is a `Result`. On `Ok` axum sends the success response; on
///   `Err` it sends the error one. The tuple `(StatusCode, Json<T>)` is axum's
///   shorthand for "this status code with this JSON body".
async fn relay(
    State(state): State<AppState>,
    Json(request): Json<RelayRequest>,
) -> Result<Json<RelayResponse>, (StatusCode, Json<ErrorResponse>)> {
    // `format!` builds a `String` the way `println!` builds a line of output.
    let url = format!("{}/echo", state.echo_url);

    // Each step below can fail independently, so each gets its own error
    // handling. `.await` is needed twice: once to send the request and get
    // headers back, once to read and parse the body.
    let response = state
        .client
        .post(&url)
        // `.json(&value)` serialises the value and sets the content type. The
        // `&` passes a reference — reqwest only needs to read it, so ownership
        // stays here.
        .json(&UpstreamRequest {
            message: request.message,
        })
        .send()
        .await
        // `.map_err(...)` transforms the error inside a `Result` while leaving
        // a success value untouched. Here it converts reqwest's error into the
        // HTTP response this function promises to return.
        .map_err(|error| {
            tracing::error!(%error, upstream = %url, "upstream request failed");
            bad_gateway("could not reach echo-service")
        })?;

    // A reply that arrived but says 500 is still a failure. `reqwest` does not
    // treat non-2xx as an error by default, so check explicitly.
    if !response.status().is_success() {
        tracing::error!(status = %response.status(), "upstream returned an error");
        return Err(bad_gateway("echo-service returned an error"));
    }

    // The turbofish `::<UpstreamResponse>` tells `json()` which type to parse
    // into. Without it the compiler cannot tell what shape to expect.
    let upstream = response.json::<UpstreamResponse>().await.map_err(|error| {
        tracing::error!(%error, "could not parse the upstream response");
        bad_gateway("echo-service sent an unexpected response")
    })?;

    Ok(Json(RelayResponse {
        echo: upstream.echo,
        via: SERVICE,
    }))
}

/// Builds the 502 response used for every upstream failure.
///
/// Pulled into its own function so the three call sites above stay readable.
fn bad_gateway(message: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorResponse {
            error: message.to_owned(),
        }),
    )
}

/// Builds the router.
///
/// # Why this is two steps
///
/// A `Router` is generic over the state its handlers need: `Router<AppState>`
/// versus plain `Router` (which is `Router<()>`, meaning "no state"). Because
/// `relay` asks for `State<AppState>`, the first router below is a
/// `Router<AppState>`.
///
/// `.with_state(state)` supplies that state and hands back a plain `Router` —
/// the state requirement is now satisfied, so it disappears from the type.
/// Only then can it merge with `health_routes`, whose handlers need no state.
///
/// Merging first and calling `.with_state` afterwards is a compile error, not a
/// runtime surprise. The type system is tracking "has this router been given
/// everything it needs yet".
fn app(state: AppState) -> Router {
    let relay_routes = Router::new().route("/relay", post(relay)).with_state(state);

    relay_routes.merge(health_routes(SERVICE))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    // Same self-probe trick as echo-service: the container's HEALTHCHECK runs
    // this binary with one argument, because distroless images have no curl.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);

    let echo_url =
        std::env::var("ECHO_SERVICE_URL").unwrap_or_else(|_| DEFAULT_ECHO_URL.to_owned());

    let state = AppState {
        client: reqwest::Client::new(),
        // `.clone()` because `echo_url` is used again in the log line below.
        // Moving it into the struct would leave nothing behind to log.
        echo_url: echo_url.clone(),
    };

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, upstream = %echo_url, "listening");

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        tracing::info!("shutdown signal received");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Starts a fake echo-service on a random free port and returns its URL.
    ///
    /// Testing against a stub rather than the real crate keeps this a *unit*
    /// test: it fails only when the gateway is wrong.
    async fn start_stub_upstream(status: StatusCode, body: &'static str) -> String {
        // Port 0 means "any free port", so parallel tests never collide.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let router = Router::new().route("/echo", post(move || async move { (status, body) }));

        // `tokio::spawn` runs the stub in the background; `serve` never
        // returns, so awaiting it here would hang the test.
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        format!("http://{addr}")
    }

    fn relay_request() -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/relay")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap()
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn relay_forwards_to_the_upstream_and_tags_the_reply() {
        let echo_url = start_stub_upstream(
            StatusCode::OK,
            r#"{"echo":"hello","service":"echo-service"}"#,
        )
        .await;

        let state = AppState {
            client: reqwest::Client::new(),
            echo_url,
        };

        let response = app(state).oneshot(relay_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_text(response).await,
            r#"{"echo":"hello","via":"gateway-service"}"#
        );
    }

    #[tokio::test]
    async fn relay_reports_bad_gateway_when_the_upstream_is_down() {
        // Bind then immediately drop, which names a port nothing is listening
        // on — the reliable way to simulate an unreachable upstream.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let state = AppState {
            client: reqwest::Client::new(),
            echo_url: format!("http://{addr}"),
        };

        let response = app(state).oneshot(relay_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            body_text(response).await,
            r#"{"error":"could not reach echo-service"}"#
        );
    }

    #[tokio::test]
    async fn relay_reports_bad_gateway_when_the_upstream_errors() {
        let echo_url = start_stub_upstream(StatusCode::INTERNAL_SERVER_ERROR, "boom").await;

        let state = AppState {
            client: reqwest::Client::new(),
            echo_url,
        };

        let response = app(state).oneshot(relay_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn relay_reports_bad_gateway_when_the_upstream_sends_junk() {
        let echo_url = start_stub_upstream(StatusCode::OK, "not json at all").await;

        let state = AppState {
            client: reqwest::Client::new(),
            echo_url,
        };

        let response = app(state).oneshot(relay_request()).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn health_routes_are_merged_in() {
        let state = AppState {
            client: reqwest::Client::new(),
            echo_url: "http://unused".to_owned(),
        };

        let request = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app(state).oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
