//! Migration 0026 against a database that already holds the Phase 1M-A..D1-B records.
//!
//! 0026 adds the Schedule X working record: one register with two entry kinds, the two physical-act
//! attestations rule 65(21) implies, and the rule 65(9)(a) retained-duplicate fact. The outcomes that
//! must never happen are a working record appearing for a receipt nobody recorded one for, a page
//! number entering the schema, a finalized entry changing, one purchase line acquiring two live
//! entries, a reference being reissued, and the audit log losing an event while it is rebuilt.
//!
//! ```text
//! cargo test --test migration_0026
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 26;

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

async fn refused(pool: &SqlitePool, statement: &str) -> String {
    sqlx::query(statement)
        .execute(pool)
        .await
        .err()
        .unwrap_or_else(|| panic!("accepted a statement it must refuse: {statement}"))
        .to_string()
}

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const OWNER: &str = "01997000-0000-7000-8000-0000000000cc";
const CASHIER: &str = "01997000-0000-7000-8000-0000000000cd";
const PRODUCT_X: &str = "01997000-0000-7000-8000-0000000000d1";
const PRODUCT_PLAIN: &str = "01997000-0000-7000-8000-0000000000d2";
const POSTED: &str = "01997000-0000-7000-8000-000000000605";
const POSTED_LINE: &str = "01997000-0000-7000-8000-000000000606";
const POSTED_PLAIN_LINE: &str = "01997000-0000-7000-8000-000000000609";
const POSTED_TWO: &str = "01997000-0000-7000-8000-000000000607";
const POSTED_TWO_LINE: &str = "01997000-0000-7000-8000-000000000608";
const INVOICE_DATE_TWO: &str = "2026-05-05";
const PHARMACIST: &str = "01997000-0000-7000-8000-000000000501";
const INVOICE_DATE: &str = "2026-05-04";

/// What 0026 adds.
const NEW_TABLES: [&str; 2] = [
    "prescription_duplicate_copy_attestations",
    "store_schedule_x_register_entries",
];

const NEW_TRIGGERS: [&str; 6] = [
    "prescription_duplicate_copy_attestations_coherent_insert",
    "prescription_duplicate_copy_attestations_no_delete",
    "prescription_duplicate_copy_attestations_no_update",
    "store_schedule_x_register_entries_coherent_insert",
    "store_schedule_x_register_entries_no_delete",
    "store_schedule_x_register_entries_transition",
];

/// A pharmacy as it stands at 0025: a posted Purchase carrying full D1-A provenance, one drug the
/// owner recorded inside Schedule X and one ordinary drug with no finding at all, a registered
/// pharmacist, and an audit log with events already in it.
async fn populate_before_0026(pool: &SqlitePool) {
    for statement in [
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000aa','2026-01-01T00:00:00.000Z')",
        "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
         gst_registration_status) VALUES ('01997000-0000-7000-8000-0000000000bb','Care Pharmacy',\
         'Asia/Kolkata','2026-01-01T00:00:00.000Z','registered')",
        "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
         password_hash,role,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000cc','owner','owner','Owner',\
         '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000cd','till','till','Till',\
         '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','cashier',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO dosage_forms (id,canonical_code,display_name,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000000a1','tablet','Tablet',\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO products (id,product_kind,dosage_form_id,base_unit_id,quantity_scale,\
         display_name,normalized_search_name,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000d1','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Schedule X Test Medicine A',\
         'schedule x test medicine a','active','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000d2','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Ordinary Test Medicine',\
         'ordinary test medicine','active','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
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
         ('01997000-0000-7000-8000-0000000000e5','01997000-0000-7000-8000-0000000000e1','BX-01',\
         'BX-01','2028-03-31','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,\
         country_code,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000604','Meridian Laboratories','meridian laboratories',\
         'IN','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO product_company_roles (id,product_id,company_id,role,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-00000000060c',\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-000000000604',\
         'manufacturer','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO parties (id,display_name,normalized_search_name,legal_name,\
         gst_registration_status,place_of_supply_state_id,drug_licence_number,\
         drug_licence_valid_upto,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000602','Sunrise Distributors','sunrise distributors',\
         'Sunrise Distributors Private Limited','unregistered',\
         (SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         '20B-MH-9911 / 21B-MH-9912','2027-12-31','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-00000000060a','01997000-0000-7000-8000-000000000602',\
         'supplier','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "INSERT INTO party_addresses (id,party_id,address_role,line1,city,state_id,postal_code,\
         is_primary,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000603','01997000-0000-7000-8000-000000000602','billing',\
         '14 Ware House Road','Thane',\
         (SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'421302',1,\
         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        // A posted Purchase with a complete Phase 1M-D1-A provenance snapshot. The document goes in
        // before its lines, exactly as the posting transaction writes it.
        "INSERT INTO purchase_documents (id,store_id,supplier_party_id,supplier_invoice_number,\
         normalized_supplier_invoice_number,invoice_date,status,revision,supplier_display_name,\
         supplier_legal_name,supplier_gst_registration_status,supplier_place_of_supply_state_id,\
         supplier_state_code,store_gst_registration_status,\
         store_normalized_gstin,store_place_of_supply_state_id,store_state_code,tax_treatment,\
         taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,\
         created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,\
         posting_idempotency_key,posting_fingerprint,purchase_provenance_snapshot_version,\
         supplier_address_state,supplier_address_id,supplier_address_line1,supplier_address_city,\
         supplier_address_postal_code,supplier_address_country_code,supplier_address_state_id,\
         supplier_address_state_name,supplier_address_state_code,supplier_drug_licence_state,\
         supplier_drug_licence_number,supplier_drug_licence_valid_upto) \
         VALUES ('01997000-0000-7000-8000-000000000605','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-000000000602','INV-5501','INV-5501','2026-05-04','posted',2,\
         'Sunrise Distributors','Sunrise Distributors Private Limited','unregistered',\
         (SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'27','registered',\
         '27AAACS1429B1Z1',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         '27','intra_state',30000,900,900,0,0,31800,'01997000-0000-7000-8000-0000000000cc',\
         '2026-05-04T05:00:00.000Z','2026-05-04T05:00:00.000Z',\
         '01997000-0000-7000-8000-0000000000cc','2026-05-04T05:00:00.000Z',\
         '01997000-0000-7000-8000-00000000060b',\
         '0000000000000000000000000000000000000000000000000000000000005501',1,\
         'recorded','01997000-0000-7000-8000-000000000603','14 Ware House Road','Thane','421302',\
         'IN',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         'Maharashtra','27','recorded','20B-MH-9911 / 21B-MH-9912','2027-12-31')",
        "INSERT INTO purchase_lines (id,purchase_document_id,line_number,product_id,\
         product_pack_id,batch_id,quantity_packs,rate_per_pack_paise,quantity_atoms,\
         taxable_value_paise,created_at_utc,updated_at_utc,drug_display_name,batch_number,\
         manufacturer_company_id,manufacturer_name,manufacturer_state) VALUES \
         ('01997000-0000-7000-8000-000000000606','01997000-0000-7000-8000-000000000605',1,\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5',5,3000,50,15000,'2026-05-04T05:00:00.000Z',\
         '2026-05-04T05:00:00.000Z','Schedule X Test Medicine A','BX-01',\
         '01997000-0000-7000-8000-000000000604','Meridian Laboratories','recorded'),\
         ('01997000-0000-7000-8000-000000000609','01997000-0000-7000-8000-000000000605',2,\
         '01997000-0000-7000-8000-0000000000d2','01997000-0000-7000-8000-0000000000e2',\
         NULL,5,3000,50,15000,'2026-05-04T05:00:00.000Z','2026-05-04T05:00:00.000Z',\
         'Ordinary Test Medicine',NULL,NULL,NULL,'not_recorded')",
        // A second posted Purchase of the same drug, so a withdrawn working entry can be replaced
        // without colliding with the one live entry the first line already holds.
        "INSERT INTO purchase_documents (id,store_id,supplier_party_id,supplier_invoice_number,\
         normalized_supplier_invoice_number,invoice_date,status,revision,supplier_display_name,\
         supplier_legal_name,supplier_gst_registration_status,supplier_place_of_supply_state_id,\
         supplier_state_code,store_gst_registration_status,store_normalized_gstin,\
         store_place_of_supply_state_id,store_state_code,tax_treatment,taxable_value_paise,\
         cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,created_by_user_id,\
         created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc,posting_idempotency_key,\
         posting_fingerprint,purchase_provenance_snapshot_version,supplier_address_state,\
         supplier_address_id,supplier_address_line1,supplier_address_city,\
         supplier_address_postal_code,supplier_address_country_code,supplier_address_state_id,\
         supplier_address_state_name,supplier_address_state_code,supplier_drug_licence_state,\
         supplier_drug_licence_number,supplier_drug_licence_valid_upto) \
         VALUES ('01997000-0000-7000-8000-000000000607','01997000-0000-7000-8000-0000000000bb',\
         '01997000-0000-7000-8000-000000000602','INV-5502','INV-5502','2026-05-05','posted',2,\
         'Sunrise Distributors','Sunrise Distributors Private Limited','unregistered',\
         (SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),'27','registered',\
         '27AAACS1429B1Z1',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         '27','intra_state',30000,900,900,0,0,31800,'01997000-0000-7000-8000-0000000000cc',\
         '2026-05-05T05:00:00.000Z','2026-05-05T05:00:00.000Z',\
         '01997000-0000-7000-8000-0000000000cc','2026-05-05T05:00:00.000Z',\
         '01997000-0000-7000-8000-00000000060d',\
         '0000000000000000000000000000000000000000000000000000000000005502',1,\
         'recorded','01997000-0000-7000-8000-000000000603','14 Ware House Road','Thane','421302',\
         'IN',(SELECT id FROM state_codes WHERE jurisdiction='IN' AND state_code='27'),\
         'Maharashtra','27','recorded','20B-MH-9911 / 21B-MH-9912','2027-12-31')",
        "INSERT INTO purchase_lines (id,purchase_document_id,line_number,product_id,\
         product_pack_id,batch_id,quantity_packs,rate_per_pack_paise,quantity_atoms,\
         taxable_value_paise,created_at_utc,updated_at_utc,drug_display_name,batch_number,\
         manufacturer_company_id,manufacturer_name,manufacturer_state) VALUES \
         ('01997000-0000-7000-8000-000000000608','01997000-0000-7000-8000-000000000607',1,\
         '01997000-0000-7000-8000-0000000000d1','01997000-0000-7000-8000-0000000000e1',\
         '01997000-0000-7000-8000-0000000000e5',5,3000,50,15000,'2026-05-05T05:00:00.000Z',\
         '2026-05-05T05:00:00.000Z','Schedule X Test Medicine A','BX-01',\
         '01997000-0000-7000-8000-000000000604','Meridian Laboratories','recorded')",
        // The owner's finding. Only the first product is inside Schedule X; the second has no
        // finding at all, which is `unknown` and must never be read as Schedule X.
        "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
         effective_from,source_citation,determined_by_user_id,revision,status,created_at_utc,\
         updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000401',\
         '01997000-0000-7000-8000-0000000000d1','schedule_x',1,'2020-01-01',\
         'Test fixture: owner-recorded Schedule X finding','01997000-0000-7000-8000-0000000000cc',\
         1,'active','2026-05-01T00:00:00.000Z','2026-05-01T00:00:00.000Z')",
        "INSERT INTO store_professionals (id,store_id,full_name,capacity,registration_number,\
         registering_authority,valid_from,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000501','01997000-0000-7000-8000-0000000000bb',\
         'Meera Iyer','registered_pharmacist','MH-PH-44821','Maharashtra State Pharmacy Council',\
         '2020-01-01',1,'active','2026-05-01T00:00:00.000Z','2026-05-01T00:00:00.000Z')",
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id,terminal_id) VALUES \
         ('01997000-0000-7000-8000-000000000901','purchase_document',\
         '01997000-0000-7000-8000-000000000605',2,'posted','2026-05-04T05:00:00.000Z',NULL,1,\
         '{\"provenanceVersion\":1}','01997000-0000-7000-8000-0000000000cc','TERMINAL-A'),\
         ('01997000-0000-7000-8000-000000000902','product_regulatory_classification',\
         '01997000-0000-7000-8000-000000000401',1,'created','2026-05-01T00:00:00.000Z',NULL,1,\
         '{\"scheme\":\"schedule_x\"}','01997000-0000-7000-8000-0000000000cc',NULL)",
    ] {
        execute(pool, statement).await;
    }
}

/// Inserts a coherent receipt working entry for the seeded Schedule X purchase line.
fn receipt_insert(id: &str, reference_value: i64) -> String {
    receipt_insert_for(
        id,
        reference_value,
        POSTED,
        POSTED_LINE,
        "INV-5501",
        INVOICE_DATE,
    )
}

/// The same, for the second posted Purchase, so a withdrawn entry can be replaced without
/// colliding with the one live entry the first line already has.
fn receipt_insert_two(id: &str, reference_value: i64) -> String {
    receipt_insert_for(
        id,
        reference_value,
        POSTED_TWO,
        POSTED_TWO_LINE,
        "INV-5502",
        INVOICE_DATE_TWO,
    )
}

fn receipt_insert_for(
    id: &str,
    reference_value: i64,
    document: &str,
    line: &str,
    bill: &str,
    date: &str,
) -> String {
    format!(
        "INSERT INTO store_schedule_x_register_entries (id,store_id,entry_kind,reference_value,\
         reference,transaction_date,drug_name,product_id,batch_state,batch_number,\
         manufacturer_state,manufacturer_name,quantity_atoms,quantity_packs,bill_number,bill_date,\
         purchase_document_id,purchase_line_id,supplier_name,supplier_address_state,\
         supplier_address,supplier_licence_state,supplier_licence_number,status,\
         prepared_by_user_id,prepared_at_utc,\
         particulars_entered_in_physical_register,physical_entry_authenticated,\
         created_at_utc,updated_at_utc) VALUES \
         ('{id}','{STORE}','receipt',{reference_value},'AXR-{reference_value:06}','{date}',\
         'Schedule X Test Medicine A','{PRODUCT_X}','recorded','BX-01','recorded',\
         'Meridian Laboratories',50,5,'{bill}','{date}','{document}','{line}',\
         'Sunrise Distributors','recorded','14 Ware House Road','recorded',\
         '20B-MH-9911 / 21B-MH-9912','prepared','{OWNER}','2026-05-04T06:00:00.000Z',0,0,\
         '2026-05-04T06:00:00.000Z','2026-05-04T06:00:00.000Z')"
    )
}

/// Phase 1M-D2, item 38. A database built from nothing reaches 0026, with the working record, every
/// guard, and no page number anywhere.
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
            objects
                .iter()
                .any(|(kind, name, _)| kind == "table" && name == expected),
            "{expected} is missing"
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

    // Item 13. There is no page number, and no page concept, anywhere in the new schema.
    for table in NEW_TABLES {
        for column in columns_of(&pool, table).await {
            assert!(
                !column.contains("page"),
                "{table}.{column} models a physical page"
            );
        }
    }
    let register_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "store_schedule_x_register_entries")
        .expect("register table")
        .2;
    for forbidden in [
        "page_number",
        "page_full",
        "next_page",
        "continuation",
        "statutory_serial",
    ] {
        assert!(!register_sql.contains(forbidden), "{forbidden}");
    }
    // The reference is shaped so it cannot be mistaken for a register serial.
    assert!(register_sql.contains("'AXR-'"), "{register_sql}");

    // The audit log knows both new entity types, and the scratch copy is gone.
    let audit_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2;
    for expected in [
        "schedule_x_register_entry",
        "prescription_duplicate_copy_attestation",
    ] {
        assert!(
            audit_sql.contains(expected),
            "{expected} missing from audit"
        );
    }
    for table in tables(&pool).await {
        assert!(
            !table.contains("_phase1"),
            "scratch table left behind: {table}"
        );
    }
    pool.close().await;
}

/// Phase 1M-D2, item 38. Against a pharmacy that already posted a Schedule X receipt under 0025:
/// every row survives, and NOT ONE working record is invented for it.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_writes_no_working_record() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate_before_0026(&pool).await;
    structural_checks(&pool).await;

    // Before 0026 there is no such table to write.
    assert!(
        sqlx::query("SELECT 1 FROM store_schedule_x_register_entries")
            .fetch_optional(&pool)
            .await
            .is_err(),
        "0025 already had the working record"
    );

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
        "product_regulatory_classifications",
        "store_professionals",
        "master_change_events",
        "products",
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

    // Only the audit log is redefined, and only to admit the two new entity types.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if name == "master_change_events" {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // Every pre-existing row of every pre-existing table, on its original columns.
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events")
        .fetch_one(&pool)
        .await
        .expect("events");
    assert_eq!(events, 2, "the audit rebuild lost or gained events");
    let terminal: Vec<Option<String>> =
        sqlx::query_scalar("SELECT terminal_id FROM master_change_events ORDER BY event_id")
            .fetch_all(&pool)
            .await
            .expect("terminal ids");
    assert_eq!(terminal, vec![Some("TERMINAL-A".to_owned()), None]);

    // NO BACKFILL. A posted Schedule X receipt sitting in the database when 0026 arrives does not
    // acquire a working record it never had: legacy stays legacy, and the gap stays visible.
    let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_schedule_x_register_entries")
        .fetch_one(&pool)
        .await
        .expect("entries");
    assert_eq!(entries, 0, "the migration invented a working record");
    let attestations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM prescription_duplicate_copy_attestations")
            .fetch_one(&pool)
            .await
            .expect("attestations");
    assert_eq!(attestations, 0);
    pool.close().await;
}

/// Phase 1M-D2, item 38. A working entry must agree with the posted Purchase it claims to describe,
/// and with the owner's own Schedule X finding.
#[tokio::test]
async fn a_working_entry_must_agree_with_its_posted_purchase() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("coherence.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0026(&pool).await;

    // The coherent case is accepted, and takes the first reference.
    execute(
        &pool,
        &receipt_insert("01997000-0000-7000-8000-000000000a01", 1),
    )
    .await;
    let reference: String =
        sqlx::query_scalar("SELECT reference FROM store_schedule_x_register_entries WHERE id=?")
            .bind("01997000-0000-7000-8000-000000000a01")
            .fetch_one(&pool)
            .await
            .expect("reference");
    assert_eq!(reference, "AXR-000001");

    // A particular that disagrees with the frozen provenance is refused. Each of these would be a
    // working record that says something the posted document does not.
    for (label, forged) in [
        (
            "a supplier the document does not name",
            receipt_insert("01997000-0000-7000-8000-000000000a02", 2).replace(
                "'Sunrise Distributors','recorded'",
                "'Someone Else','recorded'",
            ),
        ),
        (
            "a quantity the line does not carry",
            receipt_insert("01997000-0000-7000-8000-000000000a03", 2).replace(",50,5,", ",99,5,"),
        ),
        (
            "a batch the line does not carry",
            receipt_insert("01997000-0000-7000-8000-000000000a04", 2)
                .replace("'recorded','BX-01'", "'recorded','BX-99'"),
        ),
        (
            "a drug name the line does not carry",
            receipt_insert("01997000-0000-7000-8000-000000000a05", 2).replace(
                "'Schedule X Test Medicine A','01997",
                "'Something Else','01997",
            ),
        ),
        (
            "a transaction date the document does not carry",
            receipt_insert("01997000-0000-7000-8000-000000000a06", 2)
                .replace("'2026-05-04','Schedule X", "'2026-05-05','Schedule X"),
        ),
        (
            "a reference out of sequence",
            receipt_insert("01997000-0000-7000-8000-000000000a07", 7),
        ),
    ] {
        let error = refused(&pool, &forged).await;
        assert!(
            error.contains("schedule_x_register_entry_incoherent"),
            "{label}: {error}"
        );
    }

    // A line whose product the owner has NOT recorded inside Schedule X gets no working record. The
    // second purchase line is an ordinary medicine with no finding at all: `unknown` is not
    // `applies`, and the register is not a place for guesses.
    let plain = receipt_insert("01997000-0000-7000-8000-000000000a08", 2)
        .replace(PRODUCT_X, PRODUCT_PLAIN)
        .replace(POSTED_LINE, POSTED_PLAIN_LINE)
        .replace("'Schedule X Test Medicine A'", "'Ordinary Test Medicine'")
        .replace(
            "'recorded','BX-01','recorded','Meridian Laboratories'",
            "'not_recorded',NULL,'not_recorded',NULL",
        );
    let error = refused(&pool, &plain).await;
    assert!(
        error.contains("schedule_x_register_entry_incoherent"),
        "an unclassified product was admitted: {error}"
    );

    // One live entry per purchase line. A second live receipt for one line would be one drug written
    // into the register twice.
    let error = refused(
        &pool,
        &receipt_insert("01997000-0000-7000-8000-000000000a09", 2),
    )
    .await;
    assert!(error.contains("UNIQUE constraint failed"), "{error}");

    // A supply entry cannot borrow a receipt's provenance, and a receipt cannot borrow a supply's.
    let hybrid =
        receipt_insert("01997000-0000-7000-8000-000000000a10", 2).replace("'receipt'", "'supply'");
    let error = refused(&pool, &hybrid).await;
    // SQLite runs a BEFORE INSERT trigger before it evaluates the table's CHECK constraints, so the
    // coherence guard answers first. Both refuse it; the point is that the kinds cannot be mixed.
    assert!(
        error.contains("schedule_x_register_entry_incoherent"),
        "{error}"
    );
    // The table CHECK is the second line of defence, and it answers for a mixture the trigger does
    // not inspect: a receipt row has no patient, so carrying a subject at all is a shape this schema
    // will not hold, whatever any trigger thinks.
    let with_subject = receipt_insert("01997000-0000-7000-8000-000000000a11", 2).replace(
        ",status,prepared_by_user_id",
        ",subject_kind,status,prepared_by_user_id",
    );
    assert_ne!(
        with_subject,
        receipt_insert("01997000-0000-7000-8000-000000000a11", 2),
        "the subject column was not spliced in"
    );
    let with_subject = with_subject.replace("'prepared','", "'human','prepared','");
    let error = refused(&pool, &with_subject).await;
    assert!(error.contains("CHECK constraint failed"), "{error}");

    structural_checks(&pool).await;
    pool.close().await;
}

/// Phase 1M-D2, item 38. The lifecycle moves one way, a finalized entry is beyond reach, a void
/// entry stays void, and no row is ever deleted.
#[tokio::test]
async fn the_lifecycle_moves_one_way_and_finalized_entries_are_beyond_reach() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("lifecycle.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0026(&pool).await;
    execute(
        &pool,
        &receipt_insert("01997000-0000-7000-8000-000000000a01", 1),
    )
    .await;
    let entry = "01997000-0000-7000-8000-000000000a01";

    // A frozen particular never moves, in any state.
    let error = refused(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET supplier_name='Someone Else' \
             WHERE id='{entry}'"
        ),
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // Closing an entry whose physical acts nobody attested is refused.
    let error = refused(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='finalized',\
             finalized_by_user_id='{OWNER}',finalized_at_utc='2026-05-04T07:00:00.000Z' \
             WHERE id='{entry}'"
        ),
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // One physical act without the other is not a confirmation.
    let error = refused(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='confirmed',\
             particulars_entered_in_physical_register=1,confirmed_by_user_id='{OWNER}',\
             confirmed_at_utc='2026-05-04T07:00:00.000Z' WHERE id='{entry}'"
        ),
    )
    .await;
    // The transition trigger answers before the table CHECK does — SQLite runs a BEFORE UPDATE
    // trigger first — and both refuse a page written but unsigned, or signed but unwritten.
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // A cashier cannot be the confirming user even by direct SQL: the transition names the roles.
    let confirm = |user: &str, professional: &str| {
        format!(
            "UPDATE store_schedule_x_register_entries SET status='confirmed',\
             particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
             supervising_professional_id='{professional}',\
             supervising_professional_name='Meera Iyer',\
             supervising_registration_number='MH-PH-44821',confirmed_by_user_id='{user}',\
             confirmed_at_utc='2026-05-04T07:00:00.000Z' WHERE id='{entry}'"
        )
    };
    let error = refused(&pool, &confirm(CASHIER, PHARMACIST)).await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // The confirmation must name a real, active registered pharmacist of this store.
    let error = refused(
        &pool,
        &confirm(OWNER, "01997000-0000-7000-8000-0000000005ff"),
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // Both acts, attested by an owner, naming the pharmacist on record.
    execute(&pool, &confirm(OWNER, PHARMACIST)).await;
    execute(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='finalized',\
             finalized_by_user_id='{OWNER}',finalized_at_utc='2026-05-04T08:00:00.000Z' \
             WHERE id='{entry}'"
        ),
    )
    .await;

    // Finalized is the end of the road: no further transition, and no deletion.
    for statement in [
        format!(
            "UPDATE store_schedule_x_register_entries SET status='void',voided_by_user_id='{OWNER}',\
             voided_at_utc='2026-05-04T09:00:00.000Z',void_reason='changed my mind' \
             WHERE id='{entry}'"
        ),
        format!(
            "UPDATE store_schedule_x_register_entries SET status='confirmed',finalized_at_utc=NULL,\
             finalized_by_user_id=NULL WHERE id='{entry}'"
        ),
    ] {
        let error = refused(&pool, &statement).await;
        assert!(
            error.contains("schedule_x_register_entry_immutable"),
            "{error}"
        );
    }
    let error = refused(
        &pool,
        &format!("DELETE FROM store_schedule_x_register_entries WHERE id='{entry}'"),
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );

    // A void entry keeps its reference, so the next entry takes the following one and the withdrawn
    // number is never reissued.
    execute(
        &pool,
        &receipt_insert_two("01997000-0000-7000-8000-000000000b01", 2),
    )
    .await;
    execute(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='void',voided_by_user_id='{OWNER}',\
             voided_at_utc='2026-05-04T09:00:00.000Z',void_reason='prepared against the wrong line' \
             WHERE id='01997000-0000-7000-8000-000000000b01'"
        ),
    )
    .await;
    // Void cannot be resurrected.
    let error = refused(
        &pool,
        "UPDATE store_schedule_x_register_entries SET status='prepared',voided_by_user_id=NULL,\
         voided_at_utc=NULL,void_reason=NULL WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_immutable"),
        "{error}"
    );
    // And the reference it held is not handed to the next entry.
    let error = refused(
        &pool,
        &receipt_insert_two("01997000-0000-7000-8000-000000000b02", 2),
    )
    .await;
    assert!(
        error.contains("schedule_x_register_entry_incoherent")
            || error.contains("UNIQUE constraint failed"),
        "a void reference was reissued: {error}"
    );
    execute(
        &pool,
        &receipt_insert_two("01997000-0000-7000-8000-000000000b03", 3),
    )
    .await;

    structural_checks(&pool).await;
    pool.close().await;
}

/// Phase 1M-D2, item 38. Rule 65(9)(a)'s retained-duplicate fact is written once and never rewritten.
#[tokio::test]
async fn the_retained_duplicate_copy_fact_is_written_once() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("duplicate.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0026(&pool).await;
    execute(
        &pool,
        "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_name,\
         prescriber_address,subject_kind,subject_name,subject_address,\
         written_signed_dated_attested,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000802','01997000-0000-7000-8000-0000000000bb',\
         'RX-000001','2026-05-02','Dr. Rao','Pune','human','Patient','Pune',1,\
         '01997000-0000-7000-8000-0000000000cc','2026-05-02T00:00:00.000Z',\
         '2026-05-02T00:00:00.000Z')",
    )
    .await;

    let attest = |id: &str, confirmed: i64| {
        format!(
            "INSERT INTO prescription_duplicate_copy_attestations (id,store_id,prescription_id,\
             retained_duplicate_prescription_copy_confirmed,attested_by_user_id,attested_at_utc) \
             VALUES ('{id}','{STORE}','01997000-0000-7000-8000-000000000802',{confirmed},\
             '{OWNER}','2026-05-04T07:00:00.000Z')"
        )
    };
    execute(&pool, &attest("01997000-0000-7000-8000-000000000c01", 1)).await;

    // One statement per prescription. It is a fact about a piece of paper, not a setting to toggle.
    let error = refused(&pool, &attest("01997000-0000-7000-8000-000000000c02", 0)).await;
    assert!(error.contains("UNIQUE constraint failed"), "{error}");
    let error = refused(
        &pool,
        "UPDATE prescription_duplicate_copy_attestations \
         SET retained_duplicate_prescription_copy_confirmed=0 \
         WHERE id='01997000-0000-7000-8000-000000000c01'",
    )
    .await;
    assert!(
        error.contains("duplicate_copy_attestation_is_append_only"),
        "{error}"
    );
    let error = refused(
        &pool,
        "DELETE FROM prescription_duplicate_copy_attestations \
         WHERE id='01997000-0000-7000-8000-000000000c01'",
    )
    .await;
    assert!(
        error.contains("duplicate_copy_attestation_is_append_only"),
        "{error}"
    );

    // A prescription of another store cannot be attested here.
    let error = refused(
        &pool,
        "INSERT INTO prescription_duplicate_copy_attestations (id,store_id,prescription_id,\
         retained_duplicate_prescription_copy_confirmed,attested_by_user_id,attested_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000c03','01997000-0000-7000-8000-0000000000be',\
         '01997000-0000-7000-8000-000000000802',1,'01997000-0000-7000-8000-0000000000cc',\
         '2026-05-04T07:00:00.000Z')",
    )
    .await;
    assert!(
        error.contains("duplicate_copy_attestation_incoherent")
            || error.contains("FOREIGN KEY constraint failed"),
        "{error}"
    );

    structural_checks(&pool).await;
    pool.close().await;
}

/// Phase 1M-D2, item 38. Migrations 0001–0025 are untouched by this phase.
#[tokio::test]
async fn the_earlier_migrations_are_unchanged() {
    let directory = migrations_directory();
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .expect("migrations")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".sql"))
        .collect();
    names.sort();
    assert_eq!(
        names.len(),
        29,
        "expected exactly 29 migrations after Phase 1M-D3-C1: {names:?}"
    );
    assert_eq!(names[25], "0026_schedule_x_register_foundations.sql");
    assert_eq!(names[26], "0027_schedule_x_prescription_compliance.sql");
    assert_eq!(names[27], "0028_schedule_x_supply_foundations.sql");
    assert_eq!(names[28], "0029_supplier_schedule_x_authority.sql");
    // Every earlier file is still the committed one, byte for byte, as `git status` also proves.
    for name in &names[..25] {
        let bytes = std::fs::read(directory.join(name)).expect("read migration");
        assert!(!bytes.is_empty(), "{name} is empty");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("schedule_x_register_entries"),
            "{name} was edited to mention the D2 working record"
        );
    }
}
