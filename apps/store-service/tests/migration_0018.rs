//! Migration 0018 against a database that already holds customers and posted invoices.
//!
//! 0018 adds the recipient address snapshot. The outcome that must never happen is a Sale posted
//! before it came out of the migration claiming an address nobody recorded — least of all one
//! borrowed from its customer's CURRENT record, which is exactly the address a careless backfill
//! would reach for. So this target builds a database at 0017, gives a customer a billing address,
//! posts history against that customer, applies 0018, and proves nothing was lost and nothing was
//! invented.
//!
//! ```text
//! cargo test --test migration_0018
//! ```

use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 18;

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
/// list the 0017 database actually has, so the comparison cannot silently skip a column.
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
const PARTY: &str = "01997000-0000-7000-8000-0000000000dd";
const POSTED_WALK_IN: &str = "01997000-0000-7000-8000-0000000000f1";
const POSTED_TO_PARTY: &str = "01997000-0000-7000-8000-0000000000f3";
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";
const MAHARASHTRA: &str = "01997300-0000-7000-8000-000000000027";

/// A pharmacy that already traded before 0018: a customer WITH a billing address on record, a
/// posted Sale to that customer, a posted walk-in Sale, and a draft still open at the counter.
async fn populate(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
         gst_registration_status) VALUES ('01997000-0000-7000-8000-0000000000bb','Care Pharmacy',\
         'Asia/Kolkata','2026-01-01T00:00:00.000Z','unknown')",
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000cc','owner','owner','Owner',\
         '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
         place_of_supply_state_id,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000dd','Rahul Deshmukh','rahul deshmukh',\
         'unregistered','01997300-0000-7000-8000-000000000027',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000de','01997000-0000-7000-8000-0000000000dd',\
         'customer','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_addresses (id,party_id,address_role,line1,city,state_id,postal_code,\
         is_primary,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000df','01997000-0000-7000-8000-0000000000dd',\
         'billing','7 Mill Road','Pune','01997300-0000-7000-8000-000000000027','411001',1,\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }

    // Two posted Sales carrying the 1H store snapshot and the 1L-A seller snapshot, one of them to
    // the customer whose address is on record today.
    for (id, party, number, sequence) in [
        (POSTED_WALK_IN, None, "INV/2627/000001", 1),
        (POSTED_TO_PARTY, Some(PARTY), "INV/2627/000002", 2),
    ] {
        sqlx::query(
            "INSERT INTO sale_documents \
             (id,store_id,customer_party_id,business_date,status,revision,series_code,\
              financial_year,sequence_value,document_number,store_gst_registration_status,\
              store_state_code,customer_display_name,customer_gst_registration_status,\
              customer_state_code,tax_treatment,taxable_value_paise,cgst_paise,sgst_paise,\
              igst_paise,cess_paise,grand_total_paise,created_by_user_id,created_at_utc,\
              updated_at_utc,posted_by_user_id,posted_at_utc,posting_idempotency_key,\
              posting_fingerprint,seller_snapshot_version,seller_legal_name,seller_address_line1,\
              seller_licence_text) \
             VALUES (?,?,?,'2026-06-15','posted',2,'INV','2026-27',?,?,'unknown','27',?,?,?,\
              'intra_state',6000000,360000,360000,0,0,6720000,?,'2026-06-15T10:00:00.000Z',\
              '2026-06-15T10:00:00.000Z',?,'2026-06-15T10:00:00.000Z',?,?,1,\
              'Care Pharmacy Private Limited','12 Market Road','Form 20: MH-20-1234')",
        )
        .bind(id)
        .bind(STORE)
        .bind(party)
        .bind(sequence)
        .bind(number)
        .bind(party.map(|_| "Rahul Deshmukh"))
        .bind(party.map(|_| "unregistered"))
        .bind(party.map(|_| "27"))
        .bind(USER)
        .bind(USER)
        .bind(format!("01997000-0000-7000-8000-0000000001{sequence:02}"))
        .bind("a".repeat(64))
        .execute(pool)
        .await
        .expect("posted sale");
    }

    sqlx::query(
        "INSERT INTO sale_documents (id,store_id,customer_party_id,business_date,status,revision,\
         created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,'2026-06-16','draft',1,?,'2026-06-16T10:00:00.000Z','2026-06-16T10:00:00.000Z')",
    )
    .bind(OPEN_DRAFT)
    .bind(STORE)
    .bind(PARTY)
    .bind(USER)
    .execute(pool)
    .await
    .expect("open draft");
}

const NEW_COLUMNS: [&str; 18] = [
    "recipient_snapshot_version",
    "recipient_particulars_requested",
    "recipient_address_source",
    "recipient_address_line1",
    "recipient_address_line2",
    "recipient_city",
    "recipient_postal_code",
    "recipient_state_id",
    "recipient_state_name",
    "recipient_state_code",
    "delivery_same_as_recipient",
    "delivery_address_line1",
    "delivery_address_line2",
    "delivery_city",
    "delivery_postal_code",
    "delivery_state_id",
    "delivery_state_name",
    "delivery_state_code",
];

const NEW_TRIGGERS: [&str; 4] = [
    "sale_documents_recipient_draft_insert",
    "sale_documents_recipient_draft_update",
    "sale_documents_recipient_snapshot_insert",
    "sale_documents_recipient_snapshot_update",
];

/// A fresh database reaches 0018 cleanly and carries every new structure.
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

    let columns = columns_of(&pool, "sale_documents").await;
    for expected in NEW_COLUMNS {
        assert!(
            columns.iter().any(|name| name == expected),
            "{expected} is missing from a fresh database"
        );
    }
    let objects = schema_objects(&pool).await;
    for expected in NEW_TRIGGERS {
        assert!(
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} is missing from a fresh database"
        );
    }
    // Every 1L-A seller guard is still in place beside the new recipient guards.
    for expected in [
        "sale_documents_seller_snapshot_insert",
        "sale_documents_seller_snapshot_update",
        "sale_documents_seller_snapshot_draft_only",
        "sale_documents_posted_no_update",
    ] {
        assert!(
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} was lost"
        );
    }
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(&pool)
        .await
        .expect("application id");
    assert_eq!(application_id, 0x4155_5348);
    pool.close().await;
}

/// The proof that matters: 0018 applied to a database that already holds posted invoices and a
/// customer whose address is on record today.
#[tokio::test]
async fn the_new_migration_preserves_everything_and_invents_no_recipient_history() {
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
    assert_eq!(before_rows.len(), 3, "the fixture recorded nothing to lose");
    let before_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events");
    let before_addresses: Vec<(String, String)> =
        sqlx::query_as("SELECT id,line1 FROM party_addresses ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("addresses");
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
    // ...and nothing that existed was redefined. 0018 is additive: it adds columns and triggers and
    // rewrites no earlier object.
    for (kind, name, sql) in &before_objects {
        if kind == "table" && name == "sale_documents" {
            continue; // Gains columns by ALTER, which rewrites its stored CREATE statement.
        }
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .expect("present");
        assert_eq!(&after.2, sql, "{kind} {name} was redefined by 0018");
    }

    // 2. `sale_documents` only GAINED columns, in order, and gained exactly the new ones.
    let after_sale_columns = columns_of(&pool, "sale_documents").await;
    assert_eq!(
        after_sale_columns[..before_sale_columns.len()],
        before_sale_columns[..],
        "sale_documents reordered or lost an existing column"
    );
    assert_eq!(
        after_sale_columns[before_sale_columns.len()..],
        NEW_COLUMNS.map(str::to_owned)[..],
        "0018 added something other than the recipient columns"
    );

    // 3. Every pre-existing column of every Sale is byte-for-byte what it was.
    assert_eq!(
        before_rows,
        sale_rows(&pool, &before_sale_columns).await,
        "an existing Sale changed"
    );
    let after_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events");
    assert_eq!(before_events, after_events, "audit history changed");
    let after_addresses: Vec<(String, String)> =
        sqlx::query_as("SELECT id,line1 FROM party_addresses ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("addresses");
    assert_eq!(
        before_addresses, after_addresses,
        "the Party master changed"
    );

    // 4. THE POINT OF THIS FILE. Every pre-existing Sale — including the one posted to a customer
    //    whose billing address is on record right now — is version 0 with every new column NULL.
    //    Nothing was backfilled, nothing was defaulted, and no current address became history.
    for id in [POSTED_WALK_IN, POSTED_TO_PARTY, OPEN_DRAFT] {
        let version: i64 =
            sqlx::query_scalar("SELECT recipient_snapshot_version FROM sale_documents WHERE id=?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("version");
        assert_eq!(version, 0, "{id} was given a recipient snapshot version");
        for column in &NEW_COLUMNS[1..] {
            let value: Option<String> = sqlx::query_scalar(&format!(
                "SELECT CAST({column} AS TEXT) FROM sale_documents WHERE id=?"
            ))
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("new column");
            assert_eq!(value, None, "{id}.{column} was fabricated as {value:?}");
        }
    }

    // 5. The 1L-A seller snapshot is untouched.
    let seller: (i64, String, String) = sqlx::query_as(
        "SELECT seller_snapshot_version,seller_legal_name,seller_licence_text \
         FROM sale_documents WHERE id=?",
    )
    .bind(POSTED_TO_PARTY)
    .fetch_one(&pool)
    .await
    .expect("seller");
    assert_eq!(
        seller,
        (
            1,
            "Care Pharmacy Private Limited".to_owned(),
            "Form 20: MH-20-1234".to_owned()
        )
    );

    // 6. A posted Sale is still frozen: the new columns inherit the frozen posted-row guard.
    let rewrite = sqlx::query(
        "UPDATE sale_documents SET recipient_address_line1='7 Mill Road',\
         recipient_address_source='party',recipient_snapshot_version=1 WHERE id=?",
    )
    .bind(POSTED_TO_PARTY)
    .execute(&pool)
    .await;
    assert!(rewrite.is_err(), "an old posted Sale was given an address");

    // 7. The pre-existing draft still works under the new guards: it can be edited, and it cannot
    //    acquire a counter address beside its named customer or a snapshot before posting.
    sqlx::query(
        "UPDATE sale_documents SET revision=2,recipient_particulars_requested=1 WHERE id=?",
    )
    .bind(OPEN_DRAFT)
    .execute(&pool)
    .await
    .expect("an ordinary draft edit");
    for assignment in [
        "recipient_address_line1='Back Door'",
        "recipient_snapshot_version=1",
        "recipient_state_name='Maharashtra'",
        "delivery_address_line1='Somewhere'",
    ] {
        let refused = sqlx::query(&format!(
            "UPDATE sale_documents SET {assignment} WHERE id=?"
        ))
        .bind(OPEN_DRAFT)
        .execute(&pool)
        .await;
        // A premature version can be caught by either recipient guard: SQLite does not promise an
        // order between two BEFORE UPDATE triggers that both object. Either way it is a recipient
        // guard that refuses, never an unrelated failure.
        assert!(
            refused.err().is_some_and(|error| {
                let message = error.to_string();
                message.contains("recipient_draft_conflict")
                    || message.contains("recipient_snapshot_incomplete")
            }),
            "{assignment} was accepted on a draft"
        );
    }

    // 8. The database accepts a coherent version-1 recipient snapshot and refuses an incomplete one,
    //    independently of the service.
    let insert = |id: &'static str, registered: bool, address: Option<&'static str>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO sale_documents \
                 (id,store_id,customer_party_id,business_date,status,revision,series_code,\
                  financial_year,sequence_value,document_number,store_gst_registration_status,\
                  store_state_code,customer_display_name,customer_gst_registration_status,\
                  customer_normalized_gstin,tax_treatment,created_by_user_id,created_at_utc,\
                  updated_at_utc,posted_by_user_id,posted_at_utc,posting_idempotency_key,\
                  posting_fingerprint,seller_snapshot_version,seller_legal_name,\
                  seller_address_line1,seller_licence_text,recipient_snapshot_version,\
                  recipient_particulars_requested,recipient_address_source,\
                  recipient_address_line1,recipient_state_id,recipient_state_name,\
                  recipient_state_code) \
                 VALUES (?,?,?,'2026-06-17','posted',2,'INV','2026-27',?,?,'unknown','27',\
                  'Rahul Deshmukh',?,?,'intra_state',?,'2026-06-17T10:00:00.000Z',\
                  '2026-06-17T10:00:00.000Z',?,'2026-06-17T10:00:00.000Z',?,?,1,'Care',\
                  '12 Market Road','Form 20: X',1,0,?,?,?,?,?)",
            )
            .bind(id)
            .bind(STORE)
            .bind(PARTY)
            .bind(if address.is_some() { 3 } else { 4 })
            .bind(if address.is_some() {
                "INV/2627/000003"
            } else {
                "INV/2627/000004"
            })
            .bind(if registered {
                "registered"
            } else {
                "unregistered"
            })
            .bind(registered.then_some("27AAPFU0939F1ZV"))
            .bind(USER)
            .bind(USER)
            .bind(uuid::Uuid::now_v7().to_string())
            .bind("b".repeat(64))
            .bind(address.map(|_| "party"))
            .bind(address)
            .bind(address.map(|_| MAHARASHTRA))
            .bind(address.map(|_| "Maharashtra"))
            .bind(address.map(|_| "27"))
            .execute(&pool)
            .await
        }
    };
    insert(
        "01997000-0000-7000-8000-0000000000f5",
        true,
        Some("7 Mill Road"),
    )
    .await
    .expect("a coherent registered-recipient snapshot was refused");
    let incomplete = insert("01997000-0000-7000-8000-0000000000f6", true, None).await;
    assert!(
        incomplete
            .err()
            .is_some_and(|error| error.to_string().contains("recipient_snapshot_incomplete")),
        "a registered-recipient snapshot without an address was accepted"
    );

    pool.close().await;
}
