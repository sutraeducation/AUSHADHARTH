//! Migration 0017 against a database that already holds a pharmacy and its posted invoices.
//!
//! The same discipline `migration_0017`'s predecessor established, aimed at what 0017 actually
//! risks. Two things make this migration more dangerous than it looks:
//!
//!   * it rebuilds `master_change_events` again, which is the table a careless rebuild has already
//!     damaged once in this project's history;
//!   * it adds a seller snapshot to `sale_documents`, and the one outcome that must never happen is
//!     a pre-existing posted Sale coming out of the migration claiming to hold seller facts that
//!     nobody recorded.
//!
//! So this target builds a database at 0016 from the files on disk, posts real history into it,
//! applies 0017, and asserts both that nothing was lost and that nothing was invented.
//!
//! ```text
//! cargo test --test migration_0017
//! ```

use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 17;

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

/// One audit row as this file compares them: id, entity, payload, and the three columns a careless
/// rebuild would drop.
type AuditRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const USER: &str = "01997000-0000-7000-8000-0000000000cc";
const SALE: &str = "01997000-0000-7000-8000-0000000000f1";

/// A pharmacy that already traded before 0017 existed: a store, a user, and a posted invoice
/// carrying the Phase 1H store snapshot — which is all the seller identity that could be recorded
/// at the time.
async fn populate(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
    )
    .execute(pool)
    .await
    .expect("installation");
    sqlx::query(
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
         gst_registration_status) \
         VALUES (?,'Care Pharmacy','Asia/Kolkata','2026-01-01T00:00:00.000Z','unknown')",
    )
    .bind(STORE)
    .execute(pool)
    .await
    .expect("store");
    sqlx::query(
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) \
         VALUES (?,'owner','owner','Owner','$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',\
         'owner_admin','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
    )
    .bind(USER)
    .execute(pool)
    .await
    .expect("user");

    // A posted Sale with the Phase 1H snapshot set. No lines: this target is about the header
    // columns 0017 touches, and the line schema is untouched by it.
    sqlx::query(
        "INSERT INTO sale_documents \
         (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
          document_number,store_gst_registration_status,store_state_code,tax_treatment,\
          taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
          created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
          posting_idempotency_key,posting_fingerprint) \
         VALUES (?,?,'2026-06-15','posted',2,'INV','2026-27',1,'INV/2627/000001','unknown','27',\
          'intra_state',10000,900,900,0,0,11800,?,'2026-06-15T10:00:00.000Z',\
          '2026-06-15T10:00:00.000Z',?,'2026-06-15T10:00:00.000Z',\
          '01997000-0000-7000-8000-0000000000f2',?)",
    )
    .bind(SALE)
    .bind(STORE)
    .bind(USER)
    .bind(USER)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .expect("posted sale");

    for (index, entity_type) in ["product", "party", "sale_document"]
        .into_iter()
        .enumerate()
    {
        sqlx::query(
            "INSERT INTO master_change_events \
             (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
              payload_schema_version,change_payload,reason,actor_id,terminal_id) \
             VALUES (?,?,?,?,'created','2026-01-02T03:04:05.678Z',1,'{\"a\":1}',\
             'a recorded reason',?,'counter-1')",
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
        .bind(USER)
        .execute(pool)
        .await
        .expect("change event");
    }
}

/// A fresh database reaches 0017 cleanly and carries every new structure.
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

    structural_checks(&pool).await;

    let objects = schema_objects(&pool).await;
    for expected in [
        "store_addresses",
        "store_licences",
        "store_licences_active_number_uq",
        "store_identity_profile_integrity_update",
        "sale_documents_seller_snapshot_insert",
        "sale_documents_seller_snapshot_update",
        "sale_documents_seller_snapshot_draft_only",
    ] {
        assert!(
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} is missing from a fresh database"
        );
    }

    // The store marker from 0016 survives a second rebuild of master_change_events.
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&pool)
        .await
        .expect("application id");
    assert_eq!(application_id, 0x4155_5348);
    pool.close().await;
}

/// The proof that matters: 0017 applied to a database that already holds posted invoices.
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
    let before_event_columns = columns_of(&pool, "master_change_events").await;
    let before_sale_columns = columns_of(&pool, "sale_documents").await;
    let before_events: Vec<AuditRow> = sqlx::query_as(
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
    let before_sale: (String, String, i64, String) = sqlx::query_as(
        "SELECT document_number,store_state_code,grand_total_paise,posting_fingerprint \
         FROM sale_documents WHERE id=?",
    )
    .bind(SALE)
    .fetch_one(&pool)
    .await
    .expect("sale before");
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

    // 2. The rebuilt audit table kept every column, in order.
    assert_eq!(
        before_event_columns,
        columns_of(&pool, "master_change_events").await,
        "master_change_events lost or reordered a column"
    );

    // 3. `sale_documents` only GAINED columns, and the existing ones kept their order — an ALTER
    //    that reordered them would silently break every `SELECT *` in the service.
    let after_sale_columns = columns_of(&pool, "sale_documents").await;
    assert_eq!(
        after_sale_columns[..before_sale_columns.len()],
        before_sale_columns[..],
        "sale_documents reordered or lost an existing column"
    );
    for expected in [
        "seller_snapshot_version",
        "seller_legal_name",
        "seller_trade_name",
        "seller_address_line1",
        "seller_address_line2",
        "seller_city",
        "seller_postal_code",
        "seller_state_name",
        "seller_phone",
        "seller_email",
        "seller_licence_text",
    ] {
        assert!(
            after_sale_columns.iter().any(|name| name == expected),
            "{expected} is missing after the migration"
        );
    }

    // 4. Every audit row survived with every field intact.
    let after_events: Vec<AuditRow> = sqlx::query_as(
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

    // 5. The posted invoice is untouched.
    let after_sale: (String, String, i64, String) = sqlx::query_as(
        "SELECT document_number,store_state_code,grand_total_paise,posting_fingerprint \
         FROM sale_documents WHERE id=?",
    )
    .bind(SALE)
    .fetch_one(&pool)
    .await
    .expect("sale after");
    assert_eq!(before_sale, after_sale, "a posted invoice changed");

    // 6. THE POINT OF THIS FILE: the pre-existing Sale is honestly marked as having no seller
    //    snapshot, and no seller fact was invented for it.
    let legacy: (i64, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT seller_snapshot_version,seller_legal_name,seller_address_line1,seller_licence_text \
         FROM sale_documents WHERE id=?",
    )
    .bind(SALE)
    .fetch_one(&pool)
    .await
    .expect("legacy snapshot");
    assert_eq!(
        legacy.0, 0,
        "an existing Sale was given a seller snapshot version"
    );
    assert_eq!(
        legacy.1, None,
        "a seller name was fabricated for an old Sale"
    );
    assert_eq!(
        legacy.2, None,
        "a seller address was fabricated for an old Sale"
    );
    assert_eq!(
        legacy.3, None,
        "a seller licence was fabricated for an old Sale"
    );

    // 7. The new entity types are accepted and an invented one is still refused.
    for entity_type in ["store_profile", "store_address", "store_licence"] {
        sqlx::query(
            "INSERT INTO master_change_events \
             (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
              payload_schema_version,change_payload) \
             VALUES (?,?,'01997000-0000-7000-8000-0000000000ff',1,'updated',\
             '2026-01-02T03:04:05.678Z',1,'{}')",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(entity_type)
        .execute(&pool)
        .await
        .unwrap_or_else(|error| panic!("{entity_type} was refused after 0017: {error}"));
    }
    let refused = sqlx::query(
        "INSERT INTO master_change_events \
         (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,\
          payload_schema_version,change_payload) \
         VALUES (?,'not_a_real_entity','01997000-0000-7000-8000-0000000000ff',1,'updated',\
         '2026-01-02T03:04:05.678Z',1,'{}')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .execute(&pool)
    .await;
    assert!(
        refused.is_err(),
        "the rebuilt CHECK accepts anything, so it is not the frozen constraint"
    );

    // 8. A version-1 snapshot missing any of the three particulars the Drugs Rules require is
    //    refused by the database, not merely by the service.
    let incomplete = sqlx::query(
        "INSERT INTO sale_documents \
         (id,store_id,business_date,status,revision,created_by_user_id,created_at_utc,\
          updated_at_utc,seller_snapshot_version,seller_legal_name) \
         VALUES ('01997000-0000-7000-8000-0000000000f9',?,'2026-06-16','draft',1,?,\
          '2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z',1,'Care Pharmacy')",
    )
    .bind(STORE)
    .bind(USER)
    .execute(&pool)
    .await;
    assert!(
        incomplete.is_err(),
        "a seller snapshot without an address or licence was accepted"
    );

    // 9. A draft cannot quietly acquire a snapshot version before it is posted.
    sqlx::query(
        "INSERT INTO sale_documents \
         (id,store_id,business_date,status,revision,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000fa',?,'2026-06-16','draft',1,?,\
          '2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(STORE)
    .bind(USER)
    .execute(&pool)
    .await
    .expect("draft");
    let premature = sqlx::query(
        "UPDATE sale_documents SET seller_snapshot_version=1,seller_legal_name='X',\
         seller_address_line1='Y',seller_licence_text='Z' \
         WHERE id='01997000-0000-7000-8000-0000000000fa'",
    )
    .execute(&pool)
    .await;
    assert!(
        premature.is_err(),
        "a draft was allowed to carry a seller snapshot"
    );

    // 10. The new tables accept a real profile and refuse a second active licence with the same
    //     number.
    sqlx::query(
        "INSERT INTO store_addresses (id,store_id,line1,city,postal_code,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-00000000ab01',?,'12 Market Road','Pune','411001',\
          '2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(STORE)
    .execute(&pool)
    .await
    .expect("address");
    let second_address = sqlx::query(
        "INSERT INTO store_addresses (id,store_id,line1,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-00000000ab02',?,'Another Road',\
          '2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(STORE)
    .execute(&pool)
    .await;
    assert!(
        second_address.is_err(),
        "a store was allowed two operating addresses"
    );

    for (id, number) in [
        ("01997000-0000-7000-8000-00000000ac01", "20B-1234"),
        ("01997000-0000-7000-8000-00000000ac02", "21B-5678"),
    ] {
        sqlx::query(
            "INSERT INTO store_licences \
             (id,store_id,licence_type,licence_number,normalized_licence_number,\
              created_at_utc,updated_at_utc) \
             VALUES (?,?,'Retail',?,?,'2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
        )
        .bind(id)
        .bind(STORE)
        .bind(number)
        .bind(number.replace('-', ""))
        .execute(&pool)
        .await
        .expect("licence");
    }
    let duplicate = sqlx::query(
        "INSERT INTO store_licences \
         (id,store_id,licence_type,licence_number,normalized_licence_number,\
          created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-00000000ac03',?,'Retail','20b 1234','20B1234',\
          '2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(STORE)
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "the same licence was accepted twice while active"
    );

    pool.close().await;
}
