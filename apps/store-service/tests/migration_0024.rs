//! Migration 0024 against a database that already holds posted Purchases, posted Sales and the
//! Phase 1M-A/B/C regulatory records.
//!
//! 0024 adds receipt provenance: a version on each Purchase, the supplier's address and drug
//! licence as they stood at posting, and each line's drug name, lot number and manufacturer. It
//! creates no table and rebuilds none, so the outcomes that must never happen are a pre-existing
//! row changing, a historical Purchase acquiring provenance it never had, and a legacy receipt
//! being dressed up as a provenance-aware one afterwards.
//!
//! ```text
//! cargo test --test migration_0024
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 24;

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
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";
const POSTED_PURCHASE: &str = "01997000-0000-7000-8000-000000000605";
const POSTED_PURCHASE_LINE: &str = "01997000-0000-7000-8000-000000000606";
const DRAFT_PURCHASE: &str = "01997000-0000-7000-8000-000000000607";
const DRAFT_PURCHASE_LINE: &str = "01997000-0000-7000-8000-000000000608";

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
         ((SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'01997000-0000-7000-8000-0000000000bb',\
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

/// What 0024 adds: no table, no rebuild, four guards, and the columns below.
const NEW_TRIGGERS: [&str; 4] = [
    "purchase_lines_provenance_shape_insert",
    "purchase_lines_provenance_shape_update",
    "purchase_documents_provenance_insert",
    "purchase_documents_provenance_update",
];

const NEW_DOCUMENT_COLUMNS: [&str; 14] = [
    "purchase_provenance_snapshot_version",
    "supplier_address_state",
    "supplier_address_id",
    "supplier_address_line1",
    "supplier_address_line2",
    "supplier_address_city",
    "supplier_address_postal_code",
    "supplier_address_country_code",
    "supplier_address_state_id",
    "supplier_address_state_name",
    "supplier_address_state_code",
    "supplier_drug_licence_state",
    "supplier_drug_licence_number",
    "supplier_drug_licence_valid_upto",
];

const NEW_LINE_COLUMNS: [&str; 5] = [
    "drug_display_name",
    "batch_number",
    "manufacturer_company_id",
    "manufacturer_name",
    "manufacturer_state",
];

async fn populate_purchase_era(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO parties (id,display_name,normalized_search_name,legal_name,\
         gst_registration_status,gstin,normalized_gstin,place_of_supply_state_id,\
         drug_licence_number,drug_licence_valid_upto,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000602','Sunrise Distributors',\
         'sunrise distributors','Sunrise Distributors Private Limited','registered',\
         '27AABCS1429B1ZQ','27AABCS1429B1ZQ',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         '20B-MH-9911 / 21B-MH-9912','2027-12-31','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-00000000060a','01997000-0000-7000-8000-000000000602',\
         'supplier','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_addresses (id,party_id,address_role,line1,line2,city,state_id,\
         postal_code,is_primary,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000603','01997000-0000-7000-8000-000000000602',\
         'billing','14 Ware House Road','Bhiwandi','Thane',\
         (SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'421302',1,'2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z')",
        "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
         created_at_utc,updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000604',\
         'Meridian Laboratories','meridian laboratories','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z')",
        "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000609',\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-000000000604',\
         'manufacturer','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        // The posted receipt, with every fact 0011 knew how to freeze and none it did not.
        "INSERT INTO purchase_documents (id,store_id,supplier_party_id,supplier_invoice_number,\
         normalized_supplier_invoice_number,invoice_date,status,revision,supplier_display_name,\
         supplier_legal_name,supplier_gst_registration_status,supplier_normalized_gstin,\
         supplier_place_of_supply_state_id,supplier_state_code,store_gst_registration_status,\
         store_normalized_gstin,store_place_of_supply_state_id,store_state_code,tax_treatment,\
         taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
         created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
         posting_idempotency_key,posting_fingerprint) \
         VALUES ('01997000-0000-7000-8000-000000000605','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-000000000602','INV-7781','INV-7781','2026-02-01','posted',2,\
         'Sunrise Distributors','Sunrise Distributors Private Limited','registered',\
         '27AABCS1429B1ZQ',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'27','registered',\
         '27AAACS1429B1Z1',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'27','intra_state',\
         30000,900,900,0,0,31800,'01997000-0000-7000-8000-0000000000cc',\
         '2026-02-01T05:00:00.000Z','2026-02-01T05:00:00.000Z',\
         '01997000-0000-7000-8000-0000000000cc','2026-02-01T05:00:00.000Z',\
         '01997000-0000-7000-8000-00000000060b','0000000000000000000000000000000000000000000000000000000000007781')",
        "INSERT INTO purchase_lines (id,purchase_document_id,line_number,product_id,\
         product_pack_id,batch_id,quantity_packs,rate_per_pack_paise,quantity_atoms,\
         taxable_value_paise,cgst_basis_points,sgst_basis_points,igst_basis_points,\
         cess_basis_points,cgst_paise,sgst_paise,igst_paise,cess_paise,line_total_paise,\
         created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000606','01997000-0000-7000-8000-000000000605',1,\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5',10,3000,100,30000,600,600,0,0,900,900,0,0,31800,\
         '2026-02-01T05:00:00.000Z','2026-02-01T05:00:00.000Z')",
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,quantity_delta_atoms,occurred_on,purchase_line_id,idempotency_key,\
         posted_by_user_id,posted_at_utc) \
         VALUES ('01997000-0000-7000-8000-00000000060c','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5','purchase',100,'2026-02-01',\
         '01997000-0000-7000-8000-000000000606','01997000-0000-7000-8000-00000000060d',\
         '01997000-0000-7000-8000-0000000000cc','2026-02-01T05:00:00.000Z')",
        // A draft still being written when the migration arrives.
        "INSERT INTO purchase_documents (id,store_id,supplier_party_id,supplier_invoice_number,\
         normalized_supplier_invoice_number,invoice_date,status,revision,created_by_user_id,\
         created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000607','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-000000000602','INV-7782','INV-7782','2026-02-02','draft',1,\
         '01997000-0000-7000-8000-0000000000cc','2026-02-02T05:00:00.000Z',\
         '2026-02-02T05:00:00.000Z')",
        "INSERT INTO purchase_lines (id,purchase_document_id,line_number,product_id,\
         product_pack_id,new_batch_number,quantity_packs,rate_per_pack_paise,quantity_atoms,\
         taxable_value_paise,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000608','01997000-0000-7000-8000-000000000607',1,\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1','B-7782',\
         5,3000,50,15000,'2026-02-02T05:00:00.000Z','2026-02-02T05:00:00.000Z')",
    ] {
        execute(pool, statement).await;
    }
}

/// 1, 5. A database built from nothing reaches 0024, and the new columns and guards are there.
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
    for expected in NEW_TRIGGERS {
        assert!(
            objects
                .iter()
                .any(|(kind, name, _)| kind == "trigger" && name == expected),
            "{expected} is missing"
        );
    }
    let document_columns = columns_of(&pool, "purchase_documents").await;
    for expected in NEW_DOCUMENT_COLUMNS {
        assert!(
            document_columns.iter().any(|column| column == expected),
            "{expected} is missing"
        );
    }
    let line_columns = columns_of(&pool, "purchase_lines").await;
    for expected in NEW_LINE_COLUMNS {
        assert!(
            line_columns.iter().any(|column| column == expected),
            "{expected} is missing"
        );
    }
    // 0024 creates no table and rebuilds none, so no Schedule X register has appeared and no
    // scratch copy of anything is left behind.
    for absent in tables(&pool).await {
        assert!(
            !absent.contains("schedule_x") && !absent.contains("_phase1"),
            "{absent}"
        );
    }
    // Provenance is a record of what was supplied, so it holds no judgement about the licence.
    let document_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "purchase_documents")
        .expect("purchase table")
        .2;
    for forbidden in ["licence_form", "duly_licensed", "licence_verified", "BLOB"] {
        assert!(!document_sql.contains(forbidden), "{forbidden}");
    }
    pool.close().await;
}

/// 2, 3, 4, 6, 7, 8. Against a pharmacy that already traded: every row survives, every historical
/// Purchase stays at version 0 with empty provenance, nothing is backfilled from today's masters,
/// a legacy receipt cannot be rewritten into a provenance-aware one, and the version-1 shape is
/// enforced.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_backfills_nothing() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let legacy_directory = temp.path().join("legacy");
    let era_directory = temp.path().join("era");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 4, &legacy_directory);
    migrations_up_to(NEW_MIGRATION - 3, &era_directory);
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
    populate_purchase_era(&pool).await;
    structural_checks(&pool).await;

    // Before 0024 there is no such column to write.
    let early = sqlx::query(
        "UPDATE purchase_documents SET purchase_provenance_snapshot_version=1 WHERE id=?",
    )
    .bind(POSTED_PURCHASE)
    .execute(&pool)
    .await;
    assert!(early.is_err(), "0023 accepted a column it does not have");

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
        "purchase_documents",
        "purchase_lines",
        "party_addresses",
        "product_company_roles",
        "sale_documents",
        "sale_lines",
        "prescriptions",
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

    // 8. Nothing earlier is removed or redefined: 0024 rebuilds nothing at all.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if name == "purchase_documents" || name == "purchase_lines" {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 5. The new columns are appended, in order, and nothing else moved.
    for (table, expected) in [
        ("purchase_documents", NEW_DOCUMENT_COLUMNS.to_vec()),
        ("purchase_lines", NEW_LINE_COLUMNS.to_vec()),
    ] {
        let after = columns_of(&pool, table).await;
        let before = &before_rows
            .iter()
            .find(|(name, _, _)| name == table)
            .unwrap()
            .1;
        assert_eq!(after[..before.len()], before[..], "{table} columns moved");
        assert_eq!(
            after[before.len()..],
            expected
                .iter()
                .map(|column| (*column).to_owned())
                .collect::<Vec<_>>()[..],
            "{table} new columns"
        );
    }

    // 2. Every pre-existing row of every pre-existing table, on its original columns.
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }

    // 3, 4. Every historical Purchase is version 0 with no provenance at all — not the supplier's
    // address as it stands today, not the licence on the Party, not the manufacturer of the
    // product, not the lot's current text. The facts exist in the masters; they are not history.
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT purchase_provenance_snapshot_version FROM purchase_documents")
            .fetch_all(&pool)
            .await
            .expect("versions");
    assert_eq!(versions, vec![0, 0]);
    let filled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM purchase_documents WHERE supplier_address_state IS NOT NULL \
         OR supplier_address_id IS NOT NULL OR supplier_address_line1 IS NOT NULL \
         OR supplier_drug_licence_state IS NOT NULL OR supplier_drug_licence_number IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("filled");
    assert_eq!(filled, 0, "the migration invented supplier provenance");
    let lines_filled: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM purchase_lines WHERE drug_display_name IS NOT NULL \
         OR batch_number IS NOT NULL OR manufacturer_company_id IS NOT NULL \
         OR manufacturer_name IS NOT NULL OR manufacturer_state IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("lines filled");
    assert_eq!(lines_filled, 0, "the migration invented line provenance");

    // 6. A legacy posted receipt cannot be dressed up as a provenance-aware one, by any hand: the
    //    posted guard of Phase 1G refuses the update before the provenance guard is even reached.
    let forged = sqlx::query(
        "UPDATE purchase_documents SET purchase_provenance_snapshot_version=1,\
         supplier_address_state='not_recorded',supplier_drug_licence_state='not_recorded' \
         WHERE id=?",
    )
    .bind(POSTED_PURCHASE)
    .execute(&pool)
    .await
    .unwrap_err()
    .to_string();
    // SQLite fires the newest trigger first, so the provenance guard answers before the posted
    // guard does. Both refuse it; the point is that a legacy receipt cannot claim provenance.
    assert!(
        forged.contains("purchase_provenance_incomplete"),
        "{forged}"
    );
    // And a plain edit of a posted receipt, which the provenance guard has no opinion about, still
    // meets the frozen posted guard of Phase 1G.
    let plain =
        sqlx::query("UPDATE purchase_documents SET supplier_invoice_number='INV-0000' WHERE id=?")
            .bind(POSTED_PURCHASE)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string();
    assert!(plain.contains("purchase_document_is_posted"), "{plain}");
    let line_forged =
        sqlx::query("UPDATE purchase_lines SET drug_display_name='Something else' WHERE id=?")
            .bind(POSTED_PURCHASE_LINE)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string();
    assert!(
        line_forged.contains("purchase_document_is_posted"),
        "{line_forged}"
    );

    // 7. The version-1 shape is enforced where it can still be written: the open draft cannot
    //    claim provenance without being posted, cannot claim it while its line carries none, and
    //    cannot claim a recorded address with nothing recorded in it.
    for (statement, guard) in [
        (
            "UPDATE purchase_documents SET purchase_provenance_snapshot_version=1,             supplier_address_state='not_recorded',supplier_drug_licence_state='not_recorded'              WHERE id=?",
            "purchase_provenance_incomplete",
        ),
        (
            "UPDATE purchase_documents SET supplier_address_line1='14 Ware House Road' WHERE id=?",
            "purchase_provenance_incomplete",
        ),
        (
            "UPDATE purchase_documents SET purchase_provenance_snapshot_version=1,             supplier_address_state='recorded',supplier_drug_licence_state='not_recorded'              WHERE id=?",
            "purchase_provenance_incomplete",
        ),
    ] {
        let error = sqlx::query(statement)
            .bind(DRAFT_PURCHASE)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(guard), "{statement}: {error}");
    }
    // A line's own shape is guarded whatever its document's state: a name without its company, a
    // lot number with no lot, and a maker that does not make this product are all refused.
    for statement in [
        "UPDATE purchase_lines SET manufacturer_name='Meridian Laboratories',         manufacturer_state='recorded' WHERE id=?",
        "UPDATE purchase_lines SET batch_number='B-7782' WHERE id=?",
        "UPDATE purchase_lines SET manufacturer_company_id=         '01997000-0000-7000-8000-000000000604',manufacturer_state='not_recorded' WHERE id=?",
    ] {
        let error = sqlx::query(statement)
            .bind(DRAFT_PURCHASE_LINE)
            .execute(&pool)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("purchase_provenance_incoherent"),
            "{statement}: {error}"
        );
    }
    structural_checks(&pool).await;
    pool.close().await;
}
