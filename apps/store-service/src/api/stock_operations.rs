//! Phase 1J — stock operations.
//!
//! Everything a pharmacy does to its own stock that is not a purchase, a sale or a return: counting
//! a shelf, writing off a crushed strip, holding a suspect carton back, classifying an expired lot,
//! and recording that goods physically left the building.
//!
//! Three rules shape the whole module.
//!
//! 1. **The operator states intent; the server states quantity.** A physical count takes what was
//!    counted, never a delta. The variance is computed inside the posting transaction against a
//!    balance read under its write lock, so a browser that was looking at a stale screen — or a
//!    browser that was lying — cannot decide how much stock exists.
//! 2. **Typed cause, supplementary prose.** `reason_code` is what a future register, valuation or
//!    authorisation rule reads. The note is for a human. Nothing downstream ever has to parse text
//!    to learn whether stock was stolen or merely miscounted.
//! 3. **Physical custody and saleability are different facts.** Writing goods off moves them
//!    between statuses and changes no total. Custody ends only through `stock_removal`, and only
//!    for goods already written off.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, Sqlite, SqlitePool, pool::PoolConnection};
use std::collections::HashMap;
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text, validate_date, validate_uuid_v7},
    stock_operations::{self as rules, Direction},
};

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum StockOperationError {
    Auth(AuthError),
    Validation(Vec<ErrorIssue>),
    NotFound,
    NotDraft,
    Revision { expected: i64, current: i64 },
    LineConflict,
    DuplicateCountLine,
    InsufficientStock { available: i64 },
    EmptyOperation,
    IdempotencyConflict,
    ArithmeticOverflow,
    BatchNotExpired,
    ServiceBusy,
    Internal,
}

impl From<AuthError> for StockOperationError {
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
    available_atoms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ErrorIssue {
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
        available_atoms: None,
    }
}

impl IntoResponse for StockOperationError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Auth(error) => return error.into_response(),
            Self::Validation(issues) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorBody {
                    code: "validation_failed",
                    message: "The request failed validation.",
                    issues,
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: None,
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple(
                    "stock_operation_not_found",
                    "The stock operation was not found.",
                ),
            ),
            Self::NotDraft => (
                StatusCode::CONFLICT,
                simple(
                    "stock_operation_not_draft",
                    "A posted stock operation cannot be changed. Record a later one instead.",
                ),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The stock operation changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                    available_atoms: None,
                },
            ),
            Self::LineConflict => (
                StatusCode::CONFLICT,
                simple(
                    "stock_operation_line_conflict",
                    "That line does not belong on this kind of stock operation.",
                ),
            ),
            Self::DuplicateCountLine => (
                StatusCode::CONFLICT,
                simple(
                    "duplicate_count_line",
                    "This count already has a line for that item, batch and stock status.",
                ),
            ),
            Self::InsufficientStock { available } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "insufficient_stock",
                    message: "There is not enough stock of that status to complete this.",
                    issues: Vec::new(),
                    expected_revision: None,
                    current_revision: None,
                    available_atoms: Some(available),
                },
            ),
            Self::EmptyOperation => (
                StatusCode::CONFLICT,
                simple(
                    "stock_operation_empty",
                    "Add at least one line before posting.",
                ),
            ),
            Self::IdempotencyConflict => (
                StatusCode::CONFLICT,
                simple(
                    "idempotency_conflict",
                    "That posting key was already used for a different stock operation.",
                ),
            ),
            Self::ArithmeticOverflow => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(
                    "arithmetic_overflow",
                    "That quantity is too large to record.",
                ),
            ),
            Self::BatchNotExpired => (
                StatusCode::CONFLICT,
                simple(
                    "batch_not_expired",
                    "That batch has not reached its expiry date.",
                ),
            ),
            Self::ServiceBusy => (
                StatusCode::CONFLICT,
                simple(
                    "service_busy",
                    "Another posting is in progress. Try again in a moment.",
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

fn validation_of(field: &str, message: &str) -> StockOperationError {
    StockOperationError::Validation(vec![ErrorIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> StockOperationError {
    StockOperationError::Validation(vec![ErrorIssue {
        field: issue.field,
        message: issue.message,
    }])
}

fn map_database_error(error: sqlx::Error) -> StockOperationError {
    let text = error.to_string();
    if text.contains("database is locked") || text.contains("database table is locked") {
        return StockOperationError::ServiceBusy;
    }
    if text.contains("stock_operation_line_conflict") {
        return StockOperationError::LineConflict;
    }
    if text.contains("stock_operation_is_posted") {
        return StockOperationError::NotDraft;
    }
    StockOperationError::Internal
}

fn map_overflow(_: rules::StockOperationError) -> StockOperationError {
    StockOperationError::ArithmeticOverflow
}

// ---------------------------------------------------------------------------------------------
// Routes and authorization
// ---------------------------------------------------------------------------------------------

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route(
            "/api/v1/stock-operations",
            get(list_operations).post(create_operation),
        )
        .route(
            "/api/v1/stock-operations/{id}",
            get(get_operation)
                .put(update_operation)
                .delete(discard_operation),
        )
        .route("/api/v1/stock-operations/{id}/lines", post(add_line))
        .route("/api/v1/stock-operations/{id}/quote", get(quote_operation))
        .route("/api/v1/stock-operations/{id}/post", post(post_operation))
        .route(
            "/api/v1/stock-operation-lines/{id}",
            delete(remove_line).put(update_line),
        )
}

/// Reading stock is something every counter user does; nothing here mutates.
async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, StockOperationError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// Who may work on a given kind of operation.
///
/// Counting, damage, expiry and quarantine are pharmacy floor work: a pharmacist does them. The
/// generic adjustment and the end of physical custody are not floor work — one can move a number
/// with no physical event behind it at all, and the other says goods have left the building — so
/// both stay with the owner. A cashier mutates no stock by any route.
async fn require_for_kind(
    state: &ReferenceState,
    headers: &HeaderMap,
    kind: &str,
) -> Result<AuthenticatedActor, StockOperationError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    let allowed: &[&str] = match kind {
        "adjustment" | "removal" => &["owner_admin"],
        _ => &["owner_admin", "pharmacist"],
    };
    if !allowed.contains(&actor.role.as_str()) {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

// ---------------------------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateOperationRequest {
    operation_kind: String,
    business_date: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOperationRequest {
    expected_revision: i64,
    business_date: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineRequest {
    expected_revision: i64,
    product_pack_id: String,
    batch_id: Option<String>,
    stock_status: String,
    target_stock_status: Option<String>,
    reason_code: String,
    /// Physical count only: what was counted, expressed in the basis below.
    counted_quantity: Option<i64>,
    /// Everything else: the quantity asked for, expressed in the basis below.
    quantity: Option<i64>,
    quantity_basis: String,
    /// Adjustment only; every other kind's direction follows from its kind.
    direction: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostOperationRequest {
    expected_revision: i64,
    idempotency_key: String,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    kind: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct OperationRow {
    id: String,
    store_id: String,
    operation_kind: String,
    business_date: String,
    status: String,
    revision: i64,
    note: Option<String>,
    created_by_user_id: String,
    created_at_utc: String,
    updated_at_utc: String,
    posted_by_user_id: Option<String>,
    posted_at_utc: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct LineRow {
    id: String,
    stock_operation_id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    stock_status: String,
    target_stock_status: Option<String>,
    direction: String,
    reason_code: String,
    counted_atoms: Option<i64>,
    quantity_atoms: Option<i64>,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    applied_delta_atoms: Option<i64>,
    note: Option<String>,
    // Read-only presentation, joined so a screen never has to render an identifier.
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    quantity_scale: i64,
    base_quantity_atoms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationDetail {
    #[serde(flatten)]
    operation: OperationRow,
    lines: Vec<LineRow>,
}

const OPERATION_COLUMNS: &str = "id,store_id,operation_kind,business_date,status,revision,note,\
     created_by_user_id,created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc";

/// Lines always arrive with the names and scales a screen needs, so no caller has to stitch a
/// product, a pack and a batch together to show one row.
const LINE_QUERY: &str = "SELECT line.id,line.stock_operation_id,line.line_number,line.product_id,\
     line.product_pack_id,line.batch_id,line.stock_status,line.target_stock_status,line.direction,\
     line.reason_code,line.counted_atoms,line.quantity_atoms,line.quantity_basis,\
     line.quantity_packs,line.applied_delta_atoms,line.note,\
     product.display_name AS product_display_name,pack.display_label AS pack_display_label,\
     unit.display_name AS base_unit_label,batch.batch_number,batch.expires_on AS batch_expires_on,\
     product.quantity_scale,pack.base_quantity_atoms \
     FROM stock_operation_lines line \
     JOIN products product ON product.id = line.product_id \
     JOIN product_packs pack ON pack.id = line.product_pack_id \
     LEFT JOIN units_of_measure unit ON unit.id = product.base_unit_id \
     LEFT JOIN product_batches batch ON batch.id = line.batch_id \
     WHERE line.stock_operation_id=? ORDER BY line.line_number";

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

async fn list_operations(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<OperationRow>>, StockOperationError> {
    require_reader(&state, &headers).await?;
    let kind = query.kind.as_deref().unwrap_or("all");
    let sql = format!(
        "SELECT {OPERATION_COLUMNS} FROM stock_operations \
         WHERE (?1 = 'all' OR operation_kind = ?1) \
         ORDER BY business_date DESC, created_at_utc DESC LIMIT 200"
    );
    let rows = sqlx::query_as::<_, OperationRow>(&sql)
        .bind(kind)
        .fetch_all(&state.pool)
        .await
        .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<OperationDetail>, StockOperationError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn fetch_detail(pool: &SqlitePool, id: &str) -> Result<OperationDetail, StockOperationError> {
    let operation = sqlx::query_as::<_, OperationRow>(&format!(
        "SELECT {OPERATION_COLUMNS} FROM stock_operations WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(StockOperationError::NotFound)?;
    let lines = sqlx::query_as::<_, LineRow>(LINE_QUERY)
        .bind(id)
        .fetch_all(pool)
        .await
        .map_err(map_database_error)?;
    Ok(OperationDetail { operation, lines })
}

// ---------------------------------------------------------------------------------------------
// Draft lifecycle
// ---------------------------------------------------------------------------------------------

async fn create_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreateOperationRequest>,
) -> Result<(StatusCode, Json<OperationDetail>), StockOperationError> {
    let kind = request.operation_kind.trim().to_owned();
    if rules::rules_for(&kind).is_none() {
        return Err(validation_of("operationKind", "is not a stock operation"));
    }
    let actor = require_for_kind(&state, &headers, &kind).await?;
    let business_date = validate_date(Some(&request.business_date), "businessDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("businessDate", "is required"))?;
    let note = optional_text(request.note.as_deref(), "note", 500).map_err(validation_issue)?;
    let store_id = current_store(&state.pool).await?;
    let id = Uuid::now_v7().to_string();

    sqlx::query(
        "INSERT INTO stock_operations (id,store_id,operation_kind,business_date,status,revision,\
         note,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,'draft',1,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
         strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&kind)
    .bind(&business_date)
    .bind(&note)
    .bind(&actor.id)
    .execute(&state.pool)
    .await
    .map_err(map_database_error)?;

    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateOperationRequest>,
) -> Result<Json<OperationDetail>, StockOperationError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let context = load_context(&state.pool, &id).await?;
    let actor = require_for_kind(&state, &headers, &context.operation_kind).await?;
    let _ = &actor;
    let business_date = validate_date(Some(&request.business_date), "businessDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("businessDate", "is required"))?;
    let note = optional_text(request.note.as_deref(), "note", 500).map_err(validation_issue)?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query(
        "UPDATE stock_operations SET business_date=?,note=?,revision=revision+1,\
         updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
    )
    .bind(&business_date)
    .bind(&note)
    .bind(&id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

// ---------------------------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------------------------

struct PreparedLine {
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    stock_status: String,
    target_stock_status: Option<String>,
    direction: Direction,
    reason_code: String,
    counted_atoms: Option<i64>,
    quantity_atoms: Option<i64>,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    note: Option<String>,
}

#[derive(Debug, FromRow)]
struct PackRow {
    product_id: String,
    base_quantity_atoms: i64,
    status: String,
}

/// Turns a request into a line the document's kind allows, or refuses it.
///
/// The direction is derived from the kind wherever the kind determines it. Only a generic
/// adjustment asks the operator which way, because only there is it genuinely a choice.
async fn prepare_line(
    pool: &SqlitePool,
    context: &OperationContext,
    request: &LineRequest,
) -> Result<PreparedLine, StockOperationError> {
    validate_uuid_v7(&request.product_pack_id, "productPackId").map_err(validation_issue)?;
    let batch_id = match request.batch_id.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => {
            Some(validate_uuid_v7(value, "batchId").map_err(validation_issue)?)
        }
        _ => None,
    };

    let kind_rules =
        rules::rules_for(&context.operation_kind).ok_or(StockOperationError::LineConflict)?;

    let direction = match context.operation_kind.as_str() {
        "physical_count" => Direction::Count,
        "adjustment" => match request.direction.as_deref().map(str::trim) {
            Some("increase") => Direction::Increase,
            Some("decrease") => Direction::Decrease,
            _ => {
                return Err(validation_of(
                    "direction",
                    "must be increase or decrease for an adjustment",
                ));
            }
        },
        "removal" => Direction::Decrease,
        _ => Direction::Transfer,
    };

    let stock_status = request.stock_status.trim().to_owned();
    let target_stock_status = match request.target_stock_status.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => Some(value.to_owned()),
        _ => None,
    };
    let reason_code = request.reason_code.trim().to_owned();

    if !rules::line_is_permitted(
        &context.operation_kind,
        direction,
        &stock_status,
        target_stock_status.as_deref(),
        &reason_code,
    ) {
        return Err(StockOperationError::LineConflict);
    }
    let _ = kind_rules;

    // Pack and product identity come from the database, never from the request body.
    let pack = sqlx::query_as::<_, PackRow>(
        "SELECT product_id,base_quantity_atoms,status FROM product_packs WHERE id=?",
    )
    .bind(&request.product_pack_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(StockOperationError::NotFound)?;
    if pack.status != "active" {
        return Err(validation_of("productPackId", "is not an active pack"));
    }
    if let Some(batch) = &batch_id {
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT product_pack_id FROM product_batches WHERE id=? AND status='active'",
        )
        .bind(batch)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?;
        match owner {
            Some(pack_id) if pack_id == request.product_pack_id => {}
            Some(_) => return Err(validation_of("batchId", "belongs to a different pack")),
            None => return Err(validation_of("batchId", "is not an active batch")),
        }
    }

    // A transfer and a removal always concern a specific lot; the ledger requires it.
    if batch_id.is_none()
        && (direction == Direction::Transfer || context.operation_kind == "removal")
    {
        return Err(validation_of(
            "batchId",
            "is required, because this operation acts on a specific batch",
        ));
    }

    let basis = request.quantity_basis.trim().to_owned();
    if !matches!(basis.as_str(), "pack" | "base_unit") {
        return Err(validation_of("quantityBasis", "must be pack or base_unit"));
    }

    let (counted_atoms, quantity_atoms, quantity_packs) = if direction == Direction::Count {
        let counted = request
            .counted_quantity
            .ok_or_else(|| validation_of("countedQuantity", "is required for a count"))?;
        if counted < 0 {
            return Err(validation_of(
                "countedQuantity",
                "cannot be negative; a shelf holds nothing at worst",
            ));
        }
        let (atoms, packs) = to_atoms(&basis, counted, pack.base_quantity_atoms)?;
        (Some(atoms), None, packs)
    } else {
        let quantity = request
            .quantity
            .ok_or_else(|| validation_of("quantity", "is required"))?;
        if quantity < 1 {
            return Err(validation_of("quantity", "must be at least one"));
        }
        let (atoms, packs) = to_atoms(&basis, quantity, pack.base_quantity_atoms)?;
        (None, Some(atoms), packs)
    };

    let note = optional_text(request.note.as_deref(), "note", 500).map_err(validation_issue)?;
    // A hold or a correction that says nothing is an unexplained stock movement, which is what a
    // ledger exists to prevent. The typed reason says what kind of thing happened; these two say
    // which thing.
    if matches!(reason_code.as_str(), "quality_hold" | "data_correction") && note.is_none() {
        return Err(validation_of(
            "note",
            "is required, so the ledger records why this was done",
        ));
    }

    Ok(PreparedLine {
        product_id: pack.product_id,
        product_pack_id: request.product_pack_id.clone(),
        batch_id,
        stock_status,
        target_stock_status,
        direction,
        reason_code,
        counted_atoms,
        quantity_atoms,
        quantity_packs: packs_for(&basis, quantity_packs),
        quantity_basis: basis,
        note,
    })
}

fn packs_for(basis: &str, packs: Option<i64>) -> Option<i64> {
    if basis == "pack" { packs } else { None }
}

/// Exact integer conversion from the operator's basis into base-unit atoms.
fn to_atoms(
    basis: &str,
    quantity: i64,
    base_quantity_atoms: i64,
) -> Result<(i64, Option<i64>), StockOperationError> {
    if basis == "base_unit" {
        if quantity > rules::MAX_OPERATION_ATOMS {
            return Err(StockOperationError::ArithmeticOverflow);
        }
        return Ok((quantity, None));
    }
    if base_quantity_atoms < 1 {
        return Err(validation_of("productPackId", "has no pack size"));
    }
    let atoms = quantity
        .checked_mul(base_quantity_atoms)
        .ok_or(StockOperationError::ArithmeticOverflow)?;
    if atoms > rules::MAX_OPERATION_ATOMS {
        return Err(StockOperationError::ArithmeticOverflow);
    }
    Ok((atoms, Some(quantity)))
}

async fn add_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<(StatusCode, Json<OperationDetail>), StockOperationError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let context = load_context(&state.pool, &id).await?;
    require_for_kind(&state, &headers, &context.operation_kind).await?;
    let prepared = prepare_line(&state.pool, &context, &request).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;

    // Counting the same shelf twice on one document is not more information, it is a contradiction.
    if prepared.direction == Direction::Count {
        let clash: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stock_operation_lines WHERE stock_operation_id=? \
             AND product_pack_id=? AND batch_id IS ? AND stock_status=?",
        )
        .bind(&id)
        .bind(&prepared.product_pack_id)
        .bind(&prepared.batch_id)
        .bind(&prepared.stock_status)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if clash > 0 {
            return Err(StockOperationError::DuplicateCountLine);
        }
    }

    let next_line: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(line_number),0)+1 FROM stock_operation_lines WHERE stock_operation_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    insert_line(&mut transaction, &id, next_line, &prepared).await?;
    bump_revision(&mut transaction, &id).await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn insert_line(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    operation_id: &str,
    line_number: i64,
    prepared: &PreparedLine,
) -> Result<String, StockOperationError> {
    let line_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO stock_operation_lines (id,stock_operation_id,line_number,product_id,\
         product_pack_id,batch_id,stock_status,target_stock_status,direction,reason_code,\
         counted_atoms,quantity_atoms,quantity_basis,quantity_packs,note,created_at_utc,\
         updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,\
         strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(&line_id)
    .bind(operation_id)
    .bind(line_number)
    .bind(&prepared.product_id)
    .bind(&prepared.product_pack_id)
    .bind(&prepared.batch_id)
    .bind(&prepared.stock_status)
    .bind(&prepared.target_stock_status)
    .bind(prepared.direction.as_str())
    .bind(&prepared.reason_code)
    .bind(prepared.counted_atoms)
    .bind(prepared.quantity_atoms)
    .bind(&prepared.quantity_basis)
    .bind(prepared.quantity_packs)
    .bind(&prepared.note)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(line_id)
}

async fn update_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<Json<OperationDetail>, StockOperationError> {
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let operation_id = line_document(&state.pool, &line_id).await?;
    let context = load_context(&state.pool, &operation_id).await?;
    require_for_kind(&state, &headers, &context.operation_kind).await?;
    let prepared = prepare_line(&state.pool, &context, &request).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    let current = draft_state(&mut transaction, &operation_id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query(
        "UPDATE stock_operation_lines SET product_id=?,product_pack_id=?,batch_id=?,\
         stock_status=?,target_stock_status=?,direction=?,reason_code=?,counted_atoms=?,\
         quantity_atoms=?,quantity_basis=?,quantity_packs=?,note=?,\
         updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
    )
    .bind(&prepared.product_id)
    .bind(&prepared.product_pack_id)
    .bind(&prepared.batch_id)
    .bind(&prepared.stock_status)
    .bind(&prepared.target_stock_status)
    .bind(prepared.direction.as_str())
    .bind(&prepared.reason_code)
    .bind(prepared.counted_atoms)
    .bind(prepared.quantity_atoms)
    .bind(&prepared.quantity_basis)
    .bind(prepared.quantity_packs)
    .bind(&prepared.note)
    .bind(&line_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    bump_revision(&mut transaction, &operation_id).await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &operation_id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveLineRequest {
    expected_revision: i64,
}

/// Throws away a draft nobody posted.
///
/// A draft stock operation is not a record of anything: it has moved no stock, issued no number
/// and told nobody anything. Phases before this one had no way to abandon one, so every screen a
/// counter user opened and backed out of left a permanent row in a list — which is how a list
/// stops being worth reading. A posted operation is refused here and by the database.
async fn discard_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RemoveLineRequest>,
) -> Result<StatusCode, StockOperationError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let context = load_context(&state.pool, &id).await?;
    require_for_kind(&state, &headers, &context.operation_kind).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query("DELETE FROM stock_operation_lines WHERE stock_operation_id=?")
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    sqlx::query("DELETE FROM stock_operations WHERE id=?")
        .bind(&id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<RemoveLineRequest>,
) -> Result<Json<OperationDetail>, StockOperationError> {
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let operation_id = line_document(&state.pool, &line_id).await?;
    let context = load_context(&state.pool, &operation_id).await?;
    require_for_kind(&state, &headers, &context.operation_kind).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    let current = draft_state(&mut transaction, &operation_id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query("DELETE FROM stock_operation_lines WHERE id=?")
        .bind(&line_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    // Line numbers stay dense so the screen never shows a gap.
    sqlx::query(
        "UPDATE stock_operation_lines SET line_number=line_number-1 \
         WHERE stock_operation_id=? AND line_number > (SELECT COALESCE(MAX(line_number),0) \
         FROM stock_operation_lines WHERE stock_operation_id=? AND id=?)",
    )
    .bind(&operation_id)
    .bind(&operation_id)
    .bind(&line_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    bump_revision(&mut transaction, &operation_id).await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &operation_id).await?))
}

// ---------------------------------------------------------------------------------------------
// Quote — a read-only preview that writes nothing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuoteLine {
    line_id: String,
    line_number: i64,
    product_display_name: Option<String>,
    batch_number: Option<String>,
    stock_status: String,
    target_stock_status: Option<String>,
    reason_code: String,
    current_atoms: i64,
    requested_atoms: i64,
    resulting_atoms: i64,
    target_current_atoms: Option<i64>,
    target_resulting_atoms: Option<i64>,
    sufficient: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Quote {
    operation_id: String,
    operation_kind: String,
    lines: Vec<QuoteLine>,
    postable: bool,
}

/// What posting would do, computed from the server's own balances.
///
/// It writes nothing, and posting recomputes everything from scratch under a write lock. A quote is
/// a courtesy to the operator, never an input to the decision.
async fn quote_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Quote>, StockOperationError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let detail = fetch_detail(&state.pool, &id).await?;
    let store_id = current_store(&state.pool).await?;

    let mut lines = Vec::with_capacity(detail.lines.len());
    let mut postable = !detail.lines.is_empty();
    for line in &detail.lines {
        let current = balance_of(
            &state.pool,
            &store_id,
            &line.product_pack_id,
            line.batch_id.as_deref(),
            &line.stock_status,
        )
        .await?;
        let (requested, resulting) = match line.direction.as_str() {
            "count" => {
                let counted = line.counted_atoms.unwrap_or(0);
                (counted, counted)
            }
            "increase" => {
                let quantity = line.quantity_atoms.unwrap_or(0);
                (quantity, current.saturating_add(quantity))
            }
            _ => {
                let quantity = line.quantity_atoms.unwrap_or(0);
                (quantity, current.saturating_sub(quantity))
            }
        };
        let (target_current, target_resulting) = match &line.target_stock_status {
            Some(target) => {
                let held = balance_of(
                    &state.pool,
                    &store_id,
                    &line.product_pack_id,
                    line.batch_id.as_deref(),
                    target,
                )
                .await?;
                (
                    Some(held),
                    Some(held.saturating_add(line.quantity_atoms.unwrap_or(0))),
                )
            }
            None => (None, None),
        };
        let sufficient =
            line.direction == "count" || line.direction == "increase" || resulting >= 0;
        if !sufficient {
            postable = false;
        }
        lines.push(QuoteLine {
            line_id: line.id.clone(),
            line_number: line.line_number,
            product_display_name: line.product_display_name.clone(),
            batch_number: line.batch_number.clone(),
            stock_status: line.stock_status.clone(),
            target_stock_status: line.target_stock_status.clone(),
            reason_code: line.reason_code.clone(),
            current_atoms: current,
            requested_atoms: requested,
            resulting_atoms: resulting,
            target_current_atoms: target_current,
            target_resulting_atoms: target_resulting,
            sufficient,
        });
    }

    Ok(Json(Quote {
        operation_id: detail.operation.id,
        operation_kind: detail.operation.operation_kind,
        lines,
        postable,
    }))
}

// ---------------------------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------------------------

async fn post_operation(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PostOperationRequest>,
) -> Result<Json<OperationDetail>, StockOperationError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;
    let context = load_context(&state.pool, &id).await?;
    let actor = require_for_kind(&state, &headers, &context.operation_kind).await?;
    let store_id = current_store(&state.pool).await?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| StockOperationError::Internal)?;
    // Every balance this posting depends on is read after this lock is taken, so a concurrent sale
    // or removal cannot slip between the check and the write.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let outcome = post_within_transaction(
        &mut connection,
        &id,
        &store_id,
        &context,
        request.expected_revision,
        &idempotency_key,
        &actor.id,
    )
    .await;
    match outcome {
        Ok(()) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            drop(connection);
            Ok(Json(fetch_detail(&state.pool, &id).await?))
        }
        Err(StockOperationError::Auth(error)) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(StockOperationError::Auth(error))
        }
        Err(error) => {
            // Nothing survives a refused posting: not a movement, not a disposition, not a line's
            // applied delta.
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            drop(connection);
            Err(error)
        }
    }
}

#[derive(Debug, FromRow)]
struct PostingLine {
    id: String,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    stock_status: String,
    target_stock_status: Option<String>,
    direction: String,
    reason_code: String,
    counted_atoms: Option<i64>,
    quantity_atoms: Option<i64>,
    note: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn post_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    store_id: &str,
    context: &OperationContext,
    expected_revision: i64,
    idempotency_key: &str,
    actor_id: &str,
) -> Result<(), StockOperationError> {
    // A replay of the same posting is the same posting. A different document reusing the key is a
    // mistake worth refusing loudly.
    let replay: Option<String> =
        sqlx::query_scalar("SELECT id FROM stock_operations WHERE posting_idempotency_key=?")
            .bind(idempotency_key)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    if let Some(existing) = replay {
        return if existing == id {
            Ok(())
        } else {
            Err(StockOperationError::IdempotencyConflict)
        };
    }

    let current: Option<(String, i64)> =
        sqlx::query_as("SELECT status,revision FROM stock_operations WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    let (status, revision) = current.ok_or(StockOperationError::NotFound)?;
    if status != "draft" {
        return Err(StockOperationError::NotDraft);
    }
    if revision != expected_revision {
        return Err(StockOperationError::Revision {
            expected: expected_revision,
            current: revision,
        });
    }

    let lines = sqlx::query_as::<_, PostingLine>(
        "SELECT id,product_id,product_pack_id,batch_id,stock_status,target_stock_status,\
         direction,reason_code,counted_atoms,quantity_atoms,note \
         FROM stock_operation_lines WHERE stock_operation_id=? ORDER BY line_number",
    )
    .bind(id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)?;
    if lines.is_empty() {
        return Err(StockOperationError::EmptyOperation);
    }

    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;

    // Deltas applied so far by earlier lines of this same document. Two lines touching one lot must
    // see each other, or the second would be checked against a balance the first already spent.
    let mut applied: HashMap<(String, String, String), i64> = HashMap::new();

    for line in &lines {
        let key = |status: &str| {
            (
                line.product_pack_id.clone(),
                line.batch_id.clone().unwrap_or_default(),
                status.to_owned(),
            )
        };
        let source_key = key(&line.stock_status);
        let stored = status_balance(
            connection,
            store_id,
            &line.product_pack_id,
            line.batch_id.as_deref(),
            &line.stock_status,
        )
        .await?;
        let available = stored + applied.get(&source_key).copied().unwrap_or(0);

        let delta = match line.direction.as_str() {
            "count" => {
                let counted = line
                    .counted_atoms
                    .ok_or(StockOperationError::ArithmeticOverflow)?;
                rules::count_variance(counted, available).map_err(map_overflow)?
            }
            "increase" => {
                rules::applied_delta(Direction::Increase, line.quantity_atoms.unwrap_or_default())
                    .map_err(map_overflow)?
            }
            _ => rules::applied_delta(Direction::Decrease, line.quantity_atoms.unwrap_or_default())
                .map_err(map_overflow)?,
        };

        // Nothing may take a status below zero. This is the check the write lock exists for.
        if available + delta < 0 {
            return Err(StockOperationError::InsufficientStock { available });
        }

        // An expiry write-off is only honest for a lot that has actually expired.
        if context.operation_kind == "expiry" {
            let expires_on: Option<String> =
                sqlx::query_scalar("SELECT expires_on FROM product_batches WHERE id=?")
                    .bind(&line.batch_id)
                    .fetch_optional(&mut **connection)
                    .await
                    .map_err(map_database_error)?
                    .flatten();
            match expires_on {
                Some(expiry) if expiry.as_str() < context.business_date.as_str() => {}
                _ => return Err(StockOperationError::BatchNotExpired),
            }
        }

        let reason_code = if line.direction == "count" {
            rules::count_reason(delta).to_owned()
        } else {
            line.reason_code.clone()
        };

        match line.target_stock_status.as_deref() {
            // A transfer: one disposition header, two opposite movements, nothing created or lost.
            Some(target) => {
                let disposition_id = Uuid::now_v7().to_string();
                let quantity = -delta;
                sqlx::query(
                    "INSERT INTO stock_dispositions (id,store_id,product_id,product_pack_id,\
                     batch_id,quantity_atoms,from_status,to_status,reason_code,reason,occurred_on,\
                     authorised_by_user_id,created_at_utc,idempotency_key,stock_operation_line_id) \
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                .bind(&disposition_id)
                .bind(store_id)
                .bind(&line.product_id)
                .bind(&line.product_pack_id)
                .bind(&line.batch_id)
                .bind(quantity)
                .bind(&line.stock_status)
                .bind(target)
                .bind(&reason_code)
                .bind(movement_reason(&reason_code, line.note.as_deref(), None, 0))
                .bind(&context.business_date)
                .bind(actor_id)
                .bind(&now)
                .bind(Uuid::now_v7().to_string())
                .bind(&line.id)
                .execute(&mut **connection)
                .await
                .map_err(map_database_error)?;

                for (status, movement_delta) in
                    [(line.stock_status.as_str(), delta), (target, quantity)]
                {
                    write_movement(
                        connection,
                        store_id,
                        line,
                        "disposition_transfer",
                        status,
                        movement_delta,
                        &context.business_date,
                        Some(&disposition_id),
                        &reason_code,
                        None,
                        0,
                        actor_id,
                        &now,
                    )
                    .await?;
                    let entry = applied.entry(key(status)).or_insert(0);
                    *entry += movement_delta;
                }
            }
            // Everything else is a single movement at one status. A zero-variance count writes
            // none at all: the ledger forbids a zero movement, and the count line itself is the
            // durable record that the shelf was checked and found right.
            None => {
                if delta != 0 {
                    let movement_type = match line.direction.as_str() {
                        "count" => "stock_count",
                        _ if context.operation_kind == "removal" => "stock_removal",
                        _ => "adjustment",
                    };
                    write_movement(
                        connection,
                        store_id,
                        line,
                        movement_type,
                        &line.stock_status,
                        delta,
                        &context.business_date,
                        None,
                        &reason_code,
                        line.counted_atoms,
                        delta,
                        actor_id,
                        &now,
                    )
                    .await?;
                }
                let entry = applied.entry(source_key).or_insert(0);
                *entry += delta;
            }
        }

        sqlx::query(
            "UPDATE stock_operation_lines SET applied_delta_atoms=?,reason_code=?,\
             updated_at_utc=? WHERE id=?",
        )
        .bind(delta)
        .bind(&reason_code)
        .bind(&now)
        .bind(&line.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    sqlx::query(
        "UPDATE stock_operations SET status='posted',revision=revision+1,updated_at_utc=?,\
         posted_by_user_id=?,posted_at_utc=?,posting_idempotency_key=? WHERE id=?",
    )
    .bind(&now)
    .bind(actor_id)
    .bind(&now)
    .bind(idempotency_key)
    .bind(id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'stock_operation',?,?,'posted',?,?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(id)
    .bind(revision + 1)
    .bind(&now)
    .bind(&context.note)
    .bind(
        json!({
            "operationKind": context.operation_kind,
            "businessDate": context.business_date,
            "lineCount": lines.len(),
        })
        .to_string(),
    )
    .bind(actor_id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    Ok(())
}

/// The prose a movement carries, which must never be empty because the schema says so.
///
/// The operator's own note wins whenever there is one. Otherwise this writes a sentence rather
/// than a code with the underscores rubbed off: somebody reading the ledger in six months needs
/// to know what happened, and `physical count loss` tells them nothing a column heading did not.
fn movement_reason(
    reason_code: &str,
    note: Option<&str>,
    counted: Option<i64>,
    delta: i64,
) -> String {
    if let Some(text) = note.filter(|value| !value.trim().is_empty()) {
        return text.trim().to_owned();
    }
    match reason_code {
        "damage" => "Damaged in the store".to_owned(),
        "breakage" => "Broken in the store".to_owned(),
        "expiry" => "Past its expiry date".to_owned(),
        "quality_hold" => "Held pending a quality decision".to_owned(),
        "theft_or_loss" => "Missing from the shelf".to_owned(),
        "data_correction" => "Correcting a recorded quantity".to_owned(),
        "disposal" => "Removed from the store".to_owned(),
        "physical_count_gain" | "physical_count_loss" => match counted {
            Some(counted) => format!(
                "Counted {counted}; {} recorded before the count",
                counted - delta
            ),
            None => "Counted on the shelf".to_owned(),
        },
        other => other.replace('_', " "),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_movement(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    line: &PostingLine,
    movement_type: &str,
    stock_status: &str,
    delta: i64,
    occurred_on: &str,
    disposition_id: Option<&str>,
    reason_code: &str,
    counted: Option<i64>,
    delta_for_reason: i64,
    actor_id: &str,
    now: &str,
) -> Result<(), StockOperationError> {
    sqlx::query(
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,stock_status,quantity_delta_atoms,occurred_on,reason,stock_disposition_id,\
         stock_operation_line_id,idempotency_key,posted_by_user_id,posted_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(store_id)
    .bind(&line.product_id)
    .bind(&line.product_pack_id)
    .bind(&line.batch_id)
    .bind(movement_type)
    .bind(stock_status)
    .bind(delta)
    .bind(occurred_on)
    .bind(movement_reason(
        reason_code,
        line.note.as_deref(),
        counted,
        delta_for_reason,
    ))
    .bind(disposition_id)
    .bind(&line.id)
    .bind(Uuid::now_v7().to_string())
    .bind(actor_id)
    .bind(now)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Shared reads
// ---------------------------------------------------------------------------------------------

struct OperationContext {
    operation_kind: String,
    business_date: String,
    note: Option<String>,
}

#[derive(Debug, FromRow)]
struct ContextRow {
    operation_kind: String,
    business_date: String,
    note: Option<String>,
}

async fn load_context(
    pool: &SqlitePool,
    id: &str,
) -> Result<OperationContext, StockOperationError> {
    let row = sqlx::query_as::<_, ContextRow>(
        "SELECT operation_kind,business_date,note FROM stock_operations WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(StockOperationError::NotFound)?;
    Ok(OperationContext {
        operation_kind: row.operation_kind,
        business_date: row.business_date,
        note: row.note,
    })
}

async fn line_document(pool: &SqlitePool, line_id: &str) -> Result<String, StockOperationError> {
    sqlx::query_scalar("SELECT stock_operation_id FROM stock_operation_lines WHERE id=?")
        .bind(line_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(StockOperationError::NotFound)
}

async fn draft_state(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(String, i64), StockOperationError> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT status,revision FROM stock_operations WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_database_error)?;
    let (status, revision) = row.ok_or(StockOperationError::NotFound)?;
    if status != "draft" {
        return Err(StockOperationError::NotDraft);
    }
    Ok((status, revision))
}

fn require_revision(current: &(String, i64), expected: i64) -> Result<(), StockOperationError> {
    if current.1 != expected {
        return Err(StockOperationError::Revision {
            expected,
            current: current.1,
        });
    }
    Ok(())
}

async fn bump_revision(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(), StockOperationError> {
    sqlx::query(
        "UPDATE stock_operations SET revision=revision+1,\
         updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
    )
    .bind(id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn current_store(pool: &SqlitePool) -> Result<String, StockOperationError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(StockOperationError::Internal)
}

/// The balance of one status at one lot, outside a transaction. Reads only.
async fn balance_of(
    pool: &SqlitePool,
    store_id: &str,
    pack_id: &str,
    batch_id: Option<&str>,
    stock_status: &str,
) -> Result<i64, StockOperationError> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE store_id=? AND product_pack_id=? AND batch_id IS ? AND stock_status=?",
    )
    .bind(store_id)
    .bind(pack_id)
    .bind(batch_id)
    .bind(stock_status)
    .fetch_one(pool)
    .await
    .map_err(map_database_error)
}

/// The same balance, read on the connection that holds the posting's write lock.
async fn status_balance(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    pack_id: &str,
    batch_id: Option<&str>,
    stock_status: &str,
) -> Result<i64, StockOperationError> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
         WHERE store_id=? AND product_pack_id=? AND batch_id IS ? AND stock_status=?",
    )
    .bind(store_id)
    .bind(pack_id)
    .bind(batch_id)
    .bind(stock_status)
    .fetch_one(&mut **connection)
    .await
    .map_err(map_database_error)
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const TABLET: &str = "01997000-0000-7000-8000-000000000001";
    /// Archiving a batch must satisfy the lifecycle CHECK the catalogue has always enforced.
    const ARCHIVE_BATCH: &str = "UPDATE product_batches SET status='archived',archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='Withdrawn by the supplier' WHERE id=?";
    const STRIP: &str = "01997000-0000-7000-8000-000000000004";
    const MAHARASHTRA: &str = "01997300-0000-7000-8000-000000000027";
    const OWNER: &str = "stockop-owner-session-token";
    const PHARMACIST: &str = "stockop-pharmacist-session-token";
    const CASHIER: &str = "stockop-cashier-session-token";
    const TODAY: &str = "2026-06-15";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        store_id: String,
        #[allow(dead_code)]
        product_id: String,
        pack_id: String,
        /// A lot expiring well after TODAY. Ten tablets to a strip, 200 atoms on the shelf.
        batch_id: String,
        /// A lot that expired before TODAY.
        expired_batch_id: String,
    }

    /// One product, two lots, and stock placed at every status so each rule has something to act on.
    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("stockops.sqlite3"))
            .await
            .unwrap();
        let store_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc,\
             gst_registration_status,gstin,normalized_gstin,place_of_supply_state_id) \
             VALUES (?,'Care Pharmacy','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             'registered','27AAPFU0939F1ZV','27AAPFU0939F1ZV',?)",
        )
        .bind(&store_id)
        .bind(MAHARASHTRA)
        .execute(&pool)
        .await
        .unwrap();
        insert_session(&pool, "owner_admin", OWNER).await;
        insert_session(&pool, "pharmacist", PHARMACIST).await;
        insert_session(&pool, "cashier", CASHIER).await;

        let product_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO products (id,product_kind,base_unit_id,quantity_scale,display_name,\
             normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,'general_pharmacy_item',?,0,'Crocin 500 mg Tablet','crocin 500 mg tablet',\
             ?,?)",
        )
        .bind(&product_id)
        .bind(TABLET)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&pool)
        .await
        .unwrap();
        let pack_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,\
             display_label,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,10,'Strip of 10',?,?)",
        )
        .bind(&pack_id)
        .bind(&product_id)
        .bind(STRIP)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(&pool)
        .await
        .unwrap();

        let batch_id = insert_batch(&pool, &pack_id, "B-900", "2028-03-31").await;
        let expired_batch_id = insert_batch(&pool, &pack_id, "B-901", "2026-05-31").await;

        // 200 sellable on the good lot, 50 sellable on the expired one, plus stock already held
        // back and already written off so the outward rules have something to refuse or consume.
        seed(
            &pool,
            &store_id,
            &product_id,
            &pack_id,
            &batch_id,
            "sellable",
            200,
        )
        .await;
        seed(
            &pool,
            &store_id,
            &product_id,
            &pack_id,
            &expired_batch_id,
            "sellable",
            50,
        )
        .await;
        seed(
            &pool,
            &store_id,
            &product_id,
            &pack_id,
            &batch_id,
            "quarantined",
            30,
        )
        .await;
        seed(
            &pool,
            &store_id,
            &product_id,
            &pack_id,
            &batch_id,
            "non_sellable",
            40,
        )
        .await;

        Fixture {
            _temp: temp,
            pool,
            store_id,
            product_id,
            pack_id,
            batch_id,
            expired_batch_id,
        }
    }

    async fn insert_batch(pool: &SqlitePool, pack_id: &str, number: &str, expires: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
             expires_on,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,?,?,?)",
        )
        .bind(&id)
        .bind(pack_id)
        .bind(number)
        .bind(number)
        .bind(expires)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// Stock placed directly in the ledger, which is how it would have arrived in real life.
    async fn seed(
        pool: &SqlitePool,
        store_id: &str,
        product_id: &str,
        pack_id: &str,
        batch_id: &str,
        status: &str,
        atoms: i64,
    ) {
        let movement_type = if status == "sellable" {
            "opening_stock"
        } else {
            "adjustment"
        };
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,stock_status,quantity_delta_atoms,occurred_on,idempotency_key,\
             posted_by_user_id,posted_at_utc) \
             VALUES (?,?,?,?,?,?,?,?,?,?,(SELECT id FROM users LIMIT 1),?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(product_id)
        .bind(pack_id)
        .bind(batch_id)
        .bind(movement_type)
        .bind(status)
        .bind(atoms)
        .bind("2026-01-01")
        .bind(Uuid::now_v7().to_string())
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_session(pool: &SqlitePool, role: &str, token: &str) -> String {
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-{}", &user_id[24..32]);
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,\
             password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,'$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',?,?,?)",
        )
        .bind(&user_id)
        .bind(&login)
        .bind(&login)
        .bind(format!("{role} stock user"))
        .bind(role)
        .bind("2026-01-01T00:00:00.000Z")
        .bind("2026-01-01T00:00:00.000Z")
        .execute(pool)
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
        .execute(pool)
        .await
        .unwrap();
        user_id
    }

    async fn request(
        pool: SqlitePool,
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
        let response = crate::api::router(pool, None)
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
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    /// The derived balance of one status, read exactly as the service derives it.
    async fn balance(f: &Fixture, batch_id: &str, status: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
             WHERE store_id=? AND product_pack_id=? AND batch_id=? AND stock_status=?",
        )
        .bind(&f.store_id)
        .bind(&f.pack_id)
        .bind(batch_id)
        .bind(status)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    async fn custody_total(f: &Fixture, batch_id: &str) -> i64 {
        let mut total = 0;
        for status in ["sellable", "quarantined", "non_sellable"] {
            total += balance(f, batch_id, status).await;
        }
        total
    }

    async fn open(f: &Fixture, kind: &str, token: &str) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-operations",
            json!({ "operationKind": kind, "businessDate": TODAY, "note": null }),
            Some(token),
        )
        .await
    }

    async fn open_id(f: &Fixture, kind: &str) -> String {
        let token = if matches!(kind, "adjustment" | "removal") {
            OWNER
        } else {
            PHARMACIST
        };
        let (status, body) = open(f, kind, token).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    async fn add(
        f: &Fixture,
        id: &str,
        token: &str,
        revision: i64,
        line: Value,
    ) -> (StatusCode, Value) {
        let mut body = line;
        body["expectedRevision"] = json!(revision);
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/stock-operations/{id}/lines"),
            body,
            Some(token),
        )
        .await
    }

    async fn post(f: &Fixture, id: &str, token: &str, revision: i64) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/stock-operations/{id}/post"),
            json!({ "expectedRevision": revision, "idempotencyKey": Uuid::now_v7().to_string() }),
            Some(token),
        )
        .await
    }

    fn count_line(pack: &str, batch: &str, status: &str, counted: i64) -> Value {
        json!({
            "productPackId": pack, "batchId": batch, "stockStatus": status,
            "reasonCode": "physical_count_gain", "countedQuantity": counted,
            "quantityBasis": "base_unit"
        })
    }

    // -----------------------------------------------------------------------------------------
    // Physical count
    // -----------------------------------------------------------------------------------------

    /// The whole point of a count: the operator says what is on the shelf, the server says what
    /// the difference is. A browser that sent a delta could make the ledger say anything.
    #[tokio::test]
    async fn a_count_short_of_the_ledger_posts_the_loss_the_server_worked_out() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        let (status, added) = add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 185),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");

        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["appliedDeltaAtoms"], -15);
        // The reason is the arithmetic's, not the operator's.
        assert_eq!(posted["lines"][0]["reasonCode"], "physical_count_loss");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 185);

        let movement: (String, i64) = sqlx::query_as(
            "SELECT movement_type,quantity_delta_atoms FROM inventory_movements \
             WHERE stock_operation_line_id IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(movement, ("stock_count".to_owned(), -15));
    }

    #[tokio::test]
    async fn a_count_over_the_ledger_posts_the_gain() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 212),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["appliedDeltaAtoms"], 12);
        assert_eq!(posted["lines"][0]["reasonCode"], "physical_count_gain");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 212);
    }

    /// "I counted it and it was right" is an audit fact worth keeping, and it is not a movement.
    #[tokio::test]
    async fn a_count_that_matches_records_the_check_without_touching_the_ledger() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 200),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["status"], "posted");
        assert_eq!(posted["lines"][0]["appliedDeltaAtoms"], 0);
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 200);
        let movements: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE stock_operation_line_id IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(movements, 0, "a zero variance wrote a movement");
    }

    /// Counting one shelf twice on one document is a contradiction, not extra information.
    #[tokio::test]
    async fn one_count_cannot_hold_two_lines_for_the_same_shelf() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 190),
        )
        .await;
        let (status, refused) = add(
            &f,
            &id,
            PHARMACIST,
            2,
            count_line(&f.pack_id, &f.batch_id, "sellable", 195),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "duplicate_count_line");
        // A different status on the same lot is a different shelf, and is allowed.
        let (status, accepted) = add(
            &f,
            &id,
            PHARMACIST,
            2,
            count_line(&f.pack_id, &f.batch_id, "quarantined", 30),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
    }

    /// Every line of one count is true together or the count is done again.
    #[tokio::test]
    async fn a_count_posts_every_line_or_none_of_them() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 190),
        )
        .await;
        add(
            &f,
            &id,
            PHARMACIST,
            2,
            count_line(&f.pack_id, &f.expired_batch_id, "sellable", 45),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 3).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);
        assert_eq!(balance(&f, &f.expired_batch_id, "sellable").await, 45);

        // Now a count whose SECOND line cannot post. The first would have succeeded on its own,
        // so if the ledger moved at all, the document was not atomic.
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 100),
        )
        .await;
        add(
            &f,
            &id,
            PHARMACIST,
            2,
            count_line(&f.pack_id, &f.expired_batch_id, "sellable", 10),
        )
        .await;
        // The second line's lot is archived between drafting and posting, so the ledger's own
        // integrity trigger refuses its movement halfway through the document.
        sqlx::query(ARCHIVE_BATCH)
            .bind(&f.expired_batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let (status, refused) = post(&f, &id, PHARMACIST, 3).await;
        assert_ne!(status, StatusCode::OK, "{refused}");
        // Line one's variance is nowhere: not in the balance, not as a movement, not on the line.
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);
        assert_eq!(balance(&f, &f.expired_batch_id, "sellable").await, 45);
        let applied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stock_operation_lines WHERE stock_operation_id=? \
             AND applied_delta_atoms IS NOT NULL",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(applied, 0, "a refused posting left an applied delta behind");
        let still_draft: String =
            sqlx::query_scalar("SELECT status FROM stock_operations WHERE id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(still_draft, "draft");
    }

    // -----------------------------------------------------------------------------------------
    // Damage, expiry, quarantine
    // -----------------------------------------------------------------------------------------

    /// Damaged goods are still in the building. The custody total must not move.
    #[tokio::test]
    async fn damage_writes_stock_off_without_destroying_the_quantity() {
        let f = fixture().await;
        let before = custody_total(&f, &f.batch_id).await;
        let id = open_id(&f, "damage").await;
        let (status, added) = add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "breakage", "quantity": 2, "quantityBasis": "pack",
                "note": "Crushed in the delivery crate"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 180);
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 60);
        assert_eq!(custody_total(&f, &f.batch_id).await, before);

        // Two opposite movements, both naming the operation line and the disposition.
        let pair: Vec<(String, i64)> = sqlx::query_as(
            "SELECT stock_status,quantity_delta_atoms FROM inventory_movements \
             WHERE stock_operation_line_id IS NOT NULL AND stock_disposition_id IS NOT NULL \
             ORDER BY quantity_delta_atoms",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            pair,
            vec![
                ("sellable".to_owned(), -20),
                ("non_sellable".to_owned(), 20)
            ]
        );
        let reason: String = sqlx::query_scalar(
            "SELECT reason_code FROM stock_dispositions WHERE stock_operation_line_id IS NOT NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(reason, "breakage");
    }

    /// The other honest outcome: a pharmacist is not sure yet, so the goods wait.
    #[tokio::test]
    async fn damage_may_hold_goods_back_for_a_decision_instead() {
        let f = fixture().await;
        let id = open_id(&f, "damage").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "quarantined",
                "reasonCode": "damage", "quantity": 50, "quantityBasis": "base_unit",
                "note": "Carton was wet; contents may be fine"
            }),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 150);
        assert_eq!(balance(&f, &f.batch_id, "quarantined").await, 80);
    }

    #[tokio::test]
    async fn an_expiry_write_off_classifies_an_expired_lot_and_refuses_a_live_one() {
        let f = fixture().await;
        let id = open_id(&f, "expiry").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "expiry", "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        // B-900 runs to 2028; writing it off as expired would be a false record.
        let (status, refused) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "batch_not_expired");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 200);

        let id = open_id(&f, "expiry").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.expired_batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "expiry", "quantity": 50, "quantityBasis": "base_unit"
            }),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, &f.expired_batch_id, "sellable").await, 0);
        assert_eq!(balance(&f, &f.expired_batch_id, "non_sellable").await, 50);
    }

    /// An expired lot can never be quarantined: no later assessment could make it sellable.
    #[tokio::test]
    async fn an_expiry_operation_can_only_write_off_and_never_merely_hold() {
        let f = fixture().await;
        let id = open_id(&f, "expiry").await;
        let (status, refused) = add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.expired_batch_id,
                "stockStatus": "sellable", "targetStockStatus": "quarantined",
                "reasonCode": "expiry", "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "stock_operation_line_conflict");
    }

    #[tokio::test]
    async fn quarantine_holds_sellable_stock_back_and_demands_a_stated_reason() {
        let f = fixture().await;
        let id = open_id(&f, "quarantine").await;
        // A hold with nothing said about it is an unexplained stock movement.
        let (status, refused) = add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "quarantined",
                "reasonCode": "quality_hold", "quantity": 30, "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "note");

        let (status, added) = add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "quarantined",
                "reasonCode": "quality_hold", "quantity": 30, "quantityBasis": "base_unit",
                "note": "Supplier advisory pending on this lot"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 170);
        assert_eq!(balance(&f, &f.batch_id, "quarantined").await, 60);
    }

    // -----------------------------------------------------------------------------------------
    // Removal — the hole Phase 1I left
    // -----------------------------------------------------------------------------------------

    /// Custody ends. This is the only operation in the system that reduces the physical total.
    #[tokio::test]
    async fn removal_ends_custody_for_stock_already_written_off() {
        let f = fixture().await;
        let before = custody_total(&f, &f.batch_id).await;
        let id = open_id(&f, "removal").await;
        let (status, added) = add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "non_sellable", "reasonCode": "disposal",
                "quantity": 40, "quantityBasis": "base_unit",
                "note": "Collected by the authorised disposal contractor"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post(&f, &id, OWNER, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 0);
        assert_eq!(custody_total(&f, &f.batch_id).await, before - 40);
        let movement: (String, i64, String) = sqlx::query_as(
            "SELECT movement_type,quantity_delta_atoms,stock_status FROM inventory_movements \
             WHERE movement_type='stock_removal'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            movement,
            ("stock_removal".to_owned(), -40, "non_sellable".to_owned())
        );
    }

    /// Removing saleable stock would be a sale nobody recorded.
    #[tokio::test]
    async fn removal_can_never_take_goods_off_the_shelf() {
        let f = fixture().await;
        let id = open_id(&f, "removal").await;
        for status_name in ["sellable", "quarantined"] {
            let (status, refused) = add(
                &f,
                &id,
                OWNER,
                1,
                json!({
                    "productPackId": f.pack_id, "batchId": f.batch_id,
                    "stockStatus": status_name, "reasonCode": "disposal",
                    "quantity": 10, "quantityBasis": "base_unit"
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{refused}");
            assert_eq!(refused["code"], "stock_operation_line_conflict");
        }
    }

    #[tokio::test]
    async fn removal_cannot_take_more_than_was_written_off() {
        let f = fixture().await;
        let id = open_id(&f, "removal").await;
        add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "non_sellable", "reasonCode": "disposal",
                "quantity": 41, "quantityBasis": "base_unit"
            }),
        )
        .await;
        let (status, refused) = post(&f, &id, OWNER, 2).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 40);
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 40);
    }

    // -----------------------------------------------------------------------------------------
    // Adjustment — Phase 1D's correction, now reachable
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn theft_reduces_the_stock_it_was_taken_from_and_never_increases_it() {
        let f = fixture().await;
        let id = open_id(&f, "adjustment").await;
        let (status, refused) = add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id, "direction": "increase",
                "stockStatus": "sellable", "reasonCode": "theft_or_loss",
                "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");

        let (status, added) = add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id, "direction": "decrease",
                "stockStatus": "sellable", "reasonCode": "theft_or_loss",
                "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post(&f, &id, OWNER, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        // Stolen goods are gone, so the physical total falls too.
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);
        let reason: String = sqlx::query_scalar(
            "SELECT reason_code FROM stock_operation_lines WHERE stock_operation_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(reason, "theft_or_loss");
    }

    #[tokio::test]
    async fn a_data_correction_may_go_either_way_but_must_say_why() {
        let f = fixture().await;
        let id = open_id(&f, "adjustment").await;
        let (status, refused) = add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id, "direction": "increase",
                "stockStatus": "sellable", "reasonCode": "data_correction",
                "quantity": 5, "quantityBasis": "base_unit"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "note");

        let (status, added) = add(
            &f,
            &id,
            OWNER,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id, "direction": "increase",
                "stockStatus": "sellable", "reasonCode": "data_correction",
                "quantity": 5, "quantityBasis": "base_unit",
                "note": "Opening stock was keyed as 195 instead of 200"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post(&f, &id, OWNER, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 205);
    }

    // -----------------------------------------------------------------------------------------
    // Kind boundaries
    // -----------------------------------------------------------------------------------------

    /// Operator intent is an invariant, not a label. Each kind refuses the others' work.
    #[tokio::test]
    async fn a_document_of_one_kind_cannot_contain_another_kinds_line() {
        let f = fixture().await;
        let cases = [
            // A damage document trying to record an expiry.
            (
                "damage",
                json!({ "reasonCode": "expiry", "targetStockStatus": "non_sellable" }),
            ),
            // A quarantine document trying to write stock off outright.
            (
                "quarantine",
                json!({ "reasonCode": "quality_hold", "targetStockStatus": "non_sellable" }),
            ),
            // An adjustment trying to transfer between statuses.
            (
                "adjustment",
                json!({ "reasonCode": "data_correction", "targetStockStatus": "quarantined" }),
            ),
        ];
        for (kind, extra) in cases {
            let id = open_id(&f, kind).await;
            let token = if kind == "adjustment" {
                OWNER
            } else {
                PHARMACIST
            };
            let mut line = json!({
                "productPackId": f.pack_id, "batchId": f.batch_id, "stockStatus": "sellable",
                "quantity": 10, "quantityBasis": "base_unit", "direction": "decrease",
                "note": "probe"
            });
            for (key, value) in extra.as_object().unwrap() {
                line[key] = value.clone();
            }
            let (status, refused) = add(&f, &id, token, 1, line).await;
            assert_eq!(status, StatusCode::CONFLICT, "{kind} accepted: {refused}");
            assert_eq!(refused["code"], "stock_operation_line_conflict");
        }
    }

    #[tokio::test]
    async fn a_kind_the_service_does_not_implement_is_not_a_kind() {
        let f = fixture().await;
        let (status, refused) = open(&f, "stock_transfer", OWNER).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "operationKind");
    }

    // -----------------------------------------------------------------------------------------
    // Authorization
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_cashier_mutates_no_stock_by_any_route() {
        let f = fixture().await;
        for kind in [
            "physical_count",
            "adjustment",
            "damage",
            "expiry",
            "quarantine",
            "removal",
        ] {
            let (status, refused) = open(&f, kind, CASHIER).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{kind}: {refused}");
        }
        // But a cashier can still see what is there.
        let (status, listed) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/stock-operations",
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
    }

    #[tokio::test]
    async fn the_generic_adjustment_and_the_end_of_custody_belong_to_the_owner() {
        let f = fixture().await;
        for kind in ["adjustment", "removal"] {
            let (status, refused) = open(&f, kind, PHARMACIST).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{kind}: {refused}");
            let (status, allowed) = open(&f, kind, OWNER).await;
            assert_eq!(status, StatusCode::CREATED, "{kind}: {allowed}");
        }
        // Floor work is the pharmacist's.
        for kind in ["physical_count", "damage", "expiry", "quarantine"] {
            let (status, allowed) = open(&f, kind, PHARMACIST).await;
            assert_eq!(status, StatusCode::CREATED, "{kind}: {allowed}");
        }
    }

    #[tokio::test]
    async fn an_unauthenticated_caller_can_do_nothing() {
        let f = fixture().await;
        let (status, _) = open(&f, "damage", "not-a-real-token").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/stock-operations",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // -----------------------------------------------------------------------------------------
    // Lifecycle, idempotency, concurrency
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_posted_operation_cannot_be_changed_by_any_route() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 190),
        )
        .await;
        let (status, posted) = post(&f, &id, PHARMACIST, 2).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let revision = posted["revision"].as_i64().unwrap();
        let line_id = posted["lines"][0]["id"].as_str().unwrap().to_owned();

        let (status, refused) = add(
            &f,
            &id,
            PHARMACIST,
            revision,
            count_line(&f.pack_id, &f.expired_batch_id, "sellable", 10),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "stock_operation_not_draft");

        let (status, refused) = request(
            f.pool.clone(),
            "DELETE",
            &format!("/api/v1/stock-operation-lines/{line_id}"),
            json!({ "expectedRevision": revision }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");

        // And the database refuses it too, with no handler in the way.
        let direct =
            sqlx::query("UPDATE stock_operations SET business_date='2020-01-01' WHERE id=?")
                .bind(&id)
                .execute(&f.pool)
                .await;
        assert!(direct.is_err(), "a posted operation was edited directly");
    }

    #[tokio::test]
    async fn a_posting_from_a_stale_view_is_refused() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 190),
        )
        .await;
        let (status, refused) = post(&f, &id, PHARMACIST, 1).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 200);
    }

    #[tokio::test]
    async fn an_empty_operation_has_nothing_to_post() {
        let f = fixture().await;
        let id = open_id(&f, "damage").await;
        let (status, refused) = post(&f, &id, PHARMACIST, 1).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "stock_operation_empty");
    }

    #[tokio::test]
    async fn replaying_a_posting_changes_nothing_and_a_reused_key_is_refused() {
        let f = fixture().await;
        let id = open_id(&f, "damage").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "damage", "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        let key = Uuid::now_v7().to_string();
        let body = json!({ "expectedRevision": 2, "idempotencyKey": key });
        let (status, first) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/stock-operations/{id}/post"),
            body.clone(),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);

        let (status, replay) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/stock-operations/{id}/post"),
            body,
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        // The stock moved once, not twice.
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);

        // The same key on a different document is a mistake worth refusing.
        let other = open_id(&f, "damage").await;
        add(
            &f,
            &other,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "damage", "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/stock-operations/{other}/post"),
            json!({ "expectedRevision": 2, "idempotencyKey": key }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "idempotency_conflict");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 190);
    }

    /// Two lines of one document touching one lot must see each other, or the second is checked
    /// against stock the first already spent.
    #[tokio::test]
    async fn two_lines_on_one_lot_are_checked_against_each_other() {
        let f = fixture().await;
        let id = open_id(&f, "removal").await;
        for _ in 0..2 {
            add(
                &f,
                &id,
                OWNER,
                revision_of(&f, &id).await,
                json!({
                    "productPackId": f.pack_id, "batchId": f.batch_id,
                    "stockStatus": "non_sellable", "reasonCode": "disposal",
                    "quantity": 25, "quantityBasis": "base_unit"
                }),
            )
            .await;
        }
        // 25 + 25 against 40 written off: the pair cannot both be honoured.
        let (status, refused) = post(&f, &id, OWNER, revision_of(&f, &id).await).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 40);
    }

    async fn revision_of(f: &Fixture, id: &str) -> i64 {
        sqlx::query_scalar("SELECT revision FROM stock_operations WHERE id=?")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    /// Two operations competing for the last written-off atoms: exactly one may have them.
    #[tokio::test]
    async fn two_removals_cannot_both_take_the_last_stock() {
        let f = fixture().await;
        let first = open_id(&f, "removal").await;
        let second = open_id(&f, "removal").await;
        for id in [&first, &second] {
            add(
                &f,
                id,
                OWNER,
                1,
                json!({
                    "productPackId": f.pack_id, "batchId": f.batch_id,
                    "stockStatus": "non_sellable", "reasonCode": "disposal",
                    "quantity": 40, "quantityBasis": "base_unit"
                }),
            )
            .await;
        }
        let one = post(&f, &first, OWNER, 2);
        let two = post(&f, &second, OWNER, 2);
        let (left, right) = tokio::join!(one, two);
        let successes = [&left, &right]
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(successes, 1, "left={left:?} right={right:?}");
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 0);
    }

    // -----------------------------------------------------------------------------------------
    // Quote
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_quote_previews_the_result_and_writes_nothing() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 185),
        )
        .await;
        let (status, quote) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/stock-operations/{id}/quote"),
            Value::Null,
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{quote}");
        assert_eq!(quote["lines"][0]["currentAtoms"], 200);
        assert_eq!(quote["lines"][0]["resultingAtoms"], 185);
        assert_eq!(quote["postable"], true);
        // Reading a preview changed nothing at all.
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 200);
        let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(movements, 4);
    }

    // -----------------------------------------------------------------------------------------
    // Provenance and audit
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn every_movement_a_stock_operation_writes_names_the_line_that_caused_it() {
        let f = fixture().await;
        let id = open_id(&f, "damage").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "damage", "quantity": 10, "quantityBasis": "base_unit"
            }),
        )
        .await;
        post(&f, &id, PHARMACIST, 2).await;
        let anonymous: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements \
             WHERE movement_type IN ('stock_count','stock_removal') \
             AND stock_operation_line_id IS NULL",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(anonymous, 0);

        let audit: (String, String) =
            sqlx::query_as("SELECT entity_type,action FROM master_change_events WHERE entity_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(audit, ("stock_operation".to_owned(), "posted".to_owned()));
    }

    /// The audit actor is the session's, never anything the request offered.
    #[tokio::test]
    async fn the_audit_actor_comes_from_the_session() {
        let f = fixture().await;
        let id = open_id(&f, "physical_count").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            count_line(&f.pack_id, &f.batch_id, "sellable", 190),
        )
        .await;
        post(&f, &id, PHARMACIST, 2).await;
        let role: String = sqlx::query_scalar(
            "SELECT user.role FROM master_change_events event \
             JOIN users user ON user.id = event.actor_id WHERE event.entity_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(role, "pharmacist");
    }

    /// Phase 1I's release path still works, and still cannot start from a write-off.
    #[tokio::test]
    async fn phase_1i_quarantine_release_still_works_beside_the_new_operations() {
        let f = fixture().await;
        let (status, released) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-dispositions",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "quantityAtoms": 30, "fromStatus": "quarantined", "toStatus": "sellable",
                "reason": "Inspected and found fit", "occurredOn": TODAY
            }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{released}");
        assert_eq!(balance(&f, &f.batch_id, "sellable").await, 230);
        assert_eq!(balance(&f, &f.batch_id, "quarantined").await, 0);

        // A write-off is still terminal.
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-dispositions",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "quantityAtoms": 10, "fromStatus": "non_sellable", "toStatus": "sellable",
                "reason": "On reflection it looked fine", "occurredOn": TODAY
            }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 40);
    }

    /// The counter still reads sellable only, so nothing this phase writes off can be billed.
    #[tokio::test]
    async fn stock_written_off_by_an_operation_is_invisible_to_the_counter() {
        let f = fixture().await;
        let id = open_id(&f, "damage").await;
        add(
            &f,
            &id,
            PHARMACIST,
            1,
            json!({
                "productPackId": f.pack_id, "batchId": f.batch_id,
                "stockStatus": "sellable", "targetStockStatus": "non_sellable",
                "reasonCode": "damage", "quantity": 200, "quantityBasis": "base_unit"
            }),
        )
        .await;
        post(&f, &id, PHARMACIST, 2).await;
        let sellable: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
             WHERE product_pack_id=? AND batch_id=? AND stock_status='sellable'",
        )
        .bind(&f.pack_id)
        .bind(&f.batch_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(sellable, 0);
        // And the goods are still in the building.
        assert_eq!(balance(&f, &f.batch_id, "non_sellable").await, 240);
    }
}
