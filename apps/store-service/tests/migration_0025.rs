//! Migration 0025 against a database that already holds the Phase 1M-A..D1-A regulatory records.
//!
//! 0025 adds the two facts a retail Schedule X supply will one day have to rest on: whether a
//! licence stands, and which drugs it names. The outcomes that must never happen are a legacy
//! Form 20F being read as current authority, a coverage row appearing that nobody wrote, an
//! incoherent validity basis being accepted or quietly repaired, and the audit log losing a single
//! event while it is rebuilt to learn the new entity type.
//!
//! ```text
//! cargo test --test migration_0025
//! ```
use std::path::{Path, PathBuf};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions};

const NEW_MIGRATION: i64 = 25;

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

/// Runs a statement expecting a refusal, and returns the refusal text.
async fn refused(pool: &SqlitePool, statement: &str) -> String {
    sqlx::query(statement)
        .execute(pool)
        .await
        .err()
        .unwrap_or_else(|| panic!("accepted a statement it must refuse: {statement}"))
        .to_string()
}

const STORE: &str = "01997000-0000-7000-8000-0000000000bb";
const USER: &str = "01997000-0000-7000-8000-0000000000cc";
const SCHEDULE_X_TEST_MEDICINE_A: &str = "01997000-0000-7000-8000-0000000000d1";
const SCHEDULE_X_TEST_MEDICINE_B: &str = "01997000-0000-7000-8000-0000000000d2";
const WITHDRAWN_PRODUCT: &str = "01997000-0000-7000-8000-0000000000d3";
const LICENCE_20F: &str = "01997000-0000-7000-8000-000000000531";
const LICENCE_20G: &str = "01997000-0000-7000-8000-000000000532";
const LICENCE_20: &str = "01997000-0000-7000-8000-000000000533";
const ARCHIVED_20F: &str = "01997000-0000-7000-8000-000000000534";
const ANOTHER_STORE: &str = "01997000-0000-7000-8000-0000000000be";

/// What 0025 adds: one table, two columns, seven guards.
const NEW_TRIGGERS: [&str; 7] = [
    "store_compliance_licences_validity_basis_insert",
    "store_compliance_licences_validity_basis_update",
    "store_licence_drug_coverage_coherent_insert",
    "store_licence_drug_coverage_coherent_update",
    "store_licence_drug_coverage_no_overlap_insert",
    "store_licence_drug_coverage_no_overlap_update",
    "store_licence_drug_coverage_no_delete",
];

const NEW_LICENCE_COLUMNS: [&str; 2] = ["legal_status", "validity_basis"];

const COVERAGE_COLUMNS: [&str; 15] = [
    "id",
    "store_id",
    "licence_id",
    "product_id",
    "effective_from",
    "effective_to",
    "source_citation",
    "reason",
    "recorded_by_user_id",
    "revision",
    "status",
    "created_at_utc",
    "updated_at_utc",
    "archived_at_utc",
    "archive_reason",
];

/// A pharmacy as it stands at 0024: a store, an owner, three products (one withdrawn), the four
/// licence forms this phase cares about, a pharmacist, a rule 65 election, Schedule X findings and
/// an audit log with events of several kinds already in it.
async fn populate_before_0025(pool: &SqlitePool) {
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
         display_name,normalized_search_name,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-0000000000d1','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Schedule X Test Medicine A',\
         'schedule x test medicine a','active','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000d2','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Schedule X Test Medicine B',\
         'schedule x test medicine b','active','2026-01-01T00:00:00.000Z',\
         '2026-01-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-0000000000d3','medicine','01997000-0000-7000-8000-0000000000a1',\
         '01997000-0000-7000-8000-000000000001',0,'Withdrawn Tablet','withdrawn tablet',\
         'active','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        "UPDATE products SET status='archived',archived_at_utc='2026-02-01T00:00:00.000Z',\
         archive_reason='withdrawn from the shelf' \
         WHERE id='01997000-0000-7000-8000-0000000000d3'",
        // Four licences, recorded before anyone could say whether they stood.
        "INSERT INTO store_compliance_licences (id,store_id,licence_form,licence_number,\
         normalized_licence_number,issuing_authority,valid_from,revision,status,created_at_utc,\
         updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000531','01997000-0000-7000-8000-0000000000bb','form_20f',\
         'MH-PUNE-20F-4471','MHPUNE20F4471','FDA Maharashtra','2024-04-01',1,'active',\
         '2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-000000000532','01997000-0000-7000-8000-0000000000bb','form_20g',\
         'MH-PUNE-20G-8812','MHPUNE20G8812','FDA Maharashtra','2024-04-01',1,'active',\
         '2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-000000000533','01997000-0000-7000-8000-0000000000bb','form_20',\
         'MH-PUNE-20-1234','MHPUNE201234','FDA Maharashtra','2024-04-01',1,'active',\
         '2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
        "INSERT INTO store_compliance_licences (id,store_id,licence_form,licence_number,\
         normalized_licence_number,revision,status,created_at_utc,updated_at_utc,archived_at_utc,\
         archive_reason) VALUES \
         ('01997000-0000-7000-8000-000000000534','01997000-0000-7000-8000-0000000000bb','form_20f',\
         'MH-PUNE-20F-0001','MHPUNE20F0001',2,'archived','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z','superseded')",
        "INSERT INTO store_professionals (id,store_id,full_name,capacity,registration_number,\
         registering_authority,valid_from,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000501','01997000-0000-7000-8000-0000000000bb',\
         'Meera Iyer','registered_pharmacist','MH-PH-44821','Maharashtra State Pharmacy Council',\
         '2020-01-01',1,'active','2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
        "INSERT INTO store_record_elections (id,store_id,election,method,effective_from,\
         recorded_by_user_id,revision,status,created_at_utc,updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000521','01997000-0000-7000-8000-0000000000bb',\
         'rule_65_3_prescription_supply','prescription_register','2026-04-01',\
         '01997000-0000-7000-8000-0000000000cc',1,'active','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z')",
        // Both test products are owner-recorded as inside Schedule X for this fixture; product
        // names themselves do not determine regulatory classification. Being inside Schedule X is
        // what the law calls a drug; it says nothing about what this store's licence covers.
        "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
         effective_from,source_citation,determined_by_user_id,revision,status,created_at_utc,\
         updated_at_utc) VALUES \
         ('01997000-0000-7000-8000-000000000401','01997000-0000-7000-8000-0000000000d1',\
         'schedule_x',1,'2020-01-01','Test fixture: owner-recorded Schedule X finding',\
         '01997000-0000-7000-8000-0000000000cc',1,'active','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z'),\
         ('01997000-0000-7000-8000-000000000402','01997000-0000-7000-8000-0000000000d2',\
         'schedule_x',1,'2020-01-01','Test fixture: owner-recorded Schedule X finding',\
         '01997000-0000-7000-8000-0000000000cc',1,'active','2026-09-01T00:00:00.000Z',\
         '2026-09-01T00:00:00.000Z')",
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) VALUES \
         ('01997000-0000-7000-8000-000000000901','store_compliance_licence',\
         '01997000-0000-7000-8000-000000000531',1,'created','2026-09-01T00:00:00.000Z',\
         'recorded from the certificate',1,'{\"licenceForm\":\"form_20f\"}',\
         '01997000-0000-7000-8000-0000000000cc'),\
         ('01997000-0000-7000-8000-000000000902','store_compliance_licence',\
         '01997000-0000-7000-8000-000000000534',2,'archived','2026-09-01T00:00:00.000Z',\
         'superseded',1,'{}','01997000-0000-7000-8000-0000000000cc'),\
         ('01997000-0000-7000-8000-000000000903','product_regulatory_classification',\
         '01997000-0000-7000-8000-000000000401',1,'created','2026-09-01T00:00:00.000Z',NULL,1,\
         '{\"scheme\":\"schedule_x\"}','01997000-0000-7000-8000-0000000000cc'),\
         ('01997000-0000-7000-8000-000000000904','store_professional',\
         '01997000-0000-7000-8000-000000000501',1,'created','2026-09-01T00:00:00.000Z',NULL,1,\
         '{\"capacity\":\"registered_pharmacist\"}','01997000-0000-7000-8000-0000000000cc')",
    ] {
        execute(pool, statement).await;
    }
}

/// Phase 1M-D1-B, item 13.1 and 13.2. A database built from nothing reaches 0025, and the table,
/// the columns and every guard are there.
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
    assert_eq!(columns_of(&pool, "store_licence_drug_coverage").await, {
        COVERAGE_COLUMNS
            .iter()
            .map(|column| (*column).to_owned())
            .collect::<Vec<_>>()
    });
    let licence_columns = columns_of(&pool, "store_compliance_licences").await;
    for expected in NEW_LICENCE_COLUMNS {
        assert!(
            licence_columns.iter().any(|column| column == expected),
            "{expected} is missing"
        );
    }

    // The audit log knows the new entity type, and the scratch copy it was rebuilt through is gone.
    let audit_sql = &objects
        .iter()
        .find(|(kind, name, _)| kind == "table" && name == "master_change_events")
        .expect("audit table")
        .2;
    assert!(
        audit_sql.contains("store_licence_drug_coverage"),
        "{audit_sql}"
    );
    for table in tables(&pool).await {
        assert!(
            !table.contains("_phase1"),
            "scratch table left behind: {table}"
        );
        // Phase 1M-D1-B records authority. It does NOT build the Schedule X register, and a table
        // claiming to be one would be the first sign that this phase had overrun its scope.
        assert!(
            !table.contains("schedule_x"),
            "0025 must not create a Schedule X register: {table}"
        );
    }
    pool.close().await;
}

/// Phase 1M-D1-B, items 13.3 to 13.6. Against a pharmacy that already recorded its licences:
/// every row survives the audit rebuild, every legacy licence becomes `unknown` on both axes, and
/// not one coverage row is invented.
#[tokio::test]
async fn the_new_migration_preserves_every_row_and_establishes_no_authority() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let before_directory = temp.path().join("before");
    let after_directory = temp.path().join("after");
    migrations_up_to(NEW_MIGRATION - 1, &before_directory);
    migrations_up_to(NEW_MIGRATION, &after_directory);

    let database = temp.path().join("populated.sqlite3");
    let pool = open(&database).await;
    run_migrations(&pool, &before_directory).await;
    populate_before_0025(&pool).await;
    structural_checks(&pool).await;

    // Before 0025 there is no such column and no such table to write.
    assert!(
        sqlx::query("UPDATE store_compliance_licences SET legal_status='in_force'")
            .execute(&pool)
            .await
            .is_err(),
        "0024 accepted a column it does not have"
    );
    assert!(
        sqlx::query("SELECT 1 FROM store_licence_drug_coverage")
            .fetch_optional(&pool)
            .await
            .is_err(),
        "0024 already had the coverage table"
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
        "store_compliance_licences",
        "store_professionals",
        "store_record_elections",
        "product_regulatory_classifications",
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

    // 13.6. Nothing earlier is removed. Only the two tables 0025 touches may be redefined: the
    // licence table gains its columns, and the audit log is rebuilt for the new entity type.
    let after_objects = schema_objects(&pool).await;
    for (kind, name, sql) in &before_objects {
        let after = after_objects
            .iter()
            .find(|(after_kind, after_name, _)| after_kind == kind && after_name == name)
            .unwrap_or_else(|| panic!("{kind} {name} was removed"));
        if name == "store_compliance_licences" || name == "master_change_events" {
            continue;
        }
        assert_eq!(&after.2, sql, "{kind} {name} was redefined");
    }

    // The two columns are appended; nothing earlier moved.
    let after_licence_columns = columns_of(&pool, "store_compliance_licences").await;
    let before_licence_columns = &before_rows
        .iter()
        .find(|(name, _, _)| name == "store_compliance_licences")
        .unwrap()
        .1;
    assert_eq!(
        after_licence_columns[..before_licence_columns.len()],
        before_licence_columns[..],
        "licence columns moved"
    );
    assert_eq!(
        after_licence_columns[before_licence_columns.len()..],
        NEW_LICENCE_COLUMNS
            .iter()
            .map(|column| (*column).to_owned())
            .collect::<Vec<_>>()[..]
    );

    // 13.3. Every pre-existing row of every pre-existing table, on its original columns. This is
    // what proves the audit rebuild carried all four events across without touching one of them.
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
    assert_eq!(events, 4, "the audit rebuild lost or gained events");

    // 13.4. Every legacy licence is unknown on both axes. A Form 20F recorded before anyone was
    // asked whether it still stands is a piece of paper on file, not authority to supply.
    let states: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT licence_form,legal_status,validity_basis FROM store_compliance_licences \
         ORDER BY licence_number",
    )
    .fetch_all(&pool)
    .await
    .expect("licence states");
    assert_eq!(states.len(), 4);
    for (form, legal_status, validity_basis) in &states {
        assert_eq!(
            legal_status, "unknown",
            "{form} was inferred to be in force"
        );
        assert_eq!(
            validity_basis, "unknown",
            "{form} was given a validity basis"
        );
    }

    // 13.5. Not one coverage row. The store holds a Form 20F and both drugs are recorded inside
    // Schedule X; the migration still concludes nothing, because the licence's own list of drugs
    // is not something a schema upgrade can read.
    let coverage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM store_licence_drug_coverage")
        .fetch_one(&pool)
        .await
        .expect("coverage count");
    assert_eq!(coverage, 0, "the migration invented drug coverage");
    pool.close().await;
}

/// Phase 1M-D1-B, items 13.7 to 13.9. A validity basis and its dates must agree, at the schema,
/// whichever hand writes them.
#[tokio::test]
async fn a_validity_basis_and_its_dates_must_agree() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("validity.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0025(&pool).await;

    // 13.7. A fixed term with no date it runs to is not a licence that never expires.
    let error = refused(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET validity_basis='fixed_term' WHERE id='{LICENCE_20F}'"
        ),
    )
    .await;
    assert!(
        error.contains("licence_validity_basis_incoherent"),
        "{error}"
    );

    // 13.8. A perpetual licence with an expiry date is a contradiction, not a rounding error.
    let error = refused(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET validity_basis='perpetual',\
             valid_upto='2030-03-31' WHERE id='{LICENCE_20F}'"
        ),
    )
    .await;
    assert!(
        error.contains("licence_validity_basis_incoherent"),
        "{error}"
    );

    let error = refused(
        &pool,
        "INSERT INTO store_compliance_licences (id,store_id,licence_form,licence_number,\
         normalized_licence_number,validity_basis,revision,status,created_at_utc,updated_at_utc) \
         VALUES ('01997000-0000-7000-8000-000000000535','01997000-0000-7000-8000-0000000000bb',\
         'form_20f','MH-PUNE-20F-9999','MHPUNE20F9999','fixed_term',1,'active',\
         '2026-09-01T00:00:00.000Z','2026-09-01T00:00:00.000Z')",
    )
    .await;
    assert!(
        error.contains("licence_validity_basis_incoherent"),
        "{error}"
    );

    // Neither enumeration accepts a word of its own invention.
    for statement in [
        format!(
            "UPDATE store_compliance_licences SET legal_status='probably' WHERE id='{LICENCE_20F}'"
        ),
        format!(
            "UPDATE store_compliance_licences SET validity_basis='renewable' WHERE id='{LICENCE_20F}'"
        ),
    ] {
        let error = refused(&pool, &statement).await;
        assert!(
            error.contains("CHECK constraint failed"),
            "{statement}: {error}"
        );
    }

    // 13.9. Both coherent shapes are accepted, and the perpetual Form 20F of rule 61(3) — which
    // carries no expiry of its own — is one of them.
    execute(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET legal_status='in_force',\
             validity_basis='perpetual' WHERE id='{LICENCE_20F}'"
        ),
    )
    .await;
    execute(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET legal_status='in_force',\
             validity_basis='fixed_term',valid_upto='2030-03-31' WHERE id='{LICENCE_20G}'"
        ),
    )
    .await;
    structural_checks(&pool).await;
    pool.close().await;
}

/// Phase 1M-D1-B, items 13.10 to 13.13. Drug coverage must rest on this store's own active
/// Form 20F, name a real product, run forwards, and never answer the same day twice.
#[tokio::test]
async fn drug_coverage_must_rest_on_this_stores_own_form_20f() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("coverage.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0025(&pool).await;
    execute(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET legal_status='in_force',\
             validity_basis='perpetual' WHERE id='{LICENCE_20F}'"
        ),
    )
    .await;

    let insert = |id: &str, licence: &str, product: &str, from: &str, to: &str, store: &str| {
        format!(
            "INSERT INTO store_licence_drug_coverage (id,store_id,licence_id,product_id,\
             effective_from,effective_to,source_citation,recorded_by_user_id,revision,status,\
             created_at_utc,updated_at_utc) VALUES ('{id}','{store}','{licence}','{product}',\
             '{from}',{to},'Form 20F item 2, names of drugs','{USER}',1,'active',\
             '2026-09-25T00:00:00.000Z','2026-09-25T00:00:00.000Z')"
        )
    };

    // 13.10. Rule 61(3) issues Form 20G to a wholesaler. It is not retail authority, and neither
    // is a Form 20, and neither is a Form 20F the store has already superseded.
    for (label, licence) in [
        ("form 20G", LICENCE_20G),
        ("form 20", LICENCE_20),
        ("archived form 20F", ARCHIVED_20F),
    ] {
        let error = refused(
            &pool,
            &insert(
                "01997000-0000-7000-8000-000000000a01",
                licence,
                SCHEDULE_X_TEST_MEDICINE_A,
                "2026-01-01",
                "NULL",
                STORE,
            ),
        )
        .await;
        assert!(
            error.contains("licence_drug_coverage_incoherent"),
            "{label}: {error}"
        );
    }

    // A product that no longer exists, and one that has been withdrawn, cannot be covered.
    for product in [WITHDRAWN_PRODUCT, "01997000-0000-7000-8000-0000000000ff"] {
        let error = refused(
            &pool,
            &insert(
                "01997000-0000-7000-8000-000000000a02",
                LICENCE_20F,
                product,
                "2026-01-01",
                "NULL",
                STORE,
            ),
        )
        .await;
        assert!(
            error.contains("licence_drug_coverage_incoherent")
                || error.contains("FOREIGN KEY constraint failed"),
            "{product}: {error}"
        );
    }

    // 13.11. Coverage naming a store other than the one that holds the licence is refused. This
    // installation can only ever hold one store — the single-store guard of 0002 sees to that —
    // so the foreign store is refused at the reference as well as at the coherence guard.
    let error = refused(
        &pool,
        &insert(
            "01997000-0000-7000-8000-000000000a03",
            LICENCE_20F,
            SCHEDULE_X_TEST_MEDICINE_A,
            "2026-01-01",
            "NULL",
            ANOTHER_STORE,
        ),
    )
    .await;
    assert!(
        error.contains("licence_drug_coverage_incoherent")
            || error.contains("FOREIGN KEY constraint failed"),
        "{error}"
    );

    // 13.12. A period that ends before it starts, or on the day it starts, is not a period.
    for (from, to) in [
        ("2026-06-01", "'2026-01-01'"),
        ("2026-06-01", "'2026-06-01'"),
    ] {
        let error = refused(
            &pool,
            &insert(
                "01997000-0000-7000-8000-000000000a04",
                LICENCE_20F,
                SCHEDULE_X_TEST_MEDICINE_A,
                from,
                to,
                STORE,
            ),
        )
        .await;
        assert!(
            error.contains("CHECK constraint failed"),
            "{from}..{to}: {error}"
        );
    }

    // The coherent case is accepted.
    execute(
        &pool,
        &insert(
            "01997000-0000-7000-8000-000000000a10",
            LICENCE_20F,
            SCHEDULE_X_TEST_MEDICINE_A,
            "2026-01-01",
            "'2026-07-01'",
            STORE,
        ),
    )
    .await;

    // 13.13. Two active rows must never answer for the same product on the same day.
    for (from, to) in [
        ("2026-06-01", "NULL"),
        ("2025-01-01", "NULL"),
        ("2026-02-01", "'2026-03-01'"),
    ] {
        let error = refused(
            &pool,
            &insert(
                "01997000-0000-7000-8000-000000000a11",
                LICENCE_20F,
                SCHEDULE_X_TEST_MEDICINE_A,
                from,
                to,
                STORE,
            ),
        )
        .await;
        assert!(
            error.contains("licence_drug_coverage_period_overlaps"),
            "{from}..{to}: {error}"
        );
    }
    // Abutting periods do not overlap: coverage ends on the day the next begins.
    execute(
        &pool,
        &insert(
            "01997000-0000-7000-8000-000000000a12",
            LICENCE_20F,
            SCHEDULE_X_TEST_MEDICINE_A,
            "2026-07-01",
            "NULL",
            STORE,
        ),
    )
    .await;
    // Another drug over the very same days is a different question with its own answer.
    execute(
        &pool,
        &insert(
            "01997000-0000-7000-8000-000000000a13",
            LICENCE_20F,
            SCHEDULE_X_TEST_MEDICINE_B,
            "2026-01-01",
            "NULL",
            STORE,
        ),
    )
    .await;

    // An overlap cannot be reached by moving a row either.
    let error = refused(
        &pool,
        "UPDATE store_licence_drug_coverage SET effective_to=NULL \
         WHERE id='01997000-0000-7000-8000-000000000a10'",
    )
    .await;
    assert!(
        error.contains("licence_drug_coverage_period_overlaps"),
        "{error}"
    );

    // Nor can a row be re-pointed at another drug, licence or start date after the fact.
    for column in [
        format!("product_id='{WITHDRAWN_PRODUCT}'"),
        format!("licence_id='{LICENCE_20G}'"),
        "effective_from='2025-01-01'".to_owned(),
    ] {
        let error = refused(
            &pool,
            &format!(
                "UPDATE store_licence_drug_coverage SET {column} \
                 WHERE id='01997000-0000-7000-8000-000000000a13'"
            ),
        )
        .await;
        assert!(
            error.contains("licence_drug_coverage_incoherent"),
            "{column}: {error}"
        );
    }

    // Coverage is history: it is archived, never deleted.
    let error = refused(
        &pool,
        "DELETE FROM store_licence_drug_coverage WHERE id='01997000-0000-7000-8000-000000000a13'",
    )
    .await;
    assert!(
        error.contains("licence_drug_coverage_is_history"),
        "{error}"
    );

    // Archiving a row in error releases the days it held, because it never held them truthfully.
    execute(
        &pool,
        "UPDATE store_licence_drug_coverage SET status='archived',\
         archived_at_utc='2026-09-25T01:00:00.000Z',archive_reason='entered against the wrong drug',\
         revision=2 WHERE id='01997000-0000-7000-8000-000000000a13'",
    )
    .await;
    execute(
        &pool,
        &insert(
            "01997000-0000-7000-8000-000000000a14",
            LICENCE_20F,
            SCHEDULE_X_TEST_MEDICINE_B,
            "2026-01-01",
            "NULL",
            STORE,
        ),
    )
    .await;

    // The audit log accepts the new entity type, and still refuses to be rewritten.
    execute(
        &pool,
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) VALUES \
         ('01997000-0000-7000-8000-000000000a20','store_licence_drug_coverage',\
         '01997000-0000-7000-8000-000000000a14',1,'created','2026-09-25T01:00:00.000Z',NULL,1,\
         '{\"productId\":\"01997000-0000-7000-8000-0000000000d2\"}',\
         '01997000-0000-7000-8000-0000000000cc')",
    )
    .await;
    let error = refused(
        &pool,
        "UPDATE master_change_events SET reason='something else' \
         WHERE event_id='01997000-0000-7000-8000-000000000a20'",
    )
    .await;
    assert!(
        error.contains("master_change_events_are_append_only"),
        "{error}"
    );

    structural_checks(&pool).await;
    pool.close().await;
}

/// How many rows `load_form_20f_authority` would see for a product: the resolver's own predicate,
/// so a test that moves this count is testing the filter the production reader really applies.
async fn active_coverage(pool: &SqlitePool, product: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM store_licence_drug_coverage \
         WHERE store_id=? AND product_id=? AND status='active'",
    )
    .bind(STORE)
    .bind(product)
    .fetch_one(pool)
    .await
    .expect("active coverage")
}

/// KNOWN LIMITATION, pinned deliberately — corrective C1, item 4.
///
/// This test does NOT assert desired business behaviour. It records what the schema does today, so
/// that a later repository-wide correction is a visible, deliberate change rather than a surprise.
///
/// SQLite rejects only a CHECK whose expression is false; an expression evaluating to NULL passes.
/// `length(trim(NULL)) > 0` is NULL, so the archived-coherence CHECK — inherited verbatim from the
/// four Phase 1M-A tables of committed migration 0021 — catches a *blank* archive reason but not a
/// *missing* one. Adding `AND archive_reason IS NOT NULL` would close it; doing that for this one
/// table would leave it stricter than its four siblings, so the correction belongs to its own
/// repository-wide pass and is deliberately NOT made here.
///
/// What this test does prove is the property that matters: such a row is not active coverage, so
/// the resolver never sees it and no authority can rest on it. The limitation is therefore
/// fail-closed — it can lose the *reason* an archival happened, never grant an authority. That the
/// API itself refuses a missing or blank reason is proved over the real socket by
/// `real_service_archives_drug_coverage_and_the_archived_row_grants_nothing_over_http`.
#[tokio::test]
async fn an_archived_row_with_no_reason_is_a_known_limitation_that_grants_no_authority() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let directory = temp.path().join("migrations");
    migrations_up_to(NEW_MIGRATION, &directory);
    let pool = open(&temp.path().join("known-limitation.sqlite3")).await;
    run_migrations(&pool, &directory).await;
    populate_before_0025(&pool).await;
    execute(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET legal_status='in_force',\
             validity_basis='perpetual' WHERE id='{LICENCE_20F}'"
        ),
    )
    .await;
    execute(
        &pool,
        &format!(
            "INSERT INTO store_licence_drug_coverage (id,store_id,licence_id,product_id,\
             effective_from,source_citation,recorded_by_user_id,revision,status,created_at_utc,\
             updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000b01','{STORE}',\
             '{LICENCE_20F}','{SCHEDULE_X_TEST_MEDICINE_A}','2024-04-01',\
             'Form 20F item 2, names of drugs',\
             '{USER}',1,'active','2026-09-25T00:00:00.000Z','2026-09-25T00:00:00.000Z')"
        ),
    )
    .await;
    assert_eq!(active_coverage(&pool, SCHEDULE_X_TEST_MEDICINE_A).await, 1);

    // A BLANK reason is caught, so the CHECK is not simply absent.
    let error = refused(
        &pool,
        "UPDATE store_licence_drug_coverage SET status='archived',\
         archived_at_utc='2026-09-25T01:00:00.000Z',archive_reason='   ',revision=2 \
         WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .await;
    assert!(error.contains("CHECK constraint failed"), "{error}");
    // An archival with no timestamp is caught too.
    let error = refused(
        &pool,
        "UPDATE store_licence_drug_coverage SET status='archived',archive_reason='withdrawn',\
         revision=2 WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .await;
    assert!(error.contains("CHECK constraint failed"), "{error}");

    // KNOWN LIMITATION: a MISSING reason is not caught, because the CHECK evaluates to NULL.
    // Asserted as the CURRENT behaviour, never as the desired one.
    execute(
        &pool,
        "UPDATE store_licence_drug_coverage SET status='archived',\
         archived_at_utc='2026-09-25T01:00:00.000Z',archive_reason=NULL,revision=2 \
         WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .await;
    let (status, reason): (String, Option<String>) = sqlx::query_as(
        "SELECT status,archive_reason FROM store_licence_drug_coverage \
         WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .fetch_one(&pool)
    .await
    .expect("the archived row");
    assert_eq!(status, "archived");
    assert_eq!(
        reason, None,
        "if this now fails the inherited CHECK has been tightened — retire this known limitation"
    );

    // THE SAFETY PROPERTY. The row is not active coverage, so no authority can rest on it. This is
    // what makes the limitation fail-closed rather than dangerous.
    assert_eq!(
        active_coverage(&pool, SCHEDULE_X_TEST_MEDICINE_A).await,
        0,
        "an archived row with no reason must not be active coverage"
    );

    // The permissiveness is inherited, not invented by 0025: the licence table this migration
    // extends carries the identical clause from committed migration 0021, and accepts the same.
    execute(
        &pool,
        &format!(
            "UPDATE store_compliance_licences SET status='archived',\
             archived_at_utc='2026-09-25T01:00:00.000Z',archive_reason=NULL,revision=3 \
             WHERE id='{LICENCE_20G}'"
        ),
    )
    .await;

    // The row remains history either way: it cannot be deleted, reason or no reason.
    let error = refused(
        &pool,
        "DELETE FROM store_licence_drug_coverage \
         WHERE id='01997000-0000-7000-8000-000000000b01'",
    )
    .await;
    assert!(
        error.contains("licence_drug_coverage_is_history"),
        "{error}"
    );

    // And because the period is free again, the drug can be covered properly afterwards.
    execute(
        &pool,
        &format!(
            "INSERT INTO store_licence_drug_coverage (id,store_id,licence_id,product_id,\
             effective_from,source_citation,recorded_by_user_id,revision,status,created_at_utc,\
             updated_at_utc) VALUES ('01997000-0000-7000-8000-000000000b02','{STORE}',\
             '{LICENCE_20F}','{SCHEDULE_X_TEST_MEDICINE_A}','2024-04-01',\
             'Form 20F item 2, re-read on renewal',\
             '{USER}',1,'active','2026-09-25T02:00:00.000Z','2026-09-25T02:00:00.000Z')"
        ),
    )
    .await;
    assert_eq!(active_coverage(&pool, SCHEDULE_X_TEST_MEDICINE_A).await, 1);
    structural_checks(&pool).await;
    pool.close().await;
}
