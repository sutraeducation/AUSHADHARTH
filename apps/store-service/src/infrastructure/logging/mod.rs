use std::path::Path;

use anyhow::Context;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

pub fn initialize(log_directory: &Path) -> anyhow::Result<WorkerGuard> {
    std::fs::create_dir_all(log_directory).context("failed to create log directory")?;
    let file_appender = tracing_appender::rolling::daily(log_directory, "store-service.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_current_span(false)
        .with_span_list(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to initialize structured logging: {error}"))?;
    Ok(guard)
}
