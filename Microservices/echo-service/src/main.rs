//! Reference microservice. Copy this crate as the starting point for a new one.
//!
//! It exposes one endpoint, `POST /echo`, which returns the message it was
//! given. `gateway-service` calls it, which is what makes the compose stack a
//! two-service system rather than one service on its own.
//!
//! # Reading this file as a Rust newcomer
//!
//! `main.rs` is the root of a *binary* crate: it produces an executable, and
//! execution starts at `fn main`. Compare `service-core/src/lib.rs`, which is a
//! library and has no `main`.

use axum::{Json, Router, routing::post};
use serde::{Deserialize, Serialize};
use service_core::{health_routes, init_tracing, port_from_env, self_check};
use std::net::SocketAddr;

/// `const` values are compile-time constants, inlined wherever they are used.
/// SCREAMING_CASE is the convention.
const SERVICE: &str = "echo-service";
const DEFAULT_PORT: u16 = 8080;

/// The shape of the request body.
///
/// `Deserialize` (from serde) is what lets axum turn incoming JSON into this
/// struct. If a request body does not match — wrong field name, wrong type —
/// axum rejects it with `422 Unprocessable Entity` before the handler runs.
///
/// The struct is not `pub` because nothing outside this file needs it.
#[derive(Debug, Deserialize)]
struct EchoRequest {
    message: String,
}

/// The shape of the response body. `Serialize` is the mirror image of
/// `Deserialize`: struct out to JSON, rather than JSON in to struct.
#[derive(Debug, Serialize)]
struct EchoResponse {
    echo: String,
    service: &'static str,
}

/// Handles `POST /echo`.
///
/// `Json(request)` in the parameter list is both an *extractor* and a *pattern*.
/// Axum sees the `Json` type and parses the body into `EchoRequest`; the
/// surrounding `Json(...)` immediately destructures the wrapper so the body of
/// the function can use `request` directly.
async fn echo(Json(request): Json<EchoRequest>) -> Json<EchoResponse> {
    Json(EchoResponse {
        // `request.message` is *moved* out of `request` here rather than
        // copied. That is allowed because `request` is not used afterwards —
        // the compiler tracks this, and would reject the code otherwise.
        echo: request.message,
        service: SERVICE,
    })
}

/// Builds the router.
///
/// Split out from `main` so tests can drive it without binding a port.
fn app() -> Router {
    Router::new()
        .route("/echo", post(echo))
        // `.merge(...)` folds another router's routes into this one, which is
        // how every service picks up `/healthz` and `/readyz` from the shared
        // `service-core` crate without repeating them.
        .merge(health_routes(SERVICE))
        .merge(service_core::metrics_routes())
}

/// Program entry point.
///
/// `#[tokio::main]` is an *attribute macro*. It rewrites this async function
/// into a normal `fn main` that starts the Tokio async runtime and runs the
/// body on it. Without it, `async fn main` would not compile — the language has
/// no built-in runtime.
///
/// The `-> anyhow::Result<()>` return type means "either success carrying
/// nothing (`()`, the empty tuple), or an error". Returning an error from
/// `main` prints it and exits non-zero.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = port_from_env("PORT", DEFAULT_PORT);

    // The container's HEALTHCHECK runs this same binary as
    // `service healthcheck`, because a distroless image has no curl or shell.
    //
    // `std::env::args()` yields the command line; item 0 is the program path,
    // so `.nth(1)` is the first real argument. `.as_deref()` turns
    // `Option<String>` into `Option<&str>` so it can be compared to a literal.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        // `if ... { A } else { B }` is an *expression* in Rust: it evaluates to
        // a value, so it can be passed straight to `exit`.
        std::process::exit(if self_check(port) { 0 } else { 1 });
    }

    init_tracing(SERVICE);
    service_core::init_metrics();

    // `[0, 0, 0, 0]` is 0.0.0.0 — every network interface. Binding to 127.0.0.1
    // instead would make the service unreachable from outside its container.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // The `?` operator: on `Ok(value)` it unwraps to `value`; on `Err(e)` it
    // returns early from `main` with that error. It is the whole reason this
    // function returns a `Result`.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // `%addr` records the value using its `Display` formatting, the same thing
    // `{}` would print. (`?addr` would use `Debug` instead.)
    tracing::info!(%addr, "listening");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Waits for Ctrl-C (which is also what `docker stop` sends).
///
/// Letting the server drain in-flight requests beats having the container
/// killed mid-response.
async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        tracing::info!("shutdown signal received");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn echo_returns_the_message() {
        let request = Request::builder()
            .method("POST")
            .uri("/echo")
            .header(header::CONTENT_TYPE, "application/json")
            // `r#"..."#` is a *raw string*: backslashes and quotes inside are
            // literal, so JSON needs no escaping.
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();

        let response = app().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            r#"{"echo":"hello","service":"echo-service"}"#
        );
    }

    #[tokio::test]
    async fn echo_rejects_a_malformed_body() {
        let request = Request::builder()
            .method("POST")
            .uri("/echo")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"wrong":"field"}"#))
            .unwrap();

        let response = app().oneshot(request).await.unwrap();

        // Nothing in this crate writes this rule: it falls out of `EchoRequest`
        // requiring a `message` field, enforced by the `Json` extractor.
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn health_routes_are_merged_in() {
        let request = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
