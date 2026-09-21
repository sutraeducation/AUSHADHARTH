//! Migration 0021 against a database that already holds posted Sales, their lines and their stock.
//!
//! 0021 gives products an effective-dated schedule position, gives the store its professionals,
//! typed licence forms and rule 65 elections, and freezes a regulatory answer and a manufacturer on
//! every newly posted line. The outcomes that must never happen are an existing Sale line being
//! rewritten, or anything coming out of this migration claiming a schedule position, a manufacturer
//! or a licence form that nobody recorded. So this target builds a database at 0020 holding posted
//! medicine and general-item lines, applies 0021, and proves both — then proves the new guards
//! actually bite on the migrated database.
//!
//! ```text
//! cargo test --test migration_0021
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 21;

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

/// Every pre-existing column of a table, one comparable string per row, built from the column list
/// the earlier database actually has so the comparison cannot silently skip a column.
async fn rows_of(pool: &SqlitePool, table: &str, columns: &[String]) -> Vec<String> {
    let projection = columns
        .iter()
        .map(|column| format!("quote(\"{column}\")"))
        .collect::<Vec<_>>()
        .join("||'|'||");
    sqlx::query_scalar(&format!("SELECT {projection} FROM {table} ORDER BY id"))
        .fetch_all(pool)
        .await
        .expect("rows")
}

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const USER: &str = "01997000-0000-7000-8000-0000000000cc";
const TABLET: &str = "01997000-0000-7000-8000-000000000001";
const STRIP: &str = "01997000-0000-7000-8000-000000000004";
const MEDICINE: &str = "01997000-0000-7000-8000-0000000000d1";
const GENERAL: &str = "01997000-0000-7000-8000-0000000000d2";
const MEDICINE_PACK: &str = "01997000-0000-7000-8000-0000000000e1";
const GENERAL_PACK: &str = "01997000-0000-7000-8000-0000000000e2";
const MEDICINE_BATCH: &str = "01997000-0000-7000-8000-0000000000e5";
const GENERAL_BATCH: &str = "01997000-0000-7000-8000-0000000000e6";
const POSTED: &str = "01997000-0000-7000-8000-0000000000f1";
const OPEN_DRAFT: &str = "01997000-0000-7000-8000-0000000000f4";
const DOSAGE_FORM: &str = "01997000-0000-7000-8000-0000000000a1";

/// A pharmacy that traded before 0021: one posted Sale carrying a medicine line and a general-item
/// line, and a draft still open at the counter. Nothing about schedules was recordable then, and
/// nothing about schedules is true of these rows now.
async fn populate(pool: &SqlitePool) {
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
    ] {
        sqlx::query(statement)
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }

    for (product, kind, name, dosage_form) in [
        (
            MEDICINE,
            "medicine",
            "Dolo 650 mg Tablet",
            Some(DOSAGE_FORM),
        ),
        (GENERAL, "general_pharmacy_item", "Dental Floss", None),
    ] {
        sqlx::query(
            "INSERT INTO products (id,product_kind,dosage_form_id,base_unit_id,quantity_scale,\
             display_name,normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,0,?,?,'2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        )
        .bind(product)
        .bind(kind)
        .bind(dosage_form)
        .bind(TABLET)
        .bind(name)
        .bind(name.to_lowercase())
        .execute(pool)
        .await
        .expect("product");
    }
    for (pack, product, batch) in [
        (MEDICINE_PACK, MEDICINE, MEDICINE_BATCH),
        (GENERAL_PACK, GENERAL, GENERAL_BATCH),
    ] {
        sqlx::query(
            "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,\
             display_label,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,10,'Strip of 10','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        )
        .bind(pack)
        .bind(product)
        .bind(STRIP)
        .execute(pool)
        .await
        .expect("pack");
        sqlx::query(
            "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
             expires_on,created_at_utc,updated_at_utc) \
             VALUES (?,?,'B-1','B-1','2028-03-31','2026-01-01T00:00:00.000Z',\
             '2026-01-01T00:00:00.000Z')",
        )
        .bind(batch)
        .bind(pack)
        .execute(pool)
        .await
        .expect("batch");
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
    .bind(POSTED)
    .bind(STORE)
    .bind(USER)
    .bind(USER)
    .bind("a".repeat(64))
    .execute(pool)
    .await
    .expect("posted sale");

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
        .bind(POSTED)
        .bind(index as i64 + 1)
        .bind(product)
        .bind(pack)
        .bind(batch)
        .execute(pool)
        .await
        .expect("posted line");
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

/// `(regulatory_snapshot_version, manufacturer_company_id, manufacturer_name,
/// regulatory_schemes_snapshot)` of one Sale line.
type LineSnapshot = (i64, Option<String>, Option<String>, Option<String>);

const NEW_LINE_COLUMNS: [&str; 4] = [
    "regulatory_snapshot_version",
    "manufacturer_company_id",
    "manufacturer_name",
    "regulatory_schemes_snapshot",
];

const NEW_TABLES: [&str; 6] = [
    "product_regulatory_classifications",
    "product_regulatory_attributes",
    "product_pack_regulatory_attributes",
    "store_professionals",
    "store_compliance_licences",
    "store_record_elections",
];

const NEW_TRIGGERS: [&str; 8] = [
    "product_regulatory_classifications_no_overlap_insert",
    "product_regulatory_classifications_no_overlap_update",
    "store_record_elections_no_overlap_insert",
    "store_record_elections_no_overlap_update",
    "sale_lines_regulatory_draft_only_insert",
    "sale_lines_regulatory_snapshot_coherent_update",
    "sale_documents_regulatory_gate_update",
    "sale_documents_regulatory_gate_insert",
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
    structural_checks(&pool).await;

    let line_columns = columns_of(&pool, "sale_lines").await;
    for expected in NEW_LINE_COLUMNS {
        assert!(
            line_columns.iter().any(|name| name == expected),
            "{expected} missing"
        );
    }
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
            objects.iter().any(|(_, name, _)| name == expected),
            "{expected} is missing"
        );
    }
    // The audit log learned the new entity types without losing an old one.
    let audit_sql = objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2
        .clone();
    for entity in [
        "product_regulatory_classification",
        "product_regulatory_attributes",
        "product_pack_regulatory_attributes",
        "store_professional",
        "store_compliance_licence",
        "store_record_election",
        "store_licence",
        "sale_document",
    ] {
        assert!(
            audit_sql.contains(entity),
            "{entity} missing from audit log"
        );
    }
    pool.close().await;
}

#[tokio::test]
async fn the_new_migration_preserves_every_sale_line_and_invents_no_classification() {
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
    let before_line_columns = columns_of(&pool, "sale_lines").await;
    let before_lines = rows_of(&pool, "sale_lines", &before_line_columns).await;
    let before_document_columns = columns_of(&pool, "sale_documents").await;
    let before_documents = rows_of(&pool, "sale_documents", &before_document_columns).await;
    let before_product_columns = columns_of(&pool, "products").await;
    let before_products = rows_of(&pool, "products", &before_product_columns).await;
    let before_batch_columns = columns_of(&pool, "product_batches").await;
    let before_batches = rows_of(&pool, "product_batches", &before_batch_columns).await;
    let before_audit: Vec<String> = sqlx::query_scalar(
        "SELECT quote(event_id)||'|'||quote(entity_type)||'|'||quote(change_payload) \
         FROM master_change_events ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .expect("audit rows");
    pool.close().await;

    let pool = open(&database).await;
    run_migrations(&pool, &after_directory).await;
    structural_checks(&pool).await;

    // 1. Nothing removed, and nothing earlier redefined except the two tables this migration
    //    deliberately rebuilds or extends.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if kind == "table" && ["sale_lines", "master_change_events"].contains(&name.as_str()) {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // 2. Columns only appended to sale_lines; sale_documents, products and batches untouched.
    let after_line_columns = columns_of(&pool, "sale_lines").await;
    assert_eq!(
        after_line_columns[..before_line_columns.len()],
        before_line_columns[..]
    );
    assert_eq!(
        after_line_columns[before_line_columns.len()..],
        NEW_LINE_COLUMNS.map(str::to_owned)[..]
    );
    assert_eq!(
        columns_of(&pool, "sale_documents").await,
        before_document_columns
    );
    assert_eq!(columns_of(&pool, "products").await, before_product_columns);

    // 3. Every pre-existing row byte-for-byte unchanged: lines, documents, products, batches and
    //    the append-only audit log that was rebuilt to widen one CHECK.
    assert_eq!(
        before_lines,
        rows_of(&pool, "sale_lines", &before_line_columns).await
    );
    assert_eq!(
        before_documents,
        rows_of(&pool, "sale_documents", &before_document_columns).await
    );
    assert_eq!(
        before_products,
        rows_of(&pool, "products", &before_product_columns).await
    );
    assert_eq!(
        before_batches,
        rows_of(&pool, "product_batches", &before_batch_columns).await
    );
    let after_audit: Vec<String> = sqlx::query_scalar(
        "SELECT quote(event_id)||'|'||quote(entity_type)||'|'||quote(change_payload) \
         FROM master_change_events ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .expect("audit rows");
    assert_eq!(before_audit, after_audit);

    // 4. Nothing fabricated. No product acquired a schedule position, no line acquired a
    //    manufacturer, and every historical line stayed at version 0 — an unrecorded past, stated.
    let classifications: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM product_regulatory_classifications")
            .fetch_one(&pool)
            .await
            .expect("classification count");
    assert_eq!(classifications, 0, "a classification was invented");
    for table in [
        "product_regulatory_attributes",
        "product_pack_regulatory_attributes",
        "store_professionals",
        "store_compliance_licences",
        "store_record_elections",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0, "{table} was populated by the migration");
    }
    let snapshots: Vec<LineSnapshot> = sqlx::query_as(
        "SELECT regulatory_snapshot_version,manufacturer_company_id,manufacturer_name,\
         regulatory_schemes_snapshot FROM sale_lines ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("snapshots");
    assert_eq!(snapshots.len(), 2);
    for snapshot in snapshots {
        assert_eq!(snapshot, (0, None, None, None));
    }

    // 5. The new guards bite on the migrated database.
    //    (a) a posted line's frozen snapshot cannot be rewritten;
    let rewritten =
        sqlx::query("UPDATE sale_lines SET regulatory_snapshot_version=1 WHERE sale_document_id=?")
            .bind(POSTED)
            .execute(&pool)
            .await;
    let refusal = rewritten
        .expect_err("a posted line's snapshot was rewritable")
        .to_string();
    assert!(
        refusal.contains("sale_document_is_posted")
            || refusal.contains("regulatory_snapshot_incoherent"),
        "unexpected refusal: {refusal}"
    );

    //    (b) a draft cannot carry a snapshot before posting;
    let early = sqlx::query(
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,regulatory_snapshot_version,\
         regulatory_schemes_snapshot,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000003ff',?,1,?,?,?,'pack',1,10,4000,1,\
         '{\"schedule_h\":\"does_not_apply\"}','2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    )
    .bind(OPEN_DRAFT)
    .bind(GENERAL)
    .bind(GENERAL_PACK)
    .bind(GENERAL_BATCH)
    .execute(&pool)
    .await;
    assert!(
        early.err().is_some_and(|error| error
            .to_string()
            .contains("regulatory_snapshot_before_posting")),
        "a draft took a regulatory snapshot"
    );

    //    (c) two findings cannot govern the same product, scheme and day;
    for (id, from, to) in [
        (
            "01997000-0000-7000-8000-000000000401",
            "2026-01-01",
            None::<&str>,
        ),
        ("01997000-0000-7000-8000-000000000402", "2026-06-01", None),
    ] {
        let insert = sqlx::query(
            "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
             effective_from,effective_to,source_citation,determined_by_user_id,revision,status,\
             created_at_utc,updated_at_utc) \
             VALUES (?,?,'schedule_h',1,?,?,'G.S.R. 1(E)',?,1,'active',\
             '2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
        )
        .bind(id)
        .bind(MEDICINE)
        .bind(from)
        .bind(to)
        .bind(USER)
        .execute(&pool)
        .await;
        if id.ends_with("401") {
            insert.expect("the first finding is accepted");
        } else {
            assert!(
                insert.err().is_some_and(|error| {
                    error
                        .to_string()
                        .contains("regulatory_classification_period_overlaps")
                }),
                "an overlapping finding was accepted"
            );
        }
    }

    //    (d) and a draft carrying a Schedule H line cannot be flipped to posted by direct SQL.
    sqlx::query(
        "INSERT INTO sale_lines (id,sale_document_id,line_number,product_id,product_pack_id,\
         batch_id,quantity_basis,quantity_packs,quantity_atoms,selling_rate_paise,created_at_utc,\
         updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-0000000003fe',?,1,?,?,?,'pack',1,10,4000,\
         '2026-09-20T00:00:00.000Z','2026-09-20T00:00:00.000Z')",
    )
    .bind(OPEN_DRAFT)
    .bind(MEDICINE)
    .bind(MEDICINE_PACK)
    .bind(MEDICINE_BATCH)
    .execute(&pool)
    .await
    .expect("draft line");
    let posted = sqlx::query(
        "UPDATE sale_documents SET status='posted',document_number='INV/2627/000002',\
         sequence_value=2,series_code='INV',financial_year='2026-27',tax_treatment='intra_state',\
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
        "direct SQL posted a sale whose line carries no regulatory snapshot"
    );

    structural_checks(&pool).await;
    pool.close().await;
}
