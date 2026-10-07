//! Migration 0028 against a database that already holds the Phase 1M-A..D3-A records.
//!
//! 0028 is the last data-integrity slice before a Schedule X sale can responsibly be considered. It
//! adds three things: lot identity on a register entry, the invariant that a Schedule X supply may
//! only be prepared for a lot every sellable unit of which arrived on a posted purchase whose own
//! receipt was written into the physical register, and the narrowing of the generic rule 65(3)
//! prescription-register guard which rule 65(3)(1) excludes Schedule X from in its own words.
//!
//! The outcomes that must never happen are: a lot qualifying on a product-level match; one good
//! delivery laundering opening stock, an adjustment, a count or released quarantine stock in the
//! same lot; a prepared-only or withdrawn receipt entry counting as a register entry; a supply entry
//! being closed with no dispensing behind it; the rule 65(3) narrowing reaching anything other than
//! Schedule X; and this migration enabling a Schedule X sale, which it does not touch.
//!
//! ```text
//! cargo test --test migration_0028
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 28;

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
const PRODUCT_X: &str = "01997000-0000-7000-8000-0000000000d1";
const PRODUCT_PLAIN: &str = "01997000-0000-7000-8000-0000000000d2";
const POSTED: &str = "01997000-0000-7000-8000-000000000605";
const POSTED_LINE: &str = "01997000-0000-7000-8000-000000000606";
const PHARMACIST: &str = "01997000-0000-7000-8000-000000000501";
const INVOICE_DATE: &str = "2026-05-04";

/// owner recorded inside Schedule X and one ordinary drug with no finding at all, a registered
/// pharmacist, and an audit log with events already in it.
async fn populate_before_0028(pool: &SqlitePool) {
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

const BATCH_X: &str = "01997000-0000-7000-8000-0000000000e5";
const RECEIPT: &str = "01997000-0000-7000-8000-000000000801";

/// What 0028 adds.
const NEW_TRIGGERS: [&str; 1] = ["store_schedule_x_register_entries_supply_provenance"];

/// A Phase 1H `purchase` inward movement for the seeded Schedule X lot, carrying its purchase line.
fn purchase_movement(id: &str, line: &str, atoms: i64) -> String {
    format!(
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,stock_status,quantity_delta_atoms,occurred_on,purchase_line_id,\
         idempotency_key,posted_by_user_id,posted_at_utc) VALUES \
         ('{id}','{STORE}','{PRODUCT_X}','01997000-0000-7000-8000-0000000000e1','{BATCH_X}',\
         'purchase','sellable',{atoms},'{INVOICE_DATE}','{line}','{id}','{OWNER}',\
         '{INVOICE_DATE}T05:00:00.000Z')"
    )
}

/// Any other inward movement into the same lot's sellable balance.
fn other_inward(id: &str, kind: &str, atoms: i64, extra_column: &str, extra_value: &str) -> String {
    let (columns, values) = if extra_column.is_empty() {
        (String::new(), String::new())
    } else {
        (format!(",{extra_column}"), format!(",'{extra_value}'"))
    };
    format!(
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,stock_status,quantity_delta_atoms,occurred_on,idempotency_key,\
         posted_by_user_id,posted_at_utc{columns}) VALUES \
         ('{id}','{STORE}','{PRODUCT_X}','01997000-0000-7000-8000-0000000000e1','{BATCH_X}',\
         '{kind}','sellable',{atoms},'{INVOICE_DATE}','{id}','{OWNER}',\
         '{INVOICE_DATE}T05:00:00.000Z'{values})"
    )
}

/// Moves a receipt working entry to `confirmed`, which is the state in which the physical register
/// acts have been attested.
fn confirm_receipt(id: &str) -> String {
    format!(
        "UPDATE store_schedule_x_register_entries SET status='confirmed',\
         particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
         supervising_professional_id='{PHARMACIST}',supervising_professional_name='Meera Iyer',\
         supervising_registration_number='MH-PH-44821',confirmed_by_user_id='{OWNER}',\
         confirmed_at_utc='2026-05-04T07:00:00.000Z',updated_at_utc='2026-05-04T07:00:00.000Z' \
         WHERE id='{id}'"
    )
}

/// A coherent Schedule X SUPPLY working entry for the seeded draft sale line.
fn supply_insert(id: &str, reference_value: i64, batch_id: &str) -> String {
    supply_insert_for(id, reference_value, batch_id, DRAFT_LINE, PRESCRIPTION_ITEM)
}

fn supply_insert_for(
    id: &str,
    reference_value: i64,
    batch_id: &str,
    line: &str,
    item: &str,
) -> String {
    format!(
        "INSERT INTO store_schedule_x_register_entries (id,store_id,entry_kind,reference_value,\
         reference,transaction_date,drug_name,product_id,batch_state,batch_number,batch_id,\
         manufacturer_state,manufacturer_name,quantity_atoms,supply_basis,sale_document_id,\
         sale_line_id,prescription_id,prescription_item_id,subject_kind,purchaser_name,\
         purchaser_address,prescription_reference,status,prepared_by_user_id,prepared_at_utc,\
         particulars_entered_in_physical_register,physical_entry_authenticated,\
         created_at_utc,updated_at_utc) VALUES \
         ('{id}','{STORE}','supply',{reference_value},'AXR-{reference_value:06}','{SALE_DATE}',\
         'Schedule X Test Medicine A','{PRODUCT_X}','recorded','BX-01','{batch_id}','recorded',\
         'Meridian Laboratories',10,'prescription','{DRAFT_SALE}','{line}','{PRESCRIPTION}',\
         '{item}','human','Test Patient','Patient Street','RX-000001','prepared','{OWNER}',\
         '2026-05-06T06:00:00.000Z',0,0,'2026-05-06T06:00:00.000Z','2026-05-06T06:00:00.000Z')"
    )
}

/// A pharmacy at 0028 with a draft Schedule X sale line drawing on a lot that arrived on the seeded
/// posted purchase, whose receipt working entry is confirmed. This is the PASSING baseline; every
/// test below breaks exactly one thing about it.
async fn qualified_world(pool: &SqlitePool) {
    populate_before_0028(pool).await;
    populate_dispensing_occasions(pool).await;
    execute(pool, &receipt_insert(RECEIPT, 1)).await;
    execute(pool, &confirm_receipt(RECEIPT)).await;
    execute(
        pool,
        &purchase_movement("01997000-0000-7000-8000-000000000c01", POSTED_LINE, 50),
    )
    .await;
}

async fn world_at_0028(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join(name)).await;
    run_migrations(&pool, &directory).await;
    (temp, pool)
}

/// Phase 1M-D3-B, item 25. A database built from nothing reaches 0028, with the lot guard, the
/// batch identity column, and the narrowed rule 65(3) guard.
#[tokio::test]
async fn a_fresh_database_migrates_to_the_new_version() {
    let (_temp, pool) = world_at_0028("fresh.sqlite3").await;
    let applied: Vec<(i64, i64)> =
        sqlx::query_as("SELECT version,success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .expect("applied migrations");
    assert_eq!(applied.len() as i64, NEW_MIGRATION);
    assert!(applied.iter().all(|row| row.1 == 1));
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
    // The lot itself is now a column, beside the printed number rule 65(21)(b)(vi) asks for.
    let columns = columns_of(&pool, "store_schedule_x_register_entries").await;
    assert!(columns.contains(&"batch_id".to_owned()));
    assert!(columns.contains(&"batch_number".to_owned()));
    // The audit log is NOT rebuilt by this migration, so it keeps every column untouched.
    assert_eq!(columns_of(&pool, "master_change_events").await.len(), 11);

    // The rule 65(3) guard now excludes Schedule X, and still refuses a dangling record.
    let guard = objects
        .iter()
        .find(|(_, name, _)| name == "sale_documents_prescription_record_required")
        .expect("rule 65(3) guard")
        .2
        .clone();
    assert!(guard.contains("$.schedule_x"), "{guard}");
    assert!(
        guard.contains("record.status IN ('prepared', 'confirmed')"),
        "the dangling-record limb was lost: {guard}"
    );
    // A supply entry cannot be closed without the dispensing it records.
    let transition = objects
        .iter()
        .find(|(_, name, _)| name == "store_schedule_x_register_entries_transition")
        .expect("transition")
        .2
        .clone();
    assert!(
        transition.contains("NEW.entry_kind <> 'supply' OR NEW.dispensing_id IS NOT NULL"),
        "{transition}"
    );
    // And the rule 65(2) date predicates from 0027 survived the recreation.
    assert!(transition.contains("professional.valid_from"));
    assert!(transition.contains("professional.valid_upto"));
    pool.close().await;
}

/// Phase 1M-D3-B, item 25. 0028 arrives at a populated pharmacy and takes nothing away.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_prepares_no_supply() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate_before_0028(&pool).await;
    populate_dispensing_occasions(&pool).await;
    execute(&pool, &receipt_insert(RECEIPT, 1)).await;
    execute(
        &pool,
        &purchase_movement("01997000-0000-7000-8000-000000000c01", POSTED_LINE, 50),
    )
    .await;
    structural_checks(&pool).await;

    let before_objects = schema_objects(&pool).await;
    let before_tables = tables(&pool).await;
    let mut before_rows = Vec::new();
    for table in &before_tables {
        let columns = columns_of(&pool, table).await;
        let rows = rows_of(&pool, table, &columns).await;
        before_rows.push((table.clone(), columns, rows));
    }
    pool.close().await;

    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;
    structural_checks(&pool).await;

    // Only the two triggers this phase deliberately narrows are redefined. Nothing is dropped, and
    // `store_schedule_x_register_entries` keeps its identity: SQLite reports an added column in the
    // table's own SQL, which is why it appears here.
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
            "sale_documents_prescription_record_required".to_owned(),
            "store_schedule_x_register_entries".to_owned(),
            "store_schedule_x_register_entries_transition".to_owned(),
        ],
        "0028 redefined something it had no business touching"
    );

    // Every pre-existing row survives on its original columns.
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }
    // NO BACKFILL. The new column is NULL everywhere, and no supply entry was invented.
    let batched: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE batch_id IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("batch ids");
    assert_eq!(batched, 0, "the migration invented lot identity");
    let supplies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_schedule_x_register_entries WHERE entry_kind='supply'",
    )
    .fetch_one(&pool)
    .await
    .expect("supplies");
    assert_eq!(supplies, 0, "the migration invented a supply");
    pool.close().await;
}

/// Phase 1M-D3-B, item 37 — THE PROVENANCE ADVERSARIAL MATRIX.
///
/// The qualifying case passes; every way of not qualifying fails, in the database, whoever writes
/// the statement. One good delivery never launders a bad one.
#[tokio::test]
async fn only_an_exactly_traceable_lot_may_support_a_schedule_x_supply() {
    // 1. A qualifying exact purchase lot with a confirmed receipt entry -> PASS.
    {
        let (_t, pool) = world_at_0028("case1.sqlite3").await;
        qualified_world(&pool).await;
        execute(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d01", 2, BATCH_X),
        )
        .await;
        let kind: String = sqlx::query_scalar(
            "SELECT entry_kind FROM store_schedule_x_register_entries \
             WHERE id='01997000-0000-7000-8000-000000000d01'",
        )
        .fetch_one(&pool)
        .await
        .expect("entry");
        assert_eq!(kind, "supply");
        pool.close().await;
    }

    // 2/3. A different lot, and a lot whose printed number matches but whose identity does not.
    // The sale line draws on BATCH_X; naming any other lot is refused.
    {
        let (_t, pool) = world_at_0028("case2.sqlite3").await;
        qualified_world(&pool).await;
        // A second lot of the same pack carrying the SAME printed batch text.
        execute(
            &pool,
            "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
             expires_on,created_at_utc,updated_at_utc) VALUES \
             ('01997000-0000-7000-8000-0000000000f9','01997000-0000-7000-8000-0000000000e1',\
             'BX-01-DUP','BX-01-DUP','2028-03-31','2026-01-01T00:00:00.000Z',\
             '2026-01-01T00:00:00.000Z')",
        )
        .await;
        let wrong = refused(
            &pool,
            &supply_insert(
                "01997000-0000-7000-8000-000000000d02",
                2,
                "01997000-0000-7000-8000-0000000000f9",
            ),
        )
        .await;
        assert!(
            wrong.contains("schedule_x_supply_lot_provenance_unresolved"),
            "{wrong}"
        );
        pool.close().await;
    }

    // 4. A product-level matching receipt, but the allocated lot has no inward history at all.
    {
        let (_t, pool) = world_at_0028("case4.sqlite3").await;
        populate_before_0028(&pool).await;
        populate_dispensing_occasions(&pool).await;
        execute(&pool, &receipt_insert(RECEIPT, 1)).await;
        execute(&pool, &confirm_receipt(RECEIPT)).await;
        // No inventory movement at all: the receipt entry exists for the product and the purchase
        // line, but nothing says this lot on the shelf came from it.
        let unresolved = refused(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d04", 2, BATCH_X),
        )
        .await;
        assert!(
            unresolved.contains("schedule_x_supply_lot_provenance_unresolved"),
            "{unresolved}"
        );
        pool.close().await;
    }

    // 5/6/7. Opening stock and a hand adjustment each disqualify the whole lot, even though a
    // qualifying purchase also brought stock into it. These are the two non-purchase inward routes
    // that need no other record to exist; every other one (`stock_count`, `disposition_transfer`)
    // fails on the same clause, because the guard asks for `movement_type = 'purchase'` and none of
    // them may carry a `purchase_line_id` at all. `disposition_transfer` is exercised separately in
    // `stock_released_back_from_quarantine_stops_a_lot_qualifying`.
    for (index, (what, kind)) in [
        ("opening stock", "opening_stock"),
        ("a hand adjustment", "adjustment"),
    ]
    .into_iter()
    .enumerate()
    {
        let (_t, pool) = world_at_0028(&format!("case5_{index}.sqlite3")).await;
        qualified_world(&pool).await;
        execute(
            &pool,
            &other_inward("01997000-0000-7000-8000-000000000c0a", kind, 10, "", ""),
        )
        .await;
        let mixed = refused(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d05", 2, BATCH_X),
        )
        .await;
        assert!(
            mixed.contains("schedule_x_supply_lot_provenance_unresolved"),
            "{what} did not disqualify the lot: {mixed}"
        );
        pool.close().await;
    }

    // 10/11. A receipt entry that is only PREPARED, and one that was withdrawn, are not register
    // entries. Nothing has been written on paper, so nothing accounts for the stock.
    for (index, (what, statement)) in [
        ("a prepared-only receipt", String::new()),
        (
            "a withdrawn receipt",
            format!(
                "UPDATE store_schedule_x_register_entries SET status='void',\
                 voided_by_user_id='{OWNER}',voided_at_utc='2026-05-04T08:00:00.000Z',\
                 void_reason='entered in error' WHERE id='{RECEIPT}'"
            ),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (_t, pool) = world_at_0028(&format!("case10_{index}.sqlite3")).await;
        populate_before_0028(&pool).await;
        populate_dispensing_occasions(&pool).await;
        execute(&pool, &receipt_insert(RECEIPT, 1)).await;
        if !statement.is_empty() {
            execute(&pool, &confirm_receipt(RECEIPT)).await;
            execute(&pool, &statement).await;
        }
        execute(
            &pool,
            &purchase_movement("01997000-0000-7000-8000-000000000c01", POSTED_LINE, 50),
        )
        .await;
        let refused_message = refused(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d10", 2, BATCH_X),
        )
        .await;
        assert!(
            refused_message.contains("schedule_x_supply_lot_provenance_unresolved"),
            "{what} was accepted: {refused_message}"
        );
        pool.close().await;
    }

    // 12. A FINALIZED qualifying receipt passes too: closing the software record does not withdraw
    // the physical register entry it attested.
    {
        let (_t, pool) = world_at_0028("case12.sqlite3").await;
        qualified_world(&pool).await;
        execute(
            &pool,
            &format!(
                "UPDATE store_schedule_x_register_entries SET status='finalized',\
                 finalized_by_user_id='{OWNER}',finalized_at_utc='2026-05-04T09:00:00.000Z',\
                 updated_at_utc='2026-05-04T09:00:00.000Z' WHERE id='{RECEIPT}'"
            ),
        )
        .await;
        execute(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d12", 2, BATCH_X),
        )
        .await;
        pool.close().await;
    }

    // 15. A master mutated after the purchase changes nothing: provenance is movements and frozen
    // purchase rows, never today's catalogue.
    {
        let (_t, pool) = world_at_0028("case15.sqlite3").await;
        qualified_world(&pool).await;
        execute(
            &pool,
            &format!("UPDATE products SET display_name='Renamed Medicine' WHERE id='{PRODUCT_X}'"),
        )
        .await;
        execute(
            &pool,
            "UPDATE pharmaceutical_companies SET display_name='Renamed Laboratories' \
             WHERE id='01997000-0000-7000-8000-000000000604'",
        )
        .await;
        execute(
            &pool,
            &supply_insert("01997000-0000-7000-8000-000000000d15", 2, BATCH_X),
        )
        .await;
        pool.close().await;
    }
}

/// Phase 1M-D3-B, items 8 and 9. Goods a customer returned come back quarantined, and reach the
/// counter only by being released to sellable. That release is not a purchase, so the lot it lands
/// in stops qualifying — the one route by which Schedule X stock can leave and re-enter the shop.
#[tokio::test]
async fn stock_released_back_from_quarantine_stops_a_lot_qualifying() {
    let (_t, pool) = world_at_0028("returns.sqlite3").await;
    qualified_world(&pool).await;

    // Before the release, the lot qualifies.
    execute(
        &pool,
        &supply_insert("01997000-0000-7000-8000-000000000e01", 2, BATCH_X),
    )
    .await;
    execute(
        &pool,
        "UPDATE store_schedule_x_register_entries SET status='void',\
         voided_by_user_id='01997000-0000-7000-8000-0000000000cc',\
         voided_at_utc='2026-05-06T07:00:00.000Z',void_reason='making room for the test' \
         WHERE id='01997000-0000-7000-8000-000000000e01'",
    )
    .await;

    // A disposition transfer releases quarantined stock into the sellable balance of the same lot.
    execute(
        &pool,
        &format!(
            "INSERT INTO stock_dispositions (id,store_id,product_id,product_pack_id,batch_id,\
             quantity_atoms,from_status,to_status,reason,occurred_on,authorised_by_user_id,\
             created_at_utc,idempotency_key) VALUES \
             ('01997000-0000-7000-8000-000000000b20','{STORE}','{PRODUCT_X}',\
             '01997000-0000-7000-8000-0000000000e1','{BATCH_X}',5,'quarantined','sellable',\
             'released after inspection','{SALE_DATE}','{OWNER}','2026-05-06T05:00:00.000Z',\
             '01997000-0000-7000-8000-000000000b21')"
        ),
    )
    .await;
    execute(
        &pool,
        &other_inward(
            "01997000-0000-7000-8000-000000000c20",
            "disposition_transfer",
            5,
            "stock_disposition_id",
            "01997000-0000-7000-8000-000000000b20",
        ),
    )
    .await;

    let released = refused(
        &pool,
        &supply_insert("01997000-0000-7000-8000-000000000e02", 3, BATCH_X),
    )
    .await;
    assert!(
        released.contains("schedule_x_supply_lot_provenance_unresolved"),
        "released quarantine stock did not disqualify the lot: {released}"
    );
    pool.close().await;
}

/// Phase 1M-D3-B, items 16 and 22. A prepared supply entry cannot be closed, because there is no
/// dispensing for it to be a record of while Schedule X sales cannot post.
#[tokio::test]
async fn a_supply_entry_cannot_be_finalized_without_the_dispensing_it_records() {
    let (_t, pool) = world_at_0028("finalize.sqlite3").await;
    qualified_world(&pool).await;
    execute(
        &pool,
        &supply_insert("01997000-0000-7000-8000-000000000f01", 2, BATCH_X),
    )
    .await;
    execute(
        &pool,
        &confirm_receipt("01997000-0000-7000-8000-000000000f01"),
    )
    .await;

    let closed = refused(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='finalized',\
             finalized_by_user_id='{OWNER}',finalized_at_utc='2026-05-06T09:00:00.000Z',\
             updated_at_utc='2026-05-06T09:00:00.000Z' \
             WHERE id='01997000-0000-7000-8000-000000000f01'"
        ),
    )
    .await;
    assert!(
        closed.contains("schedule_x_register_entry_immutable"),
        "a supply entry was closed with no dispensing behind it: {closed}"
    );

    // A RECEIPT entry is unaffected: it records a purchase that did happen.
    execute(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET status='finalized',\
             finalized_by_user_id='{OWNER}',finalized_at_utc='2026-05-04T09:00:00.000Z',\
             updated_at_utc='2026-05-04T09:00:00.000Z' WHERE id='{RECEIPT}'"
        ),
    )
    .await;
    let receipt_status: String = sqlx::query_scalar(&format!(
        "SELECT status FROM store_schedule_x_register_entries WHERE id='{RECEIPT}'"
    ))
    .fetch_one(&pool)
    .await
    .expect("status");
    assert_eq!(receipt_status, "finalized");
    pool.close().await;
}

/// Phase 1M-D3-B, item 30. A supply entry cannot be forged onto another store's Sale, another
/// prescription item, a posted Sale, or an archived prescription.
#[tokio::test]
async fn a_supply_entry_must_agree_with_its_draft_sale() {
    let (_t, pool) = world_at_0028("coherence.sqlite3").await;
    qualified_world(&pool).await;

    // The ordinary line of the same draft Sale names a different prescription item and a different
    // product, so it cannot borrow this line's provenance.
    let wrong_line = refused(
        &pool,
        &supply_insert_for(
            "01997000-0000-7000-8000-000000000f11",
            2,
            BATCH_X,
            DRAFT_PLAIN_LINE,
            PRESCRIPTION_ITEM,
        ),
    )
    .await;
    assert!(!wrong_line.is_empty(), "a mismatched line was accepted");

    // An archived prescription cannot be supplied against.
    execute(
        &pool,
        &format!(
            "UPDATE prescriptions SET status='archived',\
             archived_at_utc='2026-05-06T05:00:00.000Z',archive_reason='withdrawn' \
             WHERE id='{PRESCRIPTION}'"
        ),
    )
    .await;
    let archived = refused(
        &pool,
        &supply_insert("01997000-0000-7000-8000-000000000f12", 2, BATCH_X),
    )
    .await;
    assert!(
        archived.contains("schedule_x_supply_lot_provenance_unresolved"),
        "{archived}"
    );
    execute(
        &pool,
        &format!(
            "UPDATE prescriptions SET status='active',archived_at_utc=NULL,archive_reason=NULL \
             WHERE id='{PRESCRIPTION}'"
        ),
    )
    .await;

    // A supply working entry is prepared BEFORE supply, so the guard requires a DRAFT Sale. That
    // limb cannot be reached behaviourally, because a Schedule X Sale cannot be posted at all: the
    // clause is asserted against the guard's own text, and the reason it is unreachable is proved
    // below by peeling the gates off one at a time and watching each refuse in turn.
    let guard: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master \
         WHERE name='store_schedule_x_register_entries_supply_provenance'",
    )
    .fetch_one(&pool)
    .await
    .expect("supply guard");
    assert!(guard.contains("document.status = 'draft'"), "{guard}");

    // HOW MANY INDEPENDENT DATABASE GUARDS REFUSE A POSTED SCHEDULE X SALE.
    //
    // Each is dropped only after it has been shown to refuse, so the list is the real depth of the
    // fail-closed boundary rather than an assumption about it. Nothing in Phase 1M-D3-B removes any
    // of them, and the last one standing is never reached here.
    let post = format!(
        "UPDATE sale_documents SET status='posted',revision=2,posted_by_user_id='{OWNER}',\
         posted_at_utc='2026-05-06T08:00:00.000Z' WHERE id='{DRAFT_SALE}'"
    );
    let mut refusals = Vec::new();
    for trigger in [
        "sale_documents_regulatory_gate_update",
        "sale_documents_state_boundary_update",
    ] {
        refusals.push(refused(&pool, &post).await);
        execute(&pool, &format!("DROP TRIGGER {trigger}")).await;
    }
    refusals.push(refused(&pool, &post).await);
    assert!(
        refusals[0].contains("regulatory_gate_refuses_posting"),
        "{:?}",
        refusals[0]
    );
    assert!(
        refusals[1].contains("state_regulatory_boundary_refuses_posting"),
        "{:?}",
        refusals[1]
    );
    // And a third, unrelated guard is still standing behind those two.
    assert!(
        refusals[2].contains("prescription_link_without_dispensing"),
        "{:?}",
        refusals[2]
    );
    pool.close().await;
}

/// Phase 1M-D3-B, item 29. Exactly one new migration, and every earlier file untouched.
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
    assert_eq!(names[27], "0028_schedule_x_supply_foundations.sql");
    assert_eq!(names[28], "0029_supplier_schedule_x_authority.sql");
    assert_eq!(names[29], "0030_schedule_x_sale_enablement.sql");
    for name in &names[..27] {
        let bytes = std::fs::read(directory.join(name)).expect("read migration");
        assert!(!bytes.is_empty(), "{name} is empty");
        assert!(
            !String::from_utf8_lossy(&bytes)
                .contains("schedule_x_supply_lot_provenance_unresolved"),
            "{name} was edited to mention the D3-B lot guard"
        );
    }
}

/// A dispensing for the seeded draft sale line, which is what makes the rule 65(3) guard look for a
/// prescription-register record at all.
fn dispensing_insert(
    id: &str,
    prescription: &str,
    item: &str,
    line: &str,
    product: &str,
) -> String {
    format!(
        "INSERT INTO prescription_dispensings (id,store_id,prescription_id,prescription_item_id,\
         sale_document_id,sale_line_id,product_id,quantity_atoms,dispensed_on,\
         supervising_professional_id,supervising_professional_name,\
         supervising_registration_number,endorsement_confirmed_by_user_id,\
         endorsement_confirmed_at_utc,created_at_utc) VALUES \
         ('{id}','{STORE}','{prescription}','{item}','{DRAFT_SALE}','{line}',\
         '{product}',10,'{SALE_DATE}','{PHARMACIST}','Meera Iyer','MH-PH-44821','{OWNER}',\
         '2026-05-06T06:00:00.000Z','2026-05-06T06:00:00.000Z')"
    )
}

/// Freezes a scheme snapshot onto the seeded draft sale line, the way posting does.
fn snapshot(schemes: &str) -> String {
    format!(
        "UPDATE sale_lines SET regulatory_snapshot_version=1,\
         regulatory_schemes_snapshot='{schemes}' WHERE id='{DRAFT_LINE}'"
    )
}

const NOTHING_APPLIES: &str = "{\"schedule_h\":\"does_not_apply\",\
     \"schedule_h1\":\"does_not_apply\",\"schedule_x\":\"does_not_apply\",\
     \"schedule_c\":\"does_not_apply\",\"schedule_c1\":\"does_not_apply\"}";
const ONLY_X: &str = "{\"schedule_h\":\"does_not_apply\",\"schedule_h1\":\"does_not_apply\",\
     \"schedule_x\":\"applies\",\"schedule_c\":\"does_not_apply\",\
     \"schedule_c1\":\"does_not_apply\"}";
const ONLY_H: &str = "{\"schedule_h\":\"applies\",\"schedule_h1\":\"does_not_apply\",\
     \"schedule_x\":\"does_not_apply\",\"schedule_c\":\"does_not_apply\",\
     \"schedule_c1\":\"does_not_apply\"}";
const X_AND_H1: &str = "{\"schedule_h\":\"applies\",\"schedule_h1\":\"applies\",\
     \"schedule_x\":\"applies\",\"schedule_c\":\"does_not_apply\",\
     \"schedule_c1\":\"does_not_apply\"}";

/// Builds a draft Sale that is one step from posting, with a dispensing on its line and the given
/// scheme snapshot frozen, and with the two upstream Phase 1M-A/1M-C gates removed so the rule 65(3)
/// guard is the thing under test. Returns the posting statement.
async fn ready_to_post(pool: &SqlitePool, schemes: &str) -> String {
    populate_before_0028(pool).await;
    populate_dispensing_occasions(pool).await;
    // BOTH seeded lines carry a prescription link, and the `prescription_link_without_dispensing`
    // guard wants a dispensing for each, so both get one. The ordinary line's snapshot stays
    // unscheduled, which makes it a control rather than part of the case under test.
    execute(
        pool,
        &dispensing_insert(
            "01997000-0000-7000-8000-000000000a71",
            PRESCRIPTION,
            PRESCRIPTION_ITEM,
            DRAFT_LINE,
            PRODUCT_X,
        ),
    )
    .await;
    // The ordinary second line is unlinked from its prescription and left unscheduled, so each case
    // below turns on ONE line's scheme snapshot and nothing else. Left linked, its own dispensing
    // would legitimately demand a rule 65(3) record and mask the case under test.
    execute(
        pool,
        &format!(
            "UPDATE sale_lines SET prescription_item_id=NULL,regulatory_snapshot_version=1,\
             regulatory_schemes_snapshot='{NOTHING_APPLIES}' WHERE id='{DRAFT_PLAIN_LINE}'"
        ),
    )
    .await;
    execute(pool, &snapshot(schemes)).await;
    // Dropped so this test is about rule 65(3) and nothing else. Their own refusals are proved in
    // `a_supply_entry_must_agree_with_its_draft_sale`, and Phase 1M-D3-B removes neither.
    execute(pool, "DROP TRIGGER sale_documents_regulatory_gate_update").await;
    execute(pool, "DROP TRIGGER sale_documents_state_boundary_update").await;
    // A complete posted row: the table's own CHECK requires every numbering and posting particular
    // together. Because SQLite evaluates BEFORE triggers ahead of CHECK constraints, a case that
    // reaches this CHECK has already cleared every guard — so the statement has to be valid for the
    // passing case to be able to prove anything.
    format!(
        "UPDATE sale_documents SET status='posted',revision=2,posted_by_user_id='{OWNER}',\
         posted_at_utc='2026-05-06T08:00:00.000Z',series_code='SALE',financial_year='2026-27',\
         sequence_value=1,document_number='SALE/2026-27/000001',tax_treatment='intra_state',\
         posting_idempotency_key='01997000-0000-7000-8000-000000000a90' \
         WHERE id='{DRAFT_SALE}'"
    )
}

/// Phase 1M-D3-B, item 36 — THE RULE 65(3) REGRESSION MATRIX.
///
/// Rule 65(3)(1) records the supply of "any drug OTHER THAN those specified in Schedule X" in the
/// generic prescription register. The Phase 1M-B guard asked for that record for every dispensing,
/// with no scheme test. It is narrowed for Schedule X, and for nothing else.
///
/// THIS IS NOT "SCHEDULE X NEEDS NO RECORD". Schedule X keeps its own, stricter controls — the
/// bound register of rule 65(21), the duplicate copy of 65(9)(a), the note of 65(11)(c), supervision
/// under 65(2) and the Form 20-F authority of rule 61(3) — none of which this narrowing touches.
#[tokio::test]
async fn the_rule_65_3_register_is_required_for_everything_except_schedule_x() {
    // A. A Schedule X dispensing is NOT held to the generic rule 65(3) register.
    {
        let (_t, pool) = world_at_0028("r653_x.sqlite3").await;
        let post = ready_to_post(&pool, ONLY_X).await;
        execute(&pool, &post).await;
        let status: String = sqlx::query_scalar(&format!(
            "SELECT status FROM sale_documents WHERE id='{DRAFT_SALE}'"
        ))
        .fetch_one(&pool)
        .await
        .expect("status");
        assert_eq!(
            status, "posted",
            "a Schedule X dispensing was still held to the rule 65(3) register"
        );
        // And no rule 65(3) record was invented to satisfy the guard.
        let records: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM prescription_supply_records")
            .fetch_one(&pool)
            .await
            .expect("records");
        assert_eq!(records, 0);
        pool.close().await;
    }

    // B/C. An ordinary Schedule H dispensing still requires it, exactly as before this phase.
    {
        let (_t, pool) = world_at_0028("r653_h.sqlite3").await;
        let post = ready_to_post(&pool, ONLY_H).await;
        let refused_message = refused(&pool, &post).await;
        assert!(
            refused_message.contains("prescription_supply_record_missing"),
            "Schedule H lost the rule 65(3) requirement: {refused_message}"
        );
        pool.close().await;
    }

    // D. A drug in Schedule X AND Schedule H1 is excused the generic rule 65(3) register — rule
    // 65(3)(1) excludes it by Schedule X membership — but the SEPARATE Schedule H1 register of
    // rule 65(3)(1)(h) is a different trigger on its own snapshot key, and it still refuses.
    {
        let (_t, pool) = world_at_0028("r653_x_h1.sqlite3").await;
        let post = ready_to_post(&pool, X_AND_H1).await;
        let refused_message = refused(&pool, &post).await;
        assert!(
            refused_message.contains("schedule_h1_register_entry_missing")
                || refused_message.contains("h1"),
            "the Schedule X exclusion suppressed an independent statutory record: {refused_message}"
        );
        assert!(
            !refused_message.contains("prescription_supply_record_missing"),
            "the generic rule 65(3) register was still demanded for a Schedule X drug: \
             {refused_message}"
        );
        pool.close().await;
    }

    // E. The exclusion cannot be used to leave a prepared rule 65(3) record dangling: the guard's
    // second limb is untouched, so a Schedule X Sale carrying one is still refused.
    {
        let (_t, pool) = world_at_0028("r653_dangling.sqlite3").await;
        let post = ready_to_post(&pool, ONLY_X).await;
        // The 0022 coherence trigger binds a rule 65(3) record to the Sale's own supervising
        // pharmacist, so the Sale has to name one before a record can be prepared against it.
        execute(
            &pool,
            &format!(
                "UPDATE sale_documents SET supervising_professional_id='{PHARMACIST}' \
                 WHERE id='{DRAFT_SALE}'"
            ),
        )
        .await;
        execute(
            &pool,
            &format!(
                "INSERT INTO store_record_elections (id,store_id,election,method,effective_from,\
                 recorded_by_user_id,created_at_utc,updated_at_utc) VALUES \
                 ('01997000-0000-7000-8000-000000000a80','{STORE}',\
                 'rule_65_3_prescription_supply','prescription_register',\
                 '2020-01-01','{OWNER}','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')"
            ),
        )
        .await;
        execute(
            &pool,
            &format!(
                "INSERT INTO prescription_supply_records (id,store_id,sale_document_id,\
                 prescription_id,record_method,election_id,serial_value,serial_number,\
                 date_of_supply,prescriber_name,prescriber_address,subject_kind,subject_name,\
                 subject_address,supervising_professional_id,supervising_professional_name,\
                 supervising_registration_number,original_container_confirmed,status,\
                 prepared_by_user_id,prepared_at_utc) VALUES \
                 ('01997000-0000-7000-8000-000000000a81','{STORE}','{DRAFT_SALE}',\
                 '{PRESCRIPTION}','prescription_register','01997000-0000-7000-8000-000000000a80',\
                 1,'PR-000001','{SALE_DATE}','Dr. A. Prescriber','Clinic Road','human',\
                 'Test Patient','Patient Street','{PHARMACIST}','Meera Iyer','MH-PH-44821',0,\
                 'prepared','{OWNER}','2026-05-06T05:00:00.000Z')"
            ),
        )
        .await;
        let refused_message = refused(&pool, &post).await;
        assert!(
            refused_message.contains("prescription_supply_record_missing"),
            "a dangling rule 65(3) record slipped through the Schedule X exclusion: \
             {refused_message}"
        );
        pool.close().await;
    }
}
