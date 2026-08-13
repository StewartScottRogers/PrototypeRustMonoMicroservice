//! Liveness and readiness probes.

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// Body returned by both probes.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Probe {
    /// `ok` for liveness, `ready` for readiness.
    pub status: &'static str,
    /// Which service answered, so a probe against the wrong port is obvious.
    pub service: String,
}

/// Builds a router exposing `/healthz` and `/readyz` for `service`.
pub fn health_routes(service: &str) -> Router {
    let live = service.to_owned();
    let ready = service.to_owned();

    Router::new()
        .route("/healthz", get(move || probe("ok", live.clone())))
        .route("/readyz", get(move || probe("ready", ready.clone())))
}

async fn probe(status: &'static str, service: String) -> Json<Probe> {
    Json(Probe { status, service })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn get_body(path: &str) -> (StatusCode, String) {
        let response = health_routes("test-service")
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

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
}
