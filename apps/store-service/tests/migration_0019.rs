//! Migration 0019 against a database that already holds posted invoices and a licence.
//!
//! 0019 corrects who may charge GST and records the facts printing will need. The outcomes that must
//! never happen are an existing Sale's tax being recomputed, and an existing Store, licence or Sale
//! coming out of the migration claiming a turnover fact, a designation or a snapshot nobody
//! recorded. So this target builds a database at 0018 that already holds a Sale taxed while the
//! Store was unregistered, applies 0019, and proves both.
//!
//! ```text
//! cargo test --test migration_0019
//! ```

use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 19;

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
const LICENCE: &str = "01997000-0000-7000-8000-0000000000dc";
const TAXED_WHILE_UNREGISTERED: &str = "01997000-0000-7000-8000-0000000000f1";
const TAXED_WHILE_REGISTERED: &str = "01997000-0000-7000-8000-0000000000f3";
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";

/// A pharmacy that traded before 0019: an active licence, one posted Sale that charged GST while
/// the Store was recorded as unregistered (the very defect this phase fixes, preserved as history),
/// one ordinary taxed Sale, and a draft still open at the counter.
async fn populate(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
         gst_registration_status) VALUES ('01997000-0000-7000-8000-0000000000bb','Care Pharmacy',\
         'Asia/Kolkata','2026-01-01T00:00:00.000Z','unregistered')",
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000cc','owner','owner','Owner',\
         '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO store_licences (id,store_id,licence_type,licence_number,\
         normalized_licence_number,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000dc','01997000-0000-7000-8000-0000000000bb',\
         'Form 20','MH-20-1234','MH201234','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    for (id, store_status, number, sequence) in [
        (
            TAXED_WHILE_UNREGISTERED,
            "unregistered",
            "INV/2627/000001",
            1,
        ),
        (TAXED_WHILE_REGISTERED, "registered", "INV/2627/000002", 2),
    ] {
        sqlx::query(
            "INSERT INTO sale_documents \
             (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
              document_number,store_gst_registration_status,store_state_code,tax_treatment,\
              taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
              created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
              posting_idempotency_key,posting_fingerprint,seller_snapshot_version,\
              seller_legal_name,seller_address_line1,seller_licence_text,\
              recipient_snapshot_version,recipient_particulars_requested) \
             VALUES (?,?,'2026-06-15','posted',2,'INV','2026-27',?,?,?,'27','intra_state',\
              8000,480,480,0,0,8960,?,'2026-06-15T10:00:00.000Z','2026-06-15T10:00:00.000Z',?,\
              '2026-06-15T10:00:00.000Z',?,?,1,'Care Pharmacy Private Limited','12 Market Road',\
              'Form 20: MH-20-1234',1,0)",
        )
        .bind(id)
        .bind(STORE)
        .bind(sequence)
        .bind(number)
        .bind(store_status)
        .bind(USER)
        .bind(USER)
        .bind(format!("01997000-0000-7000-8000-0000000001{sequence:02}"))
        .bind("a".repeat(64))
        .execute(pool)
        .await
        .expect("posted sale");
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

const NEW_SALE_COLUMNS: [&str; 7] = [
    "compliance_snapshot_version",
    "seller_retail_licence_text",
    "rule46s_declaration_snapshot",
    "einvoice_applicability_snapshot",
    "hsn_turnover_band_snapshot",
    "hsn_turnover_financial_year_snapshot",
    "hsn_required_digits",
];

const NEW_TRIGGERS: [&str; 6] = [
    "store_identity_hsn_band_integrity_insert",
    "store_identity_hsn_band_integrity_update",
    "sale_documents_compliance_draft_only",
    "sale_documents_compliance_draft_insert",
    "sale_documents_compliance_snapshot_update",
    "sale_documents_compliance_snapshot_insert",
];

/// 0018's recipient triggers, recreated by 0019 behind the seller-registration gate.
const RECREATED_RECIPIENT_TRIGGERS: [&str; 2] = [
    "sale_documents_recipient_snapshot_insert",
    "sale_documents_recipient_snapshot_update",
];
const SELLER_GATE: &str =
    "COALESCE(NEW.store_gst_registration_status, '') <> 'unregistered'\n     AND ";

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
    for (table, column) in [
        ("store_identity", "rule46s_declaration_applicability"),
        ("store_identity", "einvoice_applicability"),
        ("store_identity", "hsn_turnover_band"),
        ("store_identity", "hsn_turnover_financial_year"),
        ("store_licences", "include_on_retail_memo"),
    ] {
        assert!(
            columns_of(&pool, table)
                .await
                .iter()
                .any(|name| name == column),
            "{table}.{column} missing"
        );
    }
    let objects = schema_objects(&pool).await;
    for expected in NEW_TRIGGERS.iter().chain(
        [
            "sale_documents_posted_no_update",
            "sale_documents_seller_snapshot_update",
            "sale_documents_recipient_snapshot_update",
        ]
        .iter(),
    ) {
        assert!(
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} is missing"
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn the_new_migration_preserves_every_sale_and_invents_no_compliance_history() {
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
    let before_licence_columns = columns_of(&pool, "store_licences").await;
    let before_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events");
    pool.close().await;

    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;
    structural_checks(&pool).await;

    // 1. Nothing removed, nothing earlier redefined except the three tables that gained columns.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if kind == "table"
            && ["sale_documents", "store_identity", "store_licences"].contains(&name.as_str())
        {
            continue;
        }
        if RECREATED_RECIPIENT_TRIGGERS.contains(&name.as_str()) {
            // Recreated with exactly one change: the seller-registration gate on the Rule 46(d)
            // clause and on the Rule 46(e)/(f) clause. Remove the gate and 0018's text remains,
            // byte for byte.
            assert_eq!(after.2.matches(SELLER_GATE).count(), 2, "{name}");
            assert_eq!(
                &after.2.replace(SELLER_GATE, ""),
                sql,
                "{name} changed beyond the gate"
            );
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 2. Columns only appended.
    let after_sale_columns = columns_of(&pool, "sale_documents").await;
    assert_eq!(
        after_sale_columns[..before_sale_columns.len()],
        before_sale_columns[..]
    );
    assert_eq!(
        after_sale_columns[before_sale_columns.len()..],
        NEW_SALE_COLUMNS.map(str::to_owned)[..]
    );
    let after_licence_columns = columns_of(&pool, "store_licences").await;
    assert_eq!(
        after_licence_columns[..before_licence_columns.len()],
        before_licence_columns[..]
    );

    // 3. Every pre-existing Sale column is byte-for-byte unchanged — including the tax a Sale
    //    charged while the Store was unregistered. History is reported, never recomputed.
    assert_eq!(before_rows, sale_rows(&pool, &before_sale_columns).await);
    let taxed: (i64, i64, String) = sqlx::query_as(
        "SELECT cgst_paise,grand_total_paise,store_gst_registration_status FROM sale_documents WHERE id=?",
    )
    .bind(TAXED_WHILE_UNREGISTERED)
    .fetch_one(&pool)
    .await
    .expect("legacy tax");
    assert_eq!(taxed, (480, 8960, "unregistered".to_owned()));
    let after_events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events");
    assert_eq!(before_events, after_events);

    // 4. Every new fact starts unknown. Nothing is backfilled or defaulted into a claim.
    let store: (String, String, String, Option<String>) = sqlx::query_as(
        "SELECT rule46s_declaration_applicability,einvoice_applicability,hsn_turnover_band,\
         hsn_turnover_financial_year FROM store_identity WHERE store_id=?",
    )
    .bind(STORE)
    .fetch_one(&pool)
    .await
    .expect("store facts");
    assert_eq!(
        store,
        (
            "unknown".to_owned(),
            "unknown".to_owned(),
            "unknown".to_owned(),
            None
        )
    );
    let designated: i64 =
        sqlx::query_scalar("SELECT include_on_retail_memo FROM store_licences WHERE id=?")
            .bind(LICENCE)
            .fetch_one(&pool)
            .await
            .expect("licence");
    assert_eq!(
        designated, 0,
        "a licence was designated that nobody designated"
    );
    for id in [TAXED_WHILE_UNREGISTERED, TAXED_WHILE_REGISTERED, OPEN_DRAFT] {
        let version: i64 =
            sqlx::query_scalar("SELECT compliance_snapshot_version FROM sale_documents WHERE id=?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .expect("version");
        assert_eq!(version, 0, "{id}");
        for column in &NEW_SALE_COLUMNS[1..] {
            let value: Option<String> = sqlx::query_scalar(&format!(
                "SELECT CAST({column} AS TEXT) FROM sale_documents WHERE id=?"
            ))
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("new column");
            assert_eq!(value, None, "{id}.{column} fabricated as {value:?}");
        }
    }

    // 5. Seller and recipient snapshots preserved exactly.
    let snapshots: (i64, String, i64) = sqlx::query_as(
        "SELECT seller_snapshot_version,seller_licence_text,recipient_snapshot_version \
         FROM sale_documents WHERE id=?",
    )
    .bind(TAXED_WHILE_REGISTERED)
    .fetch_one(&pool)
    .await
    .expect("snapshots");
    assert_eq!(snapshots, (1, "Form 20: MH-20-1234".to_owned(), 1));

    // 6. Posted Sales stay frozen; a draft cannot take a snapshot early; the band names its year.
    assert!(
        sqlx::query("UPDATE sale_documents SET compliance_snapshot_version=1 WHERE id=?")
            .bind(TAXED_WHILE_REGISTERED)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE sale_documents SET hsn_required_digits=0 WHERE id=?")
            .bind(OPEN_DRAFT)
            .execute(&pool)
            .await
            .is_err()
    );
    for statement in [
        "UPDATE store_identity SET hsn_turnover_band='up_to_5_crore'",
        "UPDATE store_identity SET hsn_turnover_financial_year='2026-27'",
        "UPDATE store_identity SET rule46s_declaration_applicability='sometimes'",
        "UPDATE store_identity SET rule46s_declaration_applicability='required'",
        "UPDATE store_identity SET einvoice_applicability='applicable'",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }
    sqlx::query(
        "UPDATE store_identity SET hsn_turnover_band='up_to_5_crore',\
         hsn_turnover_financial_year='2026-27',rule46s_declaration_applicability='applicable',\
         einvoice_applicability='not_required'",
    )
    .execute(&pool)
    .await
    .expect("a coherent set of facts");

    // 7. The database itself refuses a version-1 Sale that charged GST as an unregistered seller.
    let forged = sqlx::query(
        "INSERT INTO sale_documents \
         (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
          document_number,store_gst_registration_status,store_state_code,tax_treatment,\
          taxable_value_paise,cgst_paise,sgst_paise,grand_total_paise,created_by_user_id,\
          created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,posting_idempotency_key,\
          posting_fingerprint,compliance_snapshot_version) \
         VALUES ('01997000-0000-7000-8000-0000000000f7',?,'2026-06-17','posted',2,'INV',\
          '2026-27',3,'INV/2627/000003','unregistered','27','intra_state',8000,480,480,8960,?,\
          '2026-06-17T10:00:00.000Z','2026-06-17T10:00:00.000Z',?,'2026-06-17T10:00:00.000Z',\
          '01997000-0000-7000-8000-000000000199',?,1)",
    )
    .bind(STORE)
    .bind(USER)
    .bind(USER)
    .bind("b".repeat(64))
    .execute(&pool)
    .await;
    assert!(
        forged
            .err()
            .is_some_and(|error| error.to_string().contains("compliance_snapshot_incomplete")),
        "an unregistered seller's taxed version-1 Sale was accepted"
    );

    // 8. The recreated recipient triggers: a version-1 recipient snapshot naming a registered buyer
    //    with no address is refused exactly as 0018 refused it whenever the frozen seller is not
    //    positively unregistered — and accepted only from an unregistered seller, which issues no
    //    tax invoice for Rule 46(d) to govern.
    for (id, seller, accepted) in [
        ("01997000-0000-7000-8000-0000000000f8", "unknown", false),
        ("01997000-0000-7000-8000-0000000000f9", "registered", false),
        ("01997000-0000-7000-8000-0000000000fa", "unregistered", true),
    ] {
        let attempt = sqlx::query(
            "INSERT INTO sale_documents \
             (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
              document_number,store_gst_registration_status,store_state_code,tax_treatment,\
              customer_display_name,customer_gst_registration_status,customer_normalized_gstin,\
              taxable_value_paise,cgst_paise,sgst_paise,grand_total_paise,created_by_user_id,\
              created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,posting_idempotency_key,\
              posting_fingerprint,recipient_snapshot_version,recipient_particulars_requested) \
             VALUES (?,?,'2026-06-18','posted',2,'INV','2026-27',?,?,?,'27','intra_state',\
              'Mehta Medical Stores','registered','27AAACM1234K1Z5',8000,0,0,8000,?,\
              '2026-06-18T10:00:00.000Z','2026-06-18T10:00:00.000Z',?,'2026-06-18T10:00:00.000Z',\
              ?,?,1,0)",
        )
        .bind(id)
        .bind(STORE)
        .bind(match seller {
            "unknown" => 4,
            "registered" => 5,
            _ => 6,
        })
        .bind(format!("INV/2627/00000{}", &id[35..]))
        .bind(seller)
        .bind(USER)
        .bind(USER)
        .bind(id)
        .bind("c".repeat(64))
        .execute(&pool)
        .await;
        match (accepted, attempt) {
            (true, Ok(_)) => {}
            (false, Err(error)) => assert!(
                error.to_string().contains("recipient_snapshot_incomplete"),
                "{seller}: {error}"
            ),
            (expected, outcome) => {
                panic!("{seller}: expected accepted={expected}, got {outcome:?}")
            }
        }
    }
    structural_checks(&pool).await;
    pool.close().await;
}
