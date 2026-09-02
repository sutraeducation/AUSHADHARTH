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
    let database = database::connect(&paths.database_file()).await?;

    let address = loopback_address(DEFAULT_PORT);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("failed to bind Store Service loopback listener")?;
    info!(address = %address, api_version = "v1", "Store Service started");
    let web_dist = repository_root.join("apps").join("web").join("dist");
    axum::serve(listener, api::router(database, Some(web_dist)))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Store Service stopped unexpectedly")
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
