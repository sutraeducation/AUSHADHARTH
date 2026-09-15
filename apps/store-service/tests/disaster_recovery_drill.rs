//! The disaster-recovery drill.
//!
//! A backup nobody has ever restored is a belief, not a backup. This target is the drill that turns
//! it into a fact, and it is a **hard gate**: the phase does not freeze if it does not pass.
//!
//! It is written as a drill rather than as a set of unit tests, so it reads as the sequence a
//! pharmacy would actually live through — trade, back up, lose the machine, restore onto a new one,
//! and then survive being interrupted at each of the moments where an interruption is dangerous.
//! Each step prints what it observed, so the output can be read by somebody deciding whether to
//! trust this with their business:
//!
//! ```text
//! cargo test --test disaster_recovery_drill -- --nocapture --test-threads=1
//! ```
//!
//! Nothing here touches an installed AUSHADHARTH. Every path is inside a per-run temporary
//! directory, the same discipline the real-service gate follows, and the drill asserts that before
//! it does anything else.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use aushadharth_store_service::{
    api::{self, backups::BackupService},
    infrastructure::database,
    platform::restore_journal::{self, JournalState, RestoreJournal, RestoreStage},
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const PASSWORD: &str = "Drill-Password-2026";

static STEP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn step(what: &str) {
    let number = STEP.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    println!("[{number:>2}] {what}");
}

/// One installation: its own data directory, its own database, its own port.
struct Installation {
    root: PathBuf,
    address: SocketAddr,
    pool: SqlitePool,
    shutdown: tokio::task::JoinHandle<()>,
}

impl Installation {
    fn database_path(&self) -> PathBuf {
        self.root.join("database").join("aushadharth.sqlite3")
    }

    fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// Starts the service the way `main.rs` does: recover, connect, complete, serve.
    async fn start(root: PathBuf) -> Installation {
        let database_path = root.join("database").join("aushadharth.sqlite3");
        let backups = root.join("backups");
        std::fs::create_dir_all(root.join("database")).expect("database directory");
        std::fs::create_dir_all(&backups).expect("backups directory");

        api::backups::recover_interrupted_restore(&backups, &database_path)
            .await
            .expect("startup recovery");
        let pool = database::connect(&database_path).await.expect("database");
        api::backups::complete_restore_after_open(&pool, &backups)
            .await
            .expect("restore completion");

        let service = Arc::new(BackupService::new(backups, database_path));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let router = api::router_with_backups(pool.clone(), None, Some(Arc::clone(&service)));
        let shutdown = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Installation {
            root,
            address,
            pool,
            shutdown,
        }
    }

    /// Stops serving and releases the database, as closing the application would.
    async fn stop(self) -> PathBuf {
        self.shutdown.abort();
        let _ = database::checkpoint_and_close(&self.pool).await;
        self.root
    }
}

/// Deletes a directory that a just-closed pool may still be holding.
///
/// `SqlitePool::close` returns before the operating system releases the handle, so the first
/// attempt fails with a sharing violation and a moment later the same call succeeds. The service
/// waits this out the same way, keyed on the operation rather than on a fixed pause.
async fn remove_directory_when_released(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the machine could not be destroyed: {error}"),
        }
    }
}

/// Moves a file that a just-closed pool may still be holding, for the same reason as above.
async fn rename_when_released(from: &std::path::Path, to: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => panic!("the file could not be moved: {error}"),
        }
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: Value,
    bytes: Vec<u8>,
}

async fn send(
    address: SocketAddr,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    payload: &[u8],
    cookie: Option<&str>,
) -> Reply {
    let mut stream = TcpStream::connect(address).await.expect("connect");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nAccept: application/json\r\n",
        address.port()
    );
    if let Some(content_type) = content_type {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    head.push_str("\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(payload);
    let _ = stream.write_all(&request).await;

    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw).await;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response framing");
    let headers = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut bytes = raw[split + 4..].to_vec();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        bytes = unchunk(&bytes);
    }
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("status");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        headers,
        body,
        bytes,
    }
}

fn unchunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let Some(end) = rest.windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let Ok(size) = std::str::from_utf8(&rest[..end])
            .ok()
            .map(str::trim)
            .map(|text| usize::from_str_radix(text, 16))
            .unwrap_or(Ok(0))
        else {
            break;
        };
        let start = end + 2;
        if size == 0 || rest.len() < start + size {
            break;
        }
        out.extend_from_slice(&rest[start..start + size]);
        rest = &rest[start + size..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        }
    }
    out
}

async fn json(
    address: SocketAddr,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
) -> Reply {
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    send(
        address,
        method,
        path,
        Some("application/json"),
        payload.as_bytes(),
        cookie,
    )
    .await
}

fn cookie_of(headers: &str) -> String {
    headers
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| {
            value
                .trim()
                .split(';')
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .expect("session cookie")
}

/// Records a supplier, so the drill has a business fact to lose and to find again.
async fn record_supplier(address: SocketAddr, cookie: &str, name: &str) {
    let reply = json(
        address,
        "POST",
        "/api/v1/parties",
        json!({ "party": { "displayName": name }, "roles": [{ "role": "supplier" }] }),
        Some(cookie),
    )
    .await;
    assert_eq!(reply.status, 201, "recording {name}: {:?}", reply.body);
}

async fn supplier_names(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT display_name FROM parties ORDER BY display_name")
        .fetch_all(pool)
        .await
        .expect("suppliers")
}

/// A journal describing an interruption at `stage`, exactly as the service would have written it.
fn journal_at(
    database_path: PathBuf,
    stage: RestoreStage,
    candidate: PathBuf,
    superseded: PathBuf,
) -> RestoreJournal {
    RestoreJournal {
        restore_id: uuid::Uuid::now_v7().to_string(),
        stage,
        started_at_utc: "2026-09-15T00:00:00.000Z".to_owned(),
        candidate_path: candidate,
        superseded_path: superseded,
        database_path,
        safety_backup_name: None,
        source_store_id: "drill-store".to_owned(),
        source_installation_id: "drill-installation".to_owned(),
        source_backup_created_at_utc: "2026-09-15T00:00:00.000Z".to_owned(),
        source_database_sha256: "0".repeat(64),
        pre_restore_database_sha256: None,
        backup_format_version: 1,
        source_schema_version: database::latest_schema_version(),
        initiated_by_user_id: None,
        initiated_by_login: None,
    }
}

#[tokio::test]
async fn the_disaster_recovery_drill() {
    let temp = tempfile::tempdir().expect("temporary directory");

    // ---------------------------------------------------------------------------------------
    // Part one: the drill must not be able to touch a real pharmacy
    // ---------------------------------------------------------------------------------------
    step("Confirm the drill runs entirely inside a disposable directory");
    let root = temp.path().to_string_lossy().to_ascii_lowercase();
    assert!(
        !root.contains("bizarth technologies"),
        "the drill reached the installed data directory"
    );
    assert!(
        !root.contains("onedrive"),
        "the drill reached a synchronised location"
    );
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_string_lossy()
        .to_ascii_lowercase();
    assert!(
        !root.starts_with(&repository),
        "the drill wrote inside the repository"
    );
    println!("     working inside {}", temp.path().display());

    step("Confirm the frozen path policy accepts the drill's database on its own terms");
    let original_root = temp.path().join("original");
    aushadharth_store_service::platform::runtime_paths::validate_database_path(
        &original_root.join("database").join("aushadharth.sqlite3"),
        std::path::Path::new(&repository),
    )
    .expect("the drill's database must satisfy the frozen path policy");

    // ---------------------------------------------------------------------------------------
    // Part two: a pharmacy trades, and takes a backup
    // ---------------------------------------------------------------------------------------
    step("Start a first installation the way the application starts it");
    let original = Installation::start(original_root).await;

    step("Complete first-run setup and obtain a real session");
    let setup = json(
        original.address,
        "POST",
        "/api/v1/auth/setup",
        json!({
            "storeDisplayName": "Drill Pharmacy",
            "ownerDisplayName": "Drill Owner",
            "loginIdentifier": "drill.owner",
            "password": PASSWORD
        }),
        None,
    )
    .await;
    assert_eq!(setup.status, 201, "{:?}", setup.body);
    let cookie = cookie_of(&setup.headers);

    step("Record business facts that must survive everything that follows");
    for name in [
        "Aditya Distributors",
        "Bharat Medical Agency",
        "Chandra Pharma",
    ] {
        record_supplier(original.address, &cookie, name).await;
    }
    let expected = supplier_names(&original.pool).await;
    assert_eq!(expected.len(), 3);
    println!("     recorded {:?}", expected);

    step("Confirm the installation reports itself as overdue for a backup");
    let status = json(
        original.address,
        "GET",
        "/api/v1/backups/status",
        Value::Null,
        Some(&cookie),
    )
    .await;
    assert_eq!(status.status, 200);
    assert_eq!(status.body["backupOverdue"], true);
    assert!(status.body["lastSuccessfulBackupAtUtc"].is_null());

    step("Take a backup while the service is running and the pharmacy is open");
    let created = json(
        original.address,
        "POST",
        "/api/v1/backups/create",
        json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let backup_id = created.body["backupId"]
        .as_str()
        .expect("backup id")
        .to_owned();
    let filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    println!("     produced {filename}");

    step("Confirm the reminder now reports the installation as protected");
    let status = json(
        original.address,
        "GET",
        "/api/v1/backups/status",
        Value::Null,
        Some(&cookie),
    )
    .await;
    assert_eq!(status.body["backupOverdue"], false);
    assert!(status.body["lastSuccessfulBackupAtUtc"].is_string());

    step("Download the backup through the service, as an owner would to an external drive");
    let resolved = json(
        original.address,
        "GET",
        &format!("/api/v1/backups/{backup_id}/download"),
        Value::Null,
        Some(&cookie),
    )
    .await;
    assert_eq!(resolved.status, 200, "{:?}", resolved.body);
    let url = resolved.body["url"].as_str().expect("url").to_owned();
    let downloaded = send(original.address, "GET", &url, None, b"", Some(&cookie)).await;
    assert_eq!(downloaded.status, 200);
    assert!(
        downloaded
            .headers
            .to_ascii_lowercase()
            .contains("content-disposition"),
        "the download was not offered as a file"
    );
    let archive = downloaded.bytes.clone();
    assert!(!archive.is_empty());
    println!("     {} bytes retrieved over HTTP", archive.len());

    step("Confirm the retrieved bytes are identical to the file on disk");
    let on_disk = std::fs::read(original.backups().join(&filename)).expect("backup file");
    assert_eq!(archive, on_disk, "the download is not the backup");

    step("Keep the backup somewhere the failing machine cannot take with it");
    let external_drive = temp.path().join("external-drive");
    std::fs::create_dir_all(&external_drive).expect("external drive");
    let off_machine = external_drive.join(&filename);
    std::fs::write(&off_machine, &archive).expect("copy to external drive");

    step("Record one more fact AFTER the backup, which a restore must correctly lose");
    record_supplier(original.address, &cookie, "Dev Traders").await;
    assert_eq!(supplier_names(&original.pool).await.len(), 4);

    // ---------------------------------------------------------------------------------------
    // Part three: the machine is lost
    // ---------------------------------------------------------------------------------------
    step("Shut the installation down");
    let original_root = original.stop().await;

    step("Destroy the machine: the database and everything beside it are gone");
    remove_directory_when_released(&original_root).await;
    assert!(!original_root.exists());

    step("Confirm the backup on the external drive survived the machine");
    assert!(off_machine.exists());
    assert_eq!(std::fs::read(&off_machine).expect("backup"), archive);

    // ---------------------------------------------------------------------------------------
    // Part four: a new machine, restored from the backup, before anybody has signed in
    // ---------------------------------------------------------------------------------------
    step("Start a brand-new installation, as a replacement PC would");
    let replacement_root = temp.path().join("replacement");
    let replacement = Installation::start(replacement_root).await;

    step("Confirm the replacement is blank and asks for first-run setup");
    let auth = json(
        replacement.address,
        "GET",
        "/api/v1/auth/status",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(auth.status, 200);
    assert_eq!(auth.body["setupRequired"], true);

    step("Refuse a file that is not a backup, before any of it is trusted");
    let rubbish = send(
        replacement.address,
        "POST",
        "/api/v1/setup/restore/prepare",
        Some("application/octet-stream"),
        b"a letter to the tax office, not a backup",
        None,
    )
    .await;
    assert_eq!(rubbish.status, 409, "{:?}", rubbish.body);
    println!("     refused as {}", rubbish.body["code"]);

    step("Refuse a backup whose contents were altered after it was made");
    let mut tampered = archive.clone();
    let position = tampered.len() - 1024;
    tampered[position] ^= 0xFF;
    let refused = send(
        replacement.address,
        "POST",
        "/api/v1/setup/restore/prepare",
        Some("application/octet-stream"),
        &tampered,
        None,
    )
    .await;
    assert_eq!(refused.status, 409);
    assert_eq!(refused.body["code"], "backup_checksum_mismatch");

    step("Confirm neither refusal left anything behind or changed the blank installation");
    assert!(supplier_names(&replacement.pool).await.is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(replacement.backups().join(".tmp"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "residue after a refusal: {leftovers:?}"
    );

    step("Upload the genuine backup from the external drive and have it proved");
    let prepared = send(
        replacement.address,
        "POST",
        "/api/v1/setup/restore/prepare",
        Some("application/octet-stream"),
        &std::fs::read(&off_machine).expect("backup"),
        None,
    )
    .await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    assert_eq!(
        prepared.body["report"]["storeDisplayName"],
        "Drill Pharmacy"
    );
    assert_eq!(prepared.body["report"]["checksumVerified"], true);
    assert_eq!(prepared.body["report"]["compatibility"], "ready");
    let token = prepared.body["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();
    println!(
        "     the file says: {} taken {}",
        prepared.body["report"]["storeDisplayName"], prepared.body["report"]["createdAtUtc"]
    );

    step("Commit the restore on the blank installation");
    let committed = json(
        replacement.address,
        "POST",
        "/api/v1/setup/restore/commit",
        json!({ "candidateToken": token }),
        None,
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    assert_eq!(committed.body["restartRequired"], true);

    step("Confirm the service refuses to carry on serving the database it replaced");
    let refused = json(
        replacement.address,
        "GET",
        "/api/v1/auth/status",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(refused.status, 503);
    assert_eq!(refused.body["code"], "service_restoring");

    step("Restart the replacement installation");
    let replacement_root = replacement.stop().await;
    let replacement = Installation::start(replacement_root).await;

    step("Confirm every fact recorded before the backup is present");
    let restored = supplier_names(&replacement.pool).await;
    assert_eq!(
        restored, expected,
        "the pharmacy did not come back as it was"
    );
    println!("     recovered {:?}", restored);

    step("Confirm the fact recorded after the backup is correctly absent");
    assert!(!restored.iter().any(|name| name == "Dev Traders"));

    step("Confirm the owner can sign in with the password they had before the machine failed");
    let login = json(
        replacement.address,
        "POST",
        "/api/v1/auth/login",
        json!({ "loginIdentifier": "drill.owner", "password": PASSWORD }),
        None,
    )
    .await;
    assert_eq!(login.status, 200, "{:?}", login.body);
    let cookie = cookie_of(&login.headers);

    step("Confirm every session issued before the restore is dead");
    // Exactly one live session: the one the sign-in above just created. Every session the backup
    // carried with it — issued on a machine that no longer exists — was revoked when the restore
    // completed, so anything above one would mean an inherited session survived.
    let live: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE revoked_at_utc IS NULL")
            .fetch_one(&replacement.pool)
            .await
            .expect("sessions");
    assert_eq!(live, 1, "a session from before the restore survived");
    let revoked: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE revoked_at_utc IS NOT NULL")
            .fetch_one(&replacement.pool)
            .await
            .expect("revoked sessions");
    assert!(
        revoked > 0,
        "the restore revoked nothing, so it inherited nothing to revoke"
    );

    step("Confirm the pharmacy's identity was kept and the installation's was re-issued");
    let store: String = sqlx::query_scalar("SELECT display_name FROM store_identity")
        .fetch_one(&replacement.pool)
        .await
        .expect("store");
    assert_eq!(store, "Drill Pharmacy");
    let (source, new_id): (String, String) =
        sqlx::query_as("SELECT source_installation_id,new_installation_id FROM restore_provenance")
            .fetch_one(&replacement.pool)
            .await
            .expect("provenance");
    assert_ne!(
        source, new_id,
        "the restored installation kept the old identity"
    );
    let current: String = sqlx::query_scalar("SELECT installation_id FROM installation_identity")
        .fetch_one(&replacement.pool)
        .await
        .expect("installation");
    assert_eq!(current, new_id);
    println!("     lineage recorded: {source} → {new_id}");

    step("Confirm the restore appears in business history");
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM master_change_events WHERE entity_type='restore_operation'",
    )
    .fetch_one(&replacement.pool)
    .await
    .expect("events");
    assert_eq!(events, 1);

    step("Confirm the restored installation is told to take a backup of its own");
    let status = json(
        replacement.address,
        "GET",
        "/api/v1/backups/status",
        Value::Null,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status.body["backupOverdue"], true,
        "a restored machine inherited another's backup age"
    );

    step("Confirm the restored database is structurally sound");
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&replacement.pool)
        .await
        .expect("integrity");
    assert_eq!(integrity, "ok");
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&replacement.pool)
        .await
        .expect("foreign keys");
    assert!(violations.is_empty());

    step("Trade on the restored installation, proving it is a working pharmacy and not an archive");
    record_supplier(replacement.address, &cookie, "Eknath Surgicals").await;
    assert_eq!(supplier_names(&replacement.pool).await.len(), 4);

    // ---------------------------------------------------------------------------------------
    // Part five: an established pharmacy restores over itself
    // ---------------------------------------------------------------------------------------
    step("Take a fresh backup of the now-established installation");
    let created = json(
        replacement.address,
        "POST",
        "/api/v1/backups/create",
        json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(created.status, 201, "{:?}", created.body);
    let second_filename = created.body["filename"]
        .as_str()
        .expect("filename")
        .to_owned();
    let second_archive =
        std::fs::read(replacement.backups().join(&second_filename)).expect("backup");

    step("Record a fact after that backup, to be deliberately discarded");
    record_supplier(replacement.address, &cookie, "Farhan Healthcare").await;
    assert_eq!(supplier_names(&replacement.pool).await.len(), 5);

    step("Refuse the unauthenticated first-run route now that the installation is in use");
    let refused = send(
        replacement.address,
        "POST",
        "/api/v1/setup/restore/prepare",
        Some("application/octet-stream"),
        &second_archive,
        None,
    )
    .await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.body["code"], "setup_already_complete");

    step("Prepare the restore as the signed-in owner");
    let prepared = send(
        replacement.address,
        "POST",
        "/api/v1/backups/restore/prepare",
        Some("application/octet-stream"),
        &second_archive,
        Some(&cookie),
    )
    .await;
    assert_eq!(prepared.status, 200, "{:?}", prepared.body);
    let token = prepared.body["candidateToken"]
        .as_str()
        .expect("token")
        .to_owned();

    step("Refuse the commit when the password is wrong");
    let refused = json(
        replacement.address,
        "POST",
        "/api/v1/backups/restore/commit",
        json!({ "candidateToken": token, "password": "Not-The-Password-1" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.body["code"], "invalid_password");

    step("Confirm the refusal changed nothing at all");
    assert_eq!(supplier_names(&replacement.pool).await.len(), 5);
    assert!(matches!(
        restore_journal::read(&replacement.backups()),
        JournalState::Absent
    ));

    step("Commit the restore with the owner's own password");
    let committed = json(
        replacement.address,
        "POST",
        "/api/v1/backups/restore/commit",
        json!({ "candidateToken": token, "password": PASSWORD }),
        Some(&cookie),
    )
    .await;
    assert_eq!(committed.status, 200, "{:?}", committed.body);
    let safety = committed.body["safetyBackup"]
        .as_str()
        .expect("safety backup")
        .to_owned();

    step("Confirm a safety copy of the replaced data was taken and can be read back");
    let safety_path = replacement.backups().join(&safety);
    assert!(safety_path.exists());
    let safety_bytes = std::fs::read(&safety_path).expect("safety backup");
    let mut cursor = std::io::Cursor::new(safety_bytes.clone());
    let header = aushadharth_store_service::domain::backup::read_header(
        &mut cursor,
        safety_bytes.len() as u64,
    )
    .expect("safety header");
    let manifest = aushadharth_store_service::domain::backup::read_manifest(&mut cursor, &header)
        .expect("safety manifest");
    assert_eq!(manifest.backup_kind, "pre_restore_safety");
    println!("     safety copy {safety} is a readable backup of the replaced data");

    step("Restart after the restore");
    let replacement_root = replacement.stop().await;
    let replacement = Installation::start(replacement_root).await;

    step("Confirm the deliberately discarded fact is gone and the rest remains");
    let after = supplier_names(&replacement.pool).await;
    assert_eq!(after.len(), 4);
    assert!(!after.iter().any(|name| name == "Farhan Healthcare"));

    step("Confirm the lineage now records both restores");
    let lineage: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM restore_provenance")
        .fetch_one(&replacement.pool)
        .await
        .expect("provenance");
    assert_eq!(lineage, 2);

    // ---------------------------------------------------------------------------------------
    // Part six: interruptions, at each moment where one is dangerous
    // ---------------------------------------------------------------------------------------
    step("Interrupt before anything has moved, and confirm nothing was touched");
    let root = replacement.stop().await;
    let before =
        std::fs::read(root.join("database").join("aushadharth.sqlite3")).expect("database");
    let scratch = root.join("backups").join(".tmp");
    std::fs::create_dir_all(&scratch).expect("scratch");
    let candidate = scratch.join("interrupted.candidate");
    std::fs::write(&candidate, b"a candidate nobody committed").expect("candidate");
    let journal = journal_at(
        root.join("database").join("aushadharth.sqlite3"),
        RestoreStage::SafetyTaken,
        candidate.clone(),
        scratch.join("interrupted.superseded"),
    );
    restore_journal::write(&root.join("backups"), &journal).expect("journal");
    let replacement = Installation::start(root).await;
    assert!(!candidate.exists(), "the abandoned candidate was kept");
    assert_eq!(
        std::fs::read(replacement.database_path())
            .expect("database")
            .len(),
        before.len()
    );
    assert_eq!(supplier_names(&replacement.pool).await.len(), 4);

    step("Interrupt between the two renames, and confirm the original comes back");
    let root = replacement.stop().await;
    let database_path = root.join("database").join("aushadharth.sqlite3");
    let scratch = root.join("backups").join(".tmp");
    let superseded = scratch.join("halfway.superseded");
    rename_when_released(&database_path, &superseded).await;
    let candidate = scratch.join("halfway.candidate");
    std::fs::write(&candidate, b"a candidate that never arrived").expect("candidate");
    let journal = journal_at(
        database_path.clone(),
        RestoreStage::OldMoved,
        candidate.clone(),
        superseded.clone(),
    );
    restore_journal::write(&root.join("backups"), &journal).expect("journal");
    assert!(
        !database_path.exists(),
        "the drill did not actually move the database"
    );
    let replacement = Installation::start(root).await;
    assert_eq!(
        supplier_names(&replacement.pool).await.len(),
        4,
        "the rollback lost data"
    );
    assert!(!superseded.exists());
    assert!(!candidate.exists());

    step("Interrupt with an unreadable journal, and confirm the service still recovers");
    let root = replacement.stop().await;
    std::fs::write(
        restore_journal::journal_path(&root.join("backups")),
        b"{ this journal was truncated by the power cut",
    )
    .expect("corrupt journal");
    let replacement = Installation::start(root).await;
    assert_eq!(supplier_names(&replacement.pool).await.len(), 4);

    step("Confirm a missing database with nothing recoverable makes the service refuse to start");
    let root = replacement.stop().await;
    let database_path = root.join("database").join("aushadharth.sqlite3");
    let rescued = root.join("rescued.sqlite3");
    rename_when_released(&database_path, &rescued).await;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = database_path.clone().into_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
    let journal = journal_at(
        database_path.clone(),
        RestoreStage::OldMoved,
        root.join("backups").join(".tmp").join("nothing.candidate"),
        root.join("backups").join(".tmp").join("nothing.superseded"),
    );
    restore_journal::write(&root.join("backups"), &journal).expect("journal");
    let outcome =
        api::backups::recover_interrupted_restore(&root.join("backups"), &database_path).await;
    assert!(
        outcome.is_err(),
        "the service would have started over a missing database and created an empty pharmacy"
    );
    println!("     refused to start: {}", outcome.unwrap_err());

    step("Recover the set-aside database and confirm the pharmacy is still all there");
    rename_when_released(&rescued, &database_path).await;
    restore_journal::clear(&root.join("backups")).expect("clear journal");
    let replacement = Installation::start(root).await;
    let final_names = supplier_names(&replacement.pool).await;
    assert_eq!(
        final_names,
        [
            "Aditya Distributors",
            "Bharat Medical Agency",
            "Chandra Pharma",
            "Eknath Surgicals"
        ]
    );

    step("Drill complete: the pharmacy survived a lost machine and four interruptions");
    println!("     final state {:?}", final_names);
    let _ = replacement.stop().await;
    assert_eq!(
        STEP.load(std::sync::atomic::Ordering::SeqCst),
        50,
        "the drill did not run the full sequence"
    );
}
