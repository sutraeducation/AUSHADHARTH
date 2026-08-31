use std::{path::Path, time::Duration};

use anyhow::Context;
use sqlx::{
    ConnectOptions, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(database_path: &Path) -> anyhow::Result<SqlitePool> {
    let parent = database_path
        .parent()
        .context("database path has no parent")?;
    std::fs::create_dir_all(parent).context("failed to create database directory")?;

    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(5))
        .disable_statement_logging();

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("failed to open local database")?;

    MIGRATOR
        .run(&pool)
        .await
        .context("database migration failed")?;
    ensure_installation_identity(&pool).await?;
    Ok(pool)
}

async fn ensure_installation_identity(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut transaction = pool.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM installation_identity")
        .fetch_one(&mut *transaction)
        .await?;
    if count == 0 {
        let installation_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO installation_identity (installation_id, created_at_utc) \
             VALUES (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(installation_id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::Row;

    use super::*;

    #[tokio::test]
    async fn fresh_database_has_pragmas_and_migration_is_restart_safe() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("foundation.sqlite3");

        let first = connect(&path).await.unwrap();
        let foreign_keys: i64 = sqlx::query("PRAGMA foreign_keys")
            .fetch_one(&first)
            .await
            .unwrap()
            .get(0);
        let journal_mode: String = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&first)
            .await
            .unwrap()
            .get(0);
        let metadata_count: i64 = sqlx::query("SELECT COUNT(*) FROM application_metadata")
            .fetch_one(&first)
            .await
            .unwrap()
            .get(0);
        let identity_count: i64 = sqlx::query("SELECT COUNT(*) FROM installation_identity")
            .fetch_one(&first)
            .await
            .unwrap()
            .get(0);
        assert_eq!(foreign_keys, 1);
        assert_eq!(journal_mode.to_lowercase(), "wal");
        assert_eq!(metadata_count, 1);
        assert_eq!(identity_count, 1);
        first.close().await;

        let second = connect(&path).await.unwrap();
        let metadata_count: i64 = sqlx::query("SELECT COUNT(*) FROM application_metadata")
            .fetch_one(&second)
            .await
            .unwrap()
            .get(0);
        let identity_count: i64 = sqlx::query("SELECT COUNT(*) FROM installation_identity")
            .fetch_one(&second)
            .await
            .unwrap()
            .get(0);
        assert_eq!(metadata_count, 1);
        assert_eq!(identity_count, 1);
    }
}
