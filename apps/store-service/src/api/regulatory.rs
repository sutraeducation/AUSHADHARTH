//! Phase 1M-A — the endpoints that record regulatory truth.
//!
//! Four foundations, one module, because they answer one question between them: may this pharmacy
//! lawfully hand this product over, and who says so.
//!
//!   * a product's position under each schedule, effective-dated, with its source;
//!   * the label facts a criterion-based schedule entry may one day test;
//!   * the people the Drugs Rules name — the registered pharmacist and the competent person — kept
//!     apart from the people who hold a login here;
//!   * the licence forms the store holds and the two rule 65 record elections the licensee made.
//!
//! Every mutation is owner/admin work and every one is audited. Nothing here dispenses anything and
//! nothing here decides a sale: `domain::regulatory` answers, `api::sales` refuses.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::api::auth::{self, AuthError, AuthenticatedActor};
use crate::api::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text, required_text, validate_uuid_v7},
    regulatory::SCHEMES,
};

const CAPACITIES: [&str; 2] = ["registered_pharmacist", "competent_person"];
const LICENCE_FORMS: [&str; 8] = [
    "form_20", "form_20a", "form_20b", "form_20f", "form_20g", "form_21", "form_21a", "form_21b",
];
const ELECTIONS: [&str; 2] = [
    "rule_65_3_prescription_supply",
    "rule_65_4_non_prescription_schedule_c",
];

#[derive(Debug)]
enum RegulatoryError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    Revision {
        expected: i64,
        current: i64,
    },
    /// Two active findings cannot govern the same product, scheme and day.
    PeriodOverlap,
    /// An archived record is read-only; restore it first.
    Archived,
    Conflict,
    Internal,
}

impl From<AuthError> for RegulatoryError {
    fn from(error: AuthError) -> Self {
        Self::Auth(error)
    }
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

fn simple(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
        expected_revision: None,
        current_revision: None,
    }
}

impl IntoResponse for RegulatoryError {
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
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("not_found", "That record does not exist."),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The record changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                },
            ),
            Self::PeriodOverlap => (
                StatusCode::CONFLICT,
                simple(
                    "regulatory_period_overlaps",
                    "Another finding already governs that product and scheme over part of this period.",
                ),
            ),
            Self::Archived => (
                StatusCode::CONFLICT,
                simple("record_archived", "This record is archived."),
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                simple("record_conflict", "That record is already recorded."),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple(
                    "internal_error",
                    "The Local Store Service could not complete that.",
                ),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn map_database_error(error: sqlx::Error) -> RegulatoryError {
    let text = error.to_string();
    if text.contains("regulatory_classification_period_overlaps")
        || text.contains("record_election_period_overlaps")
    {
        return RegulatoryError::PeriodOverlap;
    }
    if text.contains("UNIQUE constraint failed") {
        return RegulatoryError::Conflict;
    }
    RegulatoryError::Internal
}

fn validation(field: &str, message: &str) -> RegulatoryError {
    RegulatoryError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn issue(error: CatalogValidationIssue) -> RegulatoryError {
    RegulatoryError::Validation(vec![error])
}

/// A calendar date, or a refusal. Nothing here accepts "today" from a browser.
fn validate_date(value: &str, field: &str) -> Result<String, RegulatoryError> {
    let trimmed = value.trim();
    let shaped = trimmed.len() == 10
        && trimmed.as_bytes()[4] == b'-'
        && trimmed.as_bytes()[7] == b'-'
        && trimmed
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if !shaped {
        return Err(validation(field, "must be a date such as 2026-09-20"));
    }
    let month: u32 = trimmed[5..7]
        .parse()
        .map_err(|_| RegulatoryError::Internal)?;
    let day: u32 = trimmed[8..10]
        .parse()
        .map_err(|_| RegulatoryError::Internal)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(validation(field, "must be a real calendar date"));
    }
    Ok(trimmed.to_owned())
}

async fn require_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, RegulatoryError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, RegulatoryError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

async fn current_store(state: &ReferenceState) -> Result<String, RegulatoryError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?
        .ok_or(RegulatoryError::NotFound)
}

struct AuditEvent<'a> {
    entity_type: &'a str,
    entity_id: &'a str,
    entity_revision: i64,
    action: &'a str,
    reason: Option<&'a str>,
    payload: serde_json::Value,
}

async fn record_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: AuditEvent<'_>,
    actor_id: &str,
) -> Result<(), RegulatoryError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(event.entity_type)
    .bind(event.entity_id)
    .bind(event.entity_revision)
    .bind(event.action)
    .bind(event.reason)
    .bind(event.payload.to_string())
    .bind(actor_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<String, RegulatoryError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

// ---------------------------------------------------------------------------------------------
// Product regulatory classification
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct ClassificationResponse {
    id: String,
    revision: i64,
    status: String,
    scheme: String,
    applies: bool,
    effective_from: String,
    effective_to: Option<String>,
    source_citation: String,
    reason: Option<String>,
    determined_by_user_id: String,
    created_at_utc: String,
    updated_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductRegulatoryResponse {
    product_id: String,
    product_kind: String,
    classifications: Vec<ClassificationResponse>,
    /// Every scheme, answered as at `resolvedOn`. `unknown` is the answer when nothing was found.
    resolved: Vec<SchemeAnswer>,
    resolved_on: String,
    /// What a Sale of this product on `resolvedOn` would do. The counter sees the same words.
    sale_gate: String,
    sale_gate_scheme: Option<String>,
    alcohol_percent_vv_hundredths: Option<i64>,
    attributes_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SchemeAnswer {
    scheme: String,
    answer: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateClassificationRequest {
    scheme: String,
    applies: bool,
    effective_from: String,
    effective_to: Option<String>,
    source_citation: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveRequest {
    expected_revision: i64,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductAttributesRequest {
    expected_revision: Option<i64>,
    alcohol_percent_vv_hundredths: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackAttributesRequest {
    expected_revision: Option<i64>,
    net_volume_millilitres_hundredths: Option<i64>,
}

async fn get_product_regulatory(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AsOfQuery>,
) -> Result<Json<ProductRegulatoryResponse>, RegulatoryError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;

    let product_kind: String = sqlx::query_scalar("SELECT product_kind FROM products WHERE id=?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?
        .ok_or(RegulatoryError::NotFound)?;

    let as_of = match query.as_of.as_deref() {
        Some(value) => validate_date(value, "asOf")?,
        None => sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?,
    };

    let classifications: Vec<ClassificationResponse> = sqlx::query_as(
        "SELECT id,revision,status,scheme,applies,effective_from,effective_to,source_citation,\
         reason,determined_by_user_id,created_at_utc,updated_at_utc \
         FROM product_regulatory_classifications WHERE product_id=? \
         ORDER BY scheme, effective_from DESC",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| RegulatoryError::Internal)?;
    let resolved = crate::domain::regulatory::resolve_for_product(&mut connection, &id, &as_of)
        .await
        .map_err(map_database_error)?;
    let gate = crate::domain::regulatory::gate(&product_kind, &resolved);

    let attributes: Option<(Option<i64>, i64)> = sqlx::query_as(
        "SELECT alcohol_percent_vv_hundredths,revision FROM product_regulatory_attributes \
         WHERE product_id=?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?;

    let (sale_gate, sale_gate_scheme) = match gate {
        crate::domain::regulatory::SaleGate::Clear => ("clear".to_owned(), None),
        crate::domain::regulatory::SaleGate::Unresolved => ("unresolved".to_owned(), None),
        crate::domain::regulatory::SaleGate::PrescriptionRequired => (
            "prescription_required".to_owned(),
            Some("schedule_h".to_owned()),
        ),
        crate::domain::regulatory::SaleGate::WorkflowUnavailable { scheme } => {
            ("workflow_unavailable".to_owned(), Some(scheme.to_owned()))
        }
    };

    Ok(Json(ProductRegulatoryResponse {
        product_id: id,
        product_kind,
        classifications,
        resolved: SCHEMES
            .iter()
            .map(|scheme| SchemeAnswer {
                scheme: (*scheme).to_owned(),
                answer: resolved.answer(scheme).as_str().to_owned(),
            })
            .collect(),
        resolved_on: as_of,
        sale_gate,
        sale_gate_scheme,
        alcohol_percent_vv_hundredths: attributes.as_ref().and_then(|row| row.0),
        attributes_revision: attributes.map(|row| row.1),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AsOfQuery {
    as_of: Option<String>,
}

async fn create_classification(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<CreateClassificationRequest>,
) -> Result<(StatusCode, Json<ClassificationResponse>), RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;

    let scheme = request.scheme.trim().to_ascii_lowercase();
    if !SCHEMES.contains(&scheme.as_str()) {
        return Err(validation(
            "scheme",
            "must be one of schedule_h, schedule_h1, schedule_x, schedule_c, schedule_c1, ndps_purview",
        ));
    }
    // A finding without an authority behind it is an opinion, and an opinion must not gate a sale.
    let source_citation =
        required_text(&request.source_citation, "sourceCitation", 300).map_err(issue)?;
    if source_citation.len() < 3 {
        return Err(validation(
            "sourceCitation",
            "must name the authority for this finding, such as a gazette notification",
        ));
    }
    let reason = optional_text(request.reason.as_deref(), "reason", 500).map_err(issue)?;
    let effective_from = validate_date(&request.effective_from, "effectiveFrom")?;
    let effective_to = match request.effective_to.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "effectiveTo")?),
        _ => None,
    };
    if let Some(end) = effective_to.as_deref()
        && end <= effective_from.as_str()
    {
        return Err(validation("effectiveTo", "must be after the start date"));
    }

    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM products WHERE id=?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?;
    if exists.is_none() {
        return Err(RegulatoryError::NotFound);
    }

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let classification_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO product_regulatory_classifications (id,product_id,scheme,applies,\
         effective_from,effective_to,source_citation,reason,determined_by_user_id,revision,status,\
         created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,1,'active',?,?)",
    )
    .bind(&classification_id)
    .bind(&id)
    .bind(&scheme)
    .bind(i64::from(request.applies))
    .bind(&effective_from)
    .bind(&effective_to)
    .bind(&source_citation)
    .bind(&reason)
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "product_regulatory_classification",
            entity_id: &classification_id,
            entity_revision: 1,
            action: "created",
            reason: reason.as_deref(),
            payload: serde_json::json!({
                "productId": id,
                "scheme": scheme,
                "applies": request.applies,
                "effectiveFrom": effective_from,
                "effectiveTo": effective_to,
                "sourceCitation": source_citation,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let created: ClassificationResponse = sqlx::query_as(
        "SELECT id,revision,status,scheme,applies,effective_from,effective_to,source_citation,\
         reason,determined_by_user_id,created_at_utc,updated_at_utc \
         FROM product_regulatory_classifications WHERE id=?",
    )
    .bind(&classification_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloseClassificationRequest {
    expected_revision: i64,
    effective_to: String,
    reason: String,
}

/// The lawful way to record that the law changed: end the old finding on the day the new one
/// begins. The old finding keeps answering for every date it covered, so a question about last year
/// still gets last year's answer. Only an open-ended or later-ending period can be closed, and only
/// to a date inside it — a close shortens a finding, it never extends or rewrites one.
///
/// Archiving is different and is for a finding entered in error: an archived row stops answering
/// for ANY date. That is why a change in law is a close and not an archive.
async fn close_classification(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((product_id, classification_id)): Path<(String, String)>,
    Json(request): Json<CloseClassificationRequest>,
) -> Result<Json<ClassificationResponse>, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&product_id, "productId").map_err(issue)?;
    validate_uuid_v7(&classification_id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;
    let effective_to = validate_date(&request.effective_to, "effectiveTo")?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<(i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT revision,status,effective_from,effective_to \
         FROM product_regulatory_classifications WHERE id=? AND product_id=?",
    )
    .bind(&classification_id)
    .bind(&product_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (revision, status, effective_from, current_to) =
        current.ok_or(RegulatoryError::NotFound)?;
    if status == "archived" {
        return Err(RegulatoryError::Archived);
    }
    if revision != request.expected_revision {
        return Err(RegulatoryError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    if effective_to.as_str() <= effective_from.as_str() {
        return Err(validation(
            "effectiveTo",
            "must be after the finding's start date",
        ));
    }
    if let Some(existing) = current_to.as_deref()
        && effective_to.as_str() >= existing
    {
        return Err(validation(
            "effectiveTo",
            "must shorten the finding; it already ends on or before that date",
        ));
    }

    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE product_regulatory_classifications SET effective_to=?,revision=?,updated_at_utc=? \
         WHERE id=?",
    )
    .bind(&effective_to)
    .bind(revision + 1)
    .bind(&now)
    .bind(&classification_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "product_regulatory_classification",
            entity_id: &classification_id,
            entity_revision: revision + 1,
            action: "updated",
            reason: Some(&reason),
            payload: serde_json::json!({
                "productId": product_id,
                "effectiveToBefore": current_to,
                "effectiveTo": effective_to,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let updated: ClassificationResponse = sqlx::query_as(
        "SELECT id,revision,status,scheme,applies,effective_from,effective_to,source_citation,\
         reason,determined_by_user_id,created_at_utc,updated_at_utc \
         FROM product_regulatory_classifications WHERE id=?",
    )
    .bind(&classification_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(updated))
}

/// A finding entered in error is withdrawn, never edited into a different finding. An archived
/// finding answers for no date at all; a change in the LAW is recorded by closing instead.
async fn archive_classification(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path((product_id, classification_id)): Path<(String, String)>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<ClassificationResponse>, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&product_id, "productId").map_err(issue)?;
    validate_uuid_v7(&classification_id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision,status FROM product_regulatory_classifications WHERE id=? AND product_id=?",
    )
    .bind(&classification_id)
    .bind(&product_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(RegulatoryError::NotFound)?;
    if status == "archived" {
        return Err(RegulatoryError::Archived);
    }
    if revision != request.expected_revision {
        return Err(RegulatoryError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE product_regulatory_classifications SET revision=?,status='archived',\
         archived_at_utc=?,archive_reason=?,updated_at_utc=? WHERE id=?",
    )
    .bind(revision + 1)
    .bind(&now)
    .bind(&reason)
    .bind(&now)
    .bind(&classification_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "product_regulatory_classification",
            entity_id: &classification_id,
            entity_revision: revision + 1,
            action: "archived",
            reason: Some(&reason),
            payload: serde_json::json!({ "productId": product_id }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let updated: ClassificationResponse = sqlx::query_as(
        "SELECT id,revision,status,scheme,applies,effective_from,effective_to,source_citation,\
         reason,determined_by_user_id,created_at_utc,updated_at_utc \
         FROM product_regulatory_classifications WHERE id=?",
    )
    .bind(&classification_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(updated))
}

/// Label facts. Recording them classifies nothing: only a classification row does that.
async fn put_product_attributes(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ProductAttributesRequest>,
) -> Result<Json<ProductRegulatoryResponse>, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    if let Some(value) = request.alcohol_percent_vv_hundredths
        && !(0..=10_000).contains(&value)
    {
        return Err(validation(
            "alcoholPercentVvHundredths",
            "must be between 0 and 10000 hundredths of one per cent v/v",
        ));
    }

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM product_regulatory_attributes WHERE product_id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let revision = match (current, request.expected_revision) {
        (None, None) => {
            sqlx::query(
                "INSERT INTO product_regulatory_attributes (product_id,\
                 alcohol_percent_vv_hundredths,recorded_by_user_id,revision,created_at_utc,\
                 updated_at_utc) VALUES (?,?,?,1,?,?)",
            )
            .bind(&id)
            .bind(request.alcohol_percent_vv_hundredths)
            .bind(&actor.id)
            .bind(&now)
            .bind(&now)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            1
        }
        (Some(current), Some(expected)) if current == expected => {
            sqlx::query(
                "UPDATE product_regulatory_attributes SET alcohol_percent_vv_hundredths=?,\
                 recorded_by_user_id=?,revision=?,updated_at_utc=? WHERE product_id=?",
            )
            .bind(request.alcohol_percent_vv_hundredths)
            .bind(&actor.id)
            .bind(current + 1)
            .bind(&now)
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            current + 1
        }
        (Some(current), expected) => {
            return Err(RegulatoryError::Revision {
                expected: expected.unwrap_or(0),
                current,
            });
        }
        (None, Some(expected)) => {
            return Err(RegulatoryError::Revision {
                expected,
                current: 0,
            });
        }
    };

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "product_regulatory_attributes",
            entity_id: &id,
            entity_revision: revision,
            action: if revision == 1 { "created" } else { "updated" },
            reason: None,
            payload: serde_json::json!({
                "alcoholPercentVvHundredths": request.alcohol_percent_vv_hundredths,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    get_product_regulatory(
        State(state),
        headers,
        Path(id),
        axum::extract::Query(AsOfQuery { as_of: None }),
    )
    .await
}

async fn put_pack_attributes(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PackAttributesRequest>,
) -> Result<StatusCode, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    if let Some(value) = request.net_volume_millilitres_hundredths
        && value <= 0
    {
        return Err(validation(
            "netVolumeMillilitresHundredths",
            "must be a positive number of hundredths of a millilitre",
        ));
    }
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM product_packs WHERE id=?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?;
    if exists.is_none() {
        return Err(RegulatoryError::NotFound);
    }

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM product_pack_regulatory_attributes WHERE product_pack_id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let revision = match (current, request.expected_revision) {
        (None, None) => {
            sqlx::query(
                "INSERT INTO product_pack_regulatory_attributes (product_pack_id,\
                 net_volume_millilitres_hundredths,recorded_by_user_id,revision,created_at_utc,\
                 updated_at_utc) VALUES (?,?,?,1,?,?)",
            )
            .bind(&id)
            .bind(request.net_volume_millilitres_hundredths)
            .bind(&actor.id)
            .bind(&now)
            .bind(&now)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            1
        }
        (Some(current), Some(expected)) if current == expected => {
            sqlx::query(
                "UPDATE product_pack_regulatory_attributes SET \
                 net_volume_millilitres_hundredths=?,recorded_by_user_id=?,revision=?,\
                 updated_at_utc=? WHERE product_pack_id=?",
            )
            .bind(request.net_volume_millilitres_hundredths)
            .bind(&actor.id)
            .bind(current + 1)
            .bind(&now)
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            current + 1
        }
        (Some(current), expected) => {
            return Err(RegulatoryError::Revision {
                expected: expected.unwrap_or(0),
                current,
            });
        }
        (None, Some(expected)) => {
            return Err(RegulatoryError::Revision {
                expected,
                current: 0,
            });
        }
    };

    // Pack volume is half of a criterion-based schedule test, so who recorded it is on the record.
    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "product_pack_regulatory_attributes",
            entity_id: &id,
            entity_revision: revision,
            action: if revision == 1 { "created" } else { "updated" },
            reason: None,
            payload: serde_json::json!({
                "netVolumeMillilitresHundredths": request.net_volume_millilitres_hundredths,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------------------------
// Professionals
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct ProfessionalResponse {
    id: String,
    revision: i64,
    status: String,
    full_name: String,
    capacity: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    linked_user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfessionalRequest {
    expected_revision: Option<i64>,
    full_name: String,
    capacity: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    linked_user_id: Option<String>,
    reason: Option<String>,
}

struct PreparedProfessional {
    full_name: String,
    capacity: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    linked_user_id: Option<String>,
    reason: Option<String>,
}

fn prepare_professional(
    request: &ProfessionalRequest,
) -> Result<PreparedProfessional, RegulatoryError> {
    let full_name = required_text(&request.full_name, "fullName", 120).map_err(issue)?;
    let capacity = request.capacity.trim().to_ascii_lowercase();
    if !CAPACITIES.contains(&capacity.as_str()) {
        return Err(validation(
            "capacity",
            "must be registered_pharmacist or competent_person",
        ));
    }
    let registration_number = optional_text(
        request.registration_number.as_deref(),
        "registrationNumber",
        60,
    )
    .map_err(issue)?;
    // Section 2(i) of the Pharmacy Act, 1948 defines a registered pharmacist by registration, so a
    // claim of that capacity carries the number that makes it checkable. The Rules prescribe no
    // register for a competent person, and a number is not invented for one.
    if capacity == "registered_pharmacist" && registration_number.is_none() {
        return Err(validation(
            "registrationNumber",
            "is required for a registered pharmacist",
        ));
    }
    let registering_authority = optional_text(
        request.registering_authority.as_deref(),
        "registeringAuthority",
        160,
    )
    .map_err(issue)?;
    let valid_from = match request.valid_from.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "validFrom")?),
        _ => None,
    };
    let valid_upto = match request.valid_upto.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "validUpto")?),
        _ => None,
    };
    if let (Some(start), Some(end)) = (valid_from.as_deref(), valid_upto.as_deref())
        && end < start
    {
        return Err(validation("validUpto", "must not be before the start date"));
    }
    let linked_user_id = match request.linked_user_id.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            validate_uuid_v7(value, "linkedUserId").map_err(issue)?;
            Some(value.trim().to_owned())
        }
        _ => None,
    };
    let reason = optional_text(request.reason.as_deref(), "reason", 500).map_err(issue)?;
    Ok(PreparedProfessional {
        full_name,
        capacity,
        registration_number,
        registering_authority,
        valid_from,
        valid_upto,
        linked_user_id,
        reason,
    })
}

async fn list_professionals(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ProfessionalResponse>>, RegulatoryError> {
    let actor = require_reader(&state, &headers).await?;
    // Registration data is professional compliance data: the counter needs to know a supervising
    // pharmacist exists, not to read the register entries of everyone employed.
    let rows: Vec<ProfessionalResponse> = sqlx::query_as(
        "SELECT id,revision,status,full_name,capacity,registration_number,registering_authority,\
         valid_from,valid_upto,linked_user_id FROM store_professionals ORDER BY full_name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let visible = rows
        .into_iter()
        .map(|mut row| {
            if actor.role != "owner_admin" {
                row.registration_number = None;
                row.registering_authority = None;
            }
            row
        })
        .collect();
    Ok(Json(visible))
}

async fn create_professional(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<ProfessionalRequest>,
) -> Result<(StatusCode, Json<ProfessionalResponse>), RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    let prepared = prepare_professional(&request)?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO store_professionals (id,store_id,full_name,capacity,registration_number,\
         registering_authority,valid_from,valid_upto,linked_user_id,revision,status,\
         created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,1,'active',?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&prepared.full_name)
    .bind(&prepared.capacity)
    .bind(&prepared.registration_number)
    .bind(&prepared.registering_authority)
    .bind(&prepared.valid_from)
    .bind(&prepared.valid_upto)
    .bind(&prepared.linked_user_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_professional",
            entity_id: &id,
            entity_revision: 1,
            action: "created",
            reason: prepared.reason.as_deref(),
            payload: serde_json::json!({
                "fullName": prepared.full_name,
                "capacity": prepared.capacity,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let created: ProfessionalResponse = sqlx::query_as(
        "SELECT id,revision,status,full_name,capacity,registration_number,registering_authority,\
         valid_from,valid_upto,linked_user_id FROM store_professionals WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn update_professional(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ProfessionalRequest>,
) -> Result<Json<ProfessionalResponse>, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let prepared = prepare_professional(&request)?;
    let expected = request
        .expected_revision
        .ok_or_else(|| validation("expectedRevision", "is required"))?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM store_professionals WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(RegulatoryError::NotFound)?;
    if status == "archived" {
        return Err(RegulatoryError::Archived);
    }
    if revision != expected {
        return Err(RegulatoryError::Revision {
            expected,
            current: revision,
        });
    }
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_professionals SET full_name=?,capacity=?,registration_number=?,\
         registering_authority=?,valid_from=?,valid_upto=?,linked_user_id=?,revision=?,\
         updated_at_utc=? WHERE id=?",
    )
    .bind(&prepared.full_name)
    .bind(&prepared.capacity)
    .bind(&prepared.registration_number)
    .bind(&prepared.registering_authority)
    .bind(&prepared.valid_from)
    .bind(&prepared.valid_upto)
    .bind(&prepared.linked_user_id)
    .bind(revision + 1)
    .bind(&now)
    .bind(&id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_professional",
            entity_id: &id,
            entity_revision: revision + 1,
            action: "updated",
            reason: prepared.reason.as_deref(),
            payload: serde_json::json!({
                "fullName": prepared.full_name,
                "capacity": prepared.capacity,
            }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let updated: ProfessionalResponse = sqlx::query_as(
        "SELECT id,revision,status,full_name,capacity,registration_number,registering_authority,\
         valid_from,valid_upto,linked_user_id FROM store_professionals WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(updated))
}

/// Archived, never deleted: a person who supervised a sale two years ago must still be nameable.
async fn archive_professional(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<ProfessionalResponse>, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM store_professionals WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(RegulatoryError::NotFound)?;
    if status == "archived" {
        return Err(RegulatoryError::Archived);
    }
    if revision != request.expected_revision {
        return Err(RegulatoryError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_professionals SET status='archived',archived_at_utc=?,archive_reason=?,\
         revision=?,updated_at_utc=? WHERE id=?",
    )
    .bind(&now)
    .bind(&reason)
    .bind(revision + 1)
    .bind(&now)
    .bind(&id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_professional",
            entity_id: &id,
            entity_revision: revision + 1,
            action: "archived",
            reason: Some(&reason),
            payload: serde_json::json!({}),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let updated: ProfessionalResponse = sqlx::query_as(
        "SELECT id,revision,status,full_name,capacity,registration_number,registering_authority,\
         valid_from,valid_upto,linked_user_id FROM store_professionals WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(updated))
}

// ---------------------------------------------------------------------------------------------
// Typed licence forms and the two rule 65 elections
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct ComplianceLicenceResponse {
    id: String,
    revision: i64,
    status: String,
    licence_form: String,
    licence_number: String,
    issuing_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    display_licence_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComplianceLicenceRequest {
    licence_form: String,
    licence_number: String,
    issuing_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    display_licence_id: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct RecordElectionResponse {
    id: String,
    revision: i64,
    status: String,
    election: String,
    method: String,
    effective_from: String,
    effective_to: Option<String>,
    evidence_reference: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordElectionRequest {
    election: String,
    method: String,
    effective_from: String,
    effective_to: Option<String>,
    evidence_reference: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DrugComplianceResponse {
    compliance_licences: Vec<ComplianceLicenceResponse>,
    record_elections: Vec<RecordElectionResponse>,
    professionals: Vec<ProfessionalResponse>,
}

async fn get_drug_compliance(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<DrugComplianceResponse>, RegulatoryError> {
    let actor = require_reader(&state, &headers).await?;
    let compliance_licences: Vec<ComplianceLicenceResponse> = sqlx::query_as(
        "SELECT id,revision,status,licence_form,licence_number,issuing_authority,valid_from,\
         valid_upto,display_licence_id FROM store_compliance_licences ORDER BY licence_form",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let record_elections: Vec<RecordElectionResponse> = sqlx::query_as(
        "SELECT id,revision,status,election,method,effective_from,effective_to,\
         evidence_reference,reason FROM store_record_elections \
         ORDER BY election, effective_from DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let professionals: Vec<ProfessionalResponse> = sqlx::query_as(
        "SELECT id,revision,status,full_name,capacity,registration_number,registering_authority,\
         valid_from,valid_upto,linked_user_id FROM store_professionals ORDER BY full_name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(DrugComplianceResponse {
        compliance_licences,
        record_elections,
        professionals: professionals
            .into_iter()
            .map(|mut row| {
                if actor.role != "owner_admin" {
                    row.registration_number = None;
                    row.registering_authority = None;
                }
                row
            })
            .collect(),
    }))
}

async fn create_compliance_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<ComplianceLicenceRequest>,
) -> Result<(StatusCode, Json<ComplianceLicenceResponse>), RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    let licence_form = request.licence_form.trim().to_ascii_lowercase();
    if !LICENCE_FORMS.contains(&licence_form.as_str()) {
        return Err(validation(
            "licenceForm",
            "must be a form of licence under rule 61, such as form_20 or form_20f",
        ));
    }
    let licence_number =
        required_text(&request.licence_number, "licenceNumber", 100).map_err(issue)?;
    let normalized = licence_number
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>()
        .to_uppercase();
    if normalized.is_empty() {
        return Err(validation(
            "licenceNumber",
            "must contain at least one letter or digit",
        ));
    }
    let issuing_authority = optional_text(
        request.issuing_authority.as_deref(),
        "issuingAuthority",
        160,
    )
    .map_err(issue)?;
    let valid_from = match request.valid_from.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "validFrom")?),
        _ => None,
    };
    let valid_upto = match request.valid_upto.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "validUpto")?),
        _ => None,
    };
    let display_licence_id = match request.display_licence_id.as_deref() {
        Some(value) if !value.trim().is_empty() => {
            validate_uuid_v7(value, "displayLicenceId").map_err(issue)?;
            Some(value.trim().to_owned())
        }
        _ => None,
    };
    let reason = optional_text(request.reason.as_deref(), "reason", 500).map_err(issue)?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO store_compliance_licences (id,store_id,licence_form,licence_number,\
         normalized_licence_number,issuing_authority,valid_from,valid_upto,display_licence_id,\
         revision,status,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,1,'active',?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&licence_form)
    .bind(&licence_number)
    .bind(&normalized)
    .bind(&issuing_authority)
    .bind(&valid_from)
    .bind(&valid_upto)
    .bind(&display_licence_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_compliance_licence",
            entity_id: &id,
            entity_revision: 1,
            action: "created",
            reason: reason.as_deref(),
            payload: serde_json::json!({ "licenceForm": licence_form }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let created: ComplianceLicenceResponse = sqlx::query_as(
        "SELECT id,revision,status,licence_form,licence_number,issuing_authority,valid_from,\
         valid_upto,display_licence_id FROM store_compliance_licences WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn archive_compliance_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<StatusCode, RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let current: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM store_compliance_licences WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(RegulatoryError::NotFound)?;
    if status == "archived" {
        return Err(RegulatoryError::Archived);
    }
    if revision != request.expected_revision {
        return Err(RegulatoryError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_compliance_licences SET status='archived',archived_at_utc=?,\
         archive_reason=?,revision=?,updated_at_utc=? WHERE id=?",
    )
    .bind(&now)
    .bind(&reason)
    .bind(revision + 1)
    .bind(&now)
    .bind(&id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_compliance_licence",
            entity_id: &id,
            entity_revision: revision + 1,
            action: "archived",
            reason: Some(&reason),
            payload: serde_json::json!({}),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// One election, recorded. The other is untouched: rule 65(3)(2) and rule 65(4)(2) are separate
/// options made separately, and a counter never makes either.
async fn create_record_election(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<RecordElectionRequest>,
) -> Result<(StatusCode, Json<RecordElectionResponse>), RegulatoryError> {
    let actor = require_admin(&state, &headers).await?;
    let election = request.election.trim().to_ascii_lowercase();
    if !ELECTIONS.contains(&election.as_str()) {
        return Err(validation(
            "election",
            "must be rule_65_3_prescription_supply or rule_65_4_non_prescription_schedule_c",
        ));
    }
    let method = request.method.trim().to_ascii_lowercase();
    let permitted: &[&str] = if election == "rule_65_3_prescription_supply" {
        &["prescription_register", "cash_or_credit_memo_book"]
    } else {
        &["register", "cash_or_credit_memo_book"]
    };
    if !permitted.contains(&method.as_str()) {
        return Err(validation(
            "method",
            "must be one of the alternatives that sub-rule offers",
        ));
    }
    let effective_from = validate_date(&request.effective_from, "effectiveFrom")?;
    let effective_to = match request.effective_to.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(validate_date(value, "effectiveTo")?),
        _ => None,
    };
    if let Some(end) = effective_to.as_deref()
        && end <= effective_from.as_str()
    {
        return Err(validation("effectiveTo", "must be after the start date"));
    }
    let evidence_reference = optional_text(
        request.evidence_reference.as_deref(),
        "evidenceReference",
        300,
    )
    .map_err(issue)?;
    let reason = optional_text(request.reason.as_deref(), "reason", 500).map_err(issue)?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO store_record_elections (id,store_id,election,method,effective_from,\
         effective_to,evidence_reference,reason,recorded_by_user_id,revision,status,\
         created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,1,'active',?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&election)
    .bind(&method)
    .bind(&effective_from)
    .bind(&effective_to)
    .bind(&evidence_reference)
    .bind(&reason)
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        AuditEvent {
            entity_type: "store_record_election",
            entity_id: &id,
            entity_revision: 1,
            action: "created",
            reason: reason.as_deref(),
            payload: serde_json::json!({ "election": election, "method": method }),
        },
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let created: RecordElectionResponse = sqlx::query_as(
        "SELECT id,revision,status,election,method,effective_from,effective_to,\
         evidence_reference,reason FROM store_record_elections WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route(
            "/api/v1/products/{id}/regulatory",
            get(get_product_regulatory),
        )
        .route(
            "/api/v1/products/{id}/regulatory/attributes",
            put(put_product_attributes),
        )
        .route(
            "/api/v1/products/{id}/regulatory/classifications",
            post(create_classification),
        )
        .route(
            "/api/v1/products/{product_id}/regulatory/classifications/{id}/archive",
            post(archive_classification),
        )
        .route(
            "/api/v1/products/{product_id}/regulatory/classifications/{id}/close",
            post(close_classification),
        )
        .route(
            "/api/v1/packs/{id}/regulatory/attributes",
            put(put_pack_attributes),
        )
        .route(
            "/api/v1/store/professionals",
            get(list_professionals).post(create_professional),
        )
        .route("/api/v1/store/professionals/{id}", put(update_professional))
        .route(
            "/api/v1/store/professionals/{id}/archive",
            post(archive_professional),
        )
        .route("/api/v1/store/drug-compliance", get(get_drug_compliance))
        .route(
            "/api/v1/store/compliance-licences",
            post(create_compliance_licence),
        )
        .route(
            "/api/v1/store/compliance-licences/{id}/archive",
            post(archive_compliance_licence),
        )
        .route(
            "/api/v1/store/record-elections",
            post(create_record_election),
        )
}
