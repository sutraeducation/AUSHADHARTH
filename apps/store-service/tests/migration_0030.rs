//! Migration 0030 against a database that already holds the Phase 1M-A..D3-C1 records.
//!
//! 0030 adds no table, no column and no compliance fact. What it does is let an ALREADY CONFIRMED
//! Schedule X working entry take part in a Sale posting, which until now was impossible by
//! construction: `dispensing_id` was frozen against every update while finalization required it to be
//! non-NULL, so a supply entry could never be finalized at all. That was Phase 1M-D3-B's guarantee
//! that it could not enable a sale.
//!
//! It therefore does five things, all by DROP and CREATE because SQLite cannot alter a trigger:
//!
//!   * narrows the Schedule X limb of the three regulatory gate triggers;
//!   * lets `dispensing_id`, `bill_number` and `bill_date` go from NULL to a value EXACTLY ONCE, on
//!     `confirmed -> finalized`, and only on a supply entry;
//!   * makes a supply entry be born with all three of those NULL;
//!   * freezes `batch_id`, which 0028 had left mutable;
//!   * adds `sale_documents_schedule_x_register_required`, the database's own verdict on the whole
//!     supported chain.
//!
//! The outcomes that must never happen are: a historical entry becoming finalized by migrating; any
//! of the three bindings being written while prepared, while confirmed, or after finalization; a
//! finalized entry being changed or deleted; a lot being swapped; a Schedule X Sale posting without
//! the full chain; Schedule H, H1, C or C(1) losing any of their own protection; an unknown
//! classification becoming sellable; and the supplier-authority conflict rule disagreeing with the
//! service resolver.
//!
//! ```text
//! cargo test --test migration_0030
//! ```
//! ```

use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 30;

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
async fn populate_before_0030(pool: &SqlitePool) {
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
    populate_before_0030(pool).await;
    populate_dispensing_occasions(pool).await;
    execute(pool, &receipt_insert(RECEIPT, 1)).await;
    execute(pool, &confirm_receipt(RECEIPT)).await;
    execute(
        pool,
        &purchase_movement("01997000-0000-7000-8000-000000000c01", POSTED_LINE, 50),
    )
    .await;
}

async fn world_at_0030(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join(name)).await;
    run_migrations(&pool, &directory).await;
    (temp, pool)
}

/// What 0030 drops and recreates, and the one trigger it adds.
const RECREATED: [&str; 5] = [
    "sale_documents_regulatory_gate_insert",
    "sale_documents_regulatory_gate_update",
    "sale_lines_regulatory_gate_insert",
    "store_schedule_x_register_entries_coherent_insert",
    "store_schedule_x_register_entries_transition",
];
const ADDED: &str = "sale_documents_schedule_x_register_required";

const SUPPLY: &str = "01997000-0000-7000-8000-000000000901";

const DISPENSING: &str = "01997000-0000-7000-8000-000000000a01";

/// A dispensing for the seeded draft sale line, which is what the finalizing bindings point at.
fn dispensing_insert(id: &str) -> String {
    format!(
        "INSERT INTO prescription_dispensings (id,store_id,prescription_id,prescription_item_id,\
         sale_document_id,sale_line_id,product_id,quantity_atoms,dispensed_on,\
         supervising_professional_id,supervising_professional_name,\
         supervising_registration_number,endorsement_confirmed_by_user_id,\
         endorsement_confirmed_at_utc,created_at_utc) VALUES \
         ('{id}','{STORE}','{PRESCRIPTION}','{PRESCRIPTION_ITEM}','{DRAFT_SALE}','{DRAFT_LINE}',\
         '{PRODUCT_X}',10,'{SALE_DATE}','{PHARMACIST}','Meera Iyer','MH-PH-44821','{OWNER}',\
         '2026-05-06T06:00:00.000Z','2026-05-06T06:00:00.000Z')"
    )
}

/// Moves a prepared SUPPLY entry to `confirmed`: the particulars are written in the bound physical
/// register and the entry is signed by hand, attested by a date-valid registered pharmacist.
fn confirm_supply(id: &str) -> String {
    format!(
        "UPDATE store_schedule_x_register_entries SET status='confirmed',\
         particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
         supervising_professional_id='{PHARMACIST}',supervising_professional_name='Meera Iyer',\
         supervising_registration_number='MH-PH-44821',confirmed_by_user_id='{OWNER}',\
         confirmed_at_utc='2026-05-06T07:00:00.000Z',updated_at_utc='2026-05-06T07:00:00.000Z' \
         WHERE id='{id}'"
    )
}

/// A pharmacy at 0030 with a CONFIRMED Schedule X supply working entry on a qualified lot: the state
/// an operator is in once the page is written and signed and the only thing left is Post Sale.
async fn confirmed_supply_world(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let (temp, pool) = world_at_0030(name).await;
    qualified_world(&pool).await;
    execute(&pool, &supply_insert(SUPPLY, 2, BATCH_X)).await;
    execute(&pool, &confirm_supply(SUPPLY)).await;
    execute(&pool, &dispensing_insert(DISPENSING)).await;
    (temp, pool)
}

/// 0030-1, 0030-20. A database built from nothing reaches 0030, carrying the five recreated triggers
/// and the one new one — and there is no 0031.
#[tokio::test]
async fn a_fresh_database_migrates_to_the_new_version() {
    let (_temp, pool) = world_at_0030("fresh.sqlite3").await;
    let objects = schema_objects(&pool).await;
    let names: Vec<&str> = objects.iter().map(|(_, name, _)| name.as_str()).collect();
    for trigger in RECREATED {
        assert!(names.contains(&trigger), "{trigger} is missing");
    }
    assert!(names.contains(&ADDED), "{ADDED} is missing");

    // The Schedule X limb of the live gate is narrowed rather than absent, and every other scheme's
    // limb is still there.
    let gate = objects
        .iter()
        .find(|(_, name, _)| name == "sale_documents_regulatory_gate_update")
        .map(|(_, _, sql)| sql.clone())
        .expect("the gate trigger");
    for kept in ["schedule_c", "schedule_c1", "schedule_h", "schedule_h1"] {
        assert!(gate.contains(kept), "{kept} lost its limb: {gate}");
    }
    assert!(gate.contains("ndps_purview"), "{gate}");

    // Exactly 30 migrations, and nothing beyond.
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("applied");
    assert_eq!(applied, 30);
    let beyond = std::fs::read_dir(migrations_directory())
        .expect("migrations")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .split('_')
                .next()
                .and_then(|value| value.parse::<i64>().ok())
                .is_some_and(|version| version > 30)
        })
        .count();
    assert_eq!(beyond, 0, "a migration beyond 0030 exists");
    structural_checks(&pool).await;
    pool.close().await;
}

/// 0030-2, 0030-3. A populated pharmacy upgrades with every row intact, and NO historical Schedule X
/// entry is moved on by migrating — least of all to `finalized`.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_finalizes_nothing() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION - 1, &directory);
    let pool = open(&temp.path().join("populated.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    qualified_world(&pool).await;
    execute(&pool, &supply_insert(SUPPLY, 2, BATCH_X)).await;
    execute(&pool, &confirm_supply(SUPPLY)).await;

    // Everything as it stands at 0029.
    let before_tables = tables(&pool).await;
    let mut before_rows = Vec::new();
    for table in &before_tables {
        let columns = columns_of(&pool, table).await;
        before_rows.push((table.clone(), rows_of(&pool, table, &columns).await));
    }

    // Apply 0030 over it.
    migrations_up_to(NEW_MIGRATION, &directory);
    run_migrations(&pool, &directory).await;

    assert_eq!(
        tables(&pool).await,
        before_tables,
        "0030 added or removed a table"
    );
    for (table, rows) in &before_rows {
        let columns = columns_of(&pool, table).await;
        assert_eq!(
            &rows_of(&pool, table, &columns).await,
            rows,
            "rows of {table} changed"
        );
    }
    // Specifically: the confirmed entry is still exactly confirmed, with no binding written.
    let (status, dispensing, bill_number, bill_date, finalized): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status,dispensing_id,bill_number,bill_date,finalized_at_utc \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(SUPPLY)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(status, "confirmed");
    assert_eq!(dispensing, None);
    assert_eq!(bill_number, None);
    assert_eq!(bill_date, None);
    assert_eq!(finalized, None);
    structural_checks(&pool).await;
    pool.close().await;
}

/// 0030-4. A SUPPLY entry is born with all three posting bindings empty. A Sale that has not posted
/// has no bill number, and an entry claiming one in advance would record a particular that does not
/// exist yet. Receipt entries keep their own purchase bill, untouched.
#[tokio::test]
async fn a_supply_entry_is_born_with_no_posting_bindings() {
    let (_temp, pool) = world_at_0030("born.sqlite3").await;
    qualified_world(&pool).await;

    for (what, column, value) in [
        ("a dispensing", "dispensing_id", DISPENSING),
        ("a bill number", "bill_number", "INV/2627/000001"),
        ("a bill date", "bill_date", SALE_DATE),
    ] {
        // Written out in full rather than patched together, so the test says what it means.
        let explicit = format!(
            "INSERT INTO store_schedule_x_register_entries (id,store_id,entry_kind,reference_value,\
             reference,transaction_date,drug_name,product_id,batch_state,batch_number,batch_id,\
             manufacturer_state,manufacturer_name,quantity_atoms,supply_basis,sale_document_id,\
             sale_line_id,prescription_id,prescription_item_id,subject_kind,purchaser_name,\
             purchaser_address,prescription_reference,{column},status,prepared_by_user_id,\
             prepared_at_utc,particulars_entered_in_physical_register,\
             physical_entry_authenticated,created_at_utc,updated_at_utc) VALUES \
             ('{SUPPLY}','{STORE}','supply',2,'AXR-000002','{SALE_DATE}',\
             'Schedule X Test Medicine A','{PRODUCT_X}','recorded','BX-01','{BATCH_X}','recorded',\
             'Meridian Laboratories',10,'prescription','{DRAFT_SALE}','{DRAFT_LINE}',\
             '{PRESCRIPTION}','{PRESCRIPTION_ITEM}','human','Test Patient','Patient Street',\
             'RX-000001','{value}','prepared','{OWNER}','2026-05-06T06:00:00.000Z',0,0,\
             '2026-05-06T06:00:00.000Z','2026-05-06T06:00:00.000Z')"
        );
        let message = refused(&pool, &explicit).await;
        assert!(
            message.contains("schedule_x_register_entry_incoherent"),
            "{what} was accepted on a newborn supply entry: {message}"
        );
    }
    // And a receipt entry still carries the supplier's own invoice number and date.
    let (bill_number, bill_date): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT bill_number,bill_date FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(RECEIPT)
    .fetch_one(&pool)
    .await
    .expect("receipt");
    assert_eq!(bill_number.as_deref(), Some("INV-5501"));
    assert_eq!(bill_date.as_deref(), Some(INVOICE_DATE));
    pool.close().await;
}

/// 0030-5, 0030-6. While an entry is only `prepared`, none of the three bindings may be written, and
/// it cannot jump straight to `finalized` — the physical page has not been signed yet.
#[tokio::test]
async fn a_prepared_entry_binds_nothing_and_cannot_be_finalized() {
    let (_temp, pool) = world_at_0030("prepared.sqlite3").await;
    qualified_world(&pool).await;
    execute(&pool, &supply_insert(SUPPLY, 2, BATCH_X)).await;

    for (what, statement) in [
        (
            "a dispensing",
            format!(
                "UPDATE store_schedule_x_register_entries SET dispensing_id='{DISPENSING}' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "a bill number",
            format!(
                "UPDATE store_schedule_x_register_entries SET bill_number='INV/2627/000001' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "a bill date",
            format!(
                "UPDATE store_schedule_x_register_entries SET bill_date='{SALE_DATE}' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "finalization straight from prepared",
            format!(
                "UPDATE store_schedule_x_register_entries SET status='finalized',\
                 finalized_at_utc='2026-05-06T08:00:00.000Z',finalized_by_user_id='{OWNER}' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
    ] {
        let message = refused(&pool, &statement).await;
        assert!(
            message.contains("schedule_x_register_entry_immutable"),
            "{what} was accepted while prepared: {message}"
        );
    }
    pool.close().await;
}

/// 0030-7. A `confirmed` entry still binds nothing while it stays confirmed. The bindings belong to
/// finalization and to nothing else, so they cannot be slipped in by an update that leaves the status
/// alone.
#[tokio::test]
async fn a_confirmed_entry_binds_nothing_without_finalizing() {
    let (_temp, pool) = confirmed_supply_world("confirmed.sqlite3").await;
    for (what, statement) in [
        (
            "a dispensing",
            format!(
                "UPDATE store_schedule_x_register_entries SET dispensing_id='{DISPENSING}' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "a bill number",
            format!(
                "UPDATE store_schedule_x_register_entries SET bill_number='INV/2627/000001' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "a bill date",
            format!(
                "UPDATE store_schedule_x_register_entries SET bill_date='{SALE_DATE}' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
    ] {
        let message = refused(&pool, &statement).await;
        assert!(
            message.contains("schedule_x_register_entry_immutable"),
            "{what} was accepted on a confirmed entry: {message}"
        );
    }
    pool.close().await;
}

/// 0030-8, 0030-9. `confirmed -> finalized` permits EXACTLY the three approved NULL-to-value bindings,
/// together, and refuses the transition if any of them is left empty.
#[tokio::test]
async fn finalization_binds_exactly_the_three_particulars() {
    // Each of the three missing in turn is refused.
    for (what, set) in [
        (
            "no dispensing",
            format!("bill_number='INV/2627/000001',bill_date='{SALE_DATE}'"),
        ),
        (
            "no bill number",
            format!("dispensing_id='{DISPENSING}',bill_date='{SALE_DATE}'"),
        ),
        (
            "no bill date",
            format!("dispensing_id='{DISPENSING}',bill_number='INV/2627/000001'"),
        ),
    ] {
        let (_temp, pool) = confirmed_supply_world("partial.sqlite3").await;
        let message = refused(
            &pool,
            &format!(
                "UPDATE store_schedule_x_register_entries SET {set},status='finalized',\
                 finalized_at_utc='2026-05-06T08:00:00.000Z',finalized_by_user_id='{OWNER}' \
                 WHERE id='{SUPPLY}'"
            ),
        )
        .await;
        assert!(
            message.contains("schedule_x_register_entry_immutable"),
            "finalization with {what} was accepted: {message}"
        );
        pool.close().await;
    }

    // All three together, once, is permitted.
    let (_temp, pool) = confirmed_supply_world("finalize.sqlite3").await;
    execute(
        &pool,
        &format!(
            "UPDATE store_schedule_x_register_entries SET dispensing_id='{DISPENSING}',\
             bill_number='INV/2627/000001',bill_date='{SALE_DATE}',status='finalized',\
             finalized_at_utc='2026-05-06T08:00:00.000Z',finalized_by_user_id='{OWNER}' \
             WHERE id='{SUPPLY}'"
        ),
    )
    .await;
    let (status, dispensing, bill_number, bill_date): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status,dispensing_id,bill_number,bill_date \
         FROM store_schedule_x_register_entries WHERE id=?",
    )
    .bind(SUPPLY)
    .fetch_one(&pool)
    .await
    .expect("entry");
    assert_eq!(status, "finalized");
    assert_eq!(dispensing.as_deref(), Some(DISPENSING));
    assert_eq!(bill_number.as_deref(), Some("INV/2627/000001"));
    assert_eq!(bill_date.as_deref(), Some(SALE_DATE));

    // 0030-10, 0030-11. And from then on the record is beyond reach: not rewritten, not re-pointed,
    // not deleted.
    for (what, statement) in [
        (
            "the bill number was rewritten",
            format!(
                "UPDATE store_schedule_x_register_entries SET bill_number='INV/2627/009999' \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "the dispensing was re-pointed",
            format!(
                "UPDATE store_schedule_x_register_entries SET dispensing_id=NULL \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "a statutory particular was altered",
            format!(
                "UPDATE store_schedule_x_register_entries SET quantity_atoms=quantity_atoms+1 \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "the attestation was withdrawn",
            format!(
                "UPDATE store_schedule_x_register_entries \
                 SET physical_entry_authenticated=0,particulars_entered_in_physical_register=0 \
                 WHERE id='{SUPPLY}'"
            ),
        ),
        (
            "the entry was deleted",
            format!("DELETE FROM store_schedule_x_register_entries WHERE id='{SUPPLY}'"),
        ),
    ] {
        let message = refused(&pool, &statement).await;
        assert!(
            message.contains("schedule_x_register_entry_immutable"),
            "{what} after finalization: {message}"
        );
    }
    pool.close().await;
}

/// 0030-12. The lot is frozen. 0028 left `batch_id` mutable, which would have let the register say a
/// supply came out of a lot it did not come out of.
#[tokio::test]
async fn the_lot_of_an_entry_can_never_change() {
    let (_temp, pool) = confirmed_supply_world("lot.sqlite3").await;
    for value in ["NULL", "'01997000-0000-7000-8000-0000000000e6'"] {
        let message = refused(
            &pool,
            &format!(
                "UPDATE store_schedule_x_register_entries SET batch_id={value} \
                 WHERE id='{SUPPLY}'"
            ),
        )
        .await;
        assert!(
            message.contains("schedule_x_register_entry_immutable"),
            "the lot was changed to {value}: {message}"
        );
    }
    pool.close().await;
}

/// 0030-13, 0030-14, 0030-15. Every other scheme keeps exactly the protection it had. Schedule H and
/// H1 still need their dispensing; Schedule C and C(1) are still refused outright; and a medicine
/// whose position nobody has recorded is still not sellable.
#[tokio::test]
async fn the_other_schemes_keep_their_own_protection() {
    let (_temp, pool) = world_at_0030("others.sqlite3").await;
    let objects = schema_objects(&pool).await;
    let gate = objects
        .iter()
        .find(|(_, name, _)| name == "sale_documents_regulatory_gate_update")
        .map(|(_, _, sql)| sql.clone())
        .expect("gate");

    // Schedule C and C(1): an unconditional limb, with no narrowing of any kind.
    for scheme in ["schedule_c", "schedule_c1"] {
        assert!(
            gate.contains(&format!("'$.{scheme}') = 'applies'")),
            "{scheme} lost its unconditional limb: {gate}"
        );
    }
    // Schedule H and H1: still conditioned on their dispensing existing.
    assert!(
        gate.contains("'$.schedule_h') = 'applies'")
            && gate.contains("'$.schedule_h1') = 'applies'")
            && gate.contains("prescription_dispensings"),
        "the Schedule H/H1 limb changed: {gate}"
    );
    // An unknown position for a medicine, in every one of the five schemes.
    for scheme in [
        "schedule_h",
        "schedule_h1",
        "schedule_x",
        "schedule_c",
        "schedule_c1",
    ] {
        assert!(
            gate.contains(&format!("'$.{scheme}') = 'unknown'")),
            "{scheme} lost its unknown-position limb: {gate}"
        );
    }
    // The Schedule H1 register requirement and the rule 65(3) requirement are untouched by 0030.
    let names: Vec<&str> = objects.iter().map(|(_, name, _)| name.as_str()).collect();
    for kept in [
        "sale_documents_h1_register_required",
        "sale_documents_prescription_record_required",
        "store_schedule_x_register_entries_supply_provenance",
    ] {
        assert!(names.contains(&kept), "{kept} was lost");
    }
    pool.close().await;
}

/// 0030-16, 0030-18, 0030-19. The database's own verdict: a Schedule X Sale cannot reach `posted`
/// without the whole chain, and the NDPS axis must be established NOT to apply — an unrecorded axis is
/// not a cleared one.
#[tokio::test]
async fn a_schedule_x_sale_cannot_post_without_the_whole_chain() {
    let (_temp, pool) = confirmed_supply_world("posting.sqlite3").await;
    // The entry is confirmed but not finalized, so the chain is incomplete however favourable
    // everything else is.
    let message = refused(
        &pool,
        &format!("UPDATE sale_documents SET status='posted' WHERE id='{DRAFT_SALE}'"),
    )
    .await;
    assert!(
        message.contains("schedule_x_register_entry_missing")
            || message.contains("regulatory_gate_refuses_posting"),
        "a Schedule X Sale posted with an unfinalized entry: {message}"
    );

    // And the new trigger asks the NDPS question of the frozen snapshot, both ways.
    let required = schema_objects(&pool)
        .await
        .iter()
        .find(|(_, name, _)| name == ADDED)
        .map(|(_, _, sql)| sql.clone())
        .expect("the register-required trigger");
    assert!(
        required.contains("'$.ndps_purview'") && required.contains("IS NOT 'does_not_apply'"),
        "the NDPS axis is not asked at posting: {required}"
    );
    // A straggler left prepared or confirmed also refuses, so paper and software cannot disagree.
    assert!(
        required.contains("IN ('prepared', 'confirmed')"),
        "a straggler entry does not refuse: {required}"
    );
    pool.close().await;
}

/// 0030-17. The supplier-authority conflict rule in the trigger matches
/// `domain::schedule_x::resolve_supplier_schedule_x_authority`: every ACTIVE authority covering the
/// day is counted REGARDLESS of its recorded status, and more than one is a conflict rather than a
/// choice. Counting only the authorities that already pass the later tests would make the database
/// more permissive than the service, which is the one direction that must never happen.
#[tokio::test]
async fn the_authority_conflict_rule_matches_the_service() {
    let (_temp, pool) = world_at_0030("authority.sqlite3").await;
    let required = schema_objects(&pool)
        .await
        .iter()
        .find(|(_, name, _)| name == ADDED)
        .map(|(_, _, sql)| sql.clone())
        .expect("the register-required trigger");

    // The counting condition is on active authorities covering the invoice date, with no status or
    // basis test inside it, and requires exactly one.
    let counting = required
        .split("SELECT COUNT(*) FROM supplier_schedule_x_authorities")
        .nth(1)
        .expect("the counting subquery")
        .split(") <> 1")
        .next()
        .expect("the counting subquery's end")
        .to_owned();
    assert!(
        counting.contains("status = 'active'")
            && counting.contains("effective_from <= pdoc.invoice_date")
            && counting.contains("effective_to > pdoc.invoice_date"),
        "the conflict count is not period-scoped: {counting}"
    );
    assert!(
        !counting.contains("legal_status") && !counting.contains("validity_basis"),
        "the conflict count filters on status or basis, so two overlapping records could pass: \
         {counting}"
    );
    // And a separate condition requires that single authority to be in force, with a known basis and
    // exactly one coverage row.
    assert!(
        required.contains("legal_status = 'in_force'")
            && required.contains("validity_basis IN ('fixed_term', 'perpetual')")
            && required.contains("supplier_schedule_x_authority_coverage"),
        "the authority's own tests are missing: {required}"
    );
    // The pharmacist is revalidated at posting, not merely named.
    assert!(
        required.contains("capacity = 'registered_pharmacist'")
            && required.contains("registration_number IS NOT NULL"),
        "the supervising pharmacist is not revalidated at posting: {required}"
    );
    pool.close().await;
}

/// 0030 rebuilt no audit table, and did not need to: `schedule_x_register_entry` was already an
/// enumerated entity type and `posted` an enumerated action, so the finalization audit has somewhere
/// to go without touching `master_change_events` at all.
#[tokio::test]
async fn the_audit_log_was_not_rebuilt_and_did_not_need_to_be() {
    let (_temp, pool) = world_at_0030("audit.sqlite3").await;
    let columns = columns_of(&pool, "master_change_events").await;
    assert_eq!(columns.len(), 11, "{columns:?}");
    let sql = schema_objects(&pool)
        .await
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .map(|(_, _, sql)| sql.clone())
        .expect("the audit table");
    assert!(sql.contains("'schedule_x_register_entry'"), "{sql}");
    assert!(sql.contains("'posted'"), "{sql}");
    // 0030 does not mention the audit table at all.
    let migration =
        std::fs::read_to_string(migrations_directory().join("0030_schedule_x_sale_enablement.sql"))
            .expect("0030");
    assert!(
        !migration.contains("master_change_events"),
        "0030 touched the audit log"
    );
    pool.close().await;
}

/// Every earlier migration is untouched, and 0030 is the only new file.
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
    assert_eq!(names[29], "0030_schedule_x_sale_enablement.sql");
    for name in &names[..29] {
        let bytes = std::fs::read(directory.join(name)).expect("read migration");
        assert!(!bytes.is_empty(), "{name} is empty");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(ADDED),
            "{name} was edited to mention the D3-C2 posting guard"
        );
    }
}
