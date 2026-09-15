use std::{path::PathBuf, sync::Arc};

use axum::{Json, Router, routing::get};
use serde::Serialize;
use sqlx::SqlitePool;
use tower_http::services::{ServeDir, ServeFile};

use crate::{API_VERSION, APPLICATION_VERSION};

pub mod auth;
pub mod backups;
pub mod inventory;
pub mod parties;
pub mod product_catalog;
pub mod purchases;
pub mod reference_masters;
pub mod returns;
pub mod sales;
pub mod stock_operations;
pub mod store_profile;

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

/// The router as every test builds it: no backup service, so no route can touch a data directory.
pub fn router(pool: SqlitePool, web_dist: Option<PathBuf>) -> Router {
    router_with_backups(pool, web_dist, None)
}

/// The router the real service builds.
///
/// Opt-in rather than opt-out on purpose. Backup and restore are the only routes that write
/// outside the database and the only ones that can destroy it, so making them unreachable unless
/// a caller deliberately supplies a directory is what keeps a stray test away from a real pharmacy.
pub fn router_with_backups(
    pool: SqlitePool,
    web_dist: Option<PathBuf>,
    backups: Option<Arc<backups::BackupService>>,
) -> Router {
    let state = reference_masters::ReferenceState {
        pool,
        backups: backups.clone(),
    };
    let router = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/system/info", get(system_info))
        .merge(auth::routes())
        .merge(reference_masters::routes())
        .merge(product_catalog::routes())
        .merge(inventory::routes())
        .merge(parties::routes())
        .merge(store_profile::routes())
        .merge(purchases::routes())
        .merge(sales::routes())
        .merge(returns::routes())
        .merge(stock_operations::routes())
        .merge(backups::routes());

    // The download route exists only when there is a directory to serve, and the owner check runs
    // before `ServeDir` ever sees the request: the file server itself knows nothing about sessions.
    let router = match &backups {
        Some(service) => {
            // A router of its own so the owner check wraps only these paths. Layering the guard
            // onto the main router would put it in front of every route in the product.
            let files: Router = Router::new()
                .fallback_service(ServeDir::new(service.backups_directory.clone()))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    backups::guard_backup_files,
                ));
            router.nest_service("/api/v1/backup-files", files)
        }
        None => router,
    };

    // Once a restore has replaced the database, every route answers `service_restoring` until the
    // service restarts. The static web app is deliberately outside this: the browser still has to
    // load in order to show what is happening.
    let router = router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        backups::guard_while_restoring,
    ));

    let router = router.with_state(state);

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
