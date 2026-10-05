//! Migration 0029 against a database that already holds the Phase 1M-A..D3-B records.
//!
//! 0029 adds the fact Phase 1M-D3-B proved missing: documentary evidence, recorded by the owner,
//! that a Schedule X purchase source held the authority the sale licence Forms require it to have.
//! Rule 61(3) names Form 20-G for a dealer supplying Schedule X by wholesale; rule 70 names
//! Form 25-F for manufacture of drugs included in Schedule X. Those are the two this software can
//! represent, and nothing else is accepted.
//!
//! It is NOT verification. Every row records what an operator read off a document and when they
//! recorded it, which is why the moment of recording is kept apart from the period the document
//! asserts, and why no column is called verified or valid.
//!
//! The outcomes that must never happen are: a frozen Phase 1M-D1-A licence string becoming
//! authority; one supplier's authority satisfying another; another store's authority satisfying
//! this one; a product's manufacturer role standing in for a supplier's manufacturing licence;
//! authority evaluated against today instead of the purchase date; an unknown status or basis
//! establishing anything; overlapping records being settled by whichever was inserted last; and
//! compliance evidence being deleted.
//!
//! ```text
//! cargo test --test migration_0029
//! ```
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 29;

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
const INVOICE_DATE: &str = "2026-05-04";

/// owner recorded inside Schedule X and one ordinary drug with no finding at all, a registered
/// pharmacist, and an audit log with events already in it.
async fn populate_before_0029(pool: &SqlitePool) {
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

const SALE_DATE: &str = "2026-05-06";
const SUPPLIER: &str = "01997000-0000-7000-8000-000000000602";
const OTHER_SUPPLIER: &str = "01997000-0000-7000-8000-000000000612";
const AUTHORITY: &str = "01997000-0000-7000-8000-000000000a01";
const COVERAGE: &str = "01997000-0000-7000-8000-000000000a02";

/// What 0029 adds.
const NEW_TABLES: [&str; 2] = [
    "supplier_schedule_x_authorities",
    "supplier_schedule_x_authority_coverage",
];

/// A second supplier Party, so "same licence number, different supplier" can be tested.
async fn second_supplier(pool: &SqlitePool) {
    execute(
        pool,
        &format!(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             created_at_utc,updated_at_utc) VALUES ('{OTHER_SUPPLIER}','Rival Distributors',\
             'rival distributors','unregistered','2026-01-01T00:00:00.000Z',\
             '2026-01-01T00:00:00.000Z')"
        ),
    )
    .await;
    execute(
        pool,
        &format!(
            "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) VALUES \
             ('01997000-0000-7000-8000-000000000613','{OTHER_SUPPLIER}','supplier',\
             '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')"
        ),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
fn authority_insert(
    id: &str,
    supplier: &str,
    kind: &str,
    number: &str,
    legal_status: &str,
    validity_basis: &str,
    from: &str,
    to: Option<&str>,
    store: &str,
) -> String {
    let end = match to {
        Some(value) => format!("'{value}'"),
        None => "NULL".to_owned(),
    };
    let normalized = number
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_uppercase();
    format!(
        "INSERT INTO supplier_schedule_x_authorities (id,store_id,supplier_party_id,\
         authority_kind,authority_number,normalized_authority_number,issuing_authority,\
         legal_status,validity_basis,effective_from,effective_to,source_citation,\
         recorded_by_user_id,recorded_at_utc,created_at_utc,updated_at_utc) VALUES \
         ('{id}','{store}','{supplier}','{kind}','{number}','{normalized}',\
         'State Drugs Control Administration','{legal_status}','{validity_basis}','{from}',{end},\
         'Licence copy inspected at the counter','{OWNER}','2026-05-10T06:00:00.000Z',\
         '2026-05-10T06:00:00.000Z','2026-05-10T06:00:00.000Z')"
    )
}

/// The ordinary qualifying authority: a Form 20-G wholesale licence, in force, perpetual, from 2020.
fn authority_ok(id: &str) -> String {
    authority_insert(
        id,
        SUPPLIER,
        "form_20g",
        "20G-MH-5511",
        "in_force",
        "perpetual",
        "2020-01-01",
        None,
        STORE,
    )
}

fn coverage_insert(
    id: &str,
    authority: &str,
    product: &str,
    from: &str,
    to: Option<&str>,
) -> String {
    let end = match to {
        Some(value) => format!("'{value}'"),
        None => "NULL".to_owned(),
    };
    format!(
        "INSERT INTO supplier_schedule_x_authority_coverage (id,store_id,authority_id,product_id,\
         effective_from,effective_to,source_citation,recorded_by_user_id,recorded_at_utc,\
         created_at_utc,updated_at_utc) VALUES ('{id}','{STORE}','{authority}','{product}',\
         '{from}',{end},'Drug endorsed on the licence copy','{OWNER}','2026-05-10T06:00:00.000Z',\
         '2026-05-10T06:00:00.000Z','2026-05-10T06:00:00.000Z')"
    )
}

async fn world_at_0029(name: &str) -> (tempfile::TempDir, SqlitePool) {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join(name)).await;
    run_migrations(&pool, &directory).await;
    populate_before_0029(&pool).await;
    second_supplier(&pool).await;
    (temp, pool)
}

/// The resolver, as the service implements it, expressed in SQL so the matrix can be asserted from
/// committed state. Mirrors `domain::schedule_x::resolve_supplier_schedule_x_authority`: an ACTIVE
/// authority of this store for this supplier whose half-open period covers the date, recorded
/// in force on a known validity basis, with ACTIVE coverage naming the product on that date.
async fn resolves(pool: &SqlitePool, supplier: &str, date: &str, product: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM supplier_schedule_x_authorities authority \
         JOIN supplier_schedule_x_authority_coverage coverage \
           ON coverage.authority_id = authority.id AND coverage.store_id = authority.store_id \
         WHERE authority.store_id = '{STORE}' AND authority.supplier_party_id = '{supplier}' \
           AND authority.status = 'active' AND authority.legal_status = 'in_force' \
           AND authority.validity_basis <> 'unknown' \
           AND authority.effective_from <= '{date}' \
           AND (authority.effective_to IS NULL OR authority.effective_to > '{date}') \
           AND coverage.status = 'active' AND coverage.product_id = '{product}' \
           AND coverage.effective_from <= '{date}' \
           AND (coverage.effective_to IS NULL OR coverage.effective_to > '{date}')"
    ))
    .fetch_one(pool)
    .await
    .expect("resolve")
}

/// Phase 1M-D3-C1, item 37. A database built from nothing reaches 0029, with both authority tables,
/// their guards, and no claim anywhere that a licence has been verified.
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

    let objects = schema_objects(&pool).await;
    for expected in NEW_TABLES {
        assert!(
            objects
                .iter()
                .any(|(kind, name, _)| kind == "table" && name == expected),
            "{expected} is missing"
        );
    }
    // The legal basis is an enumeration, and only the two forms the Rules name for a Schedule X
    // source. Nothing here can claim Form 20-F or Form 28-B authority.
    let authority_sql = objects
        .iter()
        .find(|(_, name, _)| name == "supplier_schedule_x_authorities")
        .expect("authority table")
        .2
        .clone();
    assert!(authority_sql.contains("'form_20g'"), "{authority_sql}");
    assert!(authority_sql.contains("'form_25f'"), "{authority_sql}");
    assert!(
        !authority_sql.contains("'form_20f'"),
        "Form 20-F is a RETAIL licence and is not a source authority"
    );
    assert!(
        !authority_sql.contains("'form_28b'"),
        "Form 28-B is unsupported and must fail closed"
    );
    // No column claims verification.
    let columns = columns_of(&pool, "supplier_schedule_x_authorities").await;
    for forbidden in [
        "verified",
        "government_verified",
        "valid_licence",
        "verified_at_utc",
    ] {
        assert!(
            !columns.iter().any(|column| column == forbidden),
            "{forbidden} claims verification this software cannot do"
        );
    }
    // The moment of recording is kept apart from the period the document asserts.
    assert!(columns.contains(&"recorded_at_utc".to_owned()));
    assert!(columns.contains(&"effective_from".to_owned()));
    assert!(columns.contains(&"source_citation".to_owned()));
    // The audit log keeps every column it had.
    assert_eq!(columns_of(&pool, "master_change_events").await.len(), 11);
    pool.close().await;
}

/// Phase 1M-D3-C1, item 13 and item 37. 0029 arrives at a populated pharmacy, takes nothing away,
/// and — above all — turns no frozen D1-A licence string into an authority record.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_backfills_no_authority() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate_before_0029(&pool).await;
    structural_checks(&pool).await;

    // The seeded Purchase carries a frozen D1-A supplier licence string. That is the record-keeping
    // duty of rule 65(4)(4)(i) discharged, and it is exactly what must NOT become authority.
    let frozen: String = sqlx::query_scalar(&format!(
        "SELECT supplier_drug_licence_number FROM purchase_documents WHERE id='{POSTED}'"
    ))
    .fetch_one(&pool)
    .await
    .expect("frozen licence");
    assert!(
        !frozen.trim().is_empty(),
        "the fixture lost its frozen licence string"
    );

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

    // Only the audit log is redefined, and only to admit the two new entity types.
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
        vec!["master_change_events".to_owned()],
        "0029 redefined something it had no business touching"
    );
    for (table, columns, rows) in &before_rows {
        assert_eq!(
            rows,
            &rows_of(&pool, table, columns).await,
            "{table} rows changed"
        );
    }

    // NO BACKFILL. The frozen licence string did not become authority evidence.
    let authorities: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM supplier_schedule_x_authorities")
            .fetch_one(&pool)
            .await
            .expect("authorities");
    assert_eq!(authorities, 0, "the migration invented supplier authority");
    let coverage: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM supplier_schedule_x_authority_coverage")
            .fetch_one(&pool)
            .await
            .expect("coverage");
    assert_eq!(coverage, 0);
    pool.close().await;
}

/// Phase 1M-D3-C1, item 33 — THE ADVERSARIAL RESOLUTION MATRIX.
///
/// Every way of not having authority resolves to nothing, and only the exact supplier, store, date
/// and drug resolve to one. Evaluated on the HISTORICAL purchase date throughout.
#[tokio::test]
async fn only_the_exact_supplier_store_date_and_drug_resolve_to_authority() {
    let (_t, pool) = world_at_0029("matrix.sqlite3").await;

    // 1. No authority record at all.
    assert_eq!(
        resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        0,
        "case 1"
    );

    // 14. A frozen D1-A licence string, on its own, is not authority. (Nothing was recorded above,
    // and the seeded purchase carries that string, so case 1 already proves it; asserted again
    // here by name because it is the whole reason this phase exists.)
    assert_eq!(
        resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        0,
        "case 14: a frozen licence string established authority"
    );

    // 2. The matching supplier, date and drug.
    execute(&pool, &authority_ok(AUTHORITY)).await;
    execute(
        &pool,
        &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2020-01-01", None),
    )
    .await;
    assert_eq!(
        resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        1,
        "case 2"
    );

    // 3. A different supplier does not inherit it.
    assert_eq!(
        resolves(&pool, OTHER_SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        0,
        "case 3"
    );

    // 4. The same licence number recorded for a DIFFERENT supplier does not satisfy this one, and
    // vice versa: identity is the Party, never the printed number.
    execute(
        &pool,
        &authority_insert(
            "01997000-0000-7000-8000-000000000a11",
            OTHER_SUPPLIER,
            "form_20g",
            "20G-MH-5511",
            "in_force",
            "perpetual",
            "2020-01-01",
            None,
            STORE,
        ),
    )
    .await;
    assert_eq!(
        resolves(&pool, OTHER_SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        0,
        "case 4: a shared licence number carried coverage across suppliers"
    );

    // 8. The period covers the date but no coverage names this drug.
    assert_eq!(
        resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_PLAIN).await,
        0,
        "case 8: an uncovered drug resolved"
    );
    pool.close().await;
}

/// Phase 1M-D3-C1, item 33 cases 5, 6, 7 and 9. Store isolation, the period boundaries, and an
/// archived record.
#[tokio::test]
async fn authority_is_bounded_by_store_period_and_lifecycle() {
    // 6. Starts after the purchase date.
    {
        let (_t, pool) = world_at_0029("starts_after.sqlite3").await;
        execute(
            &pool,
            &authority_insert(
                AUTHORITY,
                SUPPLIER,
                "form_20g",
                "20G-MH-5511",
                "in_force",
                "perpetual",
                "2026-06-01",
                None,
                STORE,
            ),
        )
        .await;
        execute(
            &pool,
            &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2026-06-01", None),
        )
        .await;
        assert_eq!(
            resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
            0,
            "case 6"
        );
        pool.close().await;
    }

    // 7. Ends before the purchase date. Half-open: `effective_to` is exclusive, so a period ending
    // ON the purchase date does not cover it either.
    {
        let (_t, pool) = world_at_0029("ends_before.sqlite3").await;
        execute(
            &pool,
            &authority_insert(
                AUTHORITY,
                SUPPLIER,
                "form_20g",
                "20G-MH-5511",
                "in_force",
                "fixed_term",
                "2020-01-01",
                Some(INVOICE_DATE),
                STORE,
            ),
        )
        .await;
        execute(
            &pool,
            &coverage_insert(
                COVERAGE,
                AUTHORITY,
                PRODUCT_X,
                "2020-01-01",
                Some(INVOICE_DATE),
            ),
        )
        .await;
        assert_eq!(
            resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
            0,
            "case 7: a half-open period covered its own exclusive end"
        );
        pool.close().await;
    }

    // 9. An archived record no longer resolves. Archiving is how a record entered in error is
    // withdrawn; it does not delete the evidence, which is still on the row for an inspection.
    {
        let (_t, pool) = world_at_0029("archived.sqlite3").await;
        execute(&pool, &authority_ok(AUTHORITY)).await;
        execute(
            &pool,
            &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2020-01-01", None),
        )
        .await;
        assert_eq!(resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await, 1);
        execute(
            &pool,
            &format!(
                "UPDATE supplier_schedule_x_authorities SET status='archived',revision=2,\
                 archived_at_utc='2026-06-01T06:00:00.000Z',archive_reason='entered in error',\
                 updated_at_utc='2026-06-01T06:00:00.000Z' WHERE id='{AUTHORITY}'"
            ),
        )
        .await;
        assert_eq!(
            resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
            0,
            "case 9"
        );
        // And the row survives, with its reason, because deleting compliance evidence is refused.
        let kept: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM supplier_schedule_x_authorities WHERE id='{AUTHORITY}'"
        ))
        .fetch_one(&pool)
        .await
        .expect("kept");
        assert_eq!(kept, 1);
        let refused = refused(
            &pool,
            &format!("DELETE FROM supplier_schedule_x_authorities WHERE id='{AUTHORITY}'"),
        )
        .await;
        assert!(
            refused.contains("supplier_schedule_x_authority_no_delete"),
            "{refused}"
        );
        pool.close().await;
    }

    // 5 and 16. Cross-store leakage, proved impossible by three independent things rather than by
    // one assertion.
    {
        let (_t, pool) = world_at_0029("other_store.sqlite3").await;

        // (a) This installation serves ONE store, enforced by the database itself. A second store
        // cannot be created at all, which is a stronger guarantee than any authority check.
        let second_store = refused(
            &pool,
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES ('01997000-0000-7000-8000-0000000000be','Second Pharmacy','Asia/Kolkata',\
             '2026-01-01T00:00:00.000Z')",
        )
        .await;
        assert!(
            second_store.contains("store_identity_allows_exactly_one_store"),
            "{second_store}"
        );

        // (b) An authority naming a store that does not exist is refused on the foreign key, so no
        // row can carry a foreign store at all.
        let foreign = refused(
            &pool,
            &authority_insert(
                AUTHORITY,
                SUPPLIER,
                "form_20g",
                "20G-MH-5511",
                "in_force",
                "perpetual",
                "2020-01-01",
                None,
                "01997000-0000-7000-8000-0000000000be",
            ),
        )
        .await;
        assert!(foreign.contains("FOREIGN KEY"), "{foreign}");

        // (c) And coverage must belong to the same store as the authority it is written on, so even
        // if two stores could exist, one store's coverage could not hang on the other's authority.
        let guard: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master \
             WHERE name='supplier_schedule_x_authority_coverage_coherent_insert'",
        )
        .fetch_one(&pool)
        .await
        .expect("coverage guard");
        assert!(
            guard.contains("authority.store_id = NEW.store_id"),
            "the coverage guard does not bind the stores: {guard}"
        );

        // Nothing resolved, because nothing was recorded for this store.
        assert_eq!(
            resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
            0,
            "case 5"
        );
        pool.close().await;
    }
}

/// Phase 1M-D3-C1, items 21, 22 and 33 case 10. An unknown status or basis never establishes, a
/// suspension is date-effective rather than retrospective, and an ambiguity is refused at the door.
#[tokio::test]
async fn unknown_suspended_and_conflicting_records_never_establish() {
    // Unknown legal status, and unknown validity basis, each establish nothing. A nullable
    // "probably fine" is exactly the ambiguity a fail-closed model must not inherit.
    for (index, (what, status, basis, end)) in [
        ("an unknown status", "unknown", "perpetual", None),
        ("a suspended period", "suspended", "perpetual", None),
        ("a cancelled period", "cancelled", "perpetual", None),
        ("an unknown validity basis", "in_force", "unknown", None),
    ]
    .into_iter()
    .enumerate()
    {
        let (_t, pool) = world_at_0029(&format!("status_{index}.sqlite3")).await;
        execute(
            &pool,
            &authority_insert(
                AUTHORITY,
                SUPPLIER,
                "form_20g",
                "20G-MH-5511",
                status,
                basis,
                "2020-01-01",
                end,
                STORE,
            ),
        )
        .await;
        execute(
            &pool,
            &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2020-01-01", None),
        )
        .await;
        assert_eq!(
            resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
            0,
            "{what} established authority"
        );
        pool.close().await;
    }

    // A cessation recorded from a later date does NOT rewrite an earlier purchase: the period that
    // covered the purchase is closed at the cessation date, and a purchase before it still resolves.
    {
        let (_t, pool) = world_at_0029("cessation.sqlite3").await;
        execute(
            &pool,
            &authority_insert(
                AUTHORITY,
                SUPPLIER,
                "form_20g",
                "20G-MH-5511",
                "in_force",
                "fixed_term",
                "2020-01-01",
                Some("2026-05-05"),
                STORE,
            ),
        )
        .await;
        execute(
            &pool,
            &coverage_insert(
                COVERAGE,
                AUTHORITY,
                PRODUCT_X,
                "2020-01-01",
                Some("2026-05-05"),
            ),
        )
        .await;
        // The seeded purchase is 2026-05-04, inside the closed period: still authorised.
        assert_eq!(resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await, 1);
        // A purchase on the cessation date or later is not.
        assert_eq!(resolves(&pool, SUPPLIER, "2026-05-05", PRODUCT_X).await, 0);
        assert_eq!(resolves(&pool, SUPPLIER, SALE_DATE, PRODUCT_X).await, 0);
        pool.close().await;
    }

    // 10. Overlapping active records for the same supplier and kind are an ambiguity the resolver
    // must never settle by picking one, so they cannot be created at all.
    {
        let (_t, pool) = world_at_0029("conflict.sqlite3").await;
        execute(&pool, &authority_ok(AUTHORITY)).await;
        let overlap = refused(
            &pool,
            &authority_insert(
                "01997000-0000-7000-8000-000000000a21",
                SUPPLIER,
                "form_20g",
                "20G-MH-9999",
                "in_force",
                "perpetual",
                "2024-01-01",
                None,
                STORE,
            ),
        )
        .await;
        assert!(
            overlap.contains("supplier_schedule_x_authority_period_overlaps"),
            "{overlap}"
        );
        // And overlapping coverage of the same drug on one authority is refused for the same reason.
        execute(
            &pool,
            &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2020-01-01", None),
        )
        .await;
        let coverage_overlap = refused(
            &pool,
            &coverage_insert(
                "01997000-0000-7000-8000-000000000a22",
                AUTHORITY,
                PRODUCT_X,
                "2025-01-01",
                None,
            ),
        )
        .await;
        assert!(
            coverage_overlap.contains("supplier_schedule_x_coverage_period_overlaps"),
            "{coverage_overlap}"
        );
        // A DIFFERENT kind for the same supplier is not an overlap: a source may hold both a
        // wholesale licence and a manufacturing licence.
        execute(
            &pool,
            &authority_insert(
                "01997000-0000-7000-8000-000000000a23",
                SUPPLIER,
                "form_25f",
                "25F-MH-3301",
                "in_force",
                "perpetual",
                "2020-01-01",
                None,
                STORE,
            ),
        )
        .await;
        pool.close().await;
    }
}

/// Phase 1M-D3-C1, items 11, 12, 17, 18 and 30. The authority kind is an enumeration of the forms
/// the Rules actually name, identity is frozen, and nothing about a product's manufacturer role
/// establishes that a supplier is a licensed manufacturer.
#[tokio::test]
async fn the_authority_kind_is_legally_bounded_and_identity_is_frozen() {
    let (_t, pool) = world_at_0029("kinds.sqlite3").await;

    // 11. A manufacturer source with its own Form 25-F authority resolves.
    execute(
        &pool,
        &authority_insert(
            AUTHORITY,
            SUPPLIER,
            "form_25f",
            "25F-MH-3301",
            "in_force",
            "perpetual",
            "2020-01-01",
            None,
            STORE,
        ),
    )
    .await;
    execute(
        &pool,
        &coverage_insert(COVERAGE, AUTHORITY, PRODUCT_X, "2020-01-01", None),
    )
    .await;
    assert_eq!(
        resolves(&pool, SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        1,
        "case 11"
    );

    // 13 and 18. An unsupported form cannot be recorded at all: Form 20-F is a RETAIL licence, and
    // Form 28-B belongs to Schedule C/C(1) manufacture, which this software refuses outright. An
    // ordinary non-Schedule-X wholesale licence has no representation here either.
    for kind in ["form_20f", "form_28b", "form_20b", "form_21b", "wholesale"] {
        let message = refused(
            &pool,
            &authority_insert(
                "01997000-0000-7000-8000-000000000a31",
                OTHER_SUPPLIER,
                kind,
                "X-1",
                "in_force",
                "perpetual",
                "2020-01-01",
                None,
                STORE,
            ),
        )
        .await;
        assert!(
            !message.is_empty(),
            "{kind} was accepted as a source authority"
        );
    }

    // 12. PRODUCT_X has a manufacturer company recorded against it by the fixture. That says
    // nothing about whether the SUPPLIER is a licensed manufacturer, and no authority follows from
    // it: OTHER_SUPPLIER has no record and resolves to nothing.
    let manufacturer_rows: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM product_company_roles WHERE product_id='{PRODUCT_X}' \
         AND role='manufacturer'"
    ))
    .fetch_one(&pool)
    .await
    .expect("manufacturer role");
    assert!(
        manufacturer_rows >= 1,
        "the fixture lost its manufacturer role"
    );
    assert_eq!(
        resolves(&pool, OTHER_SUPPLIER, INVOICE_DATE, PRODUCT_X).await,
        0,
        "case 12: a product's manufacturer role established supplier authority"
    );

    // 30. The identity of the evidence never moves.
    for statement in [
        format!(
            "UPDATE supplier_schedule_x_authorities SET supplier_party_id='{OTHER_SUPPLIER}' WHERE id='{AUTHORITY}'"
        ),
        format!(
            "UPDATE supplier_schedule_x_authorities SET authority_kind='form_20g' WHERE id='{AUTHORITY}'"
        ),
        format!(
            "UPDATE supplier_schedule_x_authorities SET recorded_at_utc='2020-01-01T00:00:00.000Z' WHERE id='{AUTHORITY}'"
        ),
        format!(
            "UPDATE supplier_schedule_x_authorities SET store_id='01997000-0000-7000-8000-0000000000be' WHERE id='{AUTHORITY}'"
        ),
    ] {
        let message = refused(&pool, &statement).await;
        assert!(
            message.contains("supplier_schedule_x_authority_identity_frozen")
                || message.contains("FOREIGN KEY"),
            "{statement}: {message}"
        );
    }

    // 15. Retrospective recording keeps the later `recorded_at_utc`: the document speaks about 2020,
    // and the row shows AUSHADHARTH learned it in May 2026.
    let (effective_from, recorded_at): (String, String) = sqlx::query_as(&format!(
        "SELECT effective_from,recorded_at_utc FROM supplier_schedule_x_authorities \
         WHERE id='{AUTHORITY}'"
    ))
    .fetch_one(&pool)
    .await
    .expect("dates");
    assert_eq!(effective_from, "2020-01-01");
    assert!(
        recorded_at.starts_with("2026-05-10"),
        "the recording moment was backdated to the document's period: {recorded_at}"
    );
    assert!(
        &recorded_at[..10] > effective_from.as_str(),
        "a retrospective record must show it was learned later"
    );
    pool.close().await;
}

/// Phase 1M-D3-C1, item 37. Exactly one new migration, and every earlier file untouched.
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
    assert_eq!(names.len(), 29, "expected exactly 29 migrations: {names:?}");
    assert_eq!(names[28], "0029_supplier_schedule_x_authority.sql");
    for name in &names[..28] {
        let bytes = std::fs::read(directory.join(name)).expect("read migration");
        assert!(!bytes.is_empty(), "{name} is empty");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("supplier_schedule_x_authorities"),
            "{name} was edited to mention the D3-C1 authority model"
        );
    }
}
