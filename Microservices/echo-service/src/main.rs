//! Reference microservice. Copy this crate as the starting point for a new one.

use axum::{Json, Router, routing::post};
use serde::{Deserialize, Serialize};
use service_core::{health_routes, init_tracing};
use std::net::SocketAddr;

const SERVICE: &str = "echo-service";
const DEFAULT_PORT: u16 = 8080;

#[derive(Debug, Deserialize)]
struct EchoRequest {
    message: String,
}

#[derive(Debug, Serialize)]
struct EchoResponse {
    echo: String,
    service: &'static str,
}

async fn echo(Json(request): Json<EchoRequest>) -> Json<EchoResponse> {
    Json(EchoResponse {
        echo: request.message,
        service: SERVICE,
    })
}

/// Split out from `main` so tests can drive the router without binding a port.
fn app() -> Router {
    Router::new()
        .route("/echo", post(echo))
        .merge(health_routes(SERVICE))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing(SERVICE);

    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Lets the container drain in-flight requests instead of being killed.
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
