use std::sync::Arc;

use anyhow::Context;
use aushadharth_store_service::{
    DEFAULT_PORT, api, infrastructure::database, infrastructure::logging, loopback_address,
    platform::runtime_paths::RuntimePaths,
};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let repository_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .context("store-service must remain under apps/store-service")?
        .to_path_buf();
    let paths = RuntimePaths::resolve(&repository_root)?;
    paths.create_required_directories()?;
    let _logging_guard = logging::initialize(&paths.logs)?;
    let database_file = paths.database_file();

    // Before anything opens the database. A restore that was interrupted leaves the
    // authoritative path in a state only the journal can explain, and that judgement has to be
    // made while the file is still closed — afterwards the question is unanswerable.
    api::backups::recover_interrupted_restore(&paths.backups, &database_file).await?;

    let database = database::connect(&database_file).await?;

    // Now that the restored database is open, migrated and proven, the restore can be finished:
    // a new installation identity, the lineage row, and every inherited session revoked.
    if api::backups::complete_restore_after_open(&database, &paths.backups).await? {
        info!("restore completed; installation re-identified and sessions revoked");
    }

    let backups = Arc::new(api::backups::BackupService::new(
        paths.backups.clone(),
        database_file.clone(),
    ));

    let address = loopback_address(DEFAULT_PORT);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("failed to bind Store Service loopback listener")?;
    info!(address = %address, api_version = "v1", "Store Service started");
    let web_dist = repository_root.join("apps").join("web").join("dist");
    let router = api::router_with_backups(database, Some(web_dist), Some(backups));
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Store Service stopped unexpectedly")
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
