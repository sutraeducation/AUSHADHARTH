//! Migration 0016 against a database that already holds a pharmacy.
//!
//! A migration is the one change that cannot be corrected by a later commit: it runs on the
//! customer's only copy of their records, once, unattended. Phase 1I's rebuild of
//! `master_change_events` silently dropped a column and invented constraints, and the only reason it
//! never shipped was that the suite happened to exercise the table. This target removes the
//! "happened to": it builds a database at the previous migration from the files on disk, fills it
//! with rows in every table 0016 touches, applies 0016, and then asserts that nothing was lost.
//!
//! The migrations are read from the directory at runtime rather than through `sqlx::migrate!`,
//! because the embedded set always contains 0016 — a test built on it could not construct the
//! "before" state at all.

use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 16;

fn migrations_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// Copies every migration up to `highest` into a temporary directory sqlx can run from.
fn migrations_up_to(highest: i64, into: &Path) {
    std::fs::create_dir_all(into).expect("migration directory");
    let mut copied = 0;
    for entry in std::fs::read_dir(migrations_directory()).expect("migrations") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(version) = name
            .split('_')
            .next()
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        if version <= highest {
            std::fs::copy(entry.path(), into.join(&name)).expect("copy migration");
            copied += 1;
        }
    }
    assert_eq!(
        copied as i64, highest,
        "expected {highest} migrations up to {highest}, copied {copied}"
    );
}

async fn open(path: &Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePool::connect_with(options)
        .await
        .expect("open database")
}

async fn run_migrations(pool: &SqlitePool, directory: &Path) {
    Migrator::new(directory)
        .await
        .expect("migrator")
        .run(pool)
        .await
        .expect("migrations");
}

/// Every table, index, trigger and view, with its definition, as one comparable list.
async fn schema_objects(pool: &SqlitePool) -> Vec<(String, String, String)> {
    sqlx::query_as(
        "SELECT type,name,COALESCE(sql,'') FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )
    .fetch_all(pool)
    .await
    .expect("schema")
}

async fn columns_of(pool: &SqlitePool, table: &str) -> Vec<String> {
    let rows: Vec<(i64, String, String, i64, Option<String>, i64)> =
        sqlx::query_as(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .expect("table info");
    rows.into_iter().map(|row| row.1).collect()
}

async fn structural_checks(pool: &SqlitePool) {
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .expect("integrity check");
    assert_eq!(integrity, "ok");
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .expect("foreign key check");
    assert!(
        violations.is_empty(),
        "foreign key violations: {violations:?}"
    );
}

/// A pharmacy's worth of rows in the tables 0016 touches, so "nothing was lost" has something to
/// lose. `master_change_events` is the one 0016 rebuilds, and every column of it is filled —
/// including the ones a careless rebuild would quietly drop.
async fn populate(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
    )
    .execute(pool)
    .await
    .expect("installation");
    sqlx::query(
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000bb','Care Pharmacy','Asia/Kolkata',\
         '2026-01-01T00:00:00.000Z')",
    )
    .execute(pool)
    .await
    .expect("store");

    for (index, entity_type) in ["product", "party", "sale_document"]
        .into_iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO master_change_events \
             (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
              payload_schema_version,change_payload,reason,actor_id,terminal_id) \
             VALUES (?,?,?,?,'created','2026-01-02T03:04:05.678Z',1,'{\"a\":1}',\
             'a recorded reason','01997000-0000-7000-8000-0000000000cc','counter-1')",
        )
        .bind(format!(
            "01997000-0000-7000-8000-0000000000{:02x}",
            0xd0 + index
        ))
        .bind(entity_type)
        .bind(format!(
            "01997000-0000-7000-8000-0000000000{:02x}",
            0xe0 + index
        ))
        .bind(index as i64 + 1)
        .execute(pool)
        .await
        .expect("change event");
    }
}

/// A fresh database reaches the new migration cleanly and is structurally sound.
#[tokio::test]
async fn a_fresh_database_migrates_to_the_new_version() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("fresh.sqlite3")).await;
    run_migrations(&pool, &directory).await;

    let applied: Vec<(i64, i64)> =
        sqlx::query_as("SELECT version,success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("applied migrations");
    assert_eq!(applied.len() as i64, NEW_MIGRATION);
    assert!(
        applied.iter().all(|row| row.1 == 1),
        "a migration did not succeed"
    );

    // The product marker this build stamps, which a restore uses to recognise its own databases.
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&pool)
        .await
        .expect("application id");
    assert_eq!(
        application_id, 0x4155_5348,
        "the product marker was not stamped"
    );

    structural_checks(&pool).await;
    assert!(
        schema_objects(&pool)
            .await
            .iter()
            .any(|(_, name, _)| name == "restore_provenance"),
        "restore_provenance is missing from a fresh database"
    );
    pool.close().await;
}

/// The proof that matters: 0016 applied to a database that already holds a pharmacy.
#[tokio::test]
async fn the_new_migration_preserves_everything_in_a_populated_database() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate(&pool).await;

    let before_objects = schema_objects(&pool).await;
    let before_columns = columns_of(&pool, "master_change_events").await;
    let before_events: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT event_id,entity_type,change_payload,reason,actor_id,terminal_id \
             FROM master_change_events ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .expect("events before");
    assert_eq!(
        before_events.len(),
        3,
        "the fixture recorded nothing to lose"
    );
    pool.close().await;

    // The migration under test, applied exactly as it would be on a customer's machine.
    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;

    structural_checks(&pool).await;

    // 1. Nothing that existed was removed.
    let after_objects = schema_objects(&pool).await;
    let missing: Vec<&(String, String, String)> = before_objects
        .iter()
        .filter(|(kind, name, _)| {
            !after_objects
                .iter()
                .any(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "the migration removed schema objects: {missing:?}"
    );
    assert!(
        after_objects.len() > before_objects.len(),
        "the migration added nothing, so it is not the migration under test"
    );

    // 2. The rebuilt table kept every column, in order. A dropped column is the exact defect this
    //    file exists to catch.
    let after_columns = columns_of(&pool, "master_change_events").await;
    assert_eq!(
        before_columns, after_columns,
        "master_change_events lost or reordered a column"
    );

    // 3. Every row survived with every field intact, including the ones only a careful rebuild
    //    carries across.
    let after_events: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT event_id,entity_type,change_payload,reason,actor_id,terminal_id \
             FROM master_change_events ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .expect("events after");
    assert_eq!(
        before_events, after_events,
        "history did not survive the migration"
    );

    // 4. The new entity types are accepted, and an invented one is still refused.
    for entity_type in ["backup", "restore_operation"] {
        sqlx::query(
            "INSERT INTO master_change_events \
             (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
              payload_schema_version,change_payload) \
             VALUES (?,?,'01997000-0000-7000-8000-0000000000ff',1,'created',\
             '2026-01-02T03:04:05.678Z',1,'{}')",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(entity_type)
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("{entity_type} was refused after 0016: {error}"));
    }
    let refused = sqlx::query(
        "INSERT INTO master_change_events \
         (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
          payload_schema_version,change_payload) \
         VALUES (?,'not_a_real_entity','01997000-0000-7000-8000-0000000000ff',1,'created',\
         '2026-01-02T03:04:05.678Z',1,'{}')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .execute(&pool)
    .await;
    assert!(
        refused.is_err(),
        "the rebuilt CHECK accepts anything, so it is not the frozen constraint"
    );

    // 5. The new table is append-only, as its triggers claim.
    sqlx::query(
        "INSERT INTO restore_provenance \
         (restore_id,restored_at_utc,source_backup_created_at_utc,source_store_id,\
          source_installation_id,new_installation_id,source_database_sha256,\
          backup_format_version,source_schema_version,migrated_to_schema_version) \
         VALUES ('01997000-0000-7000-8000-00000000aa01','2026-02-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z','store','installation','new-installation',?,1,15,16)",
    )
    .bind("a".repeat(64))
    .execute(&pool)
    .await
    .expect("provenance row");
    for statement in [
        "UPDATE restore_provenance SET source_store_id='changed'",
        "DELETE FROM restore_provenance",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "restore_provenance allowed: {statement}"
        );
    }

    pool.close().await;
}
