//! Migration 0023 against a database that already holds posted Sales, the Phase 1M-A regulatory
//! records and Phase 1M-B prescriptions.
//!
//! 0023 adds the Schedule H1 working record and its annotations, the State-boundary snapshot column
//! on each Sale line, the `punjab_restricted_supply` scheme (a rebuild of the classification
//! table) and two new audit entity types (a rebuild of the audit log), and replaces the central
//! posting gate so a Schedule H1 line may post with its dispensing and finalized working entry.
//! The outcomes that must never happen are an existing row being rewritten, a historical line being
//! reinterpreted, or a historical Sale acquiring an H1 entry it never had.
//!
//! ```text
//! cargo test --test migration_0023
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 23;

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

/// The Phase 1M-B era, written at 0022: a prescription with one item, entered but never dispensed.
async fn populate_prescription_era(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_name,\
         prescriber_address,subject_kind,subject_name,subject_address,\
         written_signed_dated_attested,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000802','01997000-0000-7000-8000-0000000000bb',\
         'RX-000001','2026-06-01','Dr. Rao','Pune','human','Patient','Pune',1,\
         '01997000-0000-7000-8000-0000000000cc','2026-09-20T00:00:00.000Z',\
         '2026-09-20T00:00:00.000Z')",
        "INSERT INTO prescription_items (id,prescription_id,line_number,product_id,\
         written_description,prescribed_quantity_atoms,dose_text,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000803','01997000-0000-7000-8000-000000000802',1,\
         '01997000-0000-7000-8000-0000000000d1','Tab. Azee 500',30,'1 daily',\
         '2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    ] {
        execute(pool, statement).await;
    }
}

const NEW_TABLES: [&str; 2] = [
    "prescription_h1_register_entries",
    "prescription_h1_register_annotations",
];

const NEW_TRIGGERS: [&str; 13] = [
    "sale_lines_state_snapshot_draft_only_insert",
    "sale_lines_state_snapshot_coherent_insert",
    "sale_lines_state_snapshot_coherent_update",
    "sale_lines_state_boundary_insert",
    "sale_documents_state_boundary_update",
    "sale_documents_regulatory_gate_update",
    "prescription_h1_register_entries_coherent_insert",
    "prescription_h1_register_entries_transition",
    "prescription_h1_register_entries_no_delete",
    "sale_documents_h1_register_required",
    "prescription_h1_register_annotations_coherent_insert",
    "prescription_h1_register_annotations_no_update",
    "prescription_h1_register_annotations_no_delete",
];

const NEW_LINE_COLUMNS: [&str; 1] = ["regulatory_state_snapshot"];

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
    for leftover in [
        "master_change_events_phase1mb",
        "product_regulatory_classifications_phase1mb",
    ] {
        assert!(
            !objects.iter().any(|(_, name, _)| name == leftover),
            "the rebuild left {leftover} behind"
        );
    }
    let audit_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2;
    for entity in [
        "prescription_h1_register_entry",
        "prescription_h1_register_annotation",
        "prescription_supply_record",
        "product_regulatory_classification",
    ] {
        assert!(audit_sql.contains(&format!("'{entity}'")), "{entity}");
    }
    let classification_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "product_regulatory_classifications")
        .expect("classification table")
        .2;
    assert!(classification_sql.contains("'punjab_restricted_supply'"));
    // The H1 working record holds no binary data and no column that would pass for a statutory
    // serial, a page number or the patient's address.
    let entries_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "prescription_h1_register_entries")
        .expect("entries table")
        .2;
    assert!(!entries_sql.contains("BLOB"));
    for column in columns_of(&pool, "prescription_h1_register_entries").await {
        for forbidden in [
            "serial",
            "page",
            "patient_address",
            "subject_address",
            "signature",
        ] {
            assert!(!column.contains(forbidden), "{column}");
        }
    }
    pool.close().await;
}

#[tokio::test]
async fn the_new_migration_preserves_every_row_and_backfills_nothing() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let legacy_directory = temp.path().join("legacy");
    let era_directory = temp.path().join("era");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 3, &legacy_directory);
    migrations_up_to(NEW_MIGRATION - 2, &era_directory);
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &legacy_directory).await;
    populate_legacy(&pool).await;
    run_migrations(&pool, &era_directory).await;
    populate_regulatory_era(&pool).await;
    run_migrations(&pool, &before_directory).await;
    populate_prescription_era(&pool).await;
    structural_checks(&pool).await;

    // Before 0023 the Punjab axis is not a scheme at all.
    let early = sqlx::query(
        "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
         effective_from,source_citation,determined_by_user_id,revision,status,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000481',?,\
         'punjab_restricted_supply',0,'2021-03-25','Punjab notification 9/16/21-3H6/1039',?,1,\
         'active','2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
    )
    .bind(MEDICINE)
    .bind(USER)
    .execute(&pool)
    .await;
    assert!(early.is_err(), "0022 accepted a scheme it does not know");

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
        "sale_documents",
        "sale_lines",
        "sale_tenders",
        "product_regulatory_classifications",
        "store_professionals",
        "store_record_elections",
        "prescriptions",
        "prescription_items",
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

    // 1. Nothing removed; nothing earlier redefined except what 0023 deliberately rebuilds (the
    //    classification table's scheme CHECK and the audit log's entity CHECK, each with its index
    //    and triggers), extends (one sale_lines column) or replaces (the posting gate).
    let after_objects = schema_objects(&pool).await;
    let intended = [
        "sale_lines",
        "master_change_events",
        "master_change_events_entity_idx",
        "master_change_events_no_update",
        "master_change_events_no_delete",
        "product_regulatory_classifications",
        "product_regulatory_classifications_lookup_idx",
        "product_regulatory_classifications_no_overlap_insert",
        "product_regulatory_classifications_no_overlap_update",
        "sale_documents_regulatory_gate_update",
    ];
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if intended.contains(&name.as_str()) {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 2. One column appended to sale_lines, nothing else.
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

    // 3. Every pre-existing row of every pre-existing table, on its original columns — including
    //    every classification finding and every audit event carried through the two rebuilds.
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }

    // 4. Nothing fabricated: no H1 entry or annotation exists, and no historical line acquired a
    //    State snapshot — it is never reinterpreted.
    for table in NEW_TABLES {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} was populated by the migration");
    }
    let stated: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sale_lines WHERE regulatory_state_snapshot IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("state snapshots");
    assert_eq!(stated, 0);
    let (version, snapshot): (i64, String) = sqlx::query_as(
        "SELECT regulatory_snapshot_version,regulatory_schemes_snapshot FROM sale_lines \
         WHERE sale_document_id=?",
    )
    .bind(ERA_POSTED)
    .fetch_one(&pool)
    .await
    .expect("1M-A era line");
    assert_eq!(version, 1);
    assert!(!snapshot.contains("punjab"), "{snapshot}");

    // 5. The guards bite on the migrated database.
    //    (a) the Punjab axis is now a scheme an owner may record;
    execute(
        &pool,
        "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
         effective_from,source_citation,determined_by_user_id,revision,status,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000481',\
         '01997000-0000-7000-8000-0000000000d1','punjab_restricted_supply',0,'2021-03-25',\
         'Punjab notification 9/16/21-3H6/1039','01997000-0000-7000-8000-0000000000cc',1,\
         'active','2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
    )
    .await;
    //    (b) a historical posted Sale cannot acquire an H1 working entry, however its particulars
    //        are copied (C-54);
    let backfilled = sqlx::query(
        "INSERT INTO prescription_h1_register_entries (id,store_id,sale_document_id,sale_line_id,\
         supply_record_id,prescription_id,prescription_item_id,product_id,reference_value,\
         reference,date_of_supply,prescriber_name,prescriber_address,patient_name,drug_name,\
         quantity_atoms,supervising_professional_id,supervising_professional_name,\
         supervising_registration_number,prepared_by_user_id,prepared_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000901',?,?,'01997000-0000-7000-8000-000000000301',\
         '01997000-0000-7000-8000-000000000805','01997000-0000-7000-8000-000000000802',\
         '01997000-0000-7000-8000-000000000803',?,1,'AH1-000001','2026-06-15','Dr. Rao','Pune',\
         'Patient','Azee 500 Tablet',10,?,'Meera Iyer','MH-PH-44821',?,'2026-09-20T00:00:00.000Z')",
    )
    .bind(STORE)
    .bind(LEGACY_POSTED)
    .bind(MEDICINE)
    .bind(PROFESSIONAL)
    .bind(USER)
    .execute(&pool)
    .await;
    assert!(
        backfilled.is_err(),
        "a historical Sale acquired an H1 working entry"
    );
    //    (c) a line cannot be written into a posted document without its State snapshot;
    let late = sqlx::query(
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
         regulatory_snapshot_version,regulatory_schemes_snapshot,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000003f0',?,3,?,?,?,'pack',1,10,4000,1,\
         '{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"does_not_apply\",\
         \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
         \"schedule_c1\":\"does_not_apply\",\"ndps_purview\":\"does_not_apply\"}',\
         '2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    )
    .bind(ERA_POSTED)
    .bind(GENERAL)
    .bind(GENERAL_PACK)
    .bind(GENERAL_BATCH)
    .execute(&pool)
    .await;
    assert!(
        late.is_err(),
        "a line joined a posted Sale without its State snapshot"
    );
    //    (d) the audit log accepts the new entity types and stays append-only.
    execute(
        &pool,
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,payload_schema_version,change_payload) VALUES \
         ('01997000-0000-7000-8000-000000000991','prescription_h1_register_entry',\
         '01997000-0000-7000-8000-000000000901',1,'created','2026-09-20T00:00:00.000Z',1,'{}')",
    )
    .await;
    assert!(
        sqlx::query("DELETE FROM master_change_events")
            .execute(&pool)
            .await
            .is_err()
    );
    pool.close().await;
}
