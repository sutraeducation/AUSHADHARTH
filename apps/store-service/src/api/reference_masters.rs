use std::{collections::BTreeMap, str::FromStr};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{
    api::auth::{self, AuthError, AuthenticatedActor},
    domain::references::{DbValue, MasterKind, ValidationIssue, validate_attributes},
};

#[derive(Clone)]
pub struct ReferenceState {
    pub pool: SqlitePool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    search: Option<String>,
    status: Option<String>,
    parent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    attributes: Value,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    expected_revision: i64,
    attributes: Value,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleRequest {
    expected_revision: i64,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterResponse {
    id: String,
    kind: &'static str,
    revision: i64,
    status: String,
    attributes: Value,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, FromRow)]
struct StoredRow {
    id: String,
    revision: i64,
    status: String,
    attributes_json: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ReferenceError {
    Auth(AuthError),
    Validation(Vec<ValidationIssue>),
    Duplicate,
    RevisionConflict { expected: i64, current: i64 },
    NotFound,
    ArchivedConflict,
    EffectiveDateOverlap,
    Internal,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    issues: Vec<ErrorIssue>,
    expected_revision: Option<i64>,
    current_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ErrorIssue {
    field: String,
    message: String,
}

impl IntoResponse for ReferenceError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Validation(issues) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorBody {
                    code: "validation_failed",
                    message: "The request failed validation.",
                    issues: issues
                        .into_iter()
                        .map(|issue| ErrorIssue {
                            field: issue.field,
                            message: issue.message,
                        })
                        .collect(),
                    expected_revision: None,
                    current_revision: None,
                },
            ),
            Self::Duplicate => (
                StatusCode::CONFLICT,
                simple_error(
                    "duplicate_conflict",
                    "A conflicting active reference already exists.",
                ),
            ),
            Self::RevisionConflict { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The reference changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple_error("not_found", "The requested reference was not found."),
            ),
            Self::ArchivedConflict => (
                StatusCode::CONFLICT,
                simple_error(
                    "archived_conflict",
                    "The operation conflicts with the current lifecycle state.",
                ),
            ),
            Self::EffectiveDateOverlap => (
                StatusCode::CONFLICT,
                simple_error(
                    "effective_date_overlap",
                    "The effective period overlaps an active rate version.",
                ),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple_error("internal_error", "The operation could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

impl From<AuthError> for ReferenceError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

fn simple_error(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
        expected_revision: None,
        current_revision: None,
    }
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/reference/{kind}", get(list).post(create))
        .route("/api/v1/reference/{kind}/{id}", get(get_one).put(update))
        .route("/api/v1/reference/{kind}/{id}/archive", post(archive))
        .route("/api/v1/reference/{kind}/{id}/restore", post(restore))
}

async fn list(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<MasterResponse>>, ReferenceError> {
    auth::require_authenticated_actor(&state.pool, &headers).await?;
    let kind = parse_kind(&kind)?;
    let status = query.status.unwrap_or_else(|| "active".to_owned());
    if !matches!(status.as_str(), "active" | "archived" | "all") {
        return Err(validation("status", "must be active, archived, or all"));
    }
    let status_filter = if status == "all" { None } else { Some(status) };
    let search = query
        .search
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(parent_id) = query.parent_id.as_deref() {
        validate_id(parent_id)?;
    }
    let parent_column = match kind {
        MasterKind::CompanyIdentifier => Some("company_id"),
        MasterKind::TaxRateVersion => Some("tax_category_id"),
        MasterKind::PriceControlVersion => Some("controlled_formulation_id"),
        _ => None,
    };
    if query.parent_id.is_some() && parent_column.is_none() {
        return Err(validation(
            "parentId",
            "is supported only for company identifiers, tax rate versions and ceiling price versions",
        ));
    }
    let parent_expression = parent_column.unwrap_or("id");
    let sql = format!(
        "SELECT id, revision, status, {} AS attributes_json, created_at_utc, updated_at_utc, archived_at_utc, archive_reason \
         FROM {} WHERE (? IS NULL OR status = ?) AND (? IS NULL OR lower({}) LIKE '%' || ? || '%') \
         AND (? IS NULL OR {} = ?) \
         ORDER BY updated_at_utc DESC, id LIMIT 500",
        kind.json_expression(),
        kind.table(),
        kind.search_expression(),
        parent_expression
    );
    let rows = sqlx::query_as::<_, StoredRow>(&sql)
        .bind(&status_filter)
        .bind(&status_filter)
        .bind(&search)
        .bind(&search)
        .bind(&query.parent_id)
        .bind(&query.parent_id)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| ReferenceError::Internal)?;
    rows.into_iter()
        .map(|row| row.into_response(kind))
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

async fn get_one(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<MasterResponse>, ReferenceError> {
    auth::require_authenticated_actor(&state.pool, &headers).await?;
    let kind = parse_kind(&kind)?;
    validate_id(&id)?;
    fetch_one(&state.pool, kind, &id).await.map(Json)
}

async fn create(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(kind): Path<String>,
    Json(request): Json<CreateRequest>,
) -> Result<(StatusCode, Json<MasterResponse>), ReferenceError> {
    let actor = require_reference_admin(&state, &headers).await?;
    let kind = parse_kind(&kind)?;
    let fields =
        validate_attributes(kind, &request.attributes).map_err(ReferenceError::Validation)?;
    let id = Uuid::now_v7().to_string();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReferenceError::Internal)?;
    let now = database_now(&mut transaction).await?;
    insert_master(&mut transaction, kind, &id, &fields, &now).await?;
    insert_event(
        &mut transaction,
        kind,
        &id,
        1,
        "created",
        &fields,
        EventContext {
            reason: request.reason.as_deref(),
            actor_id: &actor.id,
        },
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    let response = fetch_one(&state.pool, kind, &id).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn update(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<MasterResponse>, ReferenceError> {
    let actor = require_reference_admin(&state, &headers).await?;
    let kind = parse_kind(&kind)?;
    validate_id(&id)?;
    if kind == MasterKind::TaxRateVersion {
        return Err(validation(
            "kind",
            "tax rate versions are immutable; archive and create a new effective version",
        ));
    }
    let fields =
        validate_attributes(kind, &request.attributes).map_err(ReferenceError::Validation)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReferenceError::Internal)?;
    let current = current_state(&mut transaction, kind, &id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != "active" {
        return Err(ReferenceError::ArchivedConflict);
    }
    let next_revision = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    update_master(&mut transaction, kind, &id, next_revision, &fields, &now).await?;
    insert_event(
        &mut transaction,
        kind,
        &id,
        next_revision,
        "updated",
        &fields,
        EventContext {
            reason: request.reason.as_deref(),
            actor_id: &actor.id,
        },
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    fetch_one(&state.pool, kind, &id).await.map(Json)
}

async fn archive(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<MasterResponse>, ReferenceError> {
    let actor = require_reference_admin(&state, &headers).await?;
    lifecycle_change(state, &kind, &id, request, false, &actor.id)
        .await
        .map(Json)
}

async fn restore(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, String)>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<MasterResponse>, ReferenceError> {
    let actor = require_reference_admin(&state, &headers).await?;
    lifecycle_change(state, &kind, &id, request, true, &actor.id)
        .await
        .map(Json)
}

async fn lifecycle_change(
    state: ReferenceState,
    kind_path: &str,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
    actor_id: &str,
) -> Result<MasterResponse, ReferenceError> {
    let kind = parse_kind(kind_path)?;
    validate_id(id)?;
    let reason = request.reason.trim();
    if reason.is_empty() || reason.len() > 500 {
        return Err(validation(
            "reason",
            "is required and must be at most 500 characters",
        ));
    }
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReferenceError::Internal)?;
    let current = current_state(&mut transaction, kind, id).await?;
    require_revision(&current, request.expected_revision)?;
    let expected_status = if restoring { "archived" } else { "active" };
    if current.1 != expected_status {
        return Err(ReferenceError::ArchivedConflict);
    }
    let next_revision = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    let sql = if restoring {
        format!(
            "UPDATE {} SET revision=?, status='active', updated_at_utc=?, archived_at_utc=NULL, archive_reason=NULL WHERE id=? AND revision=? AND status='archived'",
            kind.table()
        )
    } else {
        format!(
            "UPDATE {} SET revision=?, status='archived', updated_at_utc=?, archived_at_utc=?, archive_reason=? WHERE id=? AND revision=? AND status='active'",
            kind.table()
        )
    };
    let result = if restoring {
        sqlx::query(&sql)
            .bind(next_revision)
            .bind(&now)
            .bind(id)
            .bind(current.0)
            .execute(&mut *transaction)
            .await
    } else {
        sqlx::query(&sql)
            .bind(next_revision)
            .bind(&now)
            .bind(&now)
            .bind(reason)
            .bind(id)
            .bind(current.0)
            .execute(&mut *transaction)
            .await
    };
    result.map_err(map_database_error)?;
    let payload = BTreeMap::new();
    insert_event(
        &mut transaction,
        kind,
        id,
        next_revision,
        if restoring { "restored" } else { "archived" },
        &payload,
        EventContext {
            reason: Some(reason),
            actor_id,
        },
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    fetch_one(&state.pool, kind, id).await
}

async fn require_reference_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ReferenceError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

fn parse_kind(value: &str) -> Result<MasterKind, ReferenceError> {
    MasterKind::from_str(value)
        .map_err(|_| validation("kind", "is not a supported reference master"))
}

fn validate_id(value: &str) -> Result<(), ReferenceError> {
    let id = Uuid::parse_str(value).map_err(|_| validation("id", "must be UUIDv7"))?;
    if id.get_version_num() != 7 {
        return Err(validation("id", "must be UUIDv7"));
    }
    Ok(())
}

fn validation(field: &str, message: &str) -> ReferenceError {
    ReferenceError::Validation(vec![ValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), ReferenceError> {
    if current.0 != expected {
        return Err(ReferenceError::RevisionConflict {
            expected,
            current: current.0,
        });
    }
    Ok(())
}

async fn current_state(
    transaction: &mut Transaction<'_, Sqlite>,
    kind: MasterKind,
    id: &str,
) -> Result<(i64, String), ReferenceError> {
    let sql = format!("SELECT revision, status FROM {} WHERE id=?", kind.table());
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| ReferenceError::Internal)?
        .ok_or(ReferenceError::NotFound)
}

async fn database_now(transaction: &mut Transaction<'_, Sqlite>) -> Result<String, ReferenceError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| ReferenceError::Internal)
}

async fn insert_master(
    transaction: &mut Transaction<'_, Sqlite>,
    kind: MasterKind,
    id: &str,
    fields: &BTreeMap<&'static str, DbValue>,
    now: &str,
) -> Result<(), ReferenceError> {
    let columns = fields.keys().copied().collect::<Vec<_>>().join(",");
    let placeholders = std::iter::repeat_n("?", fields.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "INSERT INTO {} (id,revision,status,{},created_at_utc,updated_at_utc) VALUES (?,1,'active',{},?,?)",
        kind.table(),
        columns,
        placeholders
    );
    let mut query = sqlx::query(&sql).bind(id.to_owned());
    for value in fields.values().cloned() {
        query = bind(query, value);
    }
    query
        .bind(now.to_owned())
        .bind(now.to_owned())
        .execute(&mut **transaction)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

async fn update_master(
    transaction: &mut Transaction<'_, Sqlite>,
    kind: MasterKind,
    id: &str,
    revision: i64,
    fields: &BTreeMap<&'static str, DbValue>,
    now: &str,
) -> Result<(), ReferenceError> {
    let assignments = fields
        .keys()
        .map(|column| format!("{column}=?"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE {} SET {},revision=?,updated_at_utc=? WHERE id=? AND revision=? AND status='active'",
        kind.table(),
        assignments
    );
    let mut query = sqlx::query(&sql);
    for value in fields.values().cloned() {
        query = bind(query, value);
    }
    let result = query
        .bind(revision)
        .bind(now.to_owned())
        .bind(id.to_owned())
        .bind(revision - 1)
        .execute(&mut **transaction)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(ReferenceError::Internal);
    }
    Ok(())
}

fn bind<'q>(
    query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    value: DbValue,
) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match value {
        DbValue::Text(value) => query.bind(value),
        DbValue::Integer(value) => query.bind(value),
    }
}

struct EventContext<'a> {
    reason: Option<&'a str>,
    actor_id: &'a str,
}

async fn insert_event(
    transaction: &mut Transaction<'_, Sqlite>,
    kind: MasterKind,
    entity_id: &str,
    revision: i64,
    action: &str,
    fields: &BTreeMap<&'static str, DbValue>,
    context: EventContext<'_>,
) -> Result<(), ReferenceError> {
    let payload = normalized_payload(fields).to_string();
    sqlx::query(
        "INSERT INTO master_change_events \
         (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,?,?,?)",
    )
    .bind(Uuid::now_v7().to_string()).bind(kind.entity_type()).bind(entity_id).bind(revision)
    .bind(action).bind(context.reason).bind(1_i64).bind(payload).bind(context.actor_id)
    .execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

fn normalized_payload(fields: &BTreeMap<&'static str, DbValue>) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    DbValue::Text(Some(value)) => Value::String(value.clone()),
                    DbValue::Text(None) => Value::Null,
                    DbValue::Integer(value) => json!(value),
                };
                ((*key).to_owned(), value)
            })
            .collect::<Map<_, _>>(),
    )
}

async fn fetch_one(
    pool: &SqlitePool,
    kind: MasterKind,
    id: &str,
) -> Result<MasterResponse, ReferenceError> {
    let sql = format!(
        "SELECT id, revision, status, {} AS attributes_json, created_at_utc, updated_at_utc, archived_at_utc, archive_reason \
         FROM {} WHERE id=?",
        kind.json_expression(),
        kind.table()
    );
    let row = sqlx::query_as::<_, StoredRow>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| ReferenceError::Internal)?
        .ok_or(ReferenceError::NotFound)?;
    row.into_response(kind)
}

impl StoredRow {
    fn into_response(self, kind: MasterKind) -> Result<MasterResponse, ReferenceError> {
        Ok(MasterResponse {
            id: self.id,
            kind: kind.path(),
            revision: self.revision,
            status: self.status,
            attributes: serde_json::from_str(&self.attributes_json)
                .map_err(|_| ReferenceError::Internal)?,
            created_at_utc: self.created_at_utc,
            updated_at_utc: self.updated_at_utc,
            archived_at_utc: self.archived_at_utc,
            archive_reason: self.archive_reason,
        })
    }
}

fn map_database_error(error: sqlx::Error) -> ReferenceError {
    let message = error.to_string();
    // Every effective-period trigger in the schema raises its own typed abort. A new one that is
    // not listed here would surface as a raw 500, which is the defect class Phase 1E, 1G-0 and 1G
    // each hit in turn.
    if message.contains("tax_rate_effective_period_overlap")
        || message.contains("price_control_effective_period_overlap")
    {
        ReferenceError::EffectiveDateOverlap
    } else if message.contains("controlled_formulation_conflict")
        || message.contains("price_control_conflict")
    {
        validation(
            "attributes",
            "references a master that is missing or archived",
        )
    } else if message.contains("UNIQUE constraint failed") {
        ReferenceError::Duplicate
    } else if message.contains("FOREIGN KEY constraint failed")
        || message.contains("CHECK constraint failed")
    {
        validation("attributes", "violates a reference integrity constraint")
    } else {
        ReferenceError::Internal
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const OWNER_TOKEN: &str = "reference-owner-session-token";

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let temp = tempfile::tempdir().unwrap();
        let pool =
            crate::infrastructure::database::connect(&temp.path().join("references.sqlite3"))
                .await
                .unwrap();
        insert_session(&pool, "owner_admin", OWNER_TOKEN).await;
        (temp, pool)
    }

    async fn insert_session(pool: &SqlitePool, role: &str, token: &str) -> String {
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-{}", &user_id[0..8]);
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?, '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',?,?,?)",
        )
        .bind(&user_id)
        .bind(&login)
        .bind(&login)
        .bind(format!("{role} test user"))
        .bind(role)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,last_seen_at_utc) VALUES (?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&user_id)
        .bind(crate::api::auth::sha256_hex(token.as_bytes()))
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2099-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
        user_id
    }

    async fn request_json(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = crate::api::router(pool, None)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "127.0.0.1:47831")
                    .header("cookie", format!("aushadharth_session={OWNER_TOKEN}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn create_company(pool: &SqlitePool, name: &str) -> String {
        let (status, body) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/companies",
            json!({"attributes":{"displayName":name,"countryCode":"IN"}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        body["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn lifecycle_revisions_conflicts_and_audit_events_work_offline() {
        let (_temp, pool) = test_pool().await;
        let create_body = json!({"actorId":"browser-supplied-actor","attributes":{
            "canonicalCode":"dose_unit", "displayName":"Dose", "dimension":"count",
            "isDiscrete":true, "allowedScale":0
        }});
        let (status, created) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/units",
            create_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["revision"], 1);
        let id = created["id"].as_str().unwrap();
        assert_eq!(Uuid::parse_str(id).unwrap().get_version_num(), 7);

        let update_body = json!({"expectedRevision":1,"attributes":{
            "canonicalCode":"dose_unit", "displayName":"Dose unit", "dimension":"count",
            "isDiscrete":true, "allowedScale":0
        }});
        let (status, updated) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/reference/units/{id}"),
            update_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["revision"], 2);

        let (status, stale) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/reference/units/{id}"),
            update_body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["code"], "revision_conflict");
        assert_eq!(stale["currentRevision"], 2);

        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/units/{id}/archive"),
            json!({"expectedRevision":2,"reason":"No longer used"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(archived["status"], "archived");
        assert_eq!(archived["revision"], 3);

        let (status, restored) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/units/{id}/restore"),
            json!({"expectedRevision":3,"reason":"Required again"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["status"], "active");
        assert_eq!(restored["revision"], 4);

        let event_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='unit_of_measure' AND entity_id=?",
        ).bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(event_count, 4);
        let actor_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='unit_of_measure' AND entity_id=? AND actor_id=(SELECT id FROM users WHERE role='owner_admin')",
        ).bind(id).fetch_one(&pool).await.unwrap();
        assert_eq!(actor_count, 4);
    }

    #[tokio::test]
    async fn restore_reruns_verified_identifier_conflict_validation() {
        let (_temp, pool) = test_pool().await;
        let first_company = create_company(&pool, "First Pharma").await;
        let second_company = create_company(&pool, "Second Pharma").await;
        let attributes = |company_id: &str| {
            json!({"attributes":{
                "companyId":company_id, "namespace":"external", "normalizedValue":"ABC-123",
                "verificationState":"verified"
            }})
        };

        let (status, first) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/company-identifiers",
            attributes(&first_company),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let first_id = first["id"].as_str().unwrap();

        let (status, _) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/company-identifiers/{first_id}/archive"),
            json!({"expectedRevision":1,"reason":"Replacing identifier"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/company-identifiers",
            attributes(&second_company),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, conflict) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/company-identifiers/{first_id}/restore"),
            json!({"expectedRevision":2,"reason":"Attempt restore"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["code"], "duplicate_conflict");
    }

    #[tokio::test]
    async fn list_search_and_typed_validation_errors_work() {
        let (_temp, pool) = test_pool().await;
        let (status, units) = request_json(
            pool.clone(),
            "GET",
            "/api/v1/reference/units?search=tablet",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(units.as_array().unwrap().len(), 1);
        assert_eq!(units[0]["attributes"]["isDiscrete"], true);

        let (status, invalid) = request_json(
            pool,
            "POST",
            "/api/v1/reference/tax-rate-versions",
            json!({"attributes":{
                "taxCategoryId":"not-a-uuid", "effectiveFrom":"2025-02-30",
                "cgstBasisPoints":-1,"sgstBasisPoints":0,"igstBasisPoints":0
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid["code"], "validation_failed");
    }

    #[tokio::test]
    async fn reference_access_requires_session_and_owner_for_mutations() {
        let (_temp, pool) = test_pool().await;
        let router = || crate::api::router(pool.clone(), None);
        let unauthenticated = router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/reference/units")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let hostile_host = router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/reference/units")
                    .header("host", "attacker.example")
                    .header("cookie", format!("aushadharth_session={OWNER_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(hostile_host.status(), StatusCode::UNPROCESSABLE_ENTITY);

        for (role, token) in [
            ("cashier", "cashier-reference-token"),
            ("pharmacist", "pharmacist-reference-token"),
        ] {
            insert_session(&pool, role, token).await;
            let read = router()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/reference/units")
                        .header("cookie", format!("aushadharth_session={token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(read.status(), StatusCode::OK);

            let mutation = router()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/reference/dosage-forms")
                        .header("host", "127.0.0.1:47831")
                        .header("content-type", "application/json")
                        .header("cookie", format!("aushadharth_session={token}"))
                        .body(Body::from(
                            json!({"attributes":{
                                "canonicalCode":format!("{role}-form"),
                                "displayName":format!("{role} form")
                            }})
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(mutation.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn authenticated_duplicate_overlap_and_parent_filter_semantics_are_preserved() {
        let (_temp, pool) = test_pool().await;
        let (status, duplicate) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/units",
            json!({"attributes":{
                "canonicalCode":"tablet", "displayName":"Duplicate tablet",
                "dimension":"count", "isDiscrete":true, "allowedScale":0
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(duplicate["code"], "duplicate_conflict");

        let (status, category) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/tax-categories",
            json!({"attributes":{
                "jurisdiction":"IN", "categoryCode":"test-gst", "displayName":"Test GST",
                "taxTreatment":"taxable"
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let category_id = category["id"].as_str().unwrap();
        let rate = |from: &str, to: Option<&str>| {
            json!({"attributes":{
                "taxCategoryId":category_id, "effectiveFrom":from, "effectiveTo":to,
                "cgstBasisPoints":250, "sgstBasisPoints":250,
                "igstBasisPoints":500, "cessBasisPoints":0
            }})
        };
        let (status, _) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/tax-rate-versions",
            rate("2026-01-01", Some("2027-01-01")),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, overlap) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/tax-rate-versions",
            rate("2026-06-01", None),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(overlap["code"], "effective_date_overlap");

        let (status, rates) = request_json(
            pool,
            "GET",
            &format!("/api/v1/reference/tax-rate-versions?status=all&parentId={category_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rates.as_array().unwrap().len(), 1);
        assert_eq!(rates[0]["attributes"]["taxCategoryId"], category_id);
    }
}
