//! Migration 0022 against a database that already holds posted Sales, their tenders, their stock
//! and the Phase 1M-A regulatory records.
//!
//! 0022 adds prescribers, prescriptions and their items, the append-only dispensing ledger and its
//! reversals, a prescription pointer on each Sale line and the supervision facts on each Sale, and
//! replaces the 1M-A posting gate with one that lets a Schedule H line through only when it was
//! dispensed against a prescription. The outcomes that must never happen are an existing row being
//! rewritten, or a historical Sale acquiring a prescription, a patient or a pharmacist it never had.
//! So this target builds a database at 0021 holding legacy and 1M-A-era data, applies 0022, compares
//! every pre-existing row of every table, and then proves the new guards bite on the migrated file.
//!
//! ```text
//! cargo test --test migration_0022
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 22;

fn migrations_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

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

async fn tables(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
         AND name <> '_sqlx_migrations' ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .expect("tables")
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

/// Every listed column of a table, one comparable string per row, sorted so a table rebuilt by the
/// migration (whose rowids may differ) still compares on content alone.
async fn rows_of(pool: &SqlitePool, table: &str, columns: &[String]) -> Vec<String> {
    let projection = columns
        .iter()
        .map(|column| format!("quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join("||'|'||");
    let mut rows: Vec<String> = sqlx::query_scalar(&format!("SELECT {projection} FROM {table}"))
        .fetch_all(pool)
        .await
        .expect("rows");
    rows.sort();
    rows
}

async fn execute(pool: &SqlitePool, statement: &str) {
    sqlx::query(statement)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const USER: &str = "01997000-0000-7000-8000-0000000000cc";
const MEDICINE: &str = "01997000-0000-7000-8000-0000000000d1";
const GENERAL: &str = "01997000-0000-7000-8000-0000000000d2";
const MEDICINE_PACK: &str = "01997000-0000-7000-8000-0000000000e1";
const GENERAL_PACK: &str = "01997000-0000-7000-8000-0000000000e2";
const MEDICINE_BATCH: &str = "01997000-0000-7000-8000-0000000000e5";
const GENERAL_BATCH: &str = "01997000-0000-7000-8000-0000000000e6";
const LEGACY_POSTED: &str = "01997000-0000-7000-8000-0000000000f1";
const ERA_POSTED: &str = "01997000-0000-7000-8000-0000000000f2";
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";
const PROFESSIONAL: &str = "01997000-0000-7000-8000-000000000501";

/// A pharmacy that traded before 0021: one posted Sale with a medicine and a general-item line, and
/// a draft still open at the counter. Written at 0020, exactly as that schema allowed.
async fn populate_legacy(pool: &SqlitePool) {
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
        "INSERT INTO dosage_forms (id,canonical_code,display_name,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000a1','tablet','Tablet',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO products (id,product_kind,dosage_form_id,base_unit_id,quantity_scale,\
         display_name,normalized_search_name,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000d1','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Azee 500 Tablet','azee 500 tablet',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000d2','general_pharmacy_item',NULL,\
         '01997000-0000-7000-8000-000000000001',0,'Dental Floss','dental floss',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,\
         display_label,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000e1','01997000-0000-7000-8000-0000000000d1',\
         '01997000-0000-7000-8000-000000000004',10,'Strip of 10','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000e2','01997000-0000-7000-8000-0000000000d2',\
         '01997000-0000-7000-8000-000000000004',10,'Strip of 10','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z')",
        "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
         expires_on,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000e5','01997000-0000-7000-8000-0000000000e1','B-1','B-1',\
         '2028-03-31','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000e6','01997000-0000-7000-8000-0000000000e2','B-1','B-1',\
         '2028-03-31','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,quantity_delta_atoms,occurred_on,idempotency_key,posted_by_user_id,\
         posted_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000601','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5','opening_stock',100,'2026-04-01',\
         '01997000-0000-7000-8000-000000000611','01997000-0000-7000-8000-0000000000cc',\
         '2026-04-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-000000000602','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-0000000000d2','01997000-0000-7000-8000-0000000000e2',\
         '01997000-0000-7000-8000-0000000000e6','opening_stock',100,'2026-04-01',\
         '01997000-0000-7000-8000-000000000612','01997000-0000-7000-8000-0000000000cc',\
         '2026-04-01T00:00:00.000Z')",
    ] {
        execute(pool, statement).await;
    }

    sqlx::query(
        "INSERT INTO sale_documents \
         (id,store_id,business_date,status,revision,series_code,financial_year,sequence_value,\
          document_number,store_gst_registration_status,store_state_code,tax_treatment,\
          taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
          created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
          posting_idempotency_key,posting_fingerprint) \
         VALUES (?,?,'2026-06-15','posted',2,'INV','2026-27',1,'INV/2627/000001','registered','27',\
          'intra_state',8000,480,480,0,0,8960,?,'2026-06-15T10:00:00.000Z',\
          '2026-06-15T10:00:00.000Z',?,'2026-06-15T10:00:00.000Z',\
          '01997000-0000-7000-8000-000000000101',?)",
    )
    .bind(LEGACY_POSTED)
    .bind(STORE)
    .bind(USER)
    .bind(USER)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .expect("legacy posted sale");
    for (index, (product, pack, batch)) in [
        (MEDICINE, MEDICINE_PACK, MEDICINE_BATCH),
        (GENERAL, GENERAL_PACK, GENERAL_BATCH),
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
             batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
             product_display_name,batch_number,tax_treatment_kind,taxable_value_paise,\
             cgst_paise,sgst_paise,line_total_paise,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,?,?,'pack',1,10,4000,'Line','B-1','taxable',4000,240,240,4480,\
             '2026-06-15T10:00:00.000Z','2026-06-15T10:00:00.000Z')",
        )
        .bind(format!(
            "01997000-0000-7000-8000-0000000003{:02}",
            index + 1
        ))
        .bind(LEGACY_POSTED)
        .bind(index as i64 + 1)
        .bind(product)
        .bind(pack)
        .bind(batch)
        .execute(pool)
        .await
        .expect("legacy posted line");
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

/// The Phase 1M-A era, written at 0021: Schedule H findings for the medicine, a registered
/// pharmacist, a typed licence, a rule 65 election, and one Sale posted under the 1M-A gate with a
/// frozen regulatory snapshot and a tender.
async fn populate_regulatory_era(pool: &SqlitePool) {
    for (index, scheme, applies) in [
        (1, "schedule_h", 0),
        (2, "schedule_h1", 0),
        (3, "schedule_x", 0),
        (4, "schedule_c", 0),
        (5, "schedule_c1", 0),
        (6, "ndps_purview", 0),
    ] {
        sqlx::query(
            "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
             effective_from,source_citation,determined_by_user_id,revision,status,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,?,'2020-01-01','Drugs Rules, 1945, Schedules',?,1,\
             'active','2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
        )
        .bind(format!("01997000-0000-7000-8000-0000000004{index:02}"))
        .bind(MEDICINE)
        .bind(scheme)
        .bind(applies)
        .bind(USER)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("{scheme}: {error}"));
    }
    for statement in [
        "INSERT INTO store_professionals (id,store_id,full_name,capacity,registration_number,\
         registering_authority,valid_from,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000501','01997000-0000-7000-8000-0000000000bb',\
         'Meera Iyer','registered_pharmacist','MH-PH-44821','Maharashtra State Pharmacy Council',\
         '2020-01-01',1,'active','2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
        "INSERT INTO store_compliance_licences (id,store_id,licence_form,licence_number,\
         normalized_licence_number,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000511','01997000-0000-7000-8000-0000000000bb','form_20',\
         'MH-PUNE-20-1234','MH-PUNE-20-1234',1,'active','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z')",
        "INSERT INTO store_record_elections (id,store_id,election,method,effective_from,\
         recorded_by_user_id,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000521','01997000-0000-7000-8000-0000000000bb',\
         'rule_65_3_prescription_supply','prescription_register','2026-04-01',\
         '01997000-0000-7000-8000-0000000000cc',1,'active','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z')",
        "INSERT INTO sale_documents (id,store_id,business_date,status,revision,\
         created_by_user_id,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000f2','01997000-0000-7000-8000-0000000000bb',\
         '2026-09-02','draft',1,'01997000-0000-7000-8000-0000000000cc',\
         '2026-09-02T10:00:00.000Z','2026-09-02T10:00:00.000Z')",
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
         product_display_name,batch_number,tax_treatment_kind,taxable_value_paise,cgst_paise,\
         sgst_paise,line_total_paise,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000321','01997000-0000-7000-8000-0000000000f2',1,\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5','pack',1,10,4000,'Azee 500 Tablet','B-1',\
         'taxable',4000,240,240,4480,'2026-09-02T10:00:00.000Z','2026-09-02T10:00:00.000Z')",
        "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=\
         '{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"does_not_apply\",\
         \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
         \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}' \
         WHERE id='01997000-0000-7000-8000-000000000321'",
        "INSERT INTO sale_tenders (id,sale_document_id,method,amount_paise,reference_text,\
         created_at_utc) VALUES ('01997000-0000-7000-8000-000000000701',\
         '01997000-0000-7000-8000-0000000000f2','cash',4480,NULL,'2026-09-02T10:00:00.000Z')",
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,quantity_delta_atoms,occurred_on,sale_line_id,idempotency_key,\
         posted_by_user_id,posted_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000603','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5','sale',-10,'2026-09-02',\
         '01997000-0000-7000-8000-000000000321','01997000-0000-7000-8000-000000000613',\
         '01997000-0000-7000-8000-0000000000cc','2026-09-02T10:00:00.000Z')",
        "UPDATE sale_documents SET status='posted',revision=2,document_number='INV/2627/000002',\
         sequence_value=2,series_code='INV',financial_year='2026-27',tax_treatment='intra_state',\
         store_gst_registration_status='registered',store_state_code='27',taxable_value_paise=4000,\
         cgst_paise=240,sgst_paise=240,igst_paise=0,cess_paise=0,grand_total_paise=4480,\
         posted_by_user_id='01997000-0000-7000-8000-0000000000cc',\
         posted_at_utc='2026-09-02T10:00:00.000Z',\
         posting_idempotency_key='01997000-0000-7000-8000-000000000102' \
         WHERE id='01997000-0000-7000-8000-0000000000f2'",
    ] {
        execute(pool, statement).await;
    }
}

const NEW_TABLES: [&str; 7] = [
    "prescribers",
    "prescriptions",
    "prescription_items",
    "prescription_dispensings",
    "prescription_dispensing_reversals",
    "prescription_supply_records",
    "prescription_supply_record_lines",
];

const NEW_TRIGGERS: [&str; 20] = [
    "prescription_supply_records_coherent_insert",
    "prescription_supply_records_transition",
    "prescription_supply_records_no_delete",
    "prescription_supply_record_lines_coherent_insert",
    "prescription_supply_record_lines_link",
    "prescription_supply_record_lines_no_delete",
    "sale_documents_prescription_record_required",
    "prescription_dispensings_no_update",
    "prescription_dispensings_no_delete",
    "prescription_dispensings_coherent_insert",
    "prescription_dispensings_occasion_cap",
    "prescription_dispensings_quantity_cap",
    "prescription_dispensing_reversals_no_update",
    "prescription_dispensing_reversals_no_delete",
    "prescription_dispensing_reversals_coherent_insert",
    "prescriptions_dispensed_are_history",
    "sale_documents_regulatory_gate_update",
    "sale_documents_regulatory_gate_insert",
    "sale_documents_prescription_link_dispensed",
    "master_change_events_no_update",
];

const NEW_LINE_COLUMNS: [&str; 1] = ["prescription_item_id"];
const NEW_DOCUMENT_COLUMNS: [&str; 5] = [
    "supervising_professional_id",
    "prescription_endorsement_confirmed",
    "prescription_endorsement_confirmed_by_user_id",
    "prescription_endorsement_confirmed_at_utc",
    "prescription_original_container_confirmed",
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
    assert_eq!(applied.last().unwrap().0, NEW_MIGRATION);
    structural_checks(&pool).await;

    let objects = schema_objects(&pool).await;
    for expected in NEW_TABLES {
        assert!(
            objects.iter().any(|(kind, name, sql)| kind == "table"
                && name == expected
                && sql.contains("STRICT")),
            "{expected} is missing or not STRICT"
        );
    }
    for expected in NEW_TRIGGERS {
        assert!(
            objects
                .iter()
                .any(|(kind, name, _)| kind == "trigger" && name == expected),
            "{expected} is missing"
        );
    }
    assert!(
        !objects
            .iter()
            .any(|(_, name, _)| name == "master_change_events_phase1ma"),
        "the rebuild left its temporary table behind"
    );
    let audit_sql = objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2
        .clone();
    for entity in [
        "prescriber",
        "prescription",
        "prescription_dispensing",
        "prescription_dispensing_reversal",
        "prescription_supply_record",
        "product_regulatory_classification",
        "store_professional",
        "store_record_election",
        "sale_document",
    ] {
        assert!(
            audit_sql.contains(&format!("'{entity}'")),
            "{entity} missing from the audit log"
        );
    }
    // Nothing in the schema can hold a picture of the paper.
    for (kind, name, sql) in &objects {
        if kind == "table" && name.starts_with("prescri") {
            assert!(!sql.contains("BLOB"), "{name} can store binary data");
        }
    }
    pool.close().await;
}

#[tokio::test]
async fn the_new_migration_preserves_every_row_and_invents_no_prescription() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let legacy_directory = temp.path().join("legacy");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 2, &legacy_directory);
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &legacy_directory).await;
    populate_legacy(&pool).await;
    run_migrations(&pool, &before_directory).await;
    populate_regulatory_era(&pool).await;
    structural_checks(&pool).await;

    let before_objects = schema_objects(&pool).await;
    let before_tables = tables(&pool).await;
    let mut before_rows = Vec::new();
    for table in &before_tables {
        let columns = columns_of(&pool, table).await;
        let rows = rows_of(&pool, table, &columns).await;
        before_rows.push((table.clone(), columns, rows));
    }
    let populated: Vec<&str> = before_rows
        .iter()
        .filter(|(_, _, rows)| !rows.is_empty())
        .map(|(table, _, _)| table.as_str())
        .collect();
    for expected in [
        "products",
        "product_batches",
        "inventory_movements",
        "sale_documents",
        "sale_lines",
        "sale_tenders",
        "product_regulatory_classifications",
        "store_professionals",
        "store_compliance_licences",
        "store_record_elections",
    ] {
        assert!(
            populated.contains(&expected),
            "{expected} was not populated"
        );
    }
    pool.close().await;

    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;
    structural_checks(&pool).await;

    // 1. Nothing removed; nothing earlier redefined except what this migration deliberately
    //    rebuilds (the audit log's CHECK), extends (two ALTERs) or replaces (the posting gate).
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        let intended = [
            "sale_lines",
            "sale_documents",
            "master_change_events",
            "sale_documents_regulatory_gate_update",
            "sale_documents_regulatory_gate_insert",
        ];
        if intended.contains(&name.as_str()) {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 2. Columns only appended.
    let after_line_columns = columns_of(&pool, "sale_lines").await;
    let before_line_columns = &before_rows
        .iter()
        .find(|(table, _, _)| table == "sale_lines")
        .unwrap()
        .1;
    assert_eq!(
        after_line_columns[..before_line_columns.len()],
        before_line_columns[..]
    );
    assert_eq!(
        after_line_columns[before_line_columns.len()..],
        NEW_LINE_COLUMNS.map(str::to_owned)[..]
    );
    let after_document_columns = columns_of(&pool, "sale_documents").await;
    let before_document_columns = &before_rows
        .iter()
        .find(|(table, _, _)| table == "sale_documents")
        .unwrap()
        .1;
    assert_eq!(
        after_document_columns[..before_document_columns.len()],
        before_document_columns[..]
    );
    assert_eq!(
        after_document_columns[before_document_columns.len()..],
        NEW_DOCUMENT_COLUMNS.map(str::to_owned)[..]
    );

    // 3. Every pre-existing row of every pre-existing table, byte for byte, on its original
    //    columns: products, batches, stock, Sales, lines, tenders, the GST, DQR and seller
    //    snapshots, the 1M-A findings and line snapshots, professionals, licences, elections and
    //    the rebuilt audit log.
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }

    // 4. Nothing fabricated: every new table is empty and every historical Sale carries no
    //    prescription, no pharmacist and no endorsement.
    for table in NEW_TABLES {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} was populated by the migration");
    }
    let linked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sale_lines WHERE prescription_item_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("links");
    assert_eq!(linked, 0);
    let supervised: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sale_documents WHERE supervising_professional_id IS NOT NULL \
         OR prescription_endorsement_confirmed <> 0 \
         OR prescription_endorsement_confirmed_by_user_id IS NOT NULL \
         OR prescription_endorsement_confirmed_at_utc IS NOT NULL          OR prescription_original_container_confirmed <> 0",
    )
    .fetch_one(&pool)
    .await
    .expect("supervision");
    assert_eq!(supervised, 0);

    let era: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT document.status,line.regulatory_snapshot_version,line.prescription_item_id          FROM sale_documents document JOIN sale_lines line ON line.sale_document_id=document.id          WHERE document.id=?",
    )
    .bind(ERA_POSTED)
    .fetch_one(&pool)
    .await
    .expect("1M-A era sale");
    assert_eq!(era, ("posted".to_owned(), 1, None));

    // 5. The guards bite on the migrated database.
    //    (a) a prescription needs the attestation and a well-formed reference;
    for (reference, attested, expectation) in [
        ("RX-000001", 0, "attestation"),
        ("INV/2627/000001", 1, "reference"),
    ] {
        let result = sqlx::query(
            "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_name,\
             prescriber_address,subject_kind,subject_name,subject_address,\
             written_signed_dated_attested,created_by_user_id,created_at_utc,updated_at_utc) \
             VALUES ('01997000-0000-7000-8000-000000000801',?,?,'2026-09-01','Dr. Rao','Pune',\
             'human','Patient','Pune',?,?,'2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
        )
        .bind(STORE)
        .bind(reference)
        .bind(attested)
        .bind(USER)
        .execute(&pool)
        .await;
        assert!(
            result.is_err(),
            "a prescription without its {expectation} was stored"
        );
    }

    //    (b) a lawful prescription and item can be written, and a historical posted Sale cannot
    //        acquire a dispensing after the fact;
    execute(
        &pool,
        "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_name,\
         prescriber_address,subject_kind,subject_name,subject_address,\
         written_signed_dated_attested,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000802','01997000-0000-7000-8000-0000000000bb',\
         'RX-000001','2026-06-01','Dr. Rao','Pune','human','Patient','Pune',1,\
         '01997000-0000-7000-8000-0000000000cc','2026-09-20T00:00:00.000Z',\
         '2026-09-20T00:00:00.000Z')",
    )
    .await;
    execute(
        &pool,
        "INSERT INTO prescription_items (id,prescription_id,line_number,product_id,\
         written_description,prescribed_quantity_atoms,dose_text,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000803','01997000-0000-7000-8000-000000000802',1,\
         '01997000-0000-7000-8000-0000000000d1','Tab. Azee 500',30,'1 daily',\
         '2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    )
    .await;
    let backfilled = sqlx::query(
        "INSERT INTO prescription_dispensings (id,store_id,prescription_id,prescription_item_id,\
         sale_document_id,sale_line_id,product_id,quantity_atoms,dispensed_on,\
         supervising_professional_id,supervising_professional_name,\
         supervising_registration_number,endorsement_confirmed_by_user_id,\
         endorsement_confirmed_at_utc,created_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000804',?,'01997000-0000-7000-8000-000000000802',\
         '01997000-0000-7000-8000-000000000803',?,'01997000-0000-7000-8000-000000000301',?,10,\
         '2026-06-15',?,'Meera Iyer','MH-PH-44821',?,'2026-09-20T00:00:00.000Z',\
         '2026-09-20T00:00:00.000Z')",
    )
    .bind(STORE)
    .bind(LEGACY_POSTED)
    .bind(MEDICINE)
    .bind(PROFESSIONAL)
    .bind(USER)
    .execute(&pool)
    .await;
    assert!(
        backfilled.err().is_some_and(|error| error
            .to_string()
            .contains("prescription_dispensing_incoherent")),
        "a historical Sale acquired a dispensing"
    );

    //    (b2) nor a rule 65(3)(1) entry, however well its particulars are copied;
    let entered = sqlx::query(
        "INSERT INTO prescription_supply_records (id,store_id,sale_document_id,prescription_id,\
         record_method,election_id,serial_value,serial_number,date_of_supply,prescriber_name,\
         prescriber_address,subject_kind,subject_name,subject_address,supervising_professional_id,\
         supervising_professional_name,supervising_registration_number,\
         original_container_confirmed,prepared_by_user_id,prepared_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000805',?,?,'01997000-0000-7000-8000-000000000802',\
         'prescription_register','01997000-0000-7000-8000-000000000521',1,'PR-000001',\
         '2026-06-15','Dr. Rao','Pune','human','Patient','Pune',?,'Meera Iyer','MH-PH-44821',0,?,\
         '2026-09-20T00:00:00.000Z')",
    )
    .bind(STORE)
    .bind(LEGACY_POSTED)
    .bind(PROFESSIONAL)
    .bind(USER)
    .execute(&pool)
    .await;
    assert!(
        entered.err().is_some_and(|error| error
            .to_string()
            .contains("prescription_supply_record_incoherent")),
        "a historical Sale acquired a statutory entry"
    );

    //    (c) a posted line cannot acquire a prescription link, nor a posted Sale a pharmacist;
    for statement in [
        "UPDATE sale_lines SET prescription_item_id='01997000-0000-7000-8000-000000000803' \
         WHERE sale_document_id='01997000-0000-7000-8000-0000000000f2'",
        "UPDATE sale_documents SET supervising_professional_id='01997000-0000-7000-8000-000000000501' \
         WHERE id='01997000-0000-7000-8000-0000000000f2'",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }

    //    (d) a Schedule H line cannot be posted by hand without a dispensing;
    execute(
        &pool,
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-0000000003fe',\
         '01997000-0000-7000-8000-0000000000f4',1,'01997000-0000-7000-8000-0000000000d1',\
         '01997000-0000-7000-8000-0000000000e1','01997000-0000-7000-8000-0000000000e5','pack',1,\
         10,4000,'2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    )
    .await;
    execute(
        &pool,
        "UPDATE sale_lines SET regulatory_snapshot_version=1,regulatory_schemes_snapshot=\
         '{\"schedule_h\":\"applies\",\"schedule_h1\":\"does_not_apply\",\
         \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
         \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}' \
         WHERE id='01997000-0000-7000-8000-0000000003fe'",
    )
    .await;
    let posted = sqlx::query(
        "UPDATE sale_documents SET status='posted',document_number='INV/2627/000003',\
         sequence_value=3,series_code='INV',financial_year='2026-27',tax_treatment='intra_state',\
         posted_by_user_id=?,posted_at_utc='2026-09-20T00:00:00.000Z',\
         posting_idempotency_key='01997000-0000-7000-8000-000000000199' WHERE id=?",
    )
    .bind(USER)
    .bind(OPEN_DRAFT)
    .execute(&pool)
    .await;
    assert!(
        posted.err().is_some_and(|error| error
            .to_string()
            .contains("regulatory_gate_refuses_posting")),
        "direct SQL posted a Schedule H line without a dispensing"
    );

    //    (e) and H1 stays refused at the database even with a dispensing-shaped snapshot.
    execute(
        &pool,
        "UPDATE sale_lines SET regulatory_schemes_snapshot=\
         '{\"schedule_h\":\"applies\",\"schedule_h1\":\"applies\",\
         \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
         \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}' \
         WHERE id='01997000-0000-7000-8000-0000000003fe'",
    )
    .await;
    let posted = sqlx::query(
        "UPDATE sale_documents SET status='posted',document_number='INV/2627/000003',\
         sequence_value=3,series_code='INV',financial_year='2026-27',tax_treatment='intra_state',\
         posted_by_user_id=?,posted_at_utc='2026-09-20T00:00:00.000Z',\
         posting_idempotency_key='01997000-0000-7000-8000-000000000199' WHERE id=?",
    )
    .bind(USER)
    .bind(OPEN_DRAFT)
    .execute(&pool)
    .await;
    assert!(
        posted.err().is_some_and(|error| error
            .to_string()
            .contains("regulatory_gate_refuses_posting")),
        "direct SQL posted a Schedule H1 line"
    );

    //    (f) the rebuilt audit log is still append-only and learned the new entity types.
    execute(
        &pool,
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,payload_schema_version,change_payload) VALUES \
         ('01997000-0000-7000-8000-000000000901','prescription',\
         '01997000-0000-7000-8000-000000000802',1,'created','2026-09-20T00:00:00.000Z',1,'{}')",
    )
    .await;
    for statement in [
        "UPDATE master_change_events SET change_payload='{\"x\":1}'",
        "DELETE FROM master_change_events",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }

    structural_checks(&pool).await;
    pool.close().await;
}
