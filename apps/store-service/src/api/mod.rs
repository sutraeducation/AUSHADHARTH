use std::path::PathBuf;

use axum::{Json, Router, routing::get};
use serde::Serialize;
use sqlx::SqlitePool;
use tower_http::services::{ServeDir, ServeFile};

use crate::{API_VERSION, APPLICATION_VERSION};

pub mod product_catalog;
pub mod reference_masters;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    status: &'static str,
    api_version: &'static str,
    application_version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compatibility {
    minimum_web_version: &'static str,
    maximum_web_major_version: u16,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfoResponse {
    status: &'static str,
    api_version: &'static str,
    application_version: &'static str,
    compatibility: Compatibility,
}

pub fn router(pool: SqlitePool, web_dist: Option<PathBuf>) -> Router {
    let router = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/system/info", get(system_info))
        .merge(reference_masters::routes())
        .merge(product_catalog::routes())
        .with_state(reference_masters::ReferenceState { pool });

    if let Some(dist) = web_dist {
        router.fallback_service(
            ServeDir::new(&dist).fallback(ServeFile::new(dist.join("index.html"))),
        )
    } else {
        router
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        api_version: API_VERSION,
        application_version: APPLICATION_VERSION,
    })
}

async fn system_info() -> Json<SystemInfoResponse> {
    Json(SystemInfoResponse {
        status: "ok",
        api_version: API_VERSION,
        application_version: APPLICATION_VERSION,
        compatibility: Compatibility {
            minimum_web_version: "0.0.0",
            maximum_web_major_version: 0,
        },
    })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn health_endpoint_works_without_internet() {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("api.sqlite3"))
            .await
            .unwrap();
        let response = router(pool, None)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("\"status\":\"ok\""));
        assert!(text.contains("\"apiVersion\":\"v1\""));
    }
}
