use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::reference_masters::ReferenceState;

const SESSION_COOKIE: &str = "aushadharth_session";
const SESSION_HOURS: i64 = 12;
const ATTEMPT_WINDOW_MINUTES: i64 = 5;
const ATTEMPT_RETENTION_HOURS: i64 = 24;
const FAILURE_THRESHOLD: i64 = 5;
static AUTH_WRITE_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug)]
pub(crate) enum AuthError {
    Validation(&'static str, &'static str),
    SetupUnavailable,
    InvalidCredentials,
    RateLimited(i64),
    AuthenticationRequired,
    SessionExpired,
    AuthorizationDenied,
    ServiceBusy,
    Internal,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthErrorBody {
    code: &'static str,
    message: &'static str,
    issues: Vec<AuthIssue>,
    retry_after_seconds: Option<i64>,
}

#[derive(Debug, Serialize)]
struct AuthIssue {
    field: &'static str,
    message: &'static str,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, code, message, issues, retry_after_seconds) = match self {
            Self::Validation(field, message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
                "The request failed validation.",
                vec![AuthIssue { field, message }],
                None,
            ),
            Self::SetupUnavailable => (
                StatusCode::CONFLICT,
                "setup_unavailable",
                "Initial setup has already been completed.",
                Vec::new(),
                None,
            ),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "The login ID or password is incorrect.",
                Vec::new(),
                None,
            ),
            Self::RateLimited(seconds) => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many attempts. Wait briefly and try again.",
                Vec::new(),
                Some(seconds.max(1)),
            ),
            Self::AuthenticationRequired => (
                StatusCode::UNAUTHORIZED,
                "authentication_required",
                "Sign in to continue.",
                Vec::new(),
                None,
            ),
            Self::SessionExpired => (
                StatusCode::UNAUTHORIZED,
                "session_expired",
                "Your session has expired. Sign in again.",
                Vec::new(),
                None,
            ),
            Self::AuthorizationDenied => (
                StatusCode::FORBIDDEN,
                "authorization_denied",
                "Your role does not permit this operation.",
                Vec::new(),
                None,
            ),
            Self::ServiceBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_busy",
                "The local service is busy. Try again shortly.",
                Vec::new(),
                Some(1),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The operation could not be completed.",
                Vec::new(),
                None,
            ),
        };
        (
            status,
            Json(AuthErrorBody {
                code,
                message,
                issues,
                retry_after_seconds,
            }),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupRequest {
    store_display_name: String,
    owner_display_name: String,
    login_identifier: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest {
    login_identifier: String,
    password: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SafeUser {
    id: String,
    login_identifier: String,
    display_name: String,
    role: String,
    revision: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatusResponse {
    setup_required: bool,
    authenticated: bool,
    user: Option<SafeUser>,
    store_display_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    user: SafeUser,
    store_display_name: String,
    expires_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardSummary {
    store_display_name: String,
    active_product_count: i64,
    active_pack_count: i64,
}

#[derive(Debug, FromRow)]
struct SessionRecord {
    user_id: String,
    expires_at_utc: String,
    status: String,
    login_identifier: String,
    display_name: String,
    role: String,
    revision: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedActor {
    pub id: String,
    pub role: String,
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/setup", post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/session", get(session))
        .route("/api/v1/dashboard/summary", get(dashboard_summary))
}

async fn auth_status(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<AuthStatusResponse>, AuthError> {
    validate_host(&headers)?;
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AuthError::Internal)?;
    let store_display_name = store_name(&state.pool).await?;
    let session = optional_session(&state.pool, &headers).await?;
    Ok(Json(AuthStatusResponse {
        setup_required: user_count == 0,
        authenticated: session.is_some(),
        user: session.map(|record| safe_user(&record)),
        store_display_name,
    }))
}

async fn setup(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<SetupRequest>,
) -> Result<Response, AuthError> {
    validate_mutation_request(&headers)?;
    let store_display_name = required_text(&request.store_display_name, "storeDisplayName", 120)?;
    let owner_display_name = required_text(&request.owner_display_name, "ownerDisplayName", 120)?;
    let (login_identifier, normalized_login) = normalize_login(&request.login_identifier)?;
    validate_password(&request.password)?;
    let password_hash = hash_password_blocking(request.password).await?;

    let _write_guard = AUTH_WRITE_LOCK.lock().await;
    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| AuthError::Internal)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;

    let result: Result<(SessionResponse, String), AuthError> = async {
        let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&mut *connection)
            .await
            .map_err(map_database_error)?;
        if user_count != 0 {
            return Err(AuthError::SetupUnavailable);
        }

        let now = now_text()?;
        let existing_store: Option<(String, String)> = sqlx::query_as(
            "SELECT store_id,display_name FROM store_identity LIMIT 1",
        )
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let final_store_name = if let Some((_, name)) = existing_store {
            name
        } else {
            sqlx::query(
                "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) VALUES (?,?,?,?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(&store_display_name)
            .bind("Asia/Kolkata")
            .bind(&now)
            .execute(&mut *connection)
            .await
            .map_err(map_database_error)?;
            store_display_name.clone()
        };

        let user = SafeUser {
            id: Uuid::now_v7().to_string(),
            login_identifier,
            display_name: owner_display_name,
            role: "owner_admin".to_owned(),
            revision: 1,
        };
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,password_hash,role,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?)",
        )
        .bind(&user.id)
        .bind(&user.login_identifier)
        .bind(&normalized_login)
        .bind(&user.display_name)
        .bind(&password_hash)
        .bind(&user.role)
        .bind(&now)
        .bind(&now)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let (token, expires_at_utc) = create_session_on(&mut connection, &user.id, &now).await?;
        Ok((
            SessionResponse { user, store_display_name: final_store_name, expires_at_utc },
            token,
        ))
    }
    .await;

    match result {
        Ok((body, token)) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            Ok(with_session_cookie(
                StatusCode::CREATED,
                body,
                &token,
                is_https_request(&headers),
            ))
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

async fn login(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Response, AuthError> {
    validate_mutation_request(&headers)?;
    let (_, normalized_login) =
        normalize_login(&request.login_identifier).map_err(|_| AuthError::InvalidCredentials)?;
    if request.password.len() > 512 {
        return Err(AuthError::InvalidCredentials);
    }
    let identifier_hash = sha256_hex(normalized_login.as_bytes());
    cleanup_stale_attempts(&state.pool).await?;
    enforce_cooldown(&state.pool, &identifier_hash).await?;

    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id,password_hash,status FROM users WHERE normalized_login_identifier=?",
    )
    .bind(&normalized_login)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?;

    let verified = if let Some((_, password_hash, _)) = &row {
        verify_password_blocking(request.password, password_hash.clone()).await?
    } else {
        verify_against_dummy_blocking(request.password).await?;
        false
    };
    let Some((user_id, _, status)) = row else {
        record_failure(&state.pool, &identifier_hash).await?;
        return Err(AuthError::InvalidCredentials);
    };
    if !verified || status != "active" {
        record_failure(&state.pool, &identifier_hash).await?;
        return Err(AuthError::InvalidCredentials);
    }

    let _write_guard = AUTH_WRITE_LOCK.lock().await;
    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let now = now_text()?;
    sqlx::query("DELETE FROM login_attempts WHERE identifier_hash=?")
        .bind(&identifier_hash)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    sqlx::query(
        "UPDATE users SET last_login_at_utc=?,updated_at_utc=? WHERE id=? AND status='active'",
    )
    .bind(&now)
    .bind(&now)
    .bind(&user_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (token, expires_at_utc) =
        create_session_in_transaction(&mut transaction, &user_id, &now).await?;
    transaction.commit().await.map_err(map_database_error)?;

    let user = fetch_safe_user(&state.pool, &user_id).await?;
    let store_display_name = store_name(&state.pool)
        .await?
        .unwrap_or_else(|| "AUSHADHARTH".to_owned());
    Ok(with_session_cookie(
        StatusCode::OK,
        SessionResponse {
            user,
            store_display_name,
            expires_at_utc,
        },
        &token,
        is_https_request(&headers),
    ))
}

async fn logout(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Response, AuthError> {
    validate_mutation_request(&headers)?;
    if let Some(token) = cookie_token(&headers) {
        let _write_guard = AUTH_WRITE_LOCK.lock().await;
        sqlx::query(
            "UPDATE user_sessions SET revoked_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE token_hash=? AND revoked_at_utc IS NULL",
        )
        .bind(sha256_hex(token.as_bytes()))
        .execute(&state.pool)
        .await
        .map_err(map_database_error)?;
    }
    let cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response())
}

async fn session(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, AuthError> {
    validate_host(&headers)?;
    let record = required_session(&state.pool, &headers).await?;
    let store_display_name = store_name(&state.pool)
        .await?
        .unwrap_or_else(|| "AUSHADHARTH".to_owned());
    Ok(Json(SessionResponse {
        user: safe_user(&record),
        store_display_name,
        expires_at_utc: record.expires_at_utc,
    }))
}

async fn dashboard_summary(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<DashboardSummary>, AuthError> {
    validate_host(&headers)?;
    required_session(&state.pool, &headers).await?;
    let store_display_name = store_name(&state.pool)
        .await?
        .unwrap_or_else(|| "AUSHADHARTH".to_owned());
    let active_product_count =
        sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE status='active'")
            .fetch_one(&state.pool)
            .await
            .map_err(|_| AuthError::Internal)?;
    let active_pack_count =
        sqlx::query_scalar("SELECT COUNT(*) FROM product_packs WHERE status='active'")
            .fetch_one(&state.pool)
            .await
            .map_err(|_| AuthError::Internal)?;
    Ok(Json(DashboardSummary {
        store_display_name,
        active_product_count,
        active_pack_count,
    }))
}

async fn create_session_on(
    connection: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
    user_id: &str,
    now: &str,
) -> Result<(String, String), AuthError> {
    let (token, token_hash) = new_session_token();
    let expires = (OffsetDateTime::now_utc() + Duration::hours(SESSION_HOURS))
        .format(&Rfc3339)
        .map_err(|_| AuthError::Internal)?;
    sqlx::query("INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,last_seen_at_utc) VALUES (?,?,?,?,?,?)")
        .bind(Uuid::now_v7().to_string()).bind(user_id).bind(token_hash).bind(now).bind(&expires).bind(now)
        .execute(&mut **connection).await.map_err(map_database_error)?;
    Ok((token, expires))
}

async fn create_session_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    now: &str,
) -> Result<(String, String), AuthError> {
    let (token, token_hash) = new_session_token();
    let expires = (OffsetDateTime::now_utc() + Duration::hours(SESSION_HOURS))
        .format(&Rfc3339)
        .map_err(|_| AuthError::Internal)?;
    sqlx::query("INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,last_seen_at_utc) VALUES (?,?,?,?,?,?)")
        .bind(Uuid::now_v7().to_string()).bind(user_id).bind(token_hash).bind(now).bind(&expires).bind(now)
        .execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok((token, expires))
}

async fn optional_session(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<Option<SessionRecord>, AuthError> {
    let Some(token) = cookie_token(headers) else {
        return Ok(None);
    };
    let record = fetch_session(pool, &token).await?;
    Ok(record.filter(|record| record.status == "active" && !is_expired(&record.expires_at_utc)))
}

async fn required_session(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<SessionRecord, AuthError> {
    let token = cookie_token(headers).ok_or(AuthError::AuthenticationRequired)?;
    let record = fetch_session(pool, &token)
        .await?
        .ok_or(AuthError::AuthenticationRequired)?;
    if record.status != "active" {
        return Err(AuthError::AuthenticationRequired);
    }
    if is_expired(&record.expires_at_utc) {
        return Err(AuthError::SessionExpired);
    }
    let _write_guard = AUTH_WRITE_LOCK.lock().await;
    // A session's created_at_utc comes from the high-resolution Rust clock while this touch reads
    // SQLite's, which on Windows can lag it by up to a timer tick. Taking the later of the two keeps
    // last_seen_at_utc monotonic and can never violate the frozen
    // `last_seen_at_utc >= created_at_utc` check, which would otherwise fail the first mutation
    // performed immediately after signing in.
    sqlx::query(
        "UPDATE user_sessions SET last_seen_at_utc=max(strftime('%Y-%m-%dT%H:%M:%fZ','now'),last_seen_at_utc) \
         WHERE token_hash=?",
    )
        .bind(sha256_hex(token.as_bytes())).execute(pool).await.map_err(map_database_error)?;
    Ok(record)
}

pub(crate) async fn require_authenticated_actor(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, AuthError> {
    validate_host(headers)?;
    let record = required_session(pool, headers).await?;
    Ok(AuthenticatedActor {
        id: record.user_id,
        role: record.role,
    })
}

async fn fetch_session(pool: &SqlitePool, token: &str) -> Result<Option<SessionRecord>, AuthError> {
    sqlx::query_as(
        "SELECT session.user_id,session.expires_at_utc,user.status,user.login_identifier,user.display_name,user.role,user.revision \
         FROM user_sessions session JOIN users user ON user.id=session.user_id \
         WHERE session.token_hash=? AND session.revoked_at_utc IS NULL",
    )
    .bind(sha256_hex(token.as_bytes()))
    .fetch_optional(pool)
    .await
    .map_err(|_| AuthError::Internal)
}

async fn fetch_safe_user(pool: &SqlitePool, id: &str) -> Result<SafeUser, AuthError> {
    sqlx::query_as("SELECT id,login_identifier,display_name,role,revision FROM users WHERE id=?")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|_| AuthError::Internal)
}

async fn store_name(pool: &SqlitePool) -> Result<Option<String>, AuthError> {
    sqlx::query_scalar("SELECT display_name FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|_| AuthError::Internal)
}

fn safe_user(record: &SessionRecord) -> SafeUser {
    SafeUser {
        id: record.user_id.clone(),
        login_identifier: record.login_identifier.clone(),
        display_name: record.display_name.clone(),
        role: record.role.clone(),
        revision: record.revision,
    }
}

fn required_text(value: &str, field: &'static str, maximum: usize) -> Result<String, AuthError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AuthError::Validation(field, "is required"));
    }
    if value.len() > maximum {
        return Err(AuthError::Validation(field, "is too long"));
    }
    Ok(value.to_owned())
}

fn normalize_login(value: &str) -> Result<(String, String), AuthError> {
    let login = value.trim();
    let normalized = login.to_ascii_lowercase();
    if !(3..=64).contains(&normalized.len())
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(AuthError::Validation(
            "loginIdentifier",
            "must be 3–64 letters, digits, dots, underscores, or hyphens",
        ));
    }
    Ok((login.to_owned(), normalized))
}

fn validate_password(password: &str) -> Result<(), AuthError> {
    let scalar_count = password.chars().count();
    let categories = [
        password.chars().any(char::is_uppercase),
        password.chars().any(char::is_lowercase),
        password.chars().any(char::is_numeric),
        password
            .chars()
            .any(|value| !value.is_uppercase() && !value.is_lowercase() && !value.is_numeric()),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if !(12..=128).contains(&scalar_count) || categories < 3 {
        return Err(AuthError::Validation(
            "password",
            "must be 12–128 characters and use at least three character types",
        ));
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::Internal)
}

async fn hash_password_blocking(password: String) -> Result<String, AuthError> {
    run_password_work(move || hash_password(&password)).await?
}

fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

async fn verify_password_blocking(password: String, encoded: String) -> Result<bool, AuthError> {
    run_password_work(move || verify_password(&password, &encoded)).await
}

fn verify_against_dummy(password: &str) -> Result<(), AuthError> {
    let dummy = hash_password("Dummy-password-1!")?;
    let _ = verify_password(password, &dummy);
    Ok(())
}

async fn verify_against_dummy_blocking(password: String) -> Result<(), AuthError> {
    run_password_work(move || verify_against_dummy(&password)).await?
}

async fn run_password_work<T, F>(work: F) -> Result<T, AuthError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AuthError::Internal)
}

fn map_database_error(error: sqlx::Error) -> AuthError {
    let is_contention = match &error {
        sqlx::Error::Database(database) => {
            matches!(
                database.code().as_deref(),
                Some("5" | "6" | "261" | "262" | "517")
            ) || {
                let message = database.message().to_ascii_lowercase();
                message.contains("database is locked")
                    || message.contains("database table is locked")
                    || message.contains("database is busy")
            }
        }
        _ => false,
    };
    if is_contention {
        AuthError::ServiceBusy
    } else {
        AuthError::Internal
    }
}

fn new_session_token() -> (String, String) {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha256_hex(token.as_bytes());
    (token, hash)
}

pub(crate) fn sha256_hex(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == SESSION_COOKIE && !value.is_empty()).then(|| value.to_owned())
        })
}

fn with_session_cookie<T: Serialize>(
    status: StatusCode,
    body: T,
    token: &str,
    secure: bool,
) -> Response {
    let secure_attribute = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure_attribute}",
        SESSION_HOURS * 60 * 60
    );
    (status, [(header::SET_COOKIE, cookie)], Json(body)).into_response()
}

/// The same guard, for the one kind of request that cannot be JSON.
///
/// Requiring `application/json` is not decoration: a cross-site HTML form can only send
/// `application/x-www-form-urlencoded`, `multipart/form-data` or `text/plain`, so demanding JSON
/// shuts that door on every normal mutation. A backup upload cannot be JSON, so it demands
/// `application/octet-stream` — which is equally impossible for a form to send, and therefore
/// keeps exactly the protection it replaces. Choosing multipart here would have given it away.
pub(crate) fn validate_binary_mutation_request(headers: &HeaderMap) -> Result<(), AuthError> {
    validate_host(headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .starts_with("application/octet-stream")
    {
        return Err(AuthError::Validation(
            "contentType",
            "must be application/octet-stream",
        ));
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        == Some("cross-site")
    {
        return Err(AuthError::Validation("origin", "is not allowed"));
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && !is_loopback_origin(origin)
    {
        return Err(AuthError::Validation("origin", "is not allowed"));
    }
    Ok(())
}

/// Re-verifies a signed-in user's own password.
///
/// Used only where a session alone is too weak a claim — a restore destroys the current database,
/// and a session left open at a counter should not be enough to do that. Reuses the existing
/// Argon2 path rather than introducing a second password mechanism.
pub(crate) async fn confirm_password(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    password: &str,
) -> Result<bool, AuthError> {
    let encoded: Option<String> = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AuthError::Internal)?;
    match encoded {
        Some(hash) => verify_password_blocking(password.to_owned(), hash).await,
        None => {
            // Constant-ish work even when the user has vanished, so timing says nothing.
            verify_against_dummy(password)?;
            Ok(false)
        }
    }
}

/// Revokes every session that is still live.
///
/// A restored database carries the sessions that were open on the machine the backup came from.
/// They were issued against a database that no longer exists, and the file may have travelled on a
/// USB stick since; none of them may survive.
pub(crate) async fn revoke_all_sessions(pool: &sqlx::SqlitePool) -> Result<u64, AuthError> {
    let result = sqlx::query(
        "UPDATE user_sessions SET revoked_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
         WHERE revoked_at_utc IS NULL",
    )
    .execute(pool)
    .await
    .map_err(|_| AuthError::Internal)?;
    Ok(result.rows_affected())
}

pub(crate) fn validate_mutation_request(headers: &HeaderMap) -> Result<(), AuthError> {
    validate_host(headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type
        .to_ascii_lowercase()
        .starts_with("application/json")
    {
        return Err(AuthError::Validation(
            "contentType",
            "must be application/json",
        ));
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        == Some("cross-site")
    {
        return Err(AuthError::Validation("origin", "is not allowed"));
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && !is_loopback_origin(origin)
    {
        return Err(AuthError::Validation("origin", "is not allowed"));
    }
    Ok(())
}

fn validate_host(headers: &HeaderMap) -> Result<(), AuthError> {
    if let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        && !is_loopback_authority(host)
    {
        return Err(AuthError::Validation("host", "is not allowed"));
    }
    Ok(())
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "[::1]"
    )
}

fn is_loopback_origin(origin: &str) -> bool {
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    !authority.contains('/') && is_loopback_authority(authority)
}

fn is_https_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"))
}

async fn enforce_cooldown(pool: &SqlitePool, identifier_hash: &str) -> Result<(), AuthError> {
    let cooldown: Option<String> =
        sqlx::query_scalar("SELECT cooldown_until_utc FROM login_attempts WHERE identifier_hash=?")
            .bind(identifier_hash)
            .fetch_optional(pool)
            .await
            .map_err(|_| AuthError::Internal)?
            .flatten();
    if let Some(cooldown) = cooldown
        && let Ok(until) = OffsetDateTime::parse(&cooldown, &Rfc3339)
        && until > OffsetDateTime::now_utc()
    {
        return Err(AuthError::RateLimited(
            (until - OffsetDateTime::now_utc()).whole_seconds() + 1,
        ));
    }
    Ok(())
}

async fn cleanup_stale_attempts(pool: &SqlitePool) -> Result<(), AuthError> {
    let cutoff = (OffsetDateTime::now_utc() - Duration::hours(ATTEMPT_RETENTION_HOURS))
        .format(&Rfc3339)
        .map_err(|_| AuthError::Internal)?;
    let _write_guard = AUTH_WRITE_LOCK.lock().await;
    sqlx::query("DELETE FROM login_attempts WHERE last_failed_at_utc < ?")
        .bind(cutoff)
        .execute(pool)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

async fn record_failure(pool: &SqlitePool, identifier_hash: &str) -> Result<(), AuthError> {
    let _write_guard = AUTH_WRITE_LOCK.lock().await;
    let mut connection = pool.acquire().await.map_err(|_| AuthError::Internal)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;

    let result = record_failure_on(&mut connection, identifier_hash).await;
    match result {
        Ok(()) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            Ok(())
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

async fn record_failure_on(
    connection: &mut sqlx::pool::PoolConnection<sqlx::Sqlite>,
    identifier_hash: &str,
) -> Result<(), AuthError> {
    let now = OffsetDateTime::now_utc();
    let existing: Option<(i64, String)> = sqlx::query_as(
        "SELECT failure_count,window_started_at_utc FROM login_attempts WHERE identifier_hash=?",
    )
    .bind(identifier_hash)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let (count, window_start) = match existing.and_then(|(count, start)| {
        OffsetDateTime::parse(&start, &Rfc3339)
            .ok()
            .map(|parsed| (count, parsed))
    }) {
        Some((count, start)) if now - start <= Duration::minutes(ATTEMPT_WINDOW_MINUTES) => {
            (count + 1, start)
        }
        _ => (1, now),
    };
    let cooldown_seconds = cooldown_seconds(count);
    let now_text = now.format(&Rfc3339).map_err(|_| AuthError::Internal)?;
    let window_text = window_start
        .format(&Rfc3339)
        .map_err(|_| AuthError::Internal)?;
    let cooldown = if cooldown_seconds > 0 {
        Some(
            (now + Duration::seconds(cooldown_seconds))
                .format(&Rfc3339)
                .map_err(|_| AuthError::Internal)?,
        )
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO login_attempts (identifier_hash,failure_count,window_started_at_utc,last_failed_at_utc,cooldown_until_utc) VALUES (?,?,?,?,?) \
         ON CONFLICT(identifier_hash) DO UPDATE SET failure_count=excluded.failure_count,window_started_at_utc=excluded.window_started_at_utc,last_failed_at_utc=excluded.last_failed_at_utc,cooldown_until_utc=excluded.cooldown_until_utc",
    ).bind(identifier_hash).bind(count).bind(window_text).bind(now_text).bind(cooldown)
      .execute(&mut **connection).await.map_err(map_database_error)?;
    Ok(())
}

fn cooldown_seconds(failure_count: i64) -> i64 {
    if failure_count >= FAILURE_THRESHOLD {
        (5_i64 * 2_i64.pow((failure_count - FAILURE_THRESHOLD).min(4) as u32)).min(60)
    } else {
        0
    }
}

fn now_text() -> Result<String, AuthError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| AuthError::Internal)
}

fn is_expired(value: &str) -> bool {
    OffsetDateTime::parse(value, &Rfc3339).map_or(true, |value| value <= OffsetDateTime::now_utc())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration as StdDuration, Instant},
    };

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    /// Worst documented contention path is two sequential busy timeouts; the third allows for test
    /// scheduling on a loaded machine without letting a deadlock or retry loop pass.
    const CONTENTION_CEILING: StdDuration =
        crate::infrastructure::database::BUSY_TIMEOUT.saturating_mul(3);

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("auth.sqlite3"))
            .await
            .unwrap();
        (temp, pool)
    }

    async fn request(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
        cookie: Option<&str>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:47831")
            .header(header::ORIGIN, "http://127.0.0.1:47831")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let response = crate::api::router(pool, None)
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            json!(null)
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, body)
    }

    async fn request_with_security_headers(
        pool: SqlitePool,
        uri: &str,
        body: Value,
        host: Option<&str>,
        origin: Option<&str>,
        sec_fetch_site: Option<&str>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(site) = sec_fetch_site {
            builder = builder.header("sec-fetch-site", site);
        }
        let response = crate::api::router(pool, None)
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            json!(null)
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, body)
    }

    fn setup_body() -> Value {
        json!({"storeDisplayName":"Main Pharmacy","ownerDisplayName":"Store Owner","loginIdentifier":"Owner.Admin","password":"Strong-Password-42"})
    }

    fn session_cookie(headers: &HeaderMap) -> String {
        headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn migration_is_restart_safe_and_has_auth_tables() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("restart.sqlite3");
        let first = crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
        for table in ["users", "user_sessions", "login_attempts"] {
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
            )
            .bind(table)
            .fetch_one(&first)
            .await
            .unwrap();
            assert_eq!(exists, 1);
        }
        first.close().await;
        crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn first_setup_is_atomic_hashed_and_returns_safe_session() {
        let (_temp, pool) = test_pool().await;
        let (status, headers, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["user"]["role"], "owner_admin");
        assert!(body.get("password").is_none() && body.get("passwordHash").is_none());
        let (id, login, hash): (String, String, String) =
            sqlx::query_as("SELECT id,normalized_login_identifier,password_hash FROM users")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 7);
        assert_eq!(login, "owner.admin");
        assert!(hash.starts_with("$argon2id$") && !hash.contains("Strong-Password-42"));
        assert!(verify_password("Strong-Password-42", &hash));
        let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(
            cookie.contains("HttpOnly")
                && cookie.contains("SameSite=Strict")
                && cookie.contains("Path=/")
        );
        assert!(!cookie.contains("; Secure"));
        assert!(!cookie.contains("Strong-Password-42"));
    }

    #[test]
    fn password_policy_counts_unicode_scalar_values_without_normalizing() {
        let eleven_scalars = format!("Ää١{}", "😀".repeat(8));
        let twelve_scalars = format!("Ää١{}", "😀".repeat(9));
        assert_eq!(eleven_scalars.chars().count(), 11);
        assert_eq!(twelve_scalars.chars().count(), 12);
        assert!(validate_password(&eleven_scalars).is_err());
        assert!(validate_password(&twelve_scalars).is_ok());

        let one_hundred_twenty_eight = format!("Aa1{}", "!".repeat(125));
        let one_hundred_twenty_nine = format!("Aa1{}", "!".repeat(126));
        assert_eq!(one_hundred_twenty_eight.chars().count(), 128);
        assert_eq!(one_hundred_twenty_nine.chars().count(), 129);
        assert!(validate_password(&one_hundred_twenty_eight).is_ok());
        assert!(validate_password(&one_hundred_twenty_nine).is_err());

        let composed = "Uppercase1-éé";
        let decomposed = "Uppercase1-e\u{301}e\u{301}";
        let hash = hash_password(composed).unwrap();
        assert!(verify_password(composed, &hash));
        assert!(!verify_password(decomposed, &hash));
    }

    #[test]
    fn cooldown_progression_is_bounded() {
        assert_eq!(
            (1..=10).map(cooldown_seconds).collect::<Vec<_>>(),
            vec![0, 0, 0, 0, 5, 10, 20, 40, 60, 60]
        );
        assert_eq!(cooldown_seconds(1_000), 60);
    }

    #[tokio::test]
    async fn second_and_concurrent_setup_cannot_create_two_owners() {
        let (_temp, pool) = test_pool().await;
        let first = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        );
        let second = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        );
        let (left, right) = tokio::join!(first, second);
        assert_eq!(
            [left.0, right.0]
                .into_iter()
                .filter(|status| *status == StatusCode::CREATED)
                .count(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        let (status, _, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "setup_unavailable");
    }

    #[tokio::test]
    async fn normalized_login_is_unique() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let hash = hash_password("Different-Password-42").unwrap();
        let duplicate = sqlx::query("INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,password_hash,role,created_at_utc,updated_at_utc) VALUES (?,'OWNER.ADMIN','owner.admin','Other',?,'cashier',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
            .bind(Uuid::now_v7().to_string()).bind(hash).execute(&pool).await;
        assert!(duplicate.is_err());
    }

    #[tokio::test]
    async fn login_is_generic_and_session_token_is_only_hashed() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        for login in ["owner.admin", "missing.user"] {
            let (status, _, body) = request(
                pool.clone(),
                "POST",
                "/api/v1/auth/login",
                json!({"loginIdentifier":login,"password":"Wrong-Password-42"}),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["code"], "invalid_credentials");
        }
        let (status, headers, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"OWNER.ADMIN","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let raw = session_cookie(&headers)
            .split_once('=')
            .unwrap()
            .1
            .to_owned();
        let stored: String = sqlx::query_scalar(
            "SELECT token_hash FROM user_sessions ORDER BY created_at_utc DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored.len(), 64);
        assert_ne!(stored, raw);
        assert_eq!(stored, sha256_hex(raw.as_bytes()));
    }

    #[tokio::test]
    async fn logout_expiry_and_disabled_users_invalidate_sessions() {
        let (_temp, pool) = test_pool().await;
        let (_, headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let cookie = session_cookie(&headers);
        let (status, _, _) = request(
            pool.clone(),
            "GET",
            "/api/v1/auth/session",
            json!(null),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        sqlx::query(
            "UPDATE user_sessions SET created_at_utc='1999-01-01T00:00:00Z',last_seen_at_utc='1999-01-01T00:00:00Z',expires_at_utc='2000-01-01T00:00:00Z'",
        )
            .execute(&pool)
            .await
            .unwrap();
        let (status, _, body) = request(
            pool.clone(),
            "GET",
            "/api/v1/auth/session",
            json!(null),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "session_expired");

        let (_, headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        let cookie = session_cookie(&headers);
        let (status, _, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/logout",
            json!({}),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = request(
            pool.clone(),
            "GET",
            "/api/v1/auth/session",
            json!(null),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        sqlx::query("UPDATE users SET status='disabled',status_changed_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),status_reason='Test',revision=revision+1").execute(&pool).await.unwrap();
        let (status, _, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "invalid_credentials");
    }

    #[tokio::test]
    async fn repeated_failures_create_short_cooldown_without_enumeration() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        for _ in 0..5 {
            let _ = request(
                pool.clone(),
                "POST",
                "/api/v1/auth/login",
                json!({"loginIdentifier":"owner.admin","password":"Wrong-Password-42"}),
                None,
            )
            .await;
        }
        let (status, _, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["code"], "rate_limited");
        assert!(body["retryAfterSeconds"].as_i64().unwrap() <= 6);
    }

    #[tokio::test]
    async fn concurrent_failures_are_counted_atomically_and_trigger_cooldown() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let attempt = || {
            request(
                pool.clone(),
                "POST",
                "/api/v1/auth/login",
                json!({"loginIdentifier":" OWNER.ADMIN ","password":"Wrong-Password-42"}),
                None,
            )
        };
        let (a, b, c, d, e) = tokio::join!(attempt(), attempt(), attempt(), attempt(), attempt());
        for result in [a, b, c, d, e] {
            assert_eq!(result.0, StatusCode::UNAUTHORIZED);
        }
        let (count, cooldown): (i64, Option<String>) = sqlx::query_as(
            "SELECT failure_count,cooldown_until_utc FROM login_attempts WHERE identifier_hash=?",
        )
        .bind(sha256_hex(b"owner.admin"))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 5);
        assert!(cooldown.is_some());
        let (status, _, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["code"], "rate_limited");
    }

    #[tokio::test]
    async fn successful_login_resets_only_its_attempt_state() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let failure = || {
            request(
                pool.clone(),
                "POST",
                "/api/v1/auth/login",
                json!({"loginIdentifier":"owner.admin","password":"Wrong-Password-42"}),
                None,
            )
        };
        failure().await;
        sqlx::query("INSERT INTO login_attempts (identifier_hash,failure_count,window_started_at_utc,last_failed_at_utc) VALUES (?,1,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
            .bind(sha256_hex(b"another.user"))
            .execute(&pool)
            .await
            .unwrap();
        let (status, _, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"OWNER.ADMIN","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let owner_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"owner.admin"))
                .fetch_one(&pool)
                .await
                .unwrap();
        let other_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"another.user"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((owner_rows, other_rows), (0, 1));
        failure().await;
        let fresh_count: i64 =
            sqlx::query_scalar("SELECT failure_count FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"owner.admin"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(fresh_count, 1);
    }

    #[tokio::test]
    async fn stale_attempt_cleanup_retains_the_active_protection_window() {
        let (_temp, pool) = test_pool().await;
        let now = OffsetDateTime::now_utc();
        let stale = (now - Duration::hours(25)).format(&Rfc3339).unwrap();
        let active = (now - Duration::minutes(4)).format(&Rfc3339).unwrap();
        for (hash, timestamp) in [("a".repeat(64), stale), ("b".repeat(64), active)] {
            sqlx::query("INSERT INTO login_attempts (identifier_hash,failure_count,window_started_at_utc,last_failed_at_utc) VALUES (?,1,?,?)")
                .bind(hash)
                .bind(&timestamp)
                .bind(&timestamp)
                .execute(&pool)
                .await
                .unwrap();
        }
        cleanup_stale_attempts(&pool).await.unwrap();
        let hashes: Vec<String> = sqlx::query_scalar(
            "SELECT identifier_hash FROM login_attempts ORDER BY identifier_hash",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(hashes, vec!["b".repeat(64)]);
    }

    #[tokio::test]
    async fn archived_users_cannot_authenticate_or_continue_existing_sessions() {
        let (_temp, pool) = test_pool().await;
        let (_, headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let cookie = session_cookie(&headers);
        sqlx::query("UPDATE users SET status='archived',status_changed_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),status_reason='Test archive',revision=revision+1")
            .execute(&pool)
            .await
            .unwrap();
        let (session_status, _, _) = request(
            pool.clone(),
            "GET",
            "/api/v1/auth/session",
            json!(null),
            Some(&cookie),
        )
        .await;
        assert_eq!(session_status, StatusCode::UNAUTHORIZED);
        let (login_status, _, login_body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(login_status, StatusCode::UNAUTHORIZED);
        assert_eq!(login_body["code"], "invalid_credentials");
    }

    #[tokio::test]
    async fn unexpired_session_survives_an_actual_database_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session-reopen.sqlite3");
        let pool = crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
        let (_, headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let cookie = session_cookie(&headers);
        pool.close().await;
        let reopened = crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
        let (status, _, body) = request(
            reopened.clone(),
            "GET",
            "/api/v1/auth/session",
            json!(null),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["user"]["loginIdentifier"], "Owner.Admin");
        reopened.close().await;
    }

    #[tokio::test]
    async fn every_successful_authentication_issues_a_fresh_session() {
        let (_temp, pool) = test_pool().await;
        let (_, setup_headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let (_, first_headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        let (_, second_headers, _) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        let cookies = [
            session_cookie(&setup_headers),
            session_cookie(&first_headers),
            session_cookie(&second_headers),
        ];
        assert_ne!(cookies[0], cookies[1]);
        assert_ne!(cookies[1], cookies[2]);
        assert_ne!(cookies[0], cookies[2]);
        let distinct_hashes: i64 =
            sqlx::query_scalar("SELECT COUNT(DISTINCT token_hash) FROM user_sessions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(distinct_hashes, 3);
    }

    #[tokio::test]
    async fn https_authentication_sets_secure_cookie_attributes() {
        let (_temp, pool) = test_pool().await;
        let (setup_status, setup_headers, setup_response) = request_with_security_headers(
            pool.clone(),
            "/api/v1/auth/setup",
            setup_body(),
            Some("localhost:47831"),
            Some("https://localhost:47831"),
            Some("same-origin"),
        )
        .await;
        assert_eq!(setup_status, StatusCode::CREATED, "{setup_response}");
        let setup_cookie = setup_headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Secure"] {
            assert!(setup_cookie.contains(attribute));
        }
        let (login_status, login_headers, login_response) = request_with_security_headers(
            pool.clone(),
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            Some("127.0.0.1:47831"),
            Some("https://127.0.0.1:47831"),
            Some("same-origin"),
        )
        .await;
        assert_eq!(login_status, StatusCode::OK, "{login_response}");
        let login_cookie = login_headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Secure"] {
            assert!(login_cookie.contains(attribute));
        }
    }

    #[tokio::test]
    async fn origin_and_host_policy_covers_browser_and_internal_clients() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let wrong_password =
            json!({"loginIdentifier":"owner.admin","password":"Wrong-Password-42"});
        for (host, origin) in [
            (Some("127.0.0.1:47831"), None),
            (Some("localhost:47831"), Some("http://localhost:4173")),
            (Some("127.0.0.1:47831"), Some("http://127.0.0.1:9999")),
            (Some("[::1]:47831"), Some("http://[::1]:4173")),
            (Some("localhost:47831"), Some("https://localhost:4443")),
        ] {
            let (status, _, body) = request_with_security_headers(
                pool.clone(),
                "/api/v1/auth/login",
                wrong_password.clone(),
                host,
                origin,
                None,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{host:?} {origin:?} {body}"
            );
            assert_eq!(body["code"], "invalid_credentials");
        }
        for origin in ["https://evil.example", "localhost:4173", "null"] {
            let (status, _, _) = request_with_security_headers(
                pool.clone(),
                "/api/v1/auth/login",
                wrong_password.clone(),
                Some("127.0.0.1:47831"),
                Some(origin),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{origin}");
        }
        let (cross_site, _, _) = request_with_security_headers(
            pool.clone(),
            "/api/v1/auth/login",
            wrong_password.clone(),
            Some("127.0.0.1:47831"),
            Some("http://127.0.0.1:4173"),
            Some("cross-site"),
        )
        .await;
        assert_eq!(cross_site, StatusCode::UNPROCESSABLE_ENTITY);
        let (foreign_host, _, _) = request_with_security_headers(
            pool.clone(),
            "/api/v1/auth/login",
            wrong_password,
            Some("evil.example:47831"),
            None,
            None,
        )
        .await;
        assert_eq!(foreign_host, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn hostile_browser_origin_is_rejected_and_loopback_remains_authoritative() {
        let (_temp, pool) = test_pool().await;
        let response = crate::api::router(pool, None)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/setup")
                    .header(header::HOST, "127.0.0.1:47831")
                    .header(header::ORIGIN, "https://evil.example")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(setup_body().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            crate::loopback_address(crate::DEFAULT_PORT)
                .ip()
                .is_loopback()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn password_work_boundary_does_not_block_the_async_executor() {
        let entered = Arc::new(AtomicBool::new(false));
        let heartbeat = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let observed_heartbeat = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(run_password_work({
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            move || {
                entered.store(true, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
            }
        }));
        let watchdog = std::thread::spawn({
            let entered = Arc::clone(&entered);
            let heartbeat = Arc::clone(&heartbeat);
            let release = Arc::clone(&release);
            let observed_heartbeat = Arc::clone(&observed_heartbeat);
            move || {
                while !entered.load(Ordering::SeqCst) {
                    std::thread::yield_now();
                }
                let deadline = Instant::now() + StdDuration::from_secs(1);
                while !heartbeat.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                observed_heartbeat.store(heartbeat.load(Ordering::SeqCst), Ordering::SeqCst);
                release.store(true, Ordering::SeqCst);
            }
        });
        while !entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        heartbeat.store(true, Ordering::SeqCst);
        worker.await.unwrap().unwrap();
        watchdog.join().unwrap();
        assert!(observed_heartbeat.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn blocking_password_helpers_preserve_hash_known_and_dummy_semantics() {
        let hash = hash_password_blocking("Strong-Password-42".to_owned())
            .await
            .unwrap();
        assert!(
            verify_password_blocking("Strong-Password-42".to_owned(), hash.clone())
                .await
                .unwrap()
        );
        assert!(
            !verify_password_blocking("Wrong-Password-42".to_owned(), hash)
                .await
                .unwrap()
        );
        verify_against_dummy_blocking("Unknown-Password-42".to_owned())
            .await
            .unwrap();
    }

    /// A contended authentication write must finish on the configured busy timeout, report the typed
    /// `service_busy` code with no database detail, and never surface as `internal_error`.
    ///
    /// The ceiling is derived from `BUSY_TIMEOUT` rather than written as a literal. A request may
    /// wait through two busy timeouts in sequence, because every auth write serialises behind the
    /// process-wide `AUTH_WRITE_LOCK` and its holder may itself be inside a busy wait;
    /// `CONTENTION_CEILING` allows that documented worst case plus scheduling margin, so only a
    /// deadlock or an unbounded retry can exceed it.
    #[tokio::test]
    async fn sqlite_contention_is_bounded_and_never_reported_as_internal_error() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let mut blocker = pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let started = Instant::now();
        let response = tokio::time::timeout(
            CONTENTION_CEILING,
            request(
                pool.clone(),
                "POST",
                "/api/v1/auth/login",
                json!({"loginIdentifier":"owner.admin","password":"Wrong-Password-42"}),
                None,
            ),
        )
        .await
        .expect("authentication contention must remain bounded");
        let waited = started.elapsed();
        sqlx::query("ROLLBACK")
            .execute(&mut *blocker)
            .await
            .unwrap();
        assert_eq!(response.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.2["code"], "service_busy");
        assert_ne!(response.2["code"], "internal_error");
        assert_no_database_detail(&response.2);
        // The request really did wait on the contended lock rather than failing early for an
        // unrelated reason. Scheduling can only make this slower, never faster.
        assert!(
            waited >= crate::infrastructure::database::BUSY_TIMEOUT / 2,
            "contended authentication returned after {waited:?} without waiting on the lock"
        );

        // Releasing the lock leaves authentication fully usable: no poisoned connection, and the
        // abandoned attempt neither consumed nor corrupted the rate-limit state.
        let (status, _, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// The typed `code` is the only machine detail a client may see; the human-facing text must not
    /// carry SQLite, sqlx, or Rust internals.
    fn assert_no_database_detail(body: &Value) {
        let rendered = format!("{} {}", body["message"], body["issues"]).to_lowercase();
        for leak in ["sqlite", "locked", "sqlx", "panicked", "error code"] {
            assert!(
                !rendered.contains(leak),
                "contention response leaked {leak:?}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn mixed_success_and_failure_for_one_identifier_is_serialized_safely() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let success = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        );
        let failure = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Wrong-Password-42"}),
            None,
        );
        let (success, failure) = tokio::time::timeout(StdDuration::from_secs(15), async {
            tokio::join!(success, failure)
        })
        .await
        .expect("mixed authentication must not deadlock");
        assert_eq!(success.0, StatusCode::OK);
        assert_eq!(failure.0, StatusCode::UNAUTHORIZED);
        let attempts: Option<i64> =
            sqlx::query_scalar("SELECT failure_count FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"owner.admin"))
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(matches!(attempts, None | Some(1)));
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 2);
    }

    #[tokio::test]
    async fn different_identifiers_progress_concurrently_without_state_leakage() {
        let (_temp, pool) = test_pool().await;
        request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        let owner = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"owner.admin","password":"Strong-Password-42"}),
            None,
        );
        let unknown = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/login",
            json!({"loginIdentifier":"unknown.user","password":"Wrong-Password-42"}),
            None,
        );
        let (owner, unknown) = tokio::time::timeout(StdDuration::from_secs(15), async {
            tokio::join!(owner, unknown)
        })
        .await
        .expect("independent authentication must not deadlock");
        assert_eq!(owner.0, StatusCode::OK);
        assert_eq!(unknown.0, StatusCode::UNAUTHORIZED);
        let unknown_count: i64 =
            sqlx::query_scalar("SELECT failure_count FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"unknown.user"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unknown_count, 1);
        let owner_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM login_attempts WHERE identifier_hash=?")
                .bind(sha256_hex(b"owner.admin"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(owner_count, 0);
    }

    /// The session touch must never violate `last_seen_at_utc >= created_at_utc`.
    ///
    /// `created_at_utc` is written from the high-resolution Rust clock while the touch reads
    /// SQLite's, which on Windows can lag by up to a timer tick — so a mutation performed within
    /// milliseconds of signing in could fail with an opaque internal error. A session created ahead
    /// of SQLite's clock reproduces that deterministically.
    #[tokio::test]
    async fn touching_a_session_never_moves_last_seen_before_its_creation() {
        let (_temp, pool) = test_pool().await;
        let (status, headers, body) = request(
            pool.clone(),
            "POST",
            "/api/v1/auth/setup",
            setup_body(),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let cookie = session_cookie(&headers);

        // Push the session's creation instant ahead of SQLite's clock, as a fast Rust clock does.
        sqlx::query(
            "UPDATE user_sessions SET created_at_utc='2099-01-01T00:00:00.000Z',\
             last_seen_at_utc='2099-01-01T00:00:00.000Z',expires_at_utc='2099-06-01T00:00:00.000Z'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (status, _, session) = request(
            pool.clone(),
            "GET",
            "/api/v1/auth/session",
            Value::Null,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");

        let last_seen: String =
            sqlx::query_scalar("SELECT last_seen_at_utc FROM user_sessions LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let created: String =
            sqlx::query_scalar("SELECT created_at_utc FROM user_sessions LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            last_seen >= created,
            "last_seen {last_seen} moved before created {created}"
        );
    }
}
