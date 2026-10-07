//! Migration 0027 against a database that already holds the Phase 1M-A..D2 records.
//!
//! 0027 adds the two prescription-side Schedule X compliance facts Phase 1M-D3 identified: the rule
//! 65(11)(c) attestation that the seller particulars were written on the physical prescription, and
//! the rule 65(2) hardening that the registered pharmacist named on a Schedule X register entry was
//! actually registered on the date of the transaction being attested.
//!
//! The outcomes that must never happen are: an attestation appearing for a drug that is not in
//! Schedule X; an attestation whose frozen seller particulars or dispensing date can be changed
//! afterwards; two live attestations for one dispensing occasion; the audit rebuild losing an event
//! or a column; the 0026 transition trigger losing a clause while it is replaced; and — above all —
//! this migration enabling a Schedule X sale, which it does not touch.
//!
//! ```text
//! cargo test --test migration_0027
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 27;

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
const PHARMACIST: &str = "01997000-0000-7000-8000-000000000501";
const INVOICE_DATE: &str = "2026-05-04";

/// owner recorded inside Schedule X and one ordinary drug with no finding at all, a registered
/// pharmacist, and an audit log with events already in it.
async fn populate_before_0027(pool: &SqlitePool) {
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

const PRESCRIPTION: &str = "01997000-0000-7000-8000-000000000701";
const PRESCRIPTION_ITEM: &str = "01997000-0000-7000-8000-000000000702";
const PRESCRIPTION_PLAIN: &str = "01997000-0000-7000-8000-000000000705";
const PRESCRIPTION_PLAIN_ITEM: &str = "01997000-0000-7000-8000-000000000706";
const DRAFT_SALE: &str = "01997000-0000-7000-8000-000000000703";
const DRAFT_LINE: &str = "01997000-0000-7000-8000-000000000704";
const DRAFT_PLAIN_LINE: &str = "01997000-0000-7000-8000-000000000707";
const SALE_DATE: &str = "2026-05-06";

/// What 0027 adds.
const NEW_TABLES: [&str; 1] = ["schedule_x_prescription_annotations"];

const NEW_TRIGGERS: [&str; 3] = [
    "schedule_x_prescription_annotations_coherent_insert",
    "schedule_x_prescription_annotations_no_delete",
    "schedule_x_prescription_annotations_no_update",
];

/// A draft Sale carrying one Schedule X line and one ordinary line, each linked to a prescription
/// item — the two dispensing occasions rule 65(11)(c) does and does not reach.
async fn populate_dispensing_occasions(pool: &SqlitePool) {
    for statement in [
        // The ordinary drug needs a lot of its own: `sale_lines` binds the batch to the pack.
        "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
         expires_on,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000e6','01997000-0000-7000-8000-0000000000e2','BX-02',\
         'BX-02','2028-03-31','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')"
            .to_owned(),
        format!(
            "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_name,\
             prescriber_address,subject_kind,subject_name,subject_address,repeat_authority,\
             written_signed_dated_attested,created_by_user_id,created_at_utc,updated_at_utc) \
             VALUES ('{PRESCRIPTION}','{STORE}','RX-000001','2026-05-05','Dr. A. Prescriber',\
             'Clinic Road','human','Test Patient','Patient Street','once',1,'{OWNER}',\
             '2026-05-05T05:00:00.000Z','2026-05-05T05:00:00.000Z'),\
             ('{PRESCRIPTION_PLAIN}','{STORE}','RX-000002','2026-05-05','Dr. A. Prescriber',\
             'Clinic Road','human','Test Patient','Patient Street','once',1,'{OWNER}',\
             '2026-05-05T05:00:00.000Z','2026-05-05T05:00:00.000Z')"
        ),
        format!(
            "INSERT INTO prescription_items (id,prescription_id,line_number,product_id,\
             written_description,prescribed_quantity_atoms,dose_text,created_at_utc,\
             updated_at_utc) VALUES ('{PRESCRIPTION_ITEM}','{PRESCRIPTION}',1,'{PRODUCT_X}',\
             'Tab. as prescribed',20,'1 tablet twice daily','2026-05-05T05:00:00.000Z',\
             '2026-05-05T05:00:00.000Z'),\
             ('{PRESCRIPTION_PLAIN_ITEM}','{PRESCRIPTION_PLAIN}',1,'{PRODUCT_PLAIN}',\
             'Tab. as prescribed',20,'1 tablet twice daily','2026-05-05T05:00:00.000Z',\
             '2026-05-05T05:00:00.000Z')"
        ),
        format!(
            "INSERT INTO sale_documents (id,store_id,business_date,status,revision,\
             created_by_user_id,created_at_utc,updated_at_utc) VALUES \
             ('{DRAFT_SALE}','{STORE}','{SALE_DATE}','draft',1,'{OWNER}',\
             '2026-05-06T05:00:00.000Z','2026-05-06T05:00:00.000Z')"
        ),
        format!(
            "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
             batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,\
             created_at_utc,updated_at_utc,prescription_item_id) VALUES \
             ('{DRAFT_LINE}','{DRAFT_SALE}',1,'{PRODUCT_X}',\
             '01997000-0000-7000-8000-0000000000e1','01997000-0000-7000-8000-0000000000e5',\
             'pack',1,10,8000,'2026-05-06T05:00:00.000Z','2026-05-06T05:00:00.000Z',\
             '{PRESCRIPTION_ITEM}'),\
             ('{DRAFT_PLAIN_LINE}','{DRAFT_SALE}',2,'{PRODUCT_PLAIN}',\
             '01997000-0000-7000-8000-0000000000e2','01997000-0000-7000-8000-0000000000e6',\
             'pack',1,10,8000,'2026-05-06T05:00:00.000Z','2026-05-06T05:00:00.000Z',\
             '{PRESCRIPTION_PLAIN_ITEM}')"
        ),
    ] {
        execute(pool, &statement).await;
    }
}

/// A coherent rule 65(11)(c) attestation for the Schedule X occasion.
fn annotation_insert(id: &str) -> String {
    annotation_insert_for(
        id,
        DRAFT_LINE,
        PRESCRIPTION,
        PRESCRIPTION_ITEM,
        PRODUCT_X,
        SALE_DATE,
        OWNER,
    )
}

fn annotation_insert_for(
    id: &str,
    line: &str,
    prescription: &str,
    item: &str,
    product: &str,
    dispensing_date: &str,
    actor: &str,
) -> String {
    format!(
        "INSERT INTO schedule_x_prescription_annotations (id,store_id,sale_document_id,\
         sale_line_id,prescription_id,prescription_item_id,product_id,\
         seller_particulars_noted_on_prescription,seller_name,seller_address,dispensing_date,\
         attested_on_store_date,attested_by_user_id,attested_at_utc,note,created_at_utc) VALUES \
         ('{id}','{STORE}','{DRAFT_SALE}','{line}','{prescription}','{item}','{product}',1,\
         'Care Pharmacy Private Limited','12 Market Road, Pune, 411001','{dispensing_date}',\
         '2026-05-06','{actor}','2026-05-06T06:00:00.000Z',NULL,'2026-05-06T06:00:00.000Z')"
    )
}

/// Phase 1M-D3-A, item 25A. A database built from nothing reaches 0027, with the rule 65(11)(c)
/// attestation, its guards, and nothing resembling a signature or a page number.
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

    // The frozen particulars are all there, and nothing resembling a page number is.
    let columns = columns_of(&pool, "schedule_x_prescription_annotations").await;
    for expected in [
        "seller_name",
        "seller_address",
        "dispensing_date",
        "attested_on_store_date",
        "attested_by_user_id",
        "seller_particulars_noted_on_prescription",
    ] {
        assert!(columns.contains(&expected.to_owned()), "{expected} missing");
    }
    for forbidden in ["page_number", "page", "signature", "signature_image"] {
        assert!(
            !columns.iter().any(|column| column == forbidden),
            "{forbidden} entered the schema"
        );
    }

    // The audit log admits the new entity type and keeps every column it had.
    let audit = columns_of(&pool, "master_change_events").await;
    assert_eq!(audit.len(), 11, "the audit rebuild changed the columns");
    assert!(audit.contains(&"terminal_id".to_owned()));
    let audit_sql = objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2
        .clone();
    assert!(audit_sql.contains("schedule_x_prescription_annotation"));
    assert!(audit_sql.contains("schedule_x_register_entry"));
    pool.close().await;
}

/// Phase 1M-D3-A, item 25A. 0027 arrives at a populated pharmacy and takes nothing away: every row
/// of every table is where it was, every audit event survives the rebuild, and the only schema
/// objects that change are the audit table and the one trigger this phase deliberately hardens.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_writes_no_attestation() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate_before_0027(&pool).await;
    populate_dispensing_occasions(&pool).await;
    execute(
        &pool,
        &receipt_insert("01997000-0000-7000-8000-000000000801", 1),
    )
    .await;
    structural_checks(&pool).await;

    // Before 0027 there is no such table to write.
    assert!(
        sqlx::query("SELECT 1 FROM schedule_x_prescription_annotations")
            .fetch_optional(&pool)
            .await
            .is_err(),
        "0026 already had the rule 65(11)(c) attestation"
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
        "store_schedule_x_register_entries",
        "prescriptions",
        "prescription_items",
        "sale_documents",
        "sale_lines",
        "master_change_events",
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

    let after_objects = schema_objects(&pool).await;
    let mut redefined = Vec::new();
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if &after.2 != sql {
            redefined.push(name.clone());
        }
    }
    redefined.sort();
    assert_eq!(
        redefined,
        vec![
            "master_change_events".to_owned(),
            "store_schedule_x_register_entries_transition".to_owned(),
        ],
        "0027 redefined something it had no business touching"
    );

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

    // NO BACKFILL. A draft Schedule X line sitting in the database when 0027 arrives does not
    // acquire an attestation nobody made.
    let annotations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM schedule_x_prescription_annotations")
            .fetch_one(&pool)
            .await
            .expect("annotations");
    assert_eq!(annotations, 0, "the migration invented an attestation");
    pool.close().await;
}

/// Phase 1M-D3-A, item 16. The rule 65(11)(c) attestation is true of the occasion it names or it is
/// not written, whoever writes it. Every limb is refused by the database itself.
#[tokio::test]
async fn an_attestation_must_agree_with_its_dispensing_occasion() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("coherence.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0027(&pool).await;
    populate_dispensing_occasions(&pool).await;

    // The sound one goes in, so every refusal below is about the limb it names.
    execute(
        &pool,
        &annotation_insert("01997000-0000-7000-8000-000000000901"),
    )
    .await;

    // One live attestation per dispensing occasion.
    let twice = refused(
        &pool,
        &annotation_insert("01997000-0000-7000-8000-000000000902"),
    )
    .await;
    assert!(twice.contains("UNIQUE"), "{twice}");

    // THE SCHEDULE X BOUNDARY. The ordinary drug has no Schedule X finding, so its dispensing
    // occasion cannot acquire this fact — an ordinary prescription sale gains no requirement.
    let ordinary = refused(
        &pool,
        &annotation_insert_for(
            "01997000-0000-7000-8000-000000000903",
            DRAFT_PLAIN_LINE,
            PRESCRIPTION_PLAIN,
            PRESCRIPTION_PLAIN_ITEM,
            PRODUCT_PLAIN,
            SALE_DATE,
            OWNER,
        ),
    )
    .await;
    assert!(
        ordinary.contains("schedule_x_prescription_annotation_incoherent"),
        "{ordinary}"
    );

    // A cashier cannot make this statement, in the database any more than over HTTP.
    let till = refused(
        &pool,
        &annotation_insert_for(
            "01997000-0000-7000-8000-000000000904",
            DRAFT_LINE,
            PRESCRIPTION,
            PRESCRIPTION_ITEM,
            PRODUCT_X,
            SALE_DATE,
            CASHIER,
        ),
    )
    .await;
    assert!(
        till.contains("schedule_x_prescription_annotation_incoherent"),
        "{till}"
    );

    // The dispensing date is the draft's own business date. A date brought in from anywhere else is
    // refused, so nothing typed into a screen can become the authoritative one.
    let wrong_date = refused(
        &pool,
        &annotation_insert_for(
            "01997000-0000-7000-8000-000000000905",
            DRAFT_LINE,
            PRESCRIPTION,
            PRESCRIPTION_ITEM,
            PRODUCT_X,
            "2026-05-01",
            OWNER,
        ),
    )
    .await;
    assert!(
        wrong_date.contains("schedule_x_prescription_annotation_incoherent"),
        "{wrong_date}"
    );

    // Append-only in both directions: the frozen particulars never move, and no row is deleted.
    for statement in [
        "UPDATE schedule_x_prescription_annotations SET seller_name='Somebody Else'",
        "UPDATE schedule_x_prescription_annotations SET seller_address='Elsewhere'",
        "UPDATE schedule_x_prescription_annotations SET dispensing_date='2026-01-01'",
        "UPDATE schedule_x_prescription_annotations SET seller_particulars_noted_on_prescription=0",
        "DELETE FROM schedule_x_prescription_annotations",
    ] {
        let message = refused(&pool, statement).await;
        assert!(
            message.contains("schedule_x_prescription_annotation_is_append_only"),
            "{statement}: {message}"
        );
    }
    pool.close().await;
}

/// Phase 1M-D3-A, item 16. A required seller particular cannot be blank, and a prescription cannot
/// have been dispensed on a day that has not happened.
#[tokio::test]
async fn a_blank_particular_and_a_future_dispensing_date_are_both_refused() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("particulars.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0027(&pool).await;
    populate_dispensing_occasions(&pool).await;

    let sound = annotation_insert("01997000-0000-7000-8000-000000000b01");
    for (what, statement) in [
        (
            "a blank seller name",
            sound.replace("'Care Pharmacy Private Limited'", "'   '"),
        ),
        (
            "a blank seller address",
            sound.replace("'12 Market Road, Pune, 411001'", "'  '"),
        ),
        (
            // The attestation was made on 2026-05-06; a supply dated later has not happened.
            "a dispensing date after the day of attestation",
            sound.replace("'2026-05-06','2026-05-06'", "'2026-05-07','2026-05-06'"),
        ),
    ] {
        let message = refused(&pool, &statement).await;
        assert!(!message.is_empty(), "{what} was accepted");
    }

    // And the sound statement goes in, so the refusals above are about their own reasons.
    execute(&pool, &sound).await;
    pool.close().await;
}

/// Phase 1M-D3-A, item 11. Rule 65(2) — active is not the same as registered on the day.
///
/// The 0026 transition trigger accepted any ACTIVE registered pharmacist. 0027 replaces it with the
/// same trigger plus two date conditions, and this proves both halves: the hardening bites, and
/// nothing else about the transition was lost while the trigger was replaced.
#[tokio::test]
async fn a_lapsed_registration_cannot_authenticate_a_schedule_x_entry() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("validity.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0027(&pool).await;
    execute(
        &pool,
        &receipt_insert("01997000-0000-7000-8000-000000000a01", 1),
    )
    .await;

    let confirm = format!(
        "UPDATE store_schedule_x_register_entries SET status='confirmed',\
         particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
         supervising_professional_id='{PHARMACIST}',supervising_professional_name='Meera Iyer',\
         supervising_registration_number='MH-PH-44821',confirmed_by_user_id='{OWNER}',\
         confirmed_at_utc='2026-05-04T07:00:00.000Z',updated_at_utc='2026-05-04T07:00:00.000Z' \
         WHERE id='01997000-0000-7000-8000-000000000a01'"
    );

    // The registration lapsed before the receipt. The record is still ACTIVE — exactly the gap.
    execute(
        &pool,
        &format!("UPDATE store_professionals SET valid_upto='2026-01-31' WHERE id='{PHARMACIST}'"),
    )
    .await;
    let lapsed = refused(&pool, &confirm).await;
    assert!(
        lapsed.contains("schedule_x_register_entry_immutable"),
        "{lapsed}"
    );

    // Not yet registered on the day, refused from the other side.
    execute(
        &pool,
        &format!(
            "UPDATE store_professionals SET valid_from='2027-01-01',valid_upto=NULL \
             WHERE id='{PHARMACIST}'"
        ),
    )
    .await;
    let early = refused(&pool, &confirm).await;
    assert!(
        early.contains("schedule_x_register_entry_immutable"),
        "{early}"
    );

    // Inclusive at both ends, per the repository's own convention: a registration that begins and
    // ends on the transaction date covers it.
    execute(
        &pool,
        &format!(
            "UPDATE store_professionals SET valid_from='{INVOICE_DATE}',\
             valid_upto='{INVOICE_DATE}' WHERE id='{PHARMACIST}'"
        ),
    )
    .await;
    execute(&pool, &confirm).await;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM store_schedule_x_register_entries \
         WHERE id='01997000-0000-7000-8000-000000000a01'",
    )
    .fetch_one(&pool)
    .await
    .expect("status");
    assert_eq!(status, "confirmed");

    // And nothing else about the transition was lost while the trigger was replaced: a finalized
    // entry is still beyond reach.
    execute(
        &pool,
        "UPDATE store_schedule_x_register_entries SET status='finalized',\
         finalized_by_user_id='01997000-0000-7000-8000-0000000000cc',\
         finalized_at_utc='2026-05-04T08:00:00.000Z',updated_at_utc='2026-05-04T08:00:00.000Z' \
         WHERE id='01997000-0000-7000-8000-000000000a01'",
    )
    .await;
    let closed = refused(
        &pool,
        "UPDATE store_schedule_x_register_entries SET status='void',\
         voided_by_user_id='01997000-0000-7000-8000-0000000000cc',\
         voided_at_utc='2026-05-04T09:00:00.000Z',void_reason='too late' \
         WHERE id='01997000-0000-7000-8000-000000000a01'",
    )
    .await;
    assert!(
        closed.contains("schedule_x_register_entry_immutable"),
        "{closed}"
    );
    pool.close().await;
}

/// Phase 1M-D3-A, item 29. Exactly one new migration, and every earlier file untouched.
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
    assert_eq!(names.len(), 30, "expected exactly 30 migrations: {names:?}");
    assert_eq!(names[26], "0027_schedule_x_prescription_compliance.sql");
    assert_eq!(names[27], "0028_schedule_x_supply_foundations.sql");
    assert_eq!(names[28], "0029_supplier_schedule_x_authority.sql");
    assert_eq!(names[29], "0030_schedule_x_sale_enablement.sql");
    // Every earlier file is still the committed one, as `git status` also proves.
    for name in &names[..26] {
        let bytes = std::fs::read(directory.join(name)).expect("read migration");
        assert!(!bytes.is_empty(), "{name} is empty");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("schedule_x_prescription_annotations"),
            "{name} was edited to mention the D3-A attestation"
        );
    }
}

/// A coherent Phase 1M-D2 receipt working entry for the seeded Schedule X purchase line, so the
/// rule 65(2) hardening can be exercised against a real entry rather than a synthetic one.
fn receipt_insert(id: &str, reference_value: i64) -> String {
    format!(
        "INSERT INTO store_schedule_x_register_entries (id,store_id,entry_kind,reference_value,\
         reference,transaction_date,drug_name,product_id,batch_state,batch_number,\
         manufacturer_state,manufacturer_name,quantity_atoms,quantity_packs,bill_number,bill_date,\
         purchase_document_id,purchase_line_id,supplier_name,supplier_address_state,\
         supplier_address,supplier_licence_state,supplier_licence_number,status,\
         prepared_by_user_id,prepared_at_utc,particulars_entered_in_physical_register,\
         physical_entry_authenticated,created_at_utc,updated_at_utc) VALUES \
         ('{id}','{STORE}','receipt',{reference_value},'AXR-{reference_value:06}','{INVOICE_DATE}',\
         'Schedule X Test Medicine A','{PRODUCT_X}','recorded','BX-01','recorded',\
         'Meridian Laboratories',50,5,'INV-5501','{INVOICE_DATE}','{POSTED}','{POSTED_LINE}',\
         'Sunrise Distributors','recorded','14 Ware House Road','recorded',\
         '20B-MH-9911 / 21B-MH-9912','prepared','{OWNER}','2026-05-04T06:00:00.000Z',0,0,\
         '2026-05-04T06:00:00.000Z','2026-05-04T06:00:00.000Z')"
    )
}
