use std::{path::Path, time::Duration};

use anyhow::Context;
use sqlx::{
    ConnectOptions, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// How long a statement waits for a contended SQLite write lock before SQLite reports BUSY. This is
/// the single bound every request's lock wait is measured against, so tests derive their limits from
/// it instead of restating a wall-clock literal.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One migration this build carries, as the backup manifest records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedMigration {
    pub version: i64,
    pub description: String,
    pub checksum: String,
}

/// Every migration compiled into this binary, in order.
///
/// A restore compares this against what a backup claims was applied to it. Comparing whole chains
/// rather than a single version number is what catches a database from a fork, a tampered
/// migration, and a backup from a future build — three different problems that a version number
/// alone would report identically, or miss.
pub fn embedded_migrations() -> Vec<EmbeddedMigration> {
    MIGRATOR
        .iter()
        .map(|migration| EmbeddedMigration {
            version: migration.version,
            description: migration.description.to_string(),
            checksum: hex_lower(&migration.checksum),
        })
        .collect()
}

pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The highest migration this build knows how to apply.
pub fn latest_schema_version() -> i64 {
    MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or_default()
}

/// Opens an existing database WITHOUT migrating it.
///
/// Used to inspect a restore candidate before deciding whether it may be migrated at all. The
/// normal [`connect`] would migrate on open, which is precisely what must not happen to a file
/// whose provenance has not yet been established.
pub async fn open_existing_unmigrated(database_path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
        .disable_statement_logging();
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("failed to open candidate database")
}

/// Closes a pool after folding the write-ahead log back into the database file.
///
/// Two separate hazards make this necessary before a database file is moved, copied or hashed.
/// SQLite normally checkpoints when the last connection closes, but `SqlitePool::close` returns
/// before that work has finished — measured on Windows, the file handle is still held for a few
/// tens of milliseconds afterwards. A copy taken in that window catches the database mid-
/// checkpoint, and a rename fails outright with a sharing violation. Checkpointing explicitly,
/// while the pool is unquestionably still alive, removes the first hazard entirely: afterwards the
/// single file is the whole database and the log is empty.
pub async fn checkpoint_and_close(pool: &SqlitePool) -> anyhow::Result<()> {
    let outcome = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await
        .context("failed to fold the write-ahead log into the database");
    // Closed either way: leaving a pool open after a failed checkpoint helps nobody.
    pool.close().await;
    outcome.map(|_| ())
}

/// Applies this build's migrations to a candidate that has already been proven to be ours.
pub async fn migrate_candidate(pool: &SqlitePool) -> anyhow::Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("candidate migration failed")?;
    Ok(())
}

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
        .busy_timeout(BUSY_TIMEOUT)
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
    use uuid::Uuid;

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

    fn id() -> String {
        Uuid::now_v7().to_string()
    }

    async fn insert_company(pool: &SqlitePool, name: &str) -> String {
        let company_id = id();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies \
             (id,display_name,normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,?,lower(?),strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&company_id)
        .bind(name)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
        company_id
    }

    async fn insert_tax_category(pool: &SqlitePool) -> String {
        let category_id = id();
        sqlx::query(
            "INSERT INTO tax_categories \
             (id,jurisdiction,category_code,display_name,tax_treatment,created_at_utc,updated_at_utc) \
             VALUES (?,'IN','standard','Standard','taxable',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&category_id)
        .execute(pool)
        .await
        .unwrap();
        category_id
    }

    #[tokio::test]
    async fn reference_uniqueness_allows_legitimate_name_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("reference-uniqueness.sqlite3"))
            .await
            .unwrap();

        let first_company = insert_company(&pool, "Example Pharma").await;
        let second_company = insert_company(&pool, "Example Pharma").await;
        assert_ne!(first_company, second_company);

        for owner in [&first_company, &second_company] {
            sqlx::query(
                "INSERT INTO brands \
                 (id,display_name,normalized_search_name,brand_owner_company_id,created_at_utc,updated_at_utc) \
                 VALUES (?,'Same Brand','same brand',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(id())
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        }

        sqlx::query(
            "INSERT INTO company_identifiers \
             (id,company_id,namespace,normalized_value,verification_state,created_at_utc,updated_at_utc) \
             VALUES (?,?, 'external', 'ABC123', 'verified',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id())
        .bind(&first_company)
        .execute(&pool)
        .await
        .unwrap();
        let conflict = sqlx::query(
            "INSERT INTO company_identifiers \
             (id,company_id,namespace,normalized_value,verification_state,created_at_utc,updated_at_utc) \
             VALUES (?,?, 'external', 'ABC123', 'verified',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id())
        .bind(&second_company)
        .execute(&pool)
        .await;
        assert!(conflict.is_err());
    }

    #[tokio::test]
    async fn canonical_and_hsn_codes_are_unique() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("codes.sqlite3")).await.unwrap();

        let duplicate_unit = sqlx::query(
            "INSERT INTO units_of_measure \
             (id,canonical_code,display_name,dimension,is_discrete,allowed_scale,created_at_utc,updated_at_utc) \
             VALUES (?,'tablet','Duplicate','count',1,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id())
        .execute(&pool)
        .await;
        assert!(duplicate_unit.is_err());

        let dosage_id = id();
        for attempt in 0..2 {
            let result = sqlx::query(
                "INSERT INTO dosage_forms \
                 (id,canonical_code,display_name,created_at_utc,updated_at_utc) \
                 VALUES (?,'oral_tablet','Oral tablet',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(if attempt == 0 { dosage_id.clone() } else { id() })
            .execute(&pool)
            .await;
            assert_eq!(result.is_ok(), attempt == 0);
        }

        for attempt in 0..2 {
            let result = sqlx::query(
                "INSERT INTO hsn_codes \
                 (id,jurisdiction,hsn_code,description,created_at_utc,updated_at_utc) \
                 VALUES (?,'IN','3004','Medicaments',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(id())
            .execute(&pool)
            .await;
            assert_eq!(result.is_ok(), attempt == 0);
        }
    }

    #[tokio::test]
    async fn tax_constraints_use_integer_basis_points_and_half_open_periods() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("tax.sqlite3")).await.unwrap();
        let category_id = insert_tax_category(&pool).await;

        let invalid_treatment = sqlx::query(
            "INSERT INTO tax_categories \
             (id,jurisdiction,category_code,display_name,tax_treatment,created_at_utc,updated_at_utc) \
             VALUES (?,'IN','bad','Bad','other',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        ).bind(id()).execute(&pool).await;
        assert!(invalid_treatment.is_err());

        let insert_rate = |rate_id: String,
                           from: &'static str,
                           to: Option<&'static str>,
                           cgst: i64| {
            sqlx::query(
                "INSERT INTO tax_rate_versions \
                 (id,tax_category_id,effective_from,effective_to,cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,created_at_utc,updated_at_utc) \
                 VALUES (?,?,?,?,?,250,500,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            ).bind(rate_id).bind(category_id.clone()).bind(from).bind(to).bind(cgst)
        };

        insert_rate(id(), "2025-01-01", Some("2026-01-01"), 250)
            .execute(&pool)
            .await
            .unwrap();
        insert_rate(id(), "2026-01-01", Some("2027-01-01"), 250)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            insert_rate(id(), "2025-06-01", Some("2025-07-01"), 250)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            insert_rate(id(), "2028-01-01", Some("2027-01-01"), 250)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            insert_rate(id(), "2027-01-01", None, -1)
                .execute(&pool)
                .await
                .is_err()
        );

        let real_rate = sqlx::query(
            "INSERT INTO tax_rate_versions \
             (id,tax_category_id,effective_from,cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,created_at_utc,updated_at_utc) \
             VALUES (?,?,'2030-01-01',2.5,0,0,0,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        ).bind(id()).bind(&category_id).execute(&pool).await;
        assert!(real_rate.is_err());

        let storage_type: String =
            sqlx::query_scalar("SELECT typeof(cgst_basis_points) FROM tax_rate_versions LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(storage_type, "integer");
    }

    #[tokio::test]
    async fn audit_events_are_database_append_only() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("audit.sqlite3")).await.unwrap();
        let event_id = id();
        sqlx::query(
            "INSERT INTO master_change_events \
             (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,payload_schema_version,change_payload) \
             VALUES (?,'brand',?,1,'created',strftime('%Y-%m-%dT%H:%M:%fZ','now'),1,'{}')",
        ).bind(&event_id).bind(id()).execute(&pool).await.unwrap();
        assert!(
            sqlx::query("UPDATE master_change_events SET reason='changed' WHERE event_id=?")
                .bind(&event_id)
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM master_change_events WHERE event_id=?")
                .bind(&event_id)
                .execute(&pool)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn operational_database_accepts_exactly_one_durable_store_identity() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("single-store.sqlite3"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Main Store','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id())
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            sqlx::query(
                "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
                 VALUES (?,'Second Store','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            )
            .bind(id())
            .execute(&pool)
            .await
            .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM store_identity")
                .execute(&pool)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn archiving_preserves_a_master_referenced_by_another_master() {
        let temp = tempfile::tempdir().unwrap();
        let pool = connect(&temp.path().join("referenced-archive.sqlite3"))
            .await
            .unwrap();
        let company_id = insert_company(&pool, "Referenced Pharma").await;
        sqlx::query(
            "INSERT INTO brands \
             (id,display_name,normalized_search_name,brand_owner_company_id,created_at_utc,updated_at_utc) \
             VALUES (?,'Referenced Brand','referenced brand',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id())
        .bind(&company_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE pharmaceutical_companies SET status='archived',revision=revision+1, \
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='Inactive', \
             updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&company_id)
        .execute(&pool)
        .await
        .unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM pharmaceutical_companies WHERE id=?")
                .bind(&company_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "archived");
        assert!(
            sqlx::query("DELETE FROM pharmaceutical_companies WHERE id=?")
                .bind(&company_id)
                .execute(&pool)
                .await
                .is_err()
        );
    }
}
