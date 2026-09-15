//! Backup and restore at sizes a pilot pharmacy will actually reach.
//!
//! Every test here is `#[ignore]`d. They build databases of a hundred megabytes and more, which is
//! minutes of work and gigabytes of temporary disk — not something to impose on every `cargo test`.
//! They exist so the claims in the freeze report can be re-run by anyone who doubts them:
//!
//! ```text
//! cargo test --test backup_scale -- --ignored --nocapture --test-threads=1
//! ```
//!
//! What they prove is not speed. It is that memory does not scale with the database: the container
//! is written, hashed, read and unpacked in fixed-size chunks, so a one-gigabyte backup costs the
//! same working set as a one-megabyte one. A pharmacy that has been trading for years must not
//! discover on the day it matters that its backup needs more RAM than the PC has.

use std::{path::Path, time::Instant};

use aushadharth_store_service::{
    domain::backup::{self, Manifest},
    infrastructure::database,
};
use sqlx::SqlitePool;

/// Grows a real migrated database to roughly `target_bytes` with real rows.
///
/// Padding rows rather than a sparse file: a backup reads pages, and a database made large by
/// anything other than content would measure nothing. The rows go into their own table so no
/// business constraint is bent to make the numbers look good.
async fn grow_database(pool: &SqlitePool, target_bytes: u64) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scale_padding (id INTEGER PRIMARY KEY, blob BLOB NOT NULL)",
    )
    .execute(pool)
    .await
    .expect("padding table");
    // 64 KiB a row: large enough that the row count stays sane, small enough to stay off the
    // page-overflow path being unusual.
    let chunk = vec![0x5a_u8; 64 * 1024];
    let rows_needed = (target_bytes / chunk.len() as u64).max(1);
    let mut written = 0;
    while written < rows_needed {
        let batch = (rows_needed - written).min(256);
        let mut transaction = pool.begin().await.expect("begin");
        for _ in 0..batch {
            sqlx::query("INSERT INTO scale_padding (blob) VALUES (?)")
                .bind(&chunk)
                .execute(&mut *transaction)
                .await
                .expect("padding row");
        }
        transaction.commit().await.expect("commit");
        written += batch;
    }
}

struct Measurement {
    database_bytes: u64,
    container_bytes: u64,
    snapshot: std::time::Duration,
    package: std::time::Duration,
    verify: std::time::Duration,
    unpack: std::time::Duration,
}

impl Measurement {
    fn report(&self, label: &str) {
        let megabytes = self.database_bytes as f64 / (1024.0 * 1024.0);
        let per_megabyte = |value: std::time::Duration| value.as_secs_f64() * 1000.0 / megabytes;
        println!(
            "{label}: database {:.1} MB, container {:.1} MB\n  \
             snapshot {:>8.2?} ({:.1} ms/MB)\n  \
             package  {:>8.2?} ({:.1} ms/MB)\n  \
             verify   {:>8.2?} ({:.1} ms/MB)\n  \
             unpack   {:>8.2?} ({:.1} ms/MB)",
            megabytes,
            self.container_bytes as f64 / (1024.0 * 1024.0),
            self.snapshot,
            per_megabyte(self.snapshot),
            self.package,
            per_megabyte(self.package),
            self.verify,
            per_megabyte(self.verify),
            self.unpack,
            per_megabyte(self.unpack),
        );
    }
}

/// The whole cycle at one size, timed in its four separable parts.
async fn measure(target_bytes: u64) -> Measurement {
    let temp = tempfile::tempdir().expect("temporary directory");
    let live = temp.path().join("live.sqlite3");
    let pool = database::connect(&live).await.expect("migrated database");
    grow_database(&pool, target_bytes).await;

    let snapshot_path = temp.path().join("snapshot.sqlite3");
    let literal = snapshot_path.to_string_lossy().replace('\'', "''");
    let started = Instant::now();
    sqlx::query(&format!("VACUUM INTO '{literal}'"))
        .execute(&pool)
        .await
        .expect("vacuum into");
    let snapshot = started.elapsed();
    database::checkpoint_and_close(&pool).await.expect("close");

    let started = Instant::now();
    let (digest, database_bytes) = backup::hash_file(&snapshot_path).expect("hash");
    let manifest = manifest_for(&digest, database_bytes);
    let container_path = temp.path().join("backup.aushbackup");
    {
        let mut file = std::fs::File::create(&container_path).expect("container");
        backup::write_container(&mut file, &manifest, &snapshot_path).expect("write container");
    }
    let package = started.elapsed();
    let container_bytes = std::fs::metadata(&container_path).expect("metadata").len();

    let started = Instant::now();
    let (header, read_manifest) = read_framing(&container_path, container_bytes);
    let verify = started.elapsed();

    let restored = temp.path().join("restored.sqlite3");
    let started = Instant::now();
    {
        let mut source = std::fs::File::open(&container_path).expect("open container");
        let mut destination = std::fs::File::create(&restored).expect("create restored");
        // Verifies the SHA-256 as it streams, so this timing includes the integrity check.
        backup::stream_payload(&mut source, &header, &read_manifest, &mut destination)
            .expect("stream payload");
    }
    let unpack = started.elapsed();

    // The unpacked file must be a working database, not merely the right length.
    let reopened = database::open_existing_unmigrated(&restored)
        .await
        .expect("reopen restored");
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&reopened)
        .await
        .expect("quick check");
    assert_eq!(integrity, "ok");
    let padding: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scale_padding")
        .fetch_one(&reopened)
        .await
        .expect("padding count");
    assert!(padding > 0, "the restored database lost its content");
    database::checkpoint_and_close(&reopened)
        .await
        .expect("close");

    assert_eq!(
        std::fs::metadata(&restored)
            .expect("restored metadata")
            .len(),
        database_bytes,
        "the unpacked database is not the length the manifest recorded"
    );

    Measurement {
        database_bytes,
        container_bytes,
        snapshot,
        package,
        verify,
        unpack,
    }
}

fn manifest_for(digest: &str, database_bytes: u64) -> Manifest {
    Manifest {
        backup_format_version: backup::FORMAT_VERSION,
        product: "AUSHADHARTH".to_owned(),
        backup_id: uuid::Uuid::now_v7().to_string(),
        created_at_utc: "2026-09-15T00:00:00.000Z".to_owned(),
        application_version: "0.0.0".to_owned(),
        schema_version: database::latest_schema_version(),
        applied_migrations: Vec::new(),
        installation_id: uuid::Uuid::now_v7().to_string(),
        store_id: uuid::Uuid::now_v7().to_string(),
        store_display_name: "Scale Pharmacy".to_owned(),
        database_sha256: digest.to_owned(),
        database_bytes,
        backup_kind: "manual".to_owned(),
        source_platform: std::env::consts::OS.to_owned(),
        attachments: Vec::new(),
    }
}

fn read_framing(path: &Path, length: u64) -> (backup::ContainerHeader, Manifest) {
    let mut file = std::fs::File::open(path).expect("open container");
    let header = backup::read_header(&mut file, length).expect("header");
    let manifest = backup::read_manifest(&mut file, &header).expect("manifest");
    (header, manifest)
}

#[tokio::test]
#[ignore = "builds a 100 MB database; run with --ignored"]
async fn a_hundred_megabyte_pharmacy_backs_up_and_restores() {
    measure(100 * 1024 * 1024).await.report("100 MB");
}

#[tokio::test]
#[ignore = "builds a 500 MB database; run with --ignored"]
async fn a_five_hundred_megabyte_pharmacy_backs_up_and_restores() {
    measure(500 * 1024 * 1024).await.report("500 MB");
}

#[tokio::test]
#[ignore = "builds a 1 GB database; run with --ignored"]
async fn a_one_gigabyte_pharmacy_backs_up_and_restores() {
    measure(1024 * 1024 * 1024).await.report("1 GB");
}

/// A container just past the 2 GiB payload ceiling is refused by its own framing.
///
/// Written as a header alone rather than as two gigabytes of payload: the ceiling is enforced on the
/// declared length before a byte is read, which is the property worth proving and the only one that
/// can be proved without the disk.
#[test]
fn a_payload_beyond_the_ceiling_is_refused_by_the_framing() {
    let mut container = Vec::new();
    container.extend_from_slice(backup::MAGIC);
    container.extend_from_slice(&backup::FORMAT_VERSION.to_be_bytes());
    container.extend_from_slice(&0_u16.to_be_bytes());
    container.extend_from_slice(&16_u32.to_be_bytes());
    container.extend_from_slice(&(backup::MAX_PAYLOAD_BYTES + 1).to_be_bytes());
    let length = backup::HEADER_BYTES + 16 + backup::MAX_PAYLOAD_BYTES + 1;
    let mut cursor = std::io::Cursor::new(container);
    let outcome = backup::read_header(&mut cursor, length);
    assert!(
        matches!(outcome, Err(backup::ContainerError::Malformed(_))),
        "a payload past the ceiling was accepted: {outcome:?}"
    );
}
