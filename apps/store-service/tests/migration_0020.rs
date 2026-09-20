//! Migration 0020 against a database that already holds posted invoices and their tenders.
//!
//! 0020 records whether Notification No. 14/2020-CT applies, freezes that answer on newly posted
//! Sales, guards posted payment evidence against additions, and refuses a registered recipient's
//! mixed taxable/untaxed Sale at posting. The outcomes that must never happen are an existing Sale or
//! tender being rewritten, or an existing Store or Sale coming out of the migration claiming an
//! applicability, a transaction reference or a payment time nobody recorded. So this target builds a
//! database at 0019 holding posted Sales and their tenders — one UPI payment with no reference —
//! applies 0020, and proves both.
//!
//! ```text
//! cargo test --test migration_0020
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 20;

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

/// Every pre-existing column of every Sale, as one comparable string per row. Built from the column
/// list the 0018 database actually has, so the comparison cannot silently skip a column.
async fn sale_rows(pool: &SqlitePool, columns: &[String]) -> Vec<String> {
    let projection = columns
        .iter()
        .map(|column| format!("quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join("||'|'||");
    sqlx::query_scalar(&format!(
        "SELECT {projection} FROM sale_documents ORDER BY id"
    ))
    .fetch_all(pool)
    .await
    .expect("sale rows")
}

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const USER: &str = "01997000-0000-7000-8000-0000000000cc";
const POSTED_CASH: &str = "01997000-0000-7000-8000-0000000000f1";
const POSTED_UPI_NO_REFERENCE: &str = "01997000-0000-7000-8000-0000000000f3";
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";

/// A registered pharmacy that traded before 0020: one Sale paid in cash, one paid by UPI with no
/// transaction reference (lawful then, and history now), and a draft still open at the counter.
async fn populate(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
         gst_registration_status) VALUES ('01997000-0000-7000-8000-0000000000bb','Care Pharmacy',\
         'Asia/Kolkata','2026-01-01T00:00:00.000Z','registered')",
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000cc','owner','owner','Owner',\
         '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    for (id, number, sequence, method, reference, recorded) in [
        (
            POSTED_CASH,
            "INV/2627/000001",
            1,
            "cash",
            None,
            "2026-06-15T10:00:01.250Z",
        ),
        (
            POSTED_UPI_NO_REFERENCE,
            "INV/2627/000002",
            2,
            "upi",
            None::<&str>,
            "2026-06-15T11:30:07.500Z",
        ),
    ] {
        sqlx::query(
            "INSERT INTO sale_documents \
             (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
              document_number,store_gst_registration_status,store_state_code,tax_treatment,\
              taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
              created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
              posting_idempotency_key,posting_fingerprint) \
             VALUES (?,?,'2026-06-15','posted',2,'INV','2026-27',?,?,'registered','27','intra_state',\
              8000,480,480,0,0,8960,?,'2026-06-15T10:00:00.000Z','2026-06-15T10:00:00.000Z',?,\
              '2026-06-15T10:00:00.000Z',?,?)",
        )
        .bind(id)
        .bind(STORE)
        .bind(sequence)
        .bind(number)
        .bind(USER)
        .bind(USER)
        .bind(format!("01997000-0000-7000-8000-0000000001{sequence:02}"))
        .bind("a".repeat(64))
        .execute(pool)
        .await
        .expect("posted sale");
        sqlx::query(
            "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,reference_text,\
             created_at_utc) VALUES (?,?,?,8960,?,?)",
        )
        .bind(format!("01997000-0000-7000-8000-0000000002{sequence:02}"))
        .bind(id)
        .bind(method)
        .bind(reference)
        .bind(recorded)
        .execute(pool)
        .await
        .expect("historical tender");
    }
    sqlx::query(
        "INSERT INTO sale_documents (id,store_id,business_date,status,revision,\
         created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,'2026-06-16','draft',1,?,'2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(OPEN_DRAFT)
    .bind(STORE)
    .bind(USER)
    .execute(pool)
    .await
    .expect("open draft");
}

async fn tender_rows(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT quote(id)||'|'||quote(sale_document_id)||'|'||quote(method)||'|'||\
         quote(amount_paise)||'|'||quote(reference_text)||'|'||quote(created_at_utc) \
         FROM sale_tenders ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("tender rows")
}

const NEW_SALE_COLUMNS: [&str; 2] = [
    "dynamic_qr_snapshot_version",
    "dynamic_qr_applicability_snapshot",
];

const NEW_TRIGGERS: [&str; 6] = [
    "sale_documents_dynamic_qr_draft_only",
    "sale_documents_dynamic_qr_draft_insert",
    "sale_documents_dynamic_qr_snapshot_update",
    "sale_documents_dynamic_qr_snapshot_insert",
    "sale_tenders_posted_no_insert",
    "sale_documents_registered_mixed_supply_refused",
];

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
    assert!(applied.iter().all(|row| row.1 == 1));
    structural_checks(&pool).await;
    let sale_columns = columns_of(&pool, "sale_documents").await;
    for expected in NEW_SALE_COLUMNS {
        assert!(
            sale_columns.iter().any(|name| name == expected),
            "{expected} missing"
        );
    }
    assert!(
        columns_of(&pool, "store_identity")
            .await
            .iter()
            .any(|name| name == "dynamic_qr_applicability")
    );
    let objects = schema_objects(&pool).await;
    for expected in NEW_TRIGGERS {
        assert!(
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} is missing"
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn the_new_migration_preserves_every_sale_and_tender_and_invents_nothing() {
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
    let before_sale_columns = columns_of(&pool, "sale_documents").await;
    let before_rows = sale_rows(&pool, &before_sale_columns).await;
    let before_tenders = tender_rows(&pool).await;
    let before_tender_columns = columns_of(&pool, "sale_tenders").await;
    pool.close().await;

    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;
    structural_checks(&pool).await;

    // 1. Nothing removed, nothing earlier redefined except the two tables that gained columns.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if kind == "table" && ["sale_documents", "store_identity"].contains(&name.as_str()) {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 2. Columns only appended; the tender table is untouched.
    let after_sale_columns = columns_of(&pool, "sale_documents").await;
    assert_eq!(
        after_sale_columns[..before_sale_columns.len()],
        before_sale_columns[..]
    );
    assert_eq!(
        after_sale_columns[before_sale_columns.len()..],
        NEW_SALE_COLUMNS.map(str::to_owned)[..]
    );
    assert_eq!(
        columns_of(&pool, "sale_tenders").await,
        before_tender_columns
    );

    // 3. Every Sale and every tender byte-for-byte unchanged: tax, classification inputs,
    //    references (NULL included) and the times payments were recorded.
    assert_eq!(before_rows, sale_rows(&pool, &before_sale_columns).await);
    assert_eq!(before_tenders, tender_rows(&pool).await);

    // 4. Nothing fabricated: the Store's answer is unknown, and no Sale claims one.
    let answer: String =
        sqlx::query_scalar("SELECT dynamic_qr_applicability FROM store_identity WHERE store_id=?")
            .bind(STORE)
            .fetch_one(&pool)
            .await
            .expect("store answer");
    assert_eq!(answer, "unknown");
    for id in [POSTED_CASH, POSTED_UPI_NO_REFERENCE, OPEN_DRAFT] {
        let snapshot: (i64, Option<String>) = sqlx::query_as(
            "SELECT dynamic_qr_snapshot_version,dynamic_qr_applicability_snapshot \
             FROM sale_documents WHERE id=?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("snapshot");
        assert_eq!(snapshot, (0, None), "{id}");
    }
    let reference: Option<String> =
        sqlx::query_scalar("SELECT reference_text FROM sale_tenders WHERE sale_document_id=?")
            .bind(POSTED_UPI_NO_REFERENCE)
            .fetch_one(&pool)
            .await
            .expect("reference");
    assert_eq!(reference, None, "a transaction reference was invented");

    // 5. The new guards hold on the migrated database: a posted Sale's payments cannot be added to,
    //    a posted Sale cannot take a snapshot, and a draft cannot either.
    let forged = sqlx::query(
        "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000002ff',?,'upi',1,'2026-09-18T00:00:00.000Z')",
    )
    .bind(POSTED_CASH)
    .execute(&pool)
    .await;
    assert!(
        forged
            .err()
            .is_some_and(|error| error.to_string().contains("sale_document_is_posted"))
    );
    assert!(
        sqlx::query("UPDATE sale_documents SET dynamic_qr_snapshot_version=1 WHERE id=?")
            .bind(POSTED_CASH)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE sale_documents SET dynamic_qr_applicability_snapshot='required' WHERE id=?"
        )
        .bind(OPEN_DRAFT)
        .execute(&pool)
        .await
        .is_err()
    );
    // A draft still takes its tender at posting time as before.
    sqlx::query(
        "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000002fe',?,'cash',1,'2026-09-18T00:00:00.000Z')",
    )
    .bind(OPEN_DRAFT)
    .execute(&pool)
    .await
    .expect("a draft's tender is still accepted");
    structural_checks(&pool).await;
    pool.close().await;
}
