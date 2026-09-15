//! Backup and restore.
//!
//! The pharmacy's whole record lives in one SQLite file, so this module is the difference between a
//! disk failure being an afternoon's inconvenience and the end of the business. Four rules shape it.
//!
//! 1. **The live database is never touched until a candidate has proved itself.** Framing, checksum,
//!    product identity, migration chain, `integrity_check`, `foreign_key_check` and a trial forward
//!    migration all happen on a copy. A restore that is going to fail fails before anything moves.
//! 2. **The browser never names a path.** Backups are addressed by id, candidates by opaque token.
//!    Nothing a caller sends is ever joined onto a filesystem path.
//! 3. **Nothing large is ever held in memory.** Uploads, packaging, hashing and downloads all move
//!    in fixed chunks, so a two-gigabyte backup costs the same RAM as a small one.
//! 4. **The swap is journalled outside the database it replaces.** A crash between two renames is
//!    recoverable precisely because the record of what was happening does not live in the file being
//!    replaced.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::APPLICATION_VERSION;
use crate::domain::backup::{self, AppliedMigration, ContainerError, MAX_PAYLOAD_BYTES, Manifest};
use crate::infrastructure::database;
use crate::platform::restore_journal::{
    self, JournalState, Recovery, RestoreJournal, RestoreStage,
};

/// How long a prepared candidate waits for its commit before it is swept away.
pub const CANDIDATE_TTL_SECONDS: i64 = 30 * 60;

/// How many verifiably-good safety backups are kept.
pub const SAFETY_BACKUP_RETENTION: usize = 5;

// ---------------------------------------------------------------------------------------------
// Service state
// ---------------------------------------------------------------------------------------------

/// Everything backup and restore own on disk, plus the in-flight work.
///
/// Held behind an `Option` on the shared state: a router built without it simply has no backup
/// routes that can do anything, which is what keeps the test suite from ever writing into a real
/// pharmacy's data directory by accident.
#[derive(Debug)]
pub struct BackupService {
    pub backups_directory: PathBuf,
    pub database_path: PathBuf,
    jobs: Mutex<HashMap<String, BackupJob>>,
    candidates: Mutex<HashMap<String, Candidate>>,
    /// Set for the duration of a destructive restore so business writes can be refused.
    restoring: AtomicBool,
    /// Exactly one restore or first-run setup may be in flight at a time. An atomic rather than a
    /// lock because the gate has to be held across `await` points, and a guard that cannot cross an
    /// await is a guard that gets dropped early.
    restore_gate: AtomicBool,
}

impl BackupService {
    pub fn new(backups_directory: PathBuf, database_path: PathBuf) -> Self {
        Self {
            backups_directory,
            database_path,
            jobs: Mutex::new(HashMap::new()),
            candidates: Mutex::new(HashMap::new()),
            restoring: AtomicBool::new(false),
            restore_gate: AtomicBool::new(false),
        }
    }

    pub fn is_restoring(&self) -> bool {
        self.restoring.load(Ordering::SeqCst)
    }

    fn temp_directory(&self) -> PathBuf {
        self.backups_directory.join(".tmp")
    }

    fn package_path(&self, filename: &str) -> PathBuf {
        self.backups_directory.join(filename)
    }
}

/// A backup being produced. Stages are named rather than given a fabricated percentage: `VACUUM
/// INTO` cannot report its own progress without a new dependency, and a bar that moves on a timer
/// is a lie told to somebody waiting on their data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupJob {
    pub job_id: String,
    pub stage: &'static str,
    pub backup_id: Option<String>,
    pub filename: Option<String>,
    pub bytes: Option<u64>,
    pub error_code: Option<&'static str>,
}

#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    manifest: Manifest,
    prepared_by_user_id: Option<String>,
    expires_at_unix: i64,
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum BackupError {
    Auth(AuthError),
    Validation(&'static str, &'static str),
    FormatUnsupported,
    Corrupt,
    ProductMismatch,
    InvalidDatabase,
    TooNew,
    PartiallyMigrated,
    ChecksumMismatch,
    TooLarge,
    InsufficientDiskSpace,
    CandidateNotFound,
    CandidateExpired,
    NotFound,
    SetupAlreadyComplete,
    RestoreInProgress,
    RestoreFailed,
    InvalidPassword,
    Unavailable,
    Internal,
}

impl From<AuthError> for BackupError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
}

impl From<ContainerError> for BackupError {
    fn from(error: ContainerError) -> Self {
        match error {
            ContainerError::TooSmall | ContainerError::Malformed(_) => Self::Corrupt,
            ContainerError::NotABackup => Self::ProductMismatch,
            ContainerError::UnsupportedFormatVersion(_) => Self::FormatUnsupported,
            ContainerError::ManifestUnreadable => Self::Corrupt,
            ContainerError::ChecksumMismatch => Self::ChecksumMismatch,
            ContainerError::Io(_) => Self::Internal,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    issues: Vec<ErrorIssue>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorIssue {
    field: String,
    message: String,
}

fn simple(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
    }
}

impl IntoResponse for BackupError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Validation(field, message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorBody {
                    code: "validation_failed",
                    message: "The request failed validation.",
                    issues: vec![ErrorIssue {
                        field: field.to_owned(),
                        message: message.to_owned(),
                    }],
                },
            ),
            Self::FormatUnsupported => (
                StatusCode::CONFLICT,
                simple(
                    "backup_format_unsupported",
                    "This backup was made by a newer version of AUSHADHARTH.",
                ),
            ),
            Self::Corrupt => (
                StatusCode::CONFLICT,
                simple("backup_corrupt", "This backup file is damaged."),
            ),
            Self::ProductMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "backup_product_mismatch",
                    "That is not an AUSHADHARTH backup.",
                ),
            ),
            Self::InvalidDatabase => (
                StatusCode::CONFLICT,
                simple(
                    "backup_invalid_database",
                    "This backup does not contain a usable AUSHADHARTH database.",
                ),
            ),
            Self::TooNew => (
                StatusCode::CONFLICT,
                simple(
                    "backup_too_new",
                    "This backup was made by a newer version of AUSHADHARTH.",
                ),
            ),
            Self::PartiallyMigrated => (
                StatusCode::CONFLICT,
                simple(
                    "backup_partially_migrated",
                    "This backup was interrupted while being upgraded and cannot be used.",
                ),
            ),
            Self::ChecksumMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "backup_checksum_mismatch",
                    "This backup is damaged: its contents do not match its own record.",
                ),
            ),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                simple("backup_too_large", "That file is too large to be a backup."),
            ),
            Self::InsufficientDiskSpace => (
                StatusCode::CONFLICT,
                simple(
                    "insufficient_disk_space",
                    "There is not enough disk space to complete this safely.",
                ),
            ),
            Self::CandidateNotFound => (
                StatusCode::CONFLICT,
                simple(
                    "candidate_not_found",
                    "That restore is no longer ready. Choose the backup file again.",
                ),
            ),
            Self::CandidateExpired => (
                StatusCode::CONFLICT,
                simple(
                    "candidate_expired",
                    "That restore was prepared too long ago. Choose the backup file again.",
                ),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("backup_not_found", "That backup was not found."),
            ),
            Self::SetupAlreadyComplete => (
                StatusCode::FORBIDDEN,
                simple(
                    "setup_already_complete",
                    "This installation is already set up. Sign in to restore a backup.",
                ),
            ),
            Self::RestoreInProgress => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    "service_restoring",
                    "AUSHADHARTH is restoring a backup. Please wait.",
                ),
            ),
            Self::RestoreFailed => (
                StatusCode::CONFLICT,
                simple(
                    "restore_failed",
                    "The restore could not be completed. Your existing data has been kept.",
                ),
            ),
            Self::InvalidPassword => (
                StatusCode::FORBIDDEN,
                simple("invalid_password", "That password is not correct."),
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    "backup_unavailable",
                    "Backup is not available in this configuration.",
                ),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple("internal_error", "The request could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn io_failure(error: &std::io::Error) -> BackupError {
    // A full disk and a refused write are both operator-actionable and must not read as a bug.
    match error.kind() {
        std::io::ErrorKind::StorageFull => BackupError::InsufficientDiskSpace,
        std::io::ErrorKind::PermissionDenied => BackupError::Validation(
            "destination",
            "AUSHADHARTH could not write to its data folder",
        ),
        _ if error.raw_os_error() == Some(112) => BackupError::InsufficientDiskSpace,
        _ => BackupError::Internal,
    }
}

// ---------------------------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------------------------

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/backups/status", get(backup_status))
        .route("/api/v1/backups/create", post(create_backup))
        .route("/api/v1/backups/jobs/{id}", get(job_status))
        .route("/api/v1/backups/{id}/download", get(download_backup))
        .route(
            "/api/v1/backups/inspect",
            post(inspect_backup).layer(axum::extract::DefaultBodyLimit::max(
                MAX_PAYLOAD_BYTES as usize,
            )),
        )
        .route(
            "/api/v1/backups/restore/prepare",
            post(restore_prepare).layer(axum::extract::DefaultBodyLimit::max(
                MAX_PAYLOAD_BYTES as usize,
            )),
        )
        .route("/api/v1/backups/restore/commit", post(restore_commit))
        .route(
            "/api/v1/setup/restore/prepare",
            post(setup_restore_prepare).layer(axum::extract::DefaultBodyLimit::max(
                MAX_PAYLOAD_BYTES as usize,
            )),
        )
        .route("/api/v1/setup/restore/commit", post(setup_restore_commit))
}

/// The JSON mutation guard's content-type rule is part of the CSRF defence: a cross-site HTML form
/// can only send form encodings, so demanding JSON shuts that door. A binary upload cannot demand
/// JSON, so it demands `application/octet-stream` instead — which is equally un-form-submittable,
/// and keeps every other check the JSON guard makes.
fn validate_binary_mutation(headers: &HeaderMap) -> Result<(), BackupError> {
    auth::validate_binary_mutation_request(headers)?;
    Ok(())
}

async fn require_owner(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, BackupError> {
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

fn service(state: &ReferenceState) -> Result<Arc<BackupService>, BackupError> {
    state.backups.clone().ok_or(BackupError::Unavailable)
}

// ---------------------------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupSummary {
    backup_id: String,
    filename: String,
    created_at_utc: String,
    bytes: u64,
    backup_kind: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    /// Installation-local, never restored: a backup taken on another machine must not make this one
    /// look safe.
    last_successful_backup_at_utc: Option<String>,
    reminder_threshold_days: i64,
    backup_overdue: bool,
    backups: Vec<BackupSummary>,
}

async fn backup_status(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<StatusResponse>, BackupError> {
    require_owner(&state, &headers).await?;
    let service = service(&state)?;
    let reminder = read_reminder(&service.backups_directory);
    let backups = list_backups(&service);
    let overdue = match &reminder.last_successful_backup_at_utc {
        None => true,
        Some(stamp) => age_days(stamp) >= reminder.reminder_threshold_days,
    };
    Ok(Json(StatusResponse {
        last_successful_backup_at_utc: reminder.last_successful_backup_at_utc,
        reminder_threshold_days: reminder.reminder_threshold_days,
        backup_overdue: overdue,
        backups,
    }))
}

/// Installation-local reminder state.
///
/// Deliberately a file rather than a table. Restoring a six-month-old backup onto a new machine must
/// not restore "last backup: yesterday" along with it — the freshness of a backup is a fact about
/// this installation, not about the business.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderState {
    pub last_successful_backup_at_utc: Option<String>,
    pub reminder_threshold_days: i64,
}

impl Default for ReminderState {
    fn default() -> Self {
        Self {
            last_successful_backup_at_utc: None,
            reminder_threshold_days: 7,
        }
    }
}

fn reminder_path(backups_directory: &Path) -> PathBuf {
    backups_directory
        .parent()
        .map(|root| root.join("configuration"))
        .unwrap_or_else(|| backups_directory.to_path_buf())
        .join("backup-state.json")
}

pub fn read_reminder(backups_directory: &Path) -> ReminderState {
    std::fs::read(reminder_path(backups_directory))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_reminder(backups_directory: &Path, state: &ReminderState) -> std::io::Result<()> {
    let path = reminder_path(backups_directory);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.writing");
    std::fs::write(&temp, serde_json::to_vec_pretty(state).unwrap_or_default())?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

fn age_days(stamp: &str) -> i64 {
    // Dates are ISO-8601 UTC throughout; comparing the date part is enough for a reminder and
    // avoids pulling a calendar dependency in for something measured in days.
    let today = time::OffsetDateTime::now_utc().date();
    match time::Date::parse(
        stamp.get(0..10).unwrap_or(""),
        &time::macros::format_description!("[year]-[month]-[day]"),
    ) {
        Ok(then) => (today - then).whole_days(),
        Err(_) => i64::MAX,
    }
}

fn list_backups(service: &BackupService) -> Vec<BackupSummary> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&service.backups_directory) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("aushbackup") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(header) = backup::read_header(&mut file, metadata.len()) else {
            continue;
        };
        let Ok(manifest) = backup::read_manifest(&mut file, &header) else {
            continue;
        };
        found.push(BackupSummary {
            backup_id: manifest.backup_id,
            filename: path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned(),
            created_at_utc: manifest.created_at_utc,
            bytes: metadata.len(),
            backup_kind: manifest.backup_kind,
        });
    }
    found.sort_by(|left, right| right.created_at_utc.cmp(&left.created_at_utc));
    found
}

// ---------------------------------------------------------------------------------------------
// Creating a backup
// ---------------------------------------------------------------------------------------------

async fn create_backup(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<BackupJob>), BackupError> {
    auth::validate_mutation_request(&headers)?;
    let actor = require_owner(&state, &headers).await?;
    let service = service(&state)?;
    if service.is_restoring() {
        return Err(BackupError::RestoreInProgress);
    }
    let job_id = Uuid::now_v7().to_string();

    // Small enough to run inline: measured at 31 ms per megabyte, a year of pharmacy data completes
    // well inside a request. The job shape is kept so a future large-database path can move it to a
    // task without changing the API the browser already speaks.
    let outcome = produce_backup(&state.pool, &service, "manual").await;
    let job = match outcome {
        Ok(result) => {
            record_backup_event(&state.pool, &result, Some(&actor.id)).await;
            let _ = write_reminder(
                &service.backups_directory,
                &ReminderState {
                    last_successful_backup_at_utc: Some(result.manifest.created_at_utc.clone()),
                    reminder_threshold_days: read_reminder(&service.backups_directory)
                        .reminder_threshold_days,
                },
            );
            BackupJob {
                job_id: job_id.clone(),
                stage: "ready",
                backup_id: Some(result.manifest.backup_id.clone()),
                filename: Some(result.filename.clone()),
                bytes: Some(result.bytes),
                error_code: None,
            }
        }
        Err(error) => {
            // A failed backup changed nothing, so it belongs in the diagnostic log rather than in
            // immutable business history.
            tracing::warn!(target: "backup", "backup failed");
            return Err(error);
        }
    };
    service
        .jobs
        .lock()
        .expect("jobs")
        .insert(job_id, job.clone());
    Ok((StatusCode::CREATED, Json(job)))
}

async fn job_status(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<BackupJob>, BackupError> {
    require_owner(&state, &headers).await?;
    let service = service(&state)?;
    let jobs = service.jobs.lock().expect("jobs");
    jobs.get(&id)
        .cloned()
        .map(Json)
        .ok_or(BackupError::NotFound)
}

pub struct ProducedBackup {
    pub manifest: Manifest,
    pub filename: String,
    pub bytes: u64,
    pub path: PathBuf,
}

/// Snapshots the live database and packages it, without ever copying the live file.
///
/// `VACUUM INTO` runs in a read transaction, so the snapshot is the committed state at its start and
/// the counter can keep billing throughout. It also writes a single self-contained file with no WAL
/// sidecar, which is exactly what a backup needs and exactly what copying the live file would not
/// give.
pub(crate) async fn produce_backup(
    pool: &SqlitePool,
    service: &BackupService,
    kind: &str,
) -> Result<ProducedBackup, BackupError> {
    let temp = service.temp_directory();
    std::fs::create_dir_all(&temp).map_err(|error| io_failure(&error))?;
    let backup_id = Uuid::now_v7().to_string();
    let snapshot_path = temp.join(format!("{backup_id}.snapshot"));
    let _ = std::fs::remove_file(&snapshot_path);

    // The path is service-generated, never caller-supplied; quotes are doubled regardless because a
    // literal is a literal.
    let literal = snapshot_path.to_string_lossy().replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{literal}'"))
        .execute(pool)
        .await
        .map_err(|_| BackupError::Internal)?;

    let result = package_snapshot(pool, service, &snapshot_path, &backup_id, kind).await;
    let _ = std::fs::remove_file(&snapshot_path);
    result
}

async fn package_snapshot(
    pool: &SqlitePool,
    service: &BackupService,
    snapshot_path: &Path,
    backup_id: &str,
    kind: &str,
) -> Result<ProducedBackup, BackupError> {
    // Validate what was produced before anybody is told it is a backup.
    let snapshot = database::open_existing_unmigrated(snapshot_path)
        .await
        .map_err(|_| BackupError::Internal)?;
    let quick: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&snapshot)
        .await
        .map_err(|_| BackupError::Internal)?;
    if quick != "ok" {
        snapshot.close().await;
        return Err(BackupError::InvalidDatabase);
    }
    let applied = read_applied_migrations(&snapshot).await?;
    let schema_version = applied.iter().map(|row| row.version).max().unwrap_or(0);
    let installation_id: Option<String> =
        sqlx::query_scalar("SELECT installation_id FROM installation_identity LIMIT 1")
            .fetch_optional(&snapshot)
            .await
            .map_err(|_| BackupError::Internal)?;
    let store: Option<(String, String)> =
        sqlx::query_as("SELECT store_id,display_name FROM store_identity LIMIT 1")
            .fetch_optional(&snapshot)
            .await
            .map_err(|_| BackupError::Internal)?;
    // Folded and closed before the file is hashed: a snapshot read mid-checkpoint would produce
    // a digest of something that is not what the backup actually contains.
    database::checkpoint_and_close(&snapshot)
        .await
        .map_err(|_| BackupError::Internal)?;

    let (digest, bytes) = backup::hash_file(snapshot_path)?;
    let created_at_utc: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(pool)
        .await
        .map_err(|_| BackupError::Internal)?;
    let (store_id, store_name) = store.unwrap_or_else(|| (String::new(), String::new()));

    let manifest = Manifest {
        backup_format_version: backup::FORMAT_VERSION,
        product: "AUSHADHARTH".to_owned(),
        backup_id: backup_id.to_owned(),
        created_at_utc: created_at_utc.clone(),
        application_version: APPLICATION_VERSION.to_owned(),
        schema_version,
        applied_migrations: applied,
        installation_id: installation_id.unwrap_or_default(),
        store_id,
        store_display_name: store_name.clone(),
        database_sha256: digest,
        database_bytes: bytes,
        backup_kind: kind.to_owned(),
        source_platform: std::env::consts::OS.to_owned(),
        attachments: Vec::new(),
    };

    let filename = backup::backup_filename(&store_name, &local_stamp(&created_at_utc), backup_id);
    let final_path = service.package_path(&filename);
    if final_path.exists() {
        // Names are derived, not chosen, so a collision means a bug rather than an operator
        // mistake — and replacing an existing backup is the one outcome that cannot be undone.
        return Err(BackupError::Internal);
    }
    let partial_path = service
        .temp_directory()
        .join(format!("{backup_id}.aushbackup.partial"));
    {
        let mut file = std::fs::File::create(&partial_path).map_err(|error| io_failure(&error))?;
        backup::write_container(&mut file, &manifest, snapshot_path).map_err(
            |error| match error {
                ContainerError::Io(message) => io_failure(&std::io::Error::other(message)),
                other => BackupError::from(other),
            },
        )?;
        use std::io::Write;
        file.flush().map_err(|error| io_failure(&error))?;
        file.sync_all().map_err(|error| io_failure(&error))?;
    }
    // Only now does it earn the real name. A partial file never carries it.
    std::fs::rename(&partial_path, &final_path).map_err(|error| io_failure(&error))?;
    let package_bytes = std::fs::metadata(&final_path)
        .map_err(|error| io_failure(&error))?
        .len();

    Ok(ProducedBackup {
        manifest,
        filename,
        bytes: package_bytes,
        path: final_path,
    })
}

/// `YYYYMMDD-HHMMSS` from an ISO stamp, for the filename only.
fn local_stamp(created_at_utc: &str) -> String {
    let digits: String = created_at_utc
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.len() >= 14 {
        format!("{}-{}", &digits[0..8], &digits[8..14])
    } else {
        digits
    }
}

async fn read_applied_migrations(pool: &SqlitePool) -> Result<Vec<AppliedMigration>, BackupError> {
    let rows: Vec<(i64, String, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT version,description,checksum,success FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| BackupError::InvalidDatabase)?;
    if rows.iter().any(|row| row.3 == 0) {
        return Err(BackupError::PartiallyMigrated);
    }
    Ok(rows
        .into_iter()
        .map(|(version, description, checksum, _)| AppliedMigration {
            version,
            description,
            checksum: database::hex_lower(&checksum),
        })
        .collect())
}

async fn record_backup_event(pool: &SqlitePool, result: &ProducedBackup, actor: Option<&str>) {
    // Safe metadata only: no destination path (the browser chooses it and the service never learns
    // it), no GSTIN, and only a digest prefix — enough to correlate a file with this row, not enough
    // to be mistaken for the integrity guarantee that lives in the manifest.
    let payload = json!({
        "backupKind": result.manifest.backup_kind,
        "schemaVersion": result.manifest.schema_version,
        "bytes": result.bytes,
        "digestPrefix": result.manifest.database_sha256.chars().take(12).collect::<String>(),
    });
    let _ = sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'backup',?,1,'created',strftime('%Y-%m-%dT%H:%M:%fZ','now'),1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&result.manifest.backup_id)
    .bind(payload.to_string())
    .bind(actor)
    .execute(pool)
    .await;
}

// ---------------------------------------------------------------------------------------------
// Downloading
// ---------------------------------------------------------------------------------------------

/// Resolves a backup id to the filename the file route will serve.
///
/// The id is matched against manifests that were read from disk, never joined onto a path, so a
/// caller cannot steer this at another file. The bytes themselves are served by a `ServeDir`
/// mounted in `api::router`, which streams in bounded chunks and refuses traversal on its own.
async fn download_backup(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, BackupError> {
    require_owner(&state, &headers).await?;
    let service = service(&state)?;
    let summary = list_backups(&service)
        .into_iter()
        .find(|row| row.backup_id == id)
        .ok_or(BackupError::NotFound)?;
    let body = serde_json::to_string(&json!({
        "backupId": summary.backup_id,
        "filename": summary.filename,
        "bytes": summary.bytes,
        "url": format!("/api/v1/backup-files/{}", summary.filename),
    }))
    .map_err(|_| BackupError::Internal)?;
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

/// Refuses every request once the live database has been replaced and the service is waiting to
/// restart.
///
/// Without it the next request reaches a closed pool and comes back as an internal error, which
/// tells an operator nothing about what is happening to their pharmacy. `service_restoring` is
/// what the browser needs to show the right screen and stop asking.
pub(crate) async fn guard_while_restoring(
    State(state): State<ReferenceState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if state
        .backups
        .as_ref()
        .is_some_and(|service| service.is_restoring())
    {
        return BackupError::RestoreInProgress.into_response();
    }
    next.run(request).await
}

/// Guards the streamed file route.
///
/// `ServeDir` knows nothing about sessions, so the owner check happens here before the request
/// reaches it, and the download gets the disposition header on the way back out.
pub(crate) async fn guard_backup_files(
    State(state): State<ReferenceState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let headers = request.headers().clone();
    if let Err(error) = require_owner(&state, &headers).await {
        return error.into_response();
    }
    let filename = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut response = next.run(request).await;
    if response.status().is_success() {
        let disposition = format!("attachment; filename=\"{filename}\"");
        if let Ok(value) = HeaderValue::from_str(&disposition) {
            response
                .headers_mut()
                .insert(header::CONTENT_DISPOSITION, value);
        }
    }
    response
}

// ---------------------------------------------------------------------------------------------
// Streaming upload
// ---------------------------------------------------------------------------------------------

use axum::body::HttpBody;

/// Writes a request body to disk in frames, never holding the whole upload.
///
/// `axum::body::HttpBody` is the re-exported `http_body::Body`, so frames can be pulled one at a
/// time with nothing new added to the dependency set. The limit is enforced as bytes arrive rather
/// than after the fact: an oversized upload is cut off mid-flight instead of being written out in
/// full and then refused.
async fn read_body_to_file(
    mut body: Body,
    destination: &Path,
    limit: u64,
) -> Result<u64, BackupError> {
    use std::io::Write;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_failure(&error))?;
    }
    let mut file = std::fs::File::create(destination).map_err(|error| io_failure(&error))?;
    let mut written = 0_u64;
    loop {
        let frame =
            std::future::poll_fn(|context| std::pin::Pin::new(&mut body).poll_frame(context)).await;
        match frame {
            None => break,
            // A connection that dies mid-upload leaves a short file; it is not a backup, and the
            // container framing would reject it anyway. Saying so here is clearer.
            Some(Err(_)) => return Err(BackupError::Corrupt),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    written = written
                        .checked_add(data.len() as u64)
                        .ok_or(BackupError::TooLarge)?;
                    if written > limit {
                        return Err(BackupError::TooLarge);
                    }
                    file.write_all(&data).map_err(|error| io_failure(&error))?;
                }
            }
        }
    }
    file.flush().map_err(|error| io_failure(&error))?;
    if written == 0 {
        return Err(BackupError::Corrupt);
    }
    Ok(written)
}

/// Reads and discards an upload that is about to be refused.
///
/// A server that answers before reading the body leaves unread bytes in the socket, and closing on
/// top of them makes Windows send a reset that destroys the response the client never collected —
/// so a refusal an operator needed to read arrives as "connection lost". Discarding the frames
/// first costs nothing on the path that matters (a permitted upload is read anyway) and turns every
/// refusal into an answer the browser can actually show.
async fn drain_body(mut body: Body) {
    let mut seen = 0_u64;
    while let Some(Ok(frame)) =
        std::future::poll_fn(|context| std::pin::Pin::new(&mut body).poll_frame(context)).await
    {
        if let Ok(data) = frame.into_data() {
            seen = seen.saturating_add(data.len() as u64);
            if seen > MAX_PAYLOAD_BYTES {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Inspecting and validating a candidate
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectionReport {
    pub product: String,
    pub backup_format_version: u16,
    pub store_display_name: String,
    pub created_at_utc: String,
    pub application_version: String,
    pub schema_version: i64,
    pub current_schema_version: i64,
    pub bytes: u64,
    pub checksum_verified: bool,
    /// `ready` or `upgrade_required`.
    pub compatibility: &'static str,
    pub migration_required: bool,
}

/// Unpacks a container to a bare SQLite file and checks everything that can be checked.
///
/// The live database is never touched: the payload goes to its own temp file and every read below
/// is against that copy. The checksum is verified during unpacking by `stream_payload`, so a report
/// only ever exists for a file whose contents match its own manifest.
async fn unpack_and_check(
    package_path: &Path,
    candidate_path: &Path,
) -> Result<(Manifest, InspectionReport), BackupError> {
    let metadata = std::fs::metadata(package_path).map_err(|error| io_failure(&error))?;
    let mut file = std::fs::File::open(package_path).map_err(|error| io_failure(&error))?;
    let header = backup::read_header(&mut file, metadata.len())?;
    let manifest = backup::read_manifest(&mut file, &header)?;
    // Checked before unpacking: refusing somebody else's file should not cost a payload copy.
    if manifest.product != "AUSHADHARTH" {
        return Err(BackupError::ProductMismatch);
    }
    {
        let mut out = std::fs::File::create(candidate_path).map_err(|error| io_failure(&error))?;
        backup::stream_payload(&mut file, &header, &manifest, &mut out)?;
    }

    candidate_compatibility(candidate_path, &manifest).await?;
    let current = database::latest_schema_version();
    let migration_required = manifest.schema_version < current;
    let report = InspectionReport {
        product: manifest.product.clone(),
        backup_format_version: manifest.backup_format_version,
        store_display_name: manifest.store_display_name.clone(),
        created_at_utc: manifest.created_at_utc.clone(),
        application_version: manifest.application_version.clone(),
        schema_version: manifest.schema_version,
        current_schema_version: current,
        bytes: metadata.len(),
        checksum_verified: true,
        compatibility: if migration_required {
            "upgrade_required"
        } else {
            "ready"
        },
        migration_required,
    };
    Ok((manifest, report))
}

/// Decides whether a candidate is ours, and whether this build may move it forward.
///
/// Identity rests on two things. `application_id` settles it in one header read for anything taken
/// after the backup migration. `0` is accepted because databases older than that migration are
/// legitimately ours and carry no marker — for those the migration checksum chain does the work.
/// Any other non-zero value is somebody else's SQLite database wearing our file extension.
async fn candidate_compatibility(
    candidate_path: &Path,
    manifest: &Manifest,
) -> Result<(), BackupError> {
    let pool = database::open_existing_unmigrated(candidate_path)
        .await
        .map_err(|_| BackupError::InvalidDatabase)?;
    let result = candidate_compatibility_on(&pool, manifest).await;
    let closed = database::checkpoint_and_close(&pool).await;
    result.and(closed.map_err(|_| BackupError::InvalidDatabase))
}

pub(crate) async fn candidate_compatibility_on(
    pool: &SqlitePool,
    manifest: &Manifest,
) -> Result<(), BackupError> {
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(pool)
        .await
        .map_err(|_| BackupError::InvalidDatabase)?;
    if application_id != 0 && application_id != backup::APPLICATION_ID {
        return Err(BackupError::ProductMismatch);
    }

    let applied = read_applied_migrations(pool).await?;
    if applied.is_empty() {
        return Err(BackupError::InvalidDatabase);
    }
    let embedded = database::embedded_migrations();
    for row in &applied {
        match embedded.iter().find(|known| known.version == row.version) {
            // A migration this build has never heard of: the backup came from a newer AUSHADHARTH,
            // and downgrading a schema is not something this product will attempt.
            None => return Err(BackupError::TooNew),
            // Same number, different content — a fork, or a tampered migration. Either way, not a
            // database this build can reason about.
            Some(known) if known.checksum != row.checksum => {
                return Err(BackupError::ProductMismatch);
            }
            Some(_) => {}
        }
    }
    // A legacy database with no application_id has only the chain to prove itself with, so its
    // first migration must be the one this product was born with.
    if application_id == 0
        && !applied
            .first()
            .zip(embedded.first())
            .is_some_and(|(found, known)| found.checksum == known.checksum)
    {
        return Err(BackupError::ProductMismatch);
    }
    // The manifest is metadata; the database is the fact. Disagreement means the file was edited.
    if manifest.schema_version != applied.iter().map(|row| row.version).max().unwrap_or(0) {
        return Err(BackupError::Corrupt);
    }

    structural_check(pool, BackupError::InvalidDatabase).await
}

/// `integrity_check` plus `foreign_key_check`: the two questions SQLite can answer about whether a
/// file is a working database rather than merely a parseable one.
async fn structural_check(pool: &SqlitePool, failure: BackupError) -> Result<(), BackupError> {
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(pool)
        .await
        .map_err(|_| BackupError::InvalidDatabase)?;
    if integrity != "ok" {
        return Err(failure);
    }
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .map_err(|_| BackupError::InvalidDatabase)?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(failure)
    }
}

/// Reads a backup without committing to anything. Nothing on disk survives the call.
async fn inspect_backup(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<InspectionReport>, BackupError> {
    validate_binary_mutation(&headers)?;
    if let Err(error) = require_owner(&state, &headers).await {
        drain_body(body).await;
        return Err(error);
    }
    let service = service(&state)?;
    let token = Uuid::now_v7().to_string();
    let scratch = service.temp_directory();
    let package = scratch.join(format!("{token}.inspect"));
    let candidate = scratch.join(format!("{token}.candidate"));

    let outcome = async {
        read_body_to_file(body, &package, MAX_PAYLOAD_BYTES).await?;
        unpack_and_check(&package, &candidate).await
    }
    .await;
    let _ = std::fs::remove_file(&package);
    let _ = std::fs::remove_file(&candidate);
    Ok(Json(outcome?.1))
}

// ---------------------------------------------------------------------------------------------
// Restore — preparing a candidate
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedRestore {
    candidate_token: String,
    expires_in_seconds: i64,
    report: InspectionReport,
}

async fn restore_prepare(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<PreparedRestore>, BackupError> {
    validate_binary_mutation(&headers)?;
    let actor = match require_owner(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => {
            drain_body(body).await;
            return Err(error);
        }
    };
    prepare_candidate(&state, body, Some(actor.id)).await
}

/// First-run preparation. No account exists yet, so blankness is the only authority there can be.
async fn setup_restore_prepare(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<PreparedRestore>, BackupError> {
    validate_binary_mutation(&headers)?;
    if let Err(error) = require_blank_installation(&state).await {
        drain_body(body).await;
        return Err(error);
    }
    prepare_candidate(&state, body, None).await
}

async fn prepare_candidate(
    state: &ReferenceState,
    body: Body,
    prepared_by: Option<String>,
) -> Result<Json<PreparedRestore>, BackupError> {
    let service = service(state)?;
    if service.is_restoring() {
        return Err(BackupError::RestoreInProgress);
    }
    let token = Uuid::now_v7().to_string();
    let scratch = service.temp_directory();
    let package = scratch.join(format!("{token}.upload"));
    let candidate = scratch.join(format!("{token}.candidate"));

    let prepared = async {
        read_body_to_file(body, &package, MAX_PAYLOAD_BYTES).await?;
        let (manifest, mut report) = unpack_and_check(&package, &candidate).await?;

        // The forward migration happens HERE, on the copy, and its failure is merely a refusal. The
        // live database is never migrated as part of deciding whether a backup can be restored, and
        // an old backup that cannot be upgraded is discovered now rather than after the swap.
        if report.migration_required {
            let pool = database::open_existing_unmigrated(&candidate)
                .await
                .map_err(|_| BackupError::InvalidDatabase)?;
            let migrated = database::migrate_candidate(&pool).await;
            let verdict = match migrated {
                Ok(()) => structural_check(&pool, BackupError::RestoreFailed).await,
                Err(_) => Err(BackupError::RestoreFailed),
            };
            // Critical: the migration this build just applied may still be sitting in the
            // write-ahead log, and only the main file is renamed into place. Closing without
            // folding it in would install a database missing the very upgrade it was prepared for.
            let closed = database::checkpoint_and_close(&pool).await;
            verdict?;
            closed.map_err(|_| BackupError::RestoreFailed)?;
            report.compatibility = "ready";
        }
        Ok::<_, BackupError>((manifest, report))
    }
    .await;

    // The uploaded container has served its purpose either way; only the unpacked database is kept.
    let _ = std::fs::remove_file(&package);
    let (manifest, report) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(error);
        }
    };

    sweep_expired_candidates(&service);
    service.candidates.lock().expect("candidates").insert(
        token.clone(),
        Candidate {
            path: candidate,
            manifest,
            prepared_by_user_id: prepared_by,
            expires_at_unix: now_unix() + CANDIDATE_TTL_SECONDS,
        },
    );
    Ok(Json(PreparedRestore {
        candidate_token: token,
        expires_in_seconds: CANDIDATE_TTL_SECONDS,
        report,
    }))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or_default()
}

/// Drops candidates nobody came back for, so an abandoned restore does not leave a copy of the
/// pharmacy's entire database sitting in a temp folder indefinitely.
fn sweep_expired_candidates(service: &BackupService) {
    let now = now_unix();
    let mut candidates = service.candidates.lock().expect("candidates");
    candidates.retain(|_, candidate| {
        if candidate.expires_at_unix >= now {
            return true;
        }
        let _ = std::fs::remove_file(&candidate.path);
        false
    });
}

/// Claims a prepared candidate for exactly one commit.
fn take_candidate(
    service: &BackupService,
    token: &str,
    caller: Option<&str>,
) -> Result<Candidate, BackupError> {
    let mut candidates = service.candidates.lock().expect("candidates");
    let candidate = candidates
        .get(token)
        .cloned()
        .ok_or(BackupError::CandidateNotFound)?;
    if candidate.expires_at_unix < now_unix() {
        candidates.remove(token);
        let _ = std::fs::remove_file(&candidate.path);
        return Err(BackupError::CandidateExpired);
    }
    // A candidate belongs to whoever prepared it: one owner cannot commit another's upload, and a
    // first-run candidate cannot be committed through the authenticated route, or the reverse.
    if candidate.prepared_by_user_id.as_deref() != caller {
        return Err(BackupError::CandidateNotFound);
    }
    // Single use. A replayed commit finds nothing, so the same file cannot be installed twice.
    candidates.remove(token);
    Ok(candidate)
}

// ---------------------------------------------------------------------------------------------
// Blankness
// ---------------------------------------------------------------------------------------------

/// Whether this installation has never been used.
///
/// Setup decides on users alone, and that is not enough to authorise an unauthenticated destructive
/// operation: a database holding movements but no users is not blank, it is damaged, and restoring
/// over it would destroy the evidence of what went wrong.
pub(crate) async fn is_blank_installation(pool: &SqlitePool) -> Result<bool, BackupError> {
    for query in [
        "SELECT COUNT(*) FROM users",
        "SELECT COUNT(*) FROM store_identity",
        "SELECT COUNT(*) FROM inventory_movements",
        "SELECT COUNT(*) FROM sale_documents",
        "SELECT COUNT(*) FROM purchase_documents",
        "SELECT COUNT(*) FROM return_documents",
        "SELECT COUNT(*) FROM stock_operations",
        "SELECT COUNT(*) FROM restore_provenance",
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .fetch_one(pool)
            .await
            .map_err(|_| BackupError::Internal)?;
        if count != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn require_blank_installation(state: &ReferenceState) -> Result<(), BackupError> {
    if is_blank_installation(&state.pool).await? {
        Ok(())
    } else {
        Err(BackupError::SetupAlreadyComplete)
    }
}

// ---------------------------------------------------------------------------------------------
// Restore — committing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitRequest {
    candidate_token: String,
    /// Required on an established installation. A session can be hours old and left open at a
    /// counter, and this operation replaces the pharmacy's entire record.
    password: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommitResponse {
    restore_id: String,
    /// The database this service had open no longer exists; it must restart to reopen it.
    restart_required: bool,
    safety_backup: Option<String>,
}

async fn restore_commit(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CommitRequest>,
) -> Result<Json<CommitResponse>, BackupError> {
    auth::validate_mutation_request(&headers)?;
    let actor = require_owner(&state, &headers).await?;
    let password = request
        .password
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(BackupError::Validation("password", "is required"))?;
    if !auth::confirm_password(&state.pool, &actor.id, password).await? {
        return Err(BackupError::InvalidPassword);
    }
    // Read now, because after the swap the users table is the restored one and this id may mean
    // nothing in it. The login is the human-readable half that survives the change of database.
    let login: Option<String> = sqlx::query_scalar("SELECT login_identifier FROM users WHERE id=?")
        .bind(&actor.id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| BackupError::Internal)?;
    commit_restore(
        &state,
        &request.candidate_token,
        Some(actor.id.clone()),
        login,
    )
    .await
}

async fn setup_restore_commit(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CommitRequest>,
) -> Result<Json<CommitResponse>, BackupError> {
    auth::validate_mutation_request(&headers)?;
    require_blank_installation(&state).await?;
    commit_restore(&state, &request.candidate_token, None, None).await
}

/// Holds the single-restore gate, releasing it unless the restore actually completed.
struct RestoreGate {
    service: Arc<BackupService>,
    keep: bool,
}

impl RestoreGate {
    fn acquire(service: &Arc<BackupService>) -> Result<Self, BackupError> {
        service
            .restore_gate
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| BackupError::RestoreInProgress)?;
        Ok(Self {
            service: service.clone(),
            keep: false,
        })
    }

    /// The restore succeeded and the service is waiting to restart: the gate stays shut for good.
    fn hold(mut self) {
        self.keep = true;
    }
}

impl Drop for RestoreGate {
    fn drop(&mut self) {
        if !self.keep {
            self.service.restore_gate.store(false, Ordering::SeqCst);
        }
    }
}

/// Replaces the live database with a candidate that has already proved itself.
///
/// Everything before the first rename is reversible by doing nothing at all, and the order below is
/// the whole design: safety backup first, journal written before each move, and the pool closed
/// before any rename — because Windows refuses to rename over a file that is still open, and a
/// leaked handle must fail loudly here rather than corrupt quietly later.
async fn commit_restore(
    state: &ReferenceState,
    token: &str,
    actor_id: Option<String>,
    actor_login: Option<String>,
) -> Result<Json<CommitResponse>, BackupError> {
    let service = service(state)?;
    let gate = RestoreGate::acquire(&service)?;
    let candidate = take_candidate(&service, token, actor_id.as_deref())?;

    let restore_id = Uuid::now_v7().to_string();
    let backups = service.backups_directory.clone();
    let superseded_path = service
        .temp_directory()
        .join(format!("{restore_id}.superseded"));

    let mut journal = RestoreJournal {
        restore_id: restore_id.clone(),
        stage: RestoreStage::Prepared,
        started_at_utc: now_iso(),
        candidate_path: candidate.path.clone(),
        superseded_path,
        database_path: service.database_path.clone(),
        safety_backup_name: None,
        source_store_id: candidate.manifest.store_id.clone(),
        source_installation_id: candidate.manifest.installation_id.clone(),
        source_backup_created_at_utc: candidate.manifest.created_at_utc.clone(),
        source_database_sha256: candidate.manifest.database_sha256.clone(),
        pre_restore_database_sha256: None,
        backup_format_version: candidate.manifest.backup_format_version,
        source_schema_version: candidate.manifest.schema_version,
        initiated_by_user_id: actor_id.clone(),
        initiated_by_login: actor_login,
    };
    restore_journal::write(&backups, &journal).map_err(|error| io_failure(&error))?;

    // The safety backup is mandatory on an established installation, and it doubles as the
    // disk-space proof this platform cannot otherwise obtain: if a full copy of the live database
    // fits, the swap that follows needs no further space at all, because both files are already on
    // disk and a rename consumes none.
    let first_run = actor_id.is_none();
    if !first_run {
        match produce_backup(&state.pool, &service, "pre_restore_safety").await {
            Ok(safety) => {
                journal.safety_backup_name = Some(safety.filename.clone());
                journal.pre_restore_database_sha256 = Some(safety.manifest.database_sha256.clone());
            }
            Err(error) => {
                // Nothing has moved. Leaving the installation exactly as it was is the right answer.
                let _ = restore_journal::clear(&backups);
                let _ = std::fs::remove_file(&candidate.path);
                return Err(error);
            }
        }
    }
    restore_journal::advance(&backups, &mut journal, RestoreStage::SafetyTaken)
        .map_err(|error| io_failure(&error))?;

    // Every connection must be gone, and the log folded in, before anything is moved. A failure
    // here is still harmless: nothing has been renamed, so the installation is untouched.
    if database::checkpoint_and_close(&state.pool).await.is_err() {
        let _ = restore_journal::clear(&backups);
        let _ = std::fs::remove_file(&candidate.path);
        return Err(BackupError::RestoreFailed);
    }
    service.restoring.store(true, Ordering::SeqCst);

    match swap_database(&backups, &mut journal).await {
        Ok(()) => {
            let safety_backup = journal.safety_backup_name.clone();
            // The journal deliberately survives this call at `CandidateInstalled`. Startup validates
            // the installed file and only then records provenance, so a crash between here and the
            // restart is indistinguishable from a clean restart — which is exactly the point.
            gate.hold();
            Ok(Json(CommitResponse {
                restore_id,
                restart_required: true,
                safety_backup,
            }))
        }
        Err(error) => {
            service.restoring.store(false, Ordering::SeqCst);
            Err(error)
        }
    }
}

/// The two renames, journalled either side.
///
/// Two operations rather than one: `std::fs::rename` is atomic within a volume, but nothing in the
/// standard library swaps two files in a single step and `ReplaceFileW` would mean a new dependency.
/// The window between the renames is real, which is exactly why the journal exists and why startup
/// recovery is not optional.
pub(crate) async fn swap_database(
    backups: &Path,
    journal: &mut RestoreJournal,
) -> Result<(), BackupError> {
    if let Some(parent) = journal.superseded_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_failure(&error))?;
    }
    // The WAL and shared-memory files belong to the database being retired. Leaving them in place
    // would invite SQLite to replay one pharmacy's journal over another pharmacy's data.
    for suffix in ["-wal", "-shm"] {
        remove_when_released(&with_suffix(&journal.database_path, suffix))
            .await
            .map_err(|error| io_failure(&error))?;
    }
    if journal.database_path.exists() {
        rename_when_released(&journal.database_path, &journal.superseded_path)
            .await
            .map_err(|error| io_failure(&error))?;
    }
    restore_journal::advance(backups, journal, RestoreStage::OldMoved)
        .map_err(|error| io_failure(&error))?;

    if let Err(error) = std::fs::rename(&journal.candidate_path, &journal.database_path) {
        // Put back what was there. The journal still reads `OldMoved`, so a crash inside this very
        // branch recovers to the same place by the same route.
        let _ = std::fs::rename(&journal.superseded_path, &journal.database_path);
        let _ = restore_journal::clear(backups);
        return Err(io_failure(&error));
    }
    restore_journal::advance(backups, journal, RestoreStage::CandidateInstalled)
        .map_err(|error| io_failure(&error))?;
    Ok(())
}

/// How long to keep trying to move the database after the pool has closed.
///
/// Measured release on this platform is one retry at roughly thirty milliseconds; ten seconds is
/// far beyond that, and reaching it means something other than sqlx is holding the file — an
/// antivirus scan, a backup agent, or an operator with the file open — which is a refusal, not a
/// longer wait.
const RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Renames a file that a just-closed pool may still be holding.
///
/// `SqlitePool::close` returns before the operating system has released the handle, so the first
/// attempt fails with a sharing violation and a moment later the same call succeeds. The retry is
/// keyed on the rename itself rather than on a fixed pause: the operation is its own proof, and
/// there is no interval that could be assumed correct on somebody else's machine.
async fn rename_when_released(from: &Path, to: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + RELEASE_TIMEOUT;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if is_sharing_violation(&error) && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Deletes a file a just-closed pool may still be holding, for the same reason as the rename.
///
/// A file that is already gone counts as success: the outcome asked for is its absence.
async fn remove_when_released(path: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + RELEASE_TIMEOUT;
    loop {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if is_sharing_violation(&error) && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Windows reports a file held open elsewhere as ERROR_SHARING_VIOLATION (32).
fn is_sharing_violation(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(32) || error.kind() == std::io::ErrorKind::PermissionDenied
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_os_string();
    text.push(suffix);
    PathBuf::from(text)
}

fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
        .replace("+00:00", "Z")
}

// ---------------------------------------------------------------------------------------------
// Startup: finishing or undoing an interrupted restore
// ---------------------------------------------------------------------------------------------

/// Runs before the pool is opened, because only the journal knows what the authoritative path holds.
///
/// Every branch has one deterministic outcome, and where nothing can be proven the answer is to
/// refuse to start rather than open a database that might be half a pharmacy.
pub async fn recover_interrupted_restore(
    backups_directory: &Path,
    database_path: &Path,
) -> anyhow::Result<()> {
    let state = restore_journal::read(backups_directory);
    let recovery = restore_journal::recovery_for(&state);
    let journal = match &state {
        JournalState::Present(journal) => Some(journal.as_ref().clone()),
        _ => None,
    };
    match recovery {
        Recovery::Nothing => {}
        Recovery::DiscardCandidate => {
            // Nothing had moved yet, so the live database is untouched and the candidate is waste.
            if let Some(journal) = &journal {
                let _ = std::fs::remove_file(&journal.candidate_path);
            }
            restore_journal::clear(backups_directory)?;
        }
        Recovery::RollBack => {
            let superseded = journal.as_ref().map(|entry| entry.superseded_path.clone());
            roll_back(database_path, superseded.as_deref()).await?;
            if let Some(journal) = &journal {
                let _ = std::fs::remove_file(&journal.candidate_path);
            }
            restore_journal::clear(backups_directory)?;
        }
        Recovery::ValidateInstalled => {
            if validate_installed(database_path).await {
                // Left in place on purpose: the provenance row cannot be written until the database
                // is open and migrated, so `complete_restore_after_open` finishes the job.
                tracing::info!(target: "restore", "restored database validated; completing on open");
            } else {
                let superseded = journal.as_ref().map(|entry| entry.superseded_path.clone());
                roll_back(database_path, superseded.as_deref()).await?;
                restore_journal::clear(backups_directory)?;
                tracing::warn!(target: "restore", "restored database failed validation; rolled back");
            }
        }
        Recovery::FinishCleanup => {
            if let Some(journal) = &journal {
                let _ = std::fs::remove_file(&journal.superseded_path);
            }
            restore_journal::clear(backups_directory)?;
        }
        Recovery::Refuse(reason) => {
            anyhow::bail!("an interrupted restore could not be recovered: {reason}");
        }
    }
    Ok(())
}

/// Puts the superseded database back, and refuses to start if that cannot be done safely.
async fn roll_back(database_path: &Path, superseded: Option<&Path>) -> anyhow::Result<()> {
    // These belong to the candidate that is being taken back out, not to the database going back
    // in. Leaving one behind would hand the restored original somebody else's journal.
    for suffix in ["-wal", "-shm"] {
        remove_when_released(&with_suffix(database_path, suffix)).await?;
    }
    match superseded {
        Some(path) if path.exists() => {
            remove_when_released(database_path).await?;
            rename_when_released(path, database_path).await?;
            Ok(())
        }
        // Nothing was moved, or the journal named nothing that survives: whatever is at the
        // authoritative path is what was always there.
        _ if database_path.exists() => Ok(()),
        // The one case that must never be papered over. Starting here would create an empty
        // pharmacy where a real one used to be, and the operator would find out by selling from it.
        _ => anyhow::bail!(
            "the database is missing and no recoverable copy was found; refusing to start"
        ),
    }
}

async fn validate_installed(database_path: &Path) -> bool {
    let Ok(pool) = database::open_existing_unmigrated(database_path).await else {
        return false;
    };
    let verdict: Result<String, _> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await;
    let good = matches!(verdict, Ok(value) if value == "ok");
    let _ = database::checkpoint_and_close(&pool).await;
    good
}

/// Finishes a restore once the replaced database is open, migrated and known good.
///
/// Identity, lineage and session revocation all happen here rather than during the swap, so every
/// fact recorded is a fact about a database that actually works.
pub async fn complete_restore_after_open(
    pool: &SqlitePool,
    backups_directory: &Path,
) -> anyhow::Result<bool> {
    let JournalState::Present(journal) = restore_journal::read(backups_directory) else {
        return Ok(false);
    };
    if journal.stage != RestoreStage::CandidateInstalled {
        return Ok(false);
    }

    // A restored database is a new installation of the same pharmacy. The store identity inside it
    // is left exactly as the backup had it; only the installation is re-issued, and only now, when
    // the restore has actually succeeded.
    let new_installation_id = Uuid::now_v7().to_string();
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM installation_identity")
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO installation_identity (installation_id,created_at_utc) \
         VALUES (?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(&new_installation_id)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO restore_provenance (restore_id,restored_at_utc,initiated_by_user_id,\
         initiated_by_login,source_backup_created_at_utc,source_store_id,source_installation_id,\
         new_installation_id,source_database_sha256,pre_restore_database_sha256,\
         backup_format_version,source_schema_version,migrated_to_schema_version) \
         VALUES (?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&journal.restore_id)
    .bind(&journal.initiated_by_user_id)
    .bind(&journal.initiated_by_login)
    .bind(&journal.source_backup_created_at_utc)
    .bind(&journal.source_store_id)
    .bind(&journal.source_installation_id)
    .bind(&new_installation_id)
    .bind(&journal.source_database_sha256)
    .bind(&journal.pre_restore_database_sha256)
    .bind(i64::from(journal.backup_format_version))
    .bind(journal.source_schema_version)
    .bind(database::latest_schema_version())
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'restore_operation',?,1,'restored',strftime('%Y-%m-%dT%H:%M:%fZ','now'),1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&journal.restore_id)
    .bind(
        json!({
            "sourceInstallationId": journal.source_installation_id,
            "newInstallationId": new_installation_id,
            "sourceSchemaVersion": journal.source_schema_version,
            "migratedToSchemaVersion": database::latest_schema_version(),
        })
        .to_string(),
    )
    .bind(&journal.initiated_by_user_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    // Sessions inside the restored database were issued against a database that no longer exists,
    // on a machine this file may have left on a USB stick since. None of them may survive.
    let revoked = auth::revoke_all_sessions(pool)
        .await
        .map_err(|_| anyhow::anyhow!("failed to revoke sessions after restore"))?;
    tracing::info!(target: "restore", revoked, "sessions revoked after restore");

    let mut finished = journal.as_ref().clone();
    restore_journal::advance(backups_directory, &mut finished, RestoreStage::Validated)?;
    let _ = std::fs::remove_file(&finished.superseded_path);
    restore_journal::clear(backups_directory)?;

    // Backup freshness is a fact about this installation, and this installation has never taken a
    // backup. Inheriting "last backup: yesterday" from the restored file would be a lie that hides
    // the reminder on the one day it matters most.
    let _ = write_reminder(backups_directory, &ReminderState::default());
    prune_safety_backups(backups_directory);
    Ok(true)
}

/// Keeps the newest few safety backups and never touches a manual one.
///
/// Runs only after a restore has validated, oldest first. A file that cannot be read is quarantined
/// rather than counted, so a corrupt safety backup can never be the reason a good one is deleted.
fn prune_safety_backups(backups_directory: &Path) {
    let Ok(entries) = std::fs::read_dir(backups_directory) else {
        return;
    };
    let mut safety: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("aushbackup") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        let readable = backup::read_header(&mut file, metadata.len())
            .and_then(|header| backup::read_manifest(&mut file, &header));
        match readable {
            Err(_) => {
                let mut quarantined = path.clone().into_os_string();
                quarantined.push(".suspect");
                let _ = std::fs::rename(&path, PathBuf::from(quarantined));
            }
            Ok(manifest) if manifest.backup_kind == "pre_restore_safety" => {
                safety.push((manifest.created_at_utc, path));
            }
            Ok(_) => {}
        }
    }
    safety.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path) in safety.into_iter().skip(SAFETY_BACKUP_RETENTION) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    /// Meets the password policy and is used nowhere but here.
    const OWNER_PASSWORD: &str = "Disposable-Test-1";

    struct Harness {
        _temp: tempfile::TempDir,
        root: PathBuf,
        pool: SqlitePool,
        service: Arc<BackupService>,
        owner: String,
    }

    impl Harness {
        fn database_path(&self) -> PathBuf {
            self.root.join("database").join("aushadharth.sqlite3")
        }

        fn backups(&self) -> PathBuf {
            self.root.join("backups")
        }

        fn router(&self) -> Router {
            crate::api::router_with_backups(
                self.pool.clone(),
                None,
                Some(Arc::clone(&self.service)),
            )
        }
    }

    /// A throwaway installation with its own data directory, its own database, and one owner.
    ///
    /// Everything lives under a temporary directory that is deleted when the test ends; nothing here
    /// can reach a real pharmacy's files, and the owner is created through the real setup route so
    /// the password is hashed exactly as production hashes it.
    async fn harness() -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        std::fs::create_dir_all(root.join("database")).unwrap();
        std::fs::create_dir_all(root.join("backups")).unwrap();
        let database_path = root.join("database").join("aushadharth.sqlite3");
        let pool = database::connect(&database_path).await.unwrap();
        let service = Arc::new(BackupService::new(root.join("backups"), database_path));

        let router =
            crate::api::router_with_backups(pool.clone(), None, Some(Arc::clone(&service)));
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/setup")
                    .header("host", "127.0.0.1:47831")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "storeDisplayName": "Disposable Pharmacy",
                            "ownerDisplayName": "Test Owner",
                            "loginIdentifier": "owner",
                            "password": OWNER_PASSWORD,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success(), "setup must succeed");
        let owner = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .and_then(|pair| pair.split_once('='))
            .map(|(_, token)| token.to_owned())
            .expect("setup issues a session");

        Harness {
            _temp: temp,
            root,
            pool,
            service,
            owner,
        }
    }

    /// Adds a second owner, so "another owner" is a real account rather than a hypothetical.
    async fn second_owner(harness: &Harness) -> String {
        let token = format!("second-owner-{}", Uuid::now_v7());
        let user_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
             password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,'second','second','Second Owner',\
             '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',?,?)",
        )
        .bind(&user_id)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&harness.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,\
             last_seen_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&user_id)
        .bind(crate::api::auth::sha256_hex(token.as_bytes()))
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2099-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&harness.pool)
        .await
        .unwrap();
        token
    }

    async fn insert_role(harness: &Harness, role: &str) -> String {
        let token = format!("{role}-{}", Uuid::now_v7());
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-user");
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
             password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,'$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',?,?,?)",
        )
        .bind(&user_id)
        .bind(&login)
        .bind(&login)
        .bind(format!("{role} user"))
        .bind(role)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&harness.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,\
             last_seen_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&user_id)
        .bind(crate::api::auth::sha256_hex(token.as_bytes()))
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2099-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&harness.pool)
        .await
        .unwrap();
        token
    }

    async fn json_request(
        harness: &Harness,
        method: &str,
        uri: &str,
        body: Value,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:47831")
            .header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("cookie", format!("aushadharth_session={token}"));
        }
        let response = harness
            .router()
            .oneshot(
                request
                    .body(Body::from(if body.is_null() {
                        String::new()
                    } else {
                        body.to_string()
                    }))
                    .unwrap(),
            )
            .await
            .unwrap();
        decode(response).await
    }

    async fn binary_request(
        harness: &Harness,
        uri: &str,
        bytes: Vec<u8>,
        token: Option<&str>,
        content_type: &str,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "127.0.0.1:47831")
            .header("content-type", content_type);
        if let Some(token) = token {
            request = request.header("cookie", format!("aushadharth_session={token}"));
        }
        let response = harness
            .router()
            .oneshot(request.body(Body::from(bytes)).unwrap())
            .await
            .unwrap();
        decode(response).await
    }

    async fn decode(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, body)
    }

    fn code(body: &Value) -> &str {
        body.get("code").and_then(Value::as_str).unwrap_or("")
    }

    /// Takes a backup through the HTTP route and returns its bytes as the browser would receive them.
    async fn take_backup(harness: &Harness) -> (String, Vec<u8>) {
        let (status, body) = json_request(
            harness,
            "POST",
            "/api/v1/backups/create",
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "backup create: {body}");
        let filename = body["filename"].as_str().unwrap().to_owned();
        let bytes = std::fs::read(harness.backups().join(&filename)).unwrap();
        (filename, bytes)
    }

    // -----------------------------------------------------------------------------------------
    // Producing a backup
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn an_owner_can_take_a_backup_and_it_describes_itself_honestly() {
        let harness = harness().await;
        let (filename, bytes) = take_backup(&harness).await;
        assert!(filename.ends_with(".aushbackup"));
        assert!(
            filename.contains("disposable-pharmacy"),
            "filename: {filename}"
        );

        let mut cursor = std::io::Cursor::new(bytes.clone());
        let header = backup::read_header(&mut cursor, bytes.len() as u64).unwrap();
        let manifest = backup::read_manifest(&mut cursor, &header).unwrap();
        assert_eq!(manifest.product, "AUSHADHARTH");
        assert_eq!(manifest.backup_kind, "manual");
        assert_eq!(manifest.schema_version, database::latest_schema_version());
        assert_eq!(manifest.store_display_name, "Disposable Pharmacy");
        assert!(!manifest.installation_id.is_empty());
        // The digest is of the database inside, not of the container.
        assert_eq!(manifest.database_sha256.len(), 64);

        // The status route sees it, and the reminder is no longer overdue.
        let (status, body) = json_request(
            &harness,
            "GET",
            "/api/v1/backups/status",
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["backups"].as_array().unwrap().len(), 1);
        assert_eq!(body["backupOverdue"], Value::Bool(false));
        assert!(body["lastSuccessfulBackupAtUtc"].is_string());
    }

    #[tokio::test]
    async fn an_installation_that_has_never_backed_up_is_reported_as_overdue() {
        let harness = harness().await;
        let (status, body) = json_request(
            &harness,
            "GET",
            "/api/v1/backups/status",
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["backupOverdue"], Value::Bool(true));
        assert!(body["lastSuccessfulBackupAtUtc"].is_null());
    }

    #[tokio::test]
    async fn a_backup_records_its_own_creation_without_leaking_anything() {
        let harness = harness().await;
        take_backup(&harness).await;
        let payload: String = sqlx::query_scalar(
            "SELECT change_payload FROM master_change_events WHERE entity_type='backup'",
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert!(payload.contains("backupKind"));
        // No path, no GSTIN, and only a prefix of the digest.
        for fragment in ["\\", "/", ".aushbackup", "Temp", "C:"] {
            assert!(
                !payload.contains(fragment),
                "the payload carries {fragment}: {payload}"
            );
        }
        assert!(!payload.to_lowercase().contains("gstin"));
        let digest = payload
            .split("digestPrefix\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap();
        assert_eq!(digest.len(), 12);
    }

    #[tokio::test]
    async fn only_an_owner_may_use_the_backup_routes() {
        let harness = harness().await;
        for (role, token) in [
            ("pharmacist", insert_role(&harness, "pharmacist").await),
            ("cashier", insert_role(&harness, "cashier").await),
        ] {
            let (status, _) = json_request(
                &harness,
                "GET",
                "/api/v1/backups/status",
                Value::Null,
                Some(&token),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{role} read the backup status"
            );
            let (status, _) = json_request(
                &harness,
                "POST",
                "/api/v1/backups/create",
                Value::Null,
                Some(&token),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role} took a backup");
        }
        let (status, _) =
            json_request(&harness, "GET", "/api/v1/backups/status", Value::Null, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // -----------------------------------------------------------------------------------------
    // Refusing what is not a backup
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn an_upload_that_is_not_octet_stream_is_refused_before_anything_is_read() {
        let harness = harness().await;
        let (_, bytes) = take_backup(&harness).await;
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            bytes,
            Some(&harness.owner),
            "multipart/form-data; boundary=x",
        )
        .await;
        // The content-type rule is the CSRF defence; a form encoding must never be accepted.
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code(&body), "validation_failed");
    }

    #[tokio::test]
    async fn arbitrary_rubbish_is_refused_and_leaves_nothing_behind() {
        let harness = harness().await;
        for (label, payload) in [
            ("empty", Vec::new()),
            ("short", b"AUSH".to_vec()),
            (
                "text",
                b"this is not a backup, it is a letter to the tax office".to_vec(),
            ),
            ("zeroes", vec![0_u8; 4096]),
        ] {
            let (status, body) = binary_request(
                &harness,
                "/api/v1/backups/inspect",
                payload,
                Some(&harness.owner),
                "application/octet-stream",
            )
            .await;
            assert!(
                status.is_client_error(),
                "{label} was accepted with {status}"
            );
            assert!(
                [
                    "backup_corrupt",
                    "backup_product_mismatch",
                    "backup_format_unsupported"
                ]
                .contains(&code(&body)),
                "{label} produced {}",
                code(&body)
            );
        }
        // Nothing survives a refusal.
        let leftovers: Vec<_> = std::fs::read_dir(harness.service.temp_directory())
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "temp residue: {leftovers:?}");
        // And the live database is untouched.
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(users, 1);
    }

    #[tokio::test]
    async fn a_backup_whose_payload_was_edited_fails_its_own_checksum() {
        let harness = harness().await;
        let (_, mut bytes) = take_backup(&harness).await;
        // Flip a byte deep inside the payload, well past the header and manifest.
        let position = bytes.len() - 2048;
        bytes[position] ^= 0xFF;
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            bytes,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "backup_checksum_mismatch");
    }

    #[tokio::test]
    async fn a_backup_with_trailing_bytes_is_refused() {
        let harness = harness().await;
        let (_, mut bytes) = take_backup(&harness).await;
        bytes.extend_from_slice(b"appended by something else");
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            bytes,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "backup_corrupt");
    }

    #[tokio::test]
    async fn a_database_from_another_product_is_refused_even_inside_a_valid_container() {
        let harness = harness().await;
        let foreign = harness._temp.path().join("foreign.sqlite3");
        {
            let pool = database::connect(&foreign).await.unwrap();
            // Same schema, someone else's marker.
            sqlx::query("PRAGMA application_id = 559038737")
                .execute(&pool)
                .await
                .unwrap();
            database::checkpoint_and_close(&pool).await.unwrap();
        }
        let (digest, bytes_len) = backup::hash_file(&foreign).unwrap();
        let manifest = Manifest {
            backup_format_version: backup::FORMAT_VERSION,
            product: "AUSHADHARTH".to_owned(),
            backup_id: Uuid::now_v7().to_string(),
            created_at_utc: "2026-06-15T10:00:00.000Z".to_owned(),
            application_version: APPLICATION_VERSION.to_owned(),
            schema_version: database::latest_schema_version(),
            applied_migrations: Vec::new(),
            installation_id: Uuid::now_v7().to_string(),
            store_id: Uuid::now_v7().to_string(),
            store_display_name: "Somebody Else".to_owned(),
            database_sha256: digest,
            database_bytes: bytes_len,
            backup_kind: "manual".to_owned(),
            source_platform: "windows".to_owned(),
            attachments: Vec::new(),
        };
        let mut container = Vec::new();
        backup::write_container(&mut container, &manifest, &foreign).unwrap();

        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            container,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "backup_product_mismatch");
    }

    #[tokio::test]
    async fn a_backup_from_a_newer_build_is_refused_rather_than_downgraded() {
        let harness = harness().await;
        let future = harness._temp.path().join("future.sqlite3");
        {
            let pool = database::connect(&future).await.unwrap();
            // A migration this build has never heard of.
            sqlx::query(
                "INSERT INTO _sqlx_migrations \
                 (version,description,installed_on,success,checksum,execution_time) \
                 VALUES (9999,'from the future',strftime('%Y-%m-%dT%H:%M:%fZ','now'),1,X'00',1)",
            )
            .execute(&pool)
            .await
            .unwrap();
            database::checkpoint_and_close(&pool).await.unwrap();
        }
        let container = container_for(&future, 9999);
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            container,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "backup_too_new");
    }

    #[tokio::test]
    async fn a_backup_interrupted_mid_upgrade_is_refused() {
        let harness = harness().await;
        let broken = harness._temp.path().join("broken.sqlite3");
        {
            let pool = database::connect(&broken).await.unwrap();
            sqlx::query("UPDATE _sqlx_migrations SET success=0 WHERE version=(SELECT MAX(version) FROM _sqlx_migrations)")
                .execute(&pool)
                .await
                .unwrap();
            database::checkpoint_and_close(&pool).await.unwrap();
        }
        let container = container_for(&broken, database::latest_schema_version());
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            container,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "backup_partially_migrated");
    }

    #[tokio::test]
    async fn a_database_predating_the_application_id_is_still_recognised_as_ours() {
        let harness = harness().await;
        let legacy = harness._temp.path().join("legacy.sqlite3");
        {
            let pool = database::connect(&legacy).await.unwrap();
            // Exactly what a database taken before the backup migration looks like.
            sqlx::query("PRAGMA application_id = 0")
                .execute(&pool)
                .await
                .unwrap();
            database::checkpoint_and_close(&pool).await.unwrap();
        }
        let container = container_for(&legacy, database::latest_schema_version());
        let (status, body) = binary_request(
            &harness,
            "/api/v1/backups/inspect",
            container,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "legacy database refused: {body}");
        assert_eq!(body["compatibility"], "ready");
    }

    /// Wraps a bare database in a valid container so the test can aim at the checks inside it.
    fn container_for(database_path: &Path, schema_version: i64) -> Vec<u8> {
        let (digest, bytes) = backup::hash_file(database_path).unwrap();
        let manifest = Manifest {
            backup_format_version: backup::FORMAT_VERSION,
            product: "AUSHADHARTH".to_owned(),
            backup_id: Uuid::now_v7().to_string(),
            created_at_utc: "2026-06-15T10:00:00.000Z".to_owned(),
            application_version: APPLICATION_VERSION.to_owned(),
            schema_version,
            applied_migrations: Vec::new(),
            installation_id: Uuid::now_v7().to_string(),
            store_id: Uuid::now_v7().to_string(),
            store_display_name: "Disposable Pharmacy".to_owned(),
            database_sha256: digest,
            database_bytes: bytes,
            backup_kind: "manual".to_owned(),
            source_platform: "windows".to_owned(),
            attachments: Vec::new(),
        };
        let mut container = Vec::new();
        backup::write_container(&mut container, &manifest, database_path).unwrap();
        container
    }

    // -----------------------------------------------------------------------------------------
    // Restoring
    // -----------------------------------------------------------------------------------------

    async fn prepare(harness: &Harness, bytes: Vec<u8>) -> (StatusCode, Value) {
        binary_request(
            harness,
            "/api/v1/backups/restore/prepare",
            bytes,
            Some(&harness.owner),
            "application/octet-stream",
        )
        .await
    }

    #[tokio::test]
    async fn a_restore_replaces_the_database_re_identifies_the_installation_and_revokes_sessions() {
        let harness = harness().await;
        let original_installation: String =
            sqlx::query_scalar("SELECT installation_id FROM installation_identity")
                .fetch_one(&harness.pool)
                .await
                .unwrap();
        let (_, bytes) = take_backup(&harness).await;

        // Something happens after the backup that the restore must undo.
        sqlx::query("UPDATE store_identity SET display_name='Renamed After The Backup'")
            .execute(&harness.pool)
            .await
            .unwrap();

        let (status, prepared) = prepare(&harness, bytes).await;
        assert_eq!(status, StatusCode::OK, "prepare: {prepared}");
        assert_eq!(prepared["report"]["compatibility"], "ready");
        let token = prepared["candidateToken"].as_str().unwrap().to_owned();

        let (status, committed) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": OWNER_PASSWORD }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "commit: {committed}");
        assert_eq!(committed["restartRequired"], Value::Bool(true));
        let safety = committed["safetyBackup"].as_str().unwrap().to_owned();
        assert!(
            harness.backups().join(&safety).exists(),
            "safety backup missing"
        );

        // The service is now exactly where a restart finds it: the journal says a candidate is
        // installed and the old pool is closed.
        assert!(harness.service.is_restoring());

        // Restart.
        recover_interrupted_restore(&harness.backups(), &harness.database_path())
            .await
            .unwrap();
        let reopened = database::connect(&harness.database_path()).await.unwrap();
        assert!(
            complete_restore_after_open(&reopened, &harness.backups())
                .await
                .unwrap()
        );

        let name: String = sqlx::query_scalar("SELECT display_name FROM store_identity")
            .fetch_one(&reopened)
            .await
            .unwrap();
        assert_eq!(
            name, "Disposable Pharmacy",
            "the restore did not take effect"
        );

        let installation: String =
            sqlx::query_scalar("SELECT installation_id FROM installation_identity")
                .fetch_one(&reopened)
                .await
                .unwrap();
        assert_ne!(
            installation, original_installation,
            "a restored database must be a new installation"
        );

        let live_sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE revoked_at_utc IS NULL")
                .fetch_one(&reopened)
                .await
                .unwrap();
        assert_eq!(live_sessions, 0, "sessions survived a restore");

        let (source, new_id, migrated): (String, String, i64) = sqlx::query_as(
            "SELECT source_installation_id,new_installation_id,migrated_to_schema_version \
             FROM restore_provenance",
        )
        .fetch_one(&reopened)
        .await
        .unwrap();
        assert_eq!(source, original_installation, "lineage lost");
        assert_eq!(new_id, installation);
        assert_eq!(migrated, database::latest_schema_version());

        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='restore_operation'",
        )
        .fetch_one(&reopened)
        .await
        .unwrap();
        assert_eq!(events, 1);

        // Freshness belongs to this installation, which has never taken a backup of its own.
        assert!(
            read_reminder(&harness.backups())
                .last_successful_backup_at_utc
                .is_none()
        );
        // The journal is gone and the superseded copy with it.
        assert!(matches!(
            restore_journal::read(&harness.backups()),
            JournalState::Absent
        ));
        reopened.close().await;
    }

    #[tokio::test]
    async fn a_restore_without_the_right_password_is_refused() {
        let harness = harness().await;
        let (_, bytes) = take_backup(&harness).await;
        let (_, prepared) = prepare(&harness, bytes).await;
        let token = prepared["candidateToken"].as_str().unwrap().to_owned();

        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(code(&body), "validation_failed");

        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": "Wrong-Password-99" }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code(&body), "invalid_password");

        // Nothing moved: no journal, no safety backup, and the database is still open.
        assert!(matches!(
            restore_journal::read(&harness.backups()),
            JournalState::Absent
        ));
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(users, 1);
    }

    #[tokio::test]
    async fn a_candidate_belongs_to_whoever_prepared_it_and_can_be_committed_only_once() {
        let harness = harness().await;
        let other = second_owner(&harness).await;
        let (_, bytes) = take_backup(&harness).await;
        let (_, prepared) = prepare(&harness, bytes).await;
        let token = prepared["candidateToken"].as_str().unwrap().to_owned();

        // Another owner holding the token cannot use it.
        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": OWNER_PASSWORD }),
            Some(&other),
        )
        .await;
        assert!(
            status == StatusCode::CONFLICT || status == StatusCode::FORBIDDEN,
            "another owner got {status}: {body}"
        );

        // Neither can the first-run route, which is for blank installations only.
        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/setup/restore/commit",
            json!({ "candidateToken": token }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code(&body), "setup_already_complete");

        // The owner who prepared it may, exactly once.
        let (status, _) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": OWNER_PASSWORD }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": OWNER_PASSWORD }),
            Some(&harness.owner),
        )
        .await;
        // The service is now waiting to restart, and says so rather than reporting a database
        // error from the pool it deliberately closed.
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a replayed commit succeeded: {body}"
        );
        assert_eq!(code(&body), "service_restoring");
    }

    #[tokio::test]
    async fn an_unknown_or_expired_candidate_is_refused() {
        let harness = harness().await;
        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": "not-a-token", "password": OWNER_PASSWORD }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "candidate_not_found");

        let (_, bytes) = take_backup(&harness).await;
        let (_, prepared) = prepare(&harness, bytes).await;
        let token = prepared["candidateToken"].as_str().unwrap().to_owned();
        // Age it past its welcome.
        {
            let mut candidates = harness.service.candidates.lock().unwrap();
            candidates.get_mut(&token).unwrap().expires_at_unix = now_unix() - 1;
        }
        let (status, body) = json_request(
            &harness,
            "POST",
            "/api/v1/backups/restore/commit",
            json!({ "candidateToken": token, "password": OWNER_PASSWORD }),
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "candidate_expired");
    }

    #[tokio::test]
    async fn the_first_run_route_refuses_an_installation_that_has_already_been_used() {
        let harness = harness().await;
        let (_, bytes) = take_backup(&harness).await;
        let (status, body) = binary_request(
            &harness,
            "/api/v1/setup/restore/prepare",
            bytes,
            None,
            "application/octet-stream",
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code(&body), "setup_already_complete");
    }

    #[tokio::test]
    async fn a_database_with_history_but_no_users_is_not_blank() {
        let temp = tempfile::tempdir().unwrap();
        let pool = database::connect(&temp.path().join("damaged.sqlite3"))
            .await
            .unwrap();
        assert!(is_blank_installation(&pool).await.unwrap());
        // A pharmacy whose users table was lost is damaged, not new; restoring over it without a
        // password would destroy the only evidence of what happened.
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Damaged Pharmacy','Asia/Kolkata','2026-01-01T00:00:00.000Z')",
        )
        .bind(Uuid::now_v7().to_string())
        .execute(&pool)
        .await
        .unwrap();
        assert!(!is_blank_installation(&pool).await.unwrap());
        pool.close().await;
    }

    // -----------------------------------------------------------------------------------------
    // Interruption
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_crash_before_the_swap_leaves_the_database_exactly_as_it_was() {
        let harness = harness().await;
        let before = std::fs::metadata(harness.database_path()).unwrap().len();
        let candidate = harness.service.temp_directory().join("abandoned.candidate");
        std::fs::create_dir_all(harness.service.temp_directory()).unwrap();
        std::fs::write(&candidate, b"half an upload").unwrap();
        let journal = RestoreJournal {
            restore_id: Uuid::now_v7().to_string(),
            stage: RestoreStage::SafetyTaken,
            started_at_utc: now_iso(),
            candidate_path: candidate.clone(),
            superseded_path: harness.service.temp_directory().join("x.superseded"),
            database_path: harness.database_path(),
            safety_backup_name: None,
            source_store_id: String::new(),
            source_installation_id: String::new(),
            source_backup_created_at_utc: String::new(),
            source_database_sha256: "0".repeat(64),
            pre_restore_database_sha256: None,
            backup_format_version: 1,
            source_schema_version: 1,
            initiated_by_user_id: None,
            initiated_by_login: None,
        };
        restore_journal::write(&harness.backups(), &journal).unwrap();

        recover_interrupted_restore(&harness.backups(), &harness.database_path())
            .await
            .unwrap();
        assert!(!candidate.exists(), "the abandoned candidate was kept");
        assert_eq!(
            std::fs::metadata(harness.database_path()).unwrap().len(),
            before
        );
        assert!(matches!(
            restore_journal::read(&harness.backups()),
            JournalState::Absent
        ));
    }

    #[tokio::test]
    async fn a_crash_between_the_two_renames_rolls_the_old_database_back() {
        let harness = harness().await;
        let (_, bytes) = take_backup(&harness).await;
        database::checkpoint_and_close(&harness.pool).await.unwrap();

        // Reproduce the exact state of a machine that lost power between the renames: the old
        // database has been moved aside and nothing has taken its place.
        let superseded = harness
            .service
            .temp_directory()
            .join("interrupted.superseded");
        std::fs::create_dir_all(harness.service.temp_directory()).unwrap();
        rename_when_released(&harness.database_path(), &superseded)
            .await
            .unwrap();
        let candidate = harness
            .service
            .temp_directory()
            .join("interrupted.candidate");
        std::fs::write(&candidate, &bytes).unwrap();
        let journal = RestoreJournal {
            restore_id: Uuid::now_v7().to_string(),
            stage: RestoreStage::OldMoved,
            started_at_utc: now_iso(),
            candidate_path: candidate.clone(),
            superseded_path: superseded.clone(),
            database_path: harness.database_path(),
            safety_backup_name: None,
            source_store_id: String::new(),
            source_installation_id: String::new(),
            source_backup_created_at_utc: String::new(),
            source_database_sha256: "0".repeat(64),
            pre_restore_database_sha256: None,
            backup_format_version: 1,
            source_schema_version: 1,
            initiated_by_user_id: None,
            initiated_by_login: None,
        };
        restore_journal::write(&harness.backups(), &journal).unwrap();

        recover_interrupted_restore(&harness.backups(), &harness.database_path())
            .await
            .unwrap();
        assert!(
            harness.database_path().exists(),
            "the database was not restored"
        );
        assert!(!superseded.exists());
        assert!(!candidate.exists());

        let pool = database::connect(&harness.database_path()).await.unwrap();
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 1, "the rolled-back database is not the original");
        pool.close().await;
    }

    #[tokio::test]
    async fn a_corrupt_installed_candidate_is_rolled_back_rather_than_opened() {
        let harness = harness().await;
        database::checkpoint_and_close(&harness.pool).await.unwrap();
        let superseded = harness.service.temp_directory().join("good.superseded");
        std::fs::create_dir_all(harness.service.temp_directory()).unwrap();
        std::fs::copy(harness.database_path(), &superseded).unwrap();
        // The "installed candidate" is rubbish: this is the case where the swap completed but the
        // file turned out not to be a working database.
        remove_when_released(&harness.database_path())
            .await
            .unwrap();
        std::fs::write(harness.database_path(), b"not a database at all").unwrap();
        let journal = RestoreJournal {
            restore_id: Uuid::now_v7().to_string(),
            stage: RestoreStage::CandidateInstalled,
            started_at_utc: now_iso(),
            candidate_path: harness.service.temp_directory().join("gone.candidate"),
            superseded_path: superseded.clone(),
            database_path: harness.database_path(),
            safety_backup_name: None,
            source_store_id: String::new(),
            source_installation_id: String::new(),
            source_backup_created_at_utc: String::new(),
            source_database_sha256: "0".repeat(64),
            pre_restore_database_sha256: None,
            backup_format_version: 1,
            source_schema_version: 1,
            initiated_by_user_id: None,
            initiated_by_login: None,
        };
        restore_journal::write(&harness.backups(), &journal).unwrap();

        recover_interrupted_restore(&harness.backups(), &harness.database_path())
            .await
            .unwrap();
        let pool = database::connect(&harness.database_path()).await.unwrap();
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 1, "a corrupt candidate was left in place");
        pool.close().await;
        assert!(matches!(
            restore_journal::read(&harness.backups()),
            JournalState::Absent
        ));
    }

    #[tokio::test]
    async fn a_missing_database_with_nothing_to_restore_refuses_to_start() {
        let harness = harness().await;
        database::checkpoint_and_close(&harness.pool).await.unwrap();
        remove_when_released(&harness.database_path())
            .await
            .unwrap();
        for suffix in ["-wal", "-shm"] {
            remove_when_released(&with_suffix(&harness.database_path(), suffix))
                .await
                .unwrap();
        }
        let journal = RestoreJournal {
            restore_id: Uuid::now_v7().to_string(),
            stage: RestoreStage::OldMoved,
            started_at_utc: now_iso(),
            candidate_path: harness.service.temp_directory().join("gone.candidate"),
            superseded_path: harness
                .service
                .temp_directory()
                .join("also-gone.superseded"),
            database_path: harness.database_path(),
            safety_backup_name: None,
            source_store_id: String::new(),
            source_installation_id: String::new(),
            source_backup_created_at_utc: String::new(),
            source_database_sha256: "0".repeat(64),
            pre_restore_database_sha256: None,
            backup_format_version: 1,
            source_schema_version: 1,
            initiated_by_user_id: None,
            initiated_by_login: None,
        };
        restore_journal::write(&harness.backups(), &journal).unwrap();

        // Starting here would create an empty pharmacy where a real one used to be.
        let outcome =
            recover_interrupted_restore(&harness.backups(), &harness.database_path()).await;
        assert!(
            outcome.is_err(),
            "the service started over a missing database"
        );
        assert!(!harness.database_path().exists());
    }

    #[tokio::test]
    async fn an_unreadable_journal_is_treated_as_an_interrupted_swap() {
        let harness = harness().await;
        database::checkpoint_and_close(&harness.pool).await.unwrap();
        std::fs::write(
            restore_journal::journal_path(&harness.backups()),
            b"{ truncated",
        )
        .unwrap();
        // Unreadable means a restore may have been in flight; the database is still where it was,
        // so recovery is a no-op that leaves it alone rather than a refusal to start.
        recover_interrupted_restore(&harness.backups(), &harness.database_path())
            .await
            .unwrap();
        let pool = database::connect(&harness.database_path()).await.unwrap();
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(users, 1);
        pool.close().await;
    }

    // -----------------------------------------------------------------------------------------
    // Downloading
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_download_names_a_file_the_caller_never_chose() {
        let harness = harness().await;
        let (filename, _) = take_backup(&harness).await;
        let (_, body) = json_request(
            &harness,
            "GET",
            "/api/v1/backups/status",
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        let id = body["backups"][0]["backupId"].as_str().unwrap().to_owned();

        let (status, body) = json_request(
            &harness,
            "GET",
            &format!("/api/v1/backups/{id}/download"),
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["filename"], filename);
        assert_eq!(body["url"], format!("/api/v1/backup-files/{filename}"));

        // An id nobody issued resolves to nothing rather than to a path.
        let (status, _) = json_request(
            &harness,
            "GET",
            "/api/v1/backups/..%2F..%2Fdatabase%2Faushadharth.sqlite3/download",
            Value::Null,
            Some(&harness.owner),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_file_route_serves_an_owner_and_nobody_else() {
        let harness = harness().await;
        let (filename, bytes) = take_backup(&harness).await;
        let cashier = insert_role(&harness, "cashier").await;

        for (token, expected) in [
            (Some(harness.owner.clone()), StatusCode::OK),
            (Some(cashier), StatusCode::FORBIDDEN),
            (None, StatusCode::UNAUTHORIZED),
        ] {
            let mut request = Request::builder()
                .method("GET")
                .uri(format!("/api/v1/backup-files/{filename}"))
                .header("host", "127.0.0.1:47831");
            if let Some(token) = token {
                request = request.header("cookie", format!("aushadharth_session={token}"));
            }
            let response = harness
                .router()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                assert!(
                    response
                        .headers()
                        .get(header::CONTENT_DISPOSITION)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value.contains(&filename)),
                    "the download is not offered as an attachment"
                );
                let served = response.into_body().collect().await.unwrap().to_bytes();
                assert_eq!(
                    served.len(),
                    bytes.len(),
                    "the served file is not the backup"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_file_route_refuses_to_walk_out_of_its_directory() {
        let harness = harness().await;
        take_backup(&harness).await;
        for attempt in [
            "/api/v1/backup-files/../database/aushadharth.sqlite3",
            "/api/v1/backup-files/..%2Fdatabase%2Faushadharth.sqlite3",
            "/api/v1/backup-files/%2e%2e/%2e%2e/secrets.txt",
        ] {
            let response = harness
                .router()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(attempt)
                        .header("host", "127.0.0.1:47831")
                        .header("cookie", format!("aushadharth_session={}", harness.owner))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                !response.status().is_success(),
                "{attempt} was served with {}",
                response.status()
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // Housekeeping
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn the_backup_routes_are_unreachable_when_no_directory_was_configured() {
        // This is the configuration every test in the rest of the suite runs under.
        let temp = tempfile::tempdir().unwrap();
        let pool = database::connect(&temp.path().join("plain.sqlite3"))
            .await
            .unwrap();
        let token = "plain-owner-token";
        let user_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
             password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,'owner','owner','Owner',\
             '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA','owner_admin',?,?)",
        )
        .bind(&user_id)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,\
             last_seen_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&user_id)
        .bind(crate::api::auth::sha256_hex(token.as_bytes()))
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2099-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&pool)
        .await
        .unwrap();

        let response = crate::api::router(pool.clone(), None)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/backups/status")
                    .header("host", "127.0.0.1:47831")
                    .header("cookie", format!("aushadharth_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, body) = decode(response).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code(&body), "backup_unavailable");
        pool.close().await;
    }

    #[tokio::test]
    async fn abandoned_candidates_are_swept_and_never_accumulate() {
        let harness = harness().await;
        let (_, bytes) = take_backup(&harness).await;
        let (_, first) = prepare(&harness, bytes.clone()).await;
        let stale_token = first["candidateToken"].as_str().unwrap().to_owned();
        let stale_path = {
            let mut candidates = harness.service.candidates.lock().unwrap();
            let entry = candidates.get_mut(&stale_token).unwrap();
            entry.expires_at_unix = now_unix() - 1;
            entry.path.clone()
        };
        assert!(stale_path.exists());

        let (status, _) = prepare(&harness, bytes).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !stale_path.exists(),
            "an abandoned candidate was left on disk"
        );
        assert_eq!(harness.service.candidates.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn safety_backups_are_pruned_but_manual_ones_are_never_touched() {
        let harness = harness().await;
        take_backup(&harness).await;
        let manual_count = list_backups(&harness.service).len();
        assert_eq!(manual_count, 1);

        // More safety backups than the retention allows, each with a distinct stamp.
        for _ in 0..(SAFETY_BACKUP_RETENTION + 3) {
            produce_backup(&harness.pool, &harness.service, "pre_restore_safety")
                .await
                .unwrap();
        }
        let before = list_backups(&harness.service);
        assert_eq!(before.len(), SAFETY_BACKUP_RETENTION + 4);

        prune_safety_backups(&harness.backups());
        let after = list_backups(&harness.service);
        assert_eq!(after.len(), SAFETY_BACKUP_RETENTION + 1);
        assert_eq!(
            after
                .iter()
                .filter(|row| row.backup_kind == "manual")
                .count(),
            1,
            "a manual backup was pruned"
        );
    }

    #[tokio::test]
    async fn an_unreadable_file_is_quarantined_rather_than_counted() {
        let harness = harness().await;
        let junk = harness.backups().join("pretending.aushbackup");
        std::fs::write(&junk, b"not a container").unwrap();
        prune_safety_backups(&harness.backups());
        assert!(!junk.exists());
        assert!(
            harness
                .backups()
                .join("pretending.aushbackup.suspect")
                .exists()
        );
    }
}
