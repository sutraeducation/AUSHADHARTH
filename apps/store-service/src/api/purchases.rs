//! Phase 1G GST-aware purchase inward.
//!
//! A draft is editable and has no stock effect. Posting is a single atomic transaction that
//! resolves tax from authoritative masters, snapshots every fact that decided the document,
//! computes exact money through `domain::money`, creates any new Batch, writes immutable inventory
//! movements, and freezes the document.
//!
//! The browser supplies commercial intent — supplier, invoice, product, pack, batch, quantity,
//! rate. It never supplies atoms, tax, totals, the Store, or the actor: every one of those is
//! derived here and a spoofed value in a request body is ignored.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Sqlite, SqlitePool, pool::PoolConnection};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::product_catalog::{BatchInput, prepare_batch};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, required_text, validate_date, validate_uuid_v7},
    money::{self, LineAmounts, MoneyError, RateComponents, TaxTreatment},
    taxation,
};

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum PurchaseError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    NotDraft,
    Revision {
        expected: i64,
        current: i64,
    },
    DuplicateInvoice,
    SupplierNotEligible,
    StoreTaxIncomplete,
    SupplierTaxIncomplete,
    ClassificationIncomplete,
    TaxRateNotFound,
    PackMismatch,
    BatchPackMismatch,
    BatchConflict,
    ArithmeticOverflow,
    IdempotencyConflict,
    /// A replay of the same key with the same facts: the original document is returned.
    AlreadyPostedReplay,
    ServiceBusy,
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

fn simple(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
        expected_revision: None,
        current_revision: None,
    }
}

impl IntoResponse for PurchaseError {
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
                simple("purchase_not_found", "The purchase was not found."),
            ),
            Self::NotDraft => (
                StatusCode::CONFLICT,
                simple(
                    "purchase_not_draft",
                    "A posted purchase cannot be changed. Correct it with a later document.",
                ),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The purchase changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                },
            ),
            Self::DuplicateInvoice => (
                StatusCode::CONFLICT,
                simple(
                    "duplicate_supplier_invoice",
                    "This supplier invoice number is already recorded.",
                ),
            ),
            Self::SupplierNotEligible => (
                StatusCode::CONFLICT,
                simple(
                    "supplier_not_eligible",
                    "The party is not an active supplier.",
                ),
            ),
            Self::StoreTaxIncomplete => (
                StatusCode::CONFLICT,
                simple(
                    "store_tax_profile_incomplete",
                    "Record this store's place of supply before posting a purchase.",
                ),
            ),
            Self::SupplierTaxIncomplete => (
                StatusCode::CONFLICT,
                simple(
                    "supplier_tax_profile_incomplete",
                    "Record the supplier's place of supply before posting a purchase.",
                ),
            ),
            Self::ClassificationIncomplete => (
                StatusCode::CONFLICT,
                simple(
                    "product_tax_classification_incomplete",
                    "A product on this purchase has no Tax Category.",
                ),
            ),
            Self::TaxRateNotFound => (
                StatusCode::CONFLICT,
                simple(
                    "tax_rate_not_found",
                    "No tax rate is in force on the invoice date for a product's Tax Category.",
                ),
            ),
            Self::PackMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "product_pack_mismatch",
                    "The pack does not belong to the selected product.",
                ),
            ),
            Self::BatchPackMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "batch_pack_mismatch",
                    "The batch does not belong to the selected pack.",
                ),
            ),
            Self::BatchConflict => (
                StatusCode::CONFLICT,
                simple(
                    "batch_conflict",
                    "This batch conflicts with the pack's current state or an existing lot.",
                ),
            ),
            Self::ArithmeticOverflow => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(
                    "arithmetic_overflow",
                    "The amounts on this purchase are too large to record.",
                ),
            ),
            Self::IdempotencyConflict => (
                StatusCode::CONFLICT,
                simple(
                    "idempotency_conflict",
                    "This posting key was already used with different details.",
                ),
            ),
            Self::AlreadyPostedReplay => (
                StatusCode::OK,
                simple("posting_conflict", "This purchase was already posted."),
            ),
            Self::ServiceBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    "service_busy",
                    "The local service is busy. Try again shortly.",
                ),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple("internal_error", "The operation could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

impl From<AuthError> for PurchaseError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

impl From<MoneyError> for PurchaseError {
    fn from(value: MoneyError) -> Self {
        match value {
            MoneyError::Overflow => Self::ArithmeticOverflow,
            MoneyError::InvalidQuantity => {
                validation_of("quantityPacks", "must be a positive whole number of packs")
            }
            MoneyError::InvalidRate => {
                validation_of("ratePerPackPaise", "must be a non-negative amount in paise")
            }
        }
    }
}

fn validation_of(field: &str, message: &str) -> PurchaseError {
    PurchaseError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> PurchaseError {
    PurchaseError::Validation(vec![issue])
}

fn map_database_error(error: sqlx::Error) -> PurchaseError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return PurchaseError::ServiceBusy;
        }
        if message.contains("purchase_document_is_posted") {
            return PurchaseError::NotDraft;
        }
        // SQLite names the offending COLUMNS in a unique violation, never the index, so these must
        // match the column list. Matching index names silently never fired, and every unique
        // violation then fell through to "duplicate invoice" — which sent the operator to check an
        // invoice number that was not the problem.
        if message.contains("purchase_documents.normalized_supplier_invoice_number") {
            return PurchaseError::DuplicateInvoice;
        }
        if message.contains("purchase_documents.posting_idempotency_key") {
            return PurchaseError::IdempotencyConflict;
        }
        if message.contains("product_batch_conflict")
            || message.contains("product_batches.normalized_batch_number")
        {
            return PurchaseError::BatchConflict;
        }
        if message.contains("inventory_movement_conflict") {
            return PurchaseError::BatchConflict;
        }
        if message.contains("foreign key constraint failed") {
            return PurchaseError::PackMismatch;
        }
    }
    PurchaseError::Internal
}

// ---------------------------------------------------------------------------------------------
// Requests and responses
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftHeaderRequest {
    supplier_party_id: String,
    supplier_invoice_number: String,
    invoice_date: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateDraftRequest {
    expected_revision: i64,
    #[serde(flatten)]
    header: DraftHeaderRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineRequest {
    expected_revision: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    new_batch_number: Option<String>,
    new_batch_expires_on: Option<String>,
    new_batch_mrp_paise: Option<i64>,
    quantity_packs: i64,
    rate_per_pack_paise: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostRequest {
    expected_revision: i64,
    idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    status: Option<String>,
    supplier_party_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PurchaseHeaderResponse {
    id: String,
    store_id: String,
    supplier_party_id: String,
    supplier_invoice_number: String,
    normalized_supplier_invoice_number: String,
    invoice_date: String,
    status: String,
    revision: i64,
    supplier_display_name: Option<String>,
    supplier_gst_registration_status: Option<String>,
    supplier_normalized_gstin: Option<String>,
    supplier_place_of_supply_state_id: Option<String>,
    supplier_state_code: Option<String>,
    store_gst_registration_status: Option<String>,
    store_normalized_gstin: Option<String>,
    store_place_of_supply_state_id: Option<String>,
    store_state_code: Option<String>,
    tax_treatment: Option<String>,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
    created_by_user_id: String,
    created_at_utc: String,
    updated_at_utc: String,
    posted_by_user_id: Option<String>,
    posted_at_utc: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PurchaseLineResponse {
    id: String,
    purchase_document_id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    new_batch_number: Option<String>,
    new_batch_expires_on: Option<String>,
    new_batch_mrp_paise: Option<i64>,
    quantity_packs: i64,
    rate_per_pack_paise: i64,
    quantity_atoms: i64,
    taxable_value_paise: i64,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PurchaseDetailResponse {
    #[serde(flatten)]
    purchase: PurchaseHeaderResponse,
    lines: Vec<PurchaseLineResponse>,
}

const HEADER_COLUMNS: &str = "id,store_id,supplier_party_id,supplier_invoice_number,\
     normalized_supplier_invoice_number,invoice_date,status,revision,supplier_display_name,\
     supplier_gst_registration_status,supplier_normalized_gstin,supplier_place_of_supply_state_id,\
     supplier_state_code,store_gst_registration_status,store_normalized_gstin,\
     store_place_of_supply_state_id,store_state_code,tax_treatment,taxable_value_paise,cgst_paise,\
     sgst_paise,igst_paise,cess_paise,grand_total_paise,created_by_user_id,created_at_utc,\
     updated_at_utc,posted_by_user_id,posted_at_utc";

const LINE_COLUMNS: &str = "id,purchase_document_id,line_number,product_id,product_pack_id,batch_id,\
     new_batch_number,new_batch_expires_on,new_batch_mrp_paise,quantity_packs,rate_per_pack_paise,\
     quantity_atoms,taxable_value_paise,hsn_code_id,hsn_code,tax_category_id,tax_treatment_kind,\
     tax_rate_version_id,cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
     cgst_paise,sgst_paise,igst_paise,cess_paise,line_total_paise";

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/purchases", get(list_purchases).post(create_draft))
        .route(
            "/api/v1/purchases/{id}",
            get(get_purchase).put(update_draft),
        )
        .route("/api/v1/purchases/{id}/lines", post(add_line))
        .route("/api/v1/purchases/{id}/post", post(post_purchase))
        .route(
            "/api/v1/purchase-lines/{id}",
            delete(remove_line).put(update_line),
        )
}

async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PurchaseError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// Purchase entry is an Owner/Admin mutation behind the frozen Host/Origin protection.
async fn require_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PurchaseError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

/// The Store is resolved from the installation, never accepted from the browser.
async fn current_store(pool: &SqlitePool) -> Result<String, PurchaseError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(PurchaseError::NotFound)
}

/// Whitespace removed and uppercased, nothing else.
///
/// Punctuation is preserved deliberately: `INV/2026/001` and `INV-2026-001` are different invoice
/// numbers on paper, and stripping separators would silently merge two real documents into one.
fn normalize_invoice_number(value: &str) -> Result<(String, String), PurchaseError> {
    let display = required_text(value, "supplierInvoiceNumber", 64).map_err(validation_issue)?;
    let normalized = display
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    if normalized.is_empty() {
        return Err(validation_of(
            "supplierInvoiceNumber",
            "must contain at least one non-space character",
        ));
    }
    Ok((display, normalized))
}

// ---------------------------------------------------------------------------------------------
// Draft handlers
// ---------------------------------------------------------------------------------------------

async fn list_purchases(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<PurchaseHeaderResponse>>, PurchaseError> {
    require_reader(&state, &headers).await?;
    let status = query.status.unwrap_or_else(|| "all".to_owned());
    if !matches!(status.as_str(), "draft" | "posted" | "all") {
        return Err(validation_of("status", "must be draft, posted, or all"));
    }
    let status_filter = (status != "all").then_some(status);
    let rows = sqlx::query_as::<_, PurchaseHeaderResponse>(&format!(
        "SELECT {HEADER_COLUMNS} FROM purchase_documents \
         WHERE (?1 IS NULL OR status=?1) AND (?2 IS NULL OR supplier_party_id=?2) \
         ORDER BY invoice_date DESC, created_at_utc DESC LIMIT 200"
    ))
    .bind(&status_filter)
    .bind(&query.supplier_party_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_purchase(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<PurchaseDetailResponse>, PurchaseError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn create_draft(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<DraftHeaderRequest>,
) -> Result<(StatusCode, Json<PurchaseDetailResponse>), PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    let store_id = current_store(&state.pool).await?;
    let (display, normalized) = normalize_invoice_number(&request.supplier_invoice_number)?;
    let invoice_date = validate_date(Some(&request.invoice_date), "invoiceDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("invoiceDate", "is required"))?;
    let supplier_id = validate_uuid_v7(&request.supplier_party_id, "supplierPartyId")
        .map_err(validation_issue)?;
    require_active_supplier(&state.pool, &supplier_id).await?;

    let id = Uuid::now_v7().to_string();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "INSERT INTO purchase_documents (id,store_id,supplier_party_id,supplier_invoice_number,\
         normalized_supplier_invoice_number,invoice_date,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&supplier_id)
    .bind(&display)
    .bind(&normalized)
    .bind(&invoice_date)
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        &id,
        1,
        "created",
        &json!({ "supplierPartyId": supplier_id, "supplierInvoiceNumber": display }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_draft(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateDraftRequest>,
) -> Result<Json<PurchaseDetailResponse>, PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let (display, normalized) = normalize_invoice_number(&request.header.supplier_invoice_number)?;
    let invoice_date = validate_date(Some(&request.header.invoice_date), "invoiceDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("invoiceDate", "is required"))?;
    let supplier_id = validate_uuid_v7(&request.header.supplier_party_id, "supplierPartyId")
        .map_err(validation_issue)?;
    require_active_supplier(&state.pool, &supplier_id).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE purchase_documents SET supplier_party_id=?,supplier_invoice_number=?,\
         normalized_supplier_invoice_number=?,invoice_date=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(&supplier_id)
    .bind(&display)
    .bind(&normalized)
    .bind(&invoice_date)
    .bind(next)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        &id,
        next,
        "updated",
        &json!({ "supplierPartyId": supplier_id, "supplierInvoiceNumber": display }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn add_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<(StatusCode, Json<PurchaseDetailResponse>), PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let prepared = prepare_line(&state.pool, &request).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    let next_line: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(line_number),0)+1 FROM purchase_lines WHERE purchase_document_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let line_id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_line(&mut transaction, &line_id, &id, next_line, &prepared, &now).await?;
    bump_draft(&mut transaction, &id, current.0, &now).await?;
    audit(
        &mut transaction,
        &id,
        current.0 + 1,
        "updated",
        &json!({ "addedLine": line_id, "lineNumber": next_line }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<LineRequest>,
) -> Result<Json<PurchaseDetailResponse>, PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let prepared = prepare_line(&state.pool, &request).await?;
    let document_id = line_document(&state.pool, &line_id).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE purchase_lines SET product_id=?,product_pack_id=?,batch_id=?,new_batch_number=?,\
         new_batch_expires_on=?,new_batch_mrp_paise=?,quantity_packs=?,rate_per_pack_paise=?,\
         quantity_atoms=?,taxable_value_paise=?,updated_at_utc=? WHERE id=?",
    )
    .bind(&prepared.product_id)
    .bind(&prepared.product_pack_id)
    .bind(&prepared.batch_id)
    .bind(&prepared.new_batch_number)
    .bind(&prepared.new_batch_expires_on)
    .bind(prepared.new_batch_mrp_paise)
    .bind(prepared.quantity_packs)
    .bind(prepared.rate_per_pack_paise)
    .bind(prepared.quantity_atoms)
    .bind(prepared.taxable_value_paise)
    .bind(&now)
    .bind(&line_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    bump_draft(&mut transaction, &document_id, current.0, &now).await?;
    audit(
        &mut transaction,
        &document_id,
        current.0 + 1,
        "updated",
        &json!({ "updatedLine": line_id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &document_id).await?))
}

async fn remove_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<PostRequestRevisionOnly>,
) -> Result<Json<PurchaseDetailResponse>, PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let document_id = line_document(&state.pool, &line_id).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query("DELETE FROM purchase_lines WHERE id=?")
        .bind(&line_id)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;
    bump_draft(&mut transaction, &document_id, current.0, &now).await?;
    audit(
        &mut transaction,
        &document_id,
        current.0 + 1,
        "updated",
        &json!({ "removedLine": line_id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &document_id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostRequestRevisionOnly {
    expected_revision: i64,
}

// ---------------------------------------------------------------------------------------------
// Line preparation — the server derives atoms and taxable value; a client never supplies them.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PreparedLine {
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    new_batch_number: Option<String>,
    new_batch_expires_on: Option<String>,
    new_batch_mrp_paise: Option<i64>,
    quantity_packs: i64,
    rate_per_pack_paise: i64,
    quantity_atoms: i64,
    taxable_value_paise: i64,
}

async fn prepare_line(
    pool: &SqlitePool,
    request: &LineRequest,
) -> Result<PreparedLine, PurchaseError> {
    let product_id =
        validate_uuid_v7(&request.product_id, "productId").map_err(validation_issue)?;
    let product_pack_id =
        validate_uuid_v7(&request.product_pack_id, "productPackId").map_err(validation_issue)?;

    // The Pack must belong to the Product, and both must be active.
    let pack: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT pack.product_id,pack.base_quantity_atoms,pack.status FROM product_packs pack WHERE pack.id=?",
    )
    .bind(&product_pack_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    let (owner_product, base_quantity_atoms, pack_status) = pack.ok_or(PurchaseError::NotFound)?;
    if owner_product != product_id {
        return Err(PurchaseError::PackMismatch);
    }
    if pack_status != "active" {
        return Err(PurchaseError::PackMismatch);
    }

    let batch_id = match request.batch_id.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => {
            let id = validate_uuid_v7(value, "batchId").map_err(validation_issue)?;
            let owner: Option<String> =
                sqlx::query_scalar("SELECT product_pack_id FROM product_batches WHERE id=?")
                    .bind(&id)
                    .fetch_optional(pool)
                    .await
                    .map_err(map_database_error)?;
            match owner {
                Some(pack) if pack == product_pack_id => Some(id),
                Some(_) => return Err(PurchaseError::BatchPackMismatch),
                None => return Err(PurchaseError::NotFound),
            }
        }
        _ => None,
    };

    // A line either names an existing Batch or proposes a new one, never both.
    let proposed = request
        .new_batch_number
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if batch_id.is_some() && proposed.is_some() {
        return Err(validation_of(
            "newBatchNumber",
            "cannot propose a new batch while also selecting an existing one",
        ));
    }
    // Validated now through the frozen batch rules so a draft cannot hold an impossible lot; the
    // Batch row itself is created during posting, so an abandoned draft leaves no orphan.
    let new_batch = match proposed {
        Some(number) => {
            let (checked, _) = prepare_batch(BatchInput {
                batch_number: number.to_owned(),
                manufactured_on: None,
                expires_on: request.new_batch_expires_on.clone(),
                mrp_paise: request.new_batch_mrp_paise,
            })
            .map_err(|_| PurchaseError::BatchConflict)?;
            Some(checked)
        }
        None => None,
    };

    let taxable_value_paise =
        money::taxable_value_paise(request.quantity_packs, request.rate_per_pack_paise)?;
    let quantity_atoms = money::quantity_atoms(request.quantity_packs, base_quantity_atoms)?;

    Ok(PreparedLine {
        product_id,
        product_pack_id,
        batch_id,
        new_batch_number: new_batch.as_ref().map(|batch| batch.batch_number.clone()),
        new_batch_expires_on: new_batch
            .as_ref()
            .and_then(|batch| batch.expires_on.clone()),
        new_batch_mrp_paise: new_batch.as_ref().and_then(|batch| batch.mrp_paise),
        quantity_packs: request.quantity_packs,
        rate_per_pack_paise: request.rate_per_pack_paise,
        quantity_atoms,
        taxable_value_paise,
    })
}

// ---------------------------------------------------------------------------------------------
// Posting — one atomic transaction
// ---------------------------------------------------------------------------------------------

async fn post_purchase(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PostRequest>,
) -> Result<Json<PurchaseDetailResponse>, PurchaseError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| PurchaseError::Internal)?;
    // The frozen inventory concurrency model: take the write lock before reading balances or
    // deciding anything, so a concurrent posting cannot interleave.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let outcome = post_within_transaction(
        &mut connection,
        &id,
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
        Err(error) => {
            // Any failure discards the whole posting: no partial document, no partial stock, and no
            // Batch created by a posting that did not complete.
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            drop(connection);
            if matches!(error, PurchaseError::AlreadyPostedReplay) {
                return Ok(Json(fetch_detail(&state.pool, &id).await?));
            }
            Err(error)
        }
    }
}

async fn post_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    expected_revision: i64,
    idempotency_key: &str,
    actor_id: &str,
) -> Result<(), PurchaseError> {
    let header = sqlx::query_as::<_, PostingHeader>(
        "SELECT status,revision,supplier_party_id,invoice_date,store_id,\
         posting_idempotency_key,posting_fingerprint FROM purchase_documents WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let PostingHeader {
        status,
        revision,
        supplier_party_id: supplier_id,
        invoice_date,
        store_id,
        posting_idempotency_key: existing_key,
        posting_fingerprint: existing_print,
    } = header.ok_or(PurchaseError::NotFound)?;

    let lines = load_lines(connection, id).await?;
    let fingerprint = posting_fingerprint(id, &supplier_id, &invoice_date, &lines);

    // A replay of the same key returns the original document; the same key with different facts is
    // a conflict rather than a silent overwrite.
    if status == "posted" {
        return match (existing_key.as_deref(), existing_print.as_deref()) {
            (Some(key), Some(print)) if key == idempotency_key && print == fingerprint => {
                Err(PurchaseError::AlreadyPostedReplay)
            }
            (Some(key), _) if key == idempotency_key => Err(PurchaseError::IdempotencyConflict),
            _ => Err(PurchaseError::NotDraft),
        };
    }
    if revision != expected_revision {
        return Err(PurchaseError::Revision {
            expected: expected_revision,
            current: revision,
        });
    }
    if lines.is_empty() {
        return Err(validation_of("lines", "a purchase needs at least one line"));
    }

    // Both places of supply are re-read now, never taken from the draft or the browser.
    let store = sqlx::query_as::<_, StoreTaxSource>(
        "SELECT normalized_gstin,place_of_supply_state_id,gst_registration_status \
         FROM store_identity WHERE store_id=?",
    )
    .bind(&store_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let StoreTaxSource {
        normalized_gstin: store_gstin,
        place_of_supply_state_id: store_state_id,
        gst_registration_status: store_status,
    } = store.ok_or(PurchaseError::NotFound)?;
    let store_state_id = store_state_id.ok_or(PurchaseError::StoreTaxIncomplete)?;
    let store_state_code = state_code(connection, &store_state_id).await?;

    let supplier = sqlx::query_as::<_, SupplierTaxSource>(
        "SELECT display_name,legal_name,gst_registration_status,normalized_gstin,\
         place_of_supply_state_id,status FROM parties WHERE id=?",
    )
    .bind(&supplier_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let SupplierTaxSource {
        display_name: supplier_name,
        legal_name: supplier_legal,
        gst_registration_status: supplier_status,
        normalized_gstin: supplier_gstin,
        place_of_supply_state_id: supplier_state_id,
        status: party_status,
    } = supplier.ok_or(PurchaseError::NotFound)?;
    if party_status != "active" {
        return Err(PurchaseError::SupplierNotEligible);
    }
    let has_role: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM party_roles WHERE party_id=? AND role='supplier' AND status='active' LIMIT 1",
    )
    .bind(&supplier_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    if has_role.is_none() {
        return Err(PurchaseError::SupplierNotEligible);
    }
    let supplier_state_id = supplier_state_id.ok_or(PurchaseError::SupplierTaxIncomplete)?;
    let supplier_state_code = state_code(connection, &supplier_state_id).await?;

    // The treatment is decided here, from two persisted State codes.
    let treatment = TaxTreatment::from_state_codes(&store_state_code, &supplier_state_code);

    let mut computed = Vec::with_capacity(lines.len());
    for line in &lines {
        computed.push(resolve_and_compute(connection, line, &invoice_date, treatment).await?);
    }
    let totals = money::sum_lines(
        &computed
            .iter()
            .map(|entry| entry.amounts)
            .collect::<Vec<_>>(),
    )?;

    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;

    for (line, entry) in lines.iter().zip(computed.iter()) {
        // A proposed Batch becomes real only now, inside the posting transaction.
        let batch_id = match (&line.batch_id, &line.new_batch_number) {
            (Some(existing), _) => Some(existing.clone()),
            (None, Some(number)) => Some(create_batch(connection, line, number, &now).await?),
            (None, None) => None,
        };
        sqlx::query(
            // The proposed-batch fields are draft scaffolding. Once posting has materialised the
            // lot, batch_id is the authoritative identity, so the proposal is cleared — which also
            // keeps the "an existing batch OR a proposed one, never both" invariant true on a
            // posted line.
            "UPDATE purchase_lines SET batch_id=?,new_batch_number=NULL,new_batch_expires_on=NULL,\
             new_batch_mrp_paise=NULL,hsn_code_id=?,hsn_code=?,tax_category_id=?,\
             tax_treatment_kind=?,tax_rate_version_id=?,cgst_basis_points=?,sgst_basis_points=?,\
             igst_basis_points=?,cess_basis_points=?,cgst_paise=?,sgst_paise=?,igst_paise=?,\
             cess_paise=?,line_total_paise=?,updated_at_utc=? WHERE id=?",
        )
        .bind(&batch_id)
        .bind(&entry.hsn_code_id)
        .bind(&entry.hsn_code)
        .bind(&entry.tax_category_id)
        .bind(&entry.tax_treatment_kind)
        .bind(&entry.tax_rate_version_id)
        .bind(entry.rate.cgst_basis_points)
        .bind(entry.rate.sgst_basis_points)
        .bind(entry.rate.igst_basis_points)
        .bind(entry.rate.cess_basis_points)
        .bind(entry.amounts.cgst_paise)
        .bind(entry.amounts.sgst_paise)
        .bind(entry.amounts.igst_paise)
        .bind(entry.amounts.cess_paise)
        .bind(entry.amounts.line_total_paise)
        .bind(&now)
        .bind(&line.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;

        // One immutable inward movement per line, carrying a real provenance foreign key.
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,quantity_delta_atoms,occurred_on,purchase_line_id,idempotency_key,\
             posted_by_user_id,posted_at_utc) VALUES (?,?,?,?,?,'purchase',?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&store_id)
        .bind(&line.product_id)
        .bind(&line.product_pack_id)
        .bind(&batch_id)
        .bind(line.quantity_atoms)
        .bind(&invoice_date)
        .bind(&line.id)
        .bind(Uuid::now_v7().to_string())
        .bind(actor_id)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    let next = revision + 1;
    sqlx::query(
        "UPDATE purchase_documents SET status='posted',revision=?,supplier_display_name=?,\
         supplier_legal_name=?,supplier_gst_registration_status=?,supplier_normalized_gstin=?,\
         supplier_place_of_supply_state_id=?,supplier_state_code=?,store_gst_registration_status=?,\
         store_normalized_gstin=?,store_place_of_supply_state_id=?,store_state_code=?,\
         tax_treatment=?,taxable_value_paise=?,cgst_paise=?,sgst_paise=?,igst_paise=?,cess_paise=?,\
         grand_total_paise=?,posted_by_user_id=?,posted_at_utc=?,posting_idempotency_key=?,\
         posting_fingerprint=?,updated_at_utc=? WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(next)
    .bind(&supplier_name)
    .bind(&supplier_legal)
    .bind(&supplier_status)
    .bind(&supplier_gstin)
    .bind(&supplier_state_id)
    .bind(&supplier_state_code)
    .bind(&store_status)
    .bind(&store_gstin)
    .bind(&store_state_id)
    .bind(&store_state_code)
    .bind(treatment.as_str())
    .bind(totals.taxable_value_paise)
    .bind(totals.cgst_paise)
    .bind(totals.sgst_paise)
    .bind(totals.igst_paise)
    .bind(totals.cess_paise)
    .bind(totals.line_total_paise)
    .bind(actor_id)
    .bind(&now)
    .bind(idempotency_key)
    .bind(&fingerprint)
    .bind(&now)
    .bind(id)
    .bind(revision)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'purchase_document',?,?,'posted',strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(id)
    .bind(next)
    .bind(
        json!({
            "taxTreatment": treatment.as_str(),
            "grandTotalPaise": totals.line_total_paise,
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

/// A resolved line: the classification and rate snapshot plus the computed amounts.
struct ComputedLine {
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    rate: RateComponents,
    amounts: LineAmounts,
}

async fn resolve_and_compute(
    connection: &mut PoolConnection<Sqlite>,
    line: &DraftLine,
    invoice_date: &str,
    treatment: TaxTreatment,
) -> Result<ComputedLine, PurchaseError> {
    let classification: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT hsn_code_id,tax_category_id FROM products WHERE id=?")
            .bind(&line.product_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    let (hsn_code_id, tax_category_id) = classification.ok_or(PurchaseError::NotFound)?;
    // A Product with no Tax Category cannot be taxed. That is an error, never zero tax.
    let tax_category_id = tax_category_id.ok_or(PurchaseError::ClassificationIncomplete)?;

    let hsn_code: Option<String> = match hsn_code_id.as_deref() {
        Some(id) => sqlx::query_scalar("SELECT hsn_code FROM hsn_codes WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?,
        None => None,
    };

    let treatment_kind: Option<String> =
        sqlx::query_scalar("SELECT tax_treatment FROM tax_categories WHERE id=?")
            .bind(&tax_category_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    let treatment_kind = treatment_kind.ok_or(PurchaseError::ClassificationIncomplete)?;

    // A zero-rate category asserts zero tax by classification; a taxable one must resolve a rate in
    // force on the invoice date, and the absence of one is an error rather than zero.
    let (rate_version_id, rate) = if treatment_kind == "taxable" {
        let resolved =
            taxation::resolve_tax_rate(&mut **connection, &tax_category_id, invoice_date)
                .await
                .map_err(|error| match error {
                    taxation::TaxResolutionError::InvalidDate => {
                        validation_of("invoiceDate", "must be a valid YYYY-MM-DD date")
                    }
                    taxation::TaxResolutionError::Database(_) => PurchaseError::Internal,
                })?
                .ok_or(PurchaseError::TaxRateNotFound)?;
        (
            Some(resolved.id),
            RateComponents {
                cgst_basis_points: resolved.cgst_basis_points,
                sgst_basis_points: resolved.sgst_basis_points,
                igst_basis_points: resolved.igst_basis_points,
                cess_basis_points: resolved.cess_basis_points,
            },
        )
    } else {
        (None, RateComponents::default())
    };

    let amounts = money::compute_line(
        line.quantity_packs,
        line.rate_per_pack_paise,
        treatment,
        rate,
    )?;
    Ok(ComputedLine {
        hsn_code_id,
        hsn_code,
        tax_category_id: Some(tax_category_id),
        tax_treatment_kind: Some(treatment_kind),
        tax_rate_version_id: rate_version_id,
        rate,
        amounts,
    })
}

async fn create_batch(
    connection: &mut PoolConnection<Sqlite>,
    line: &DraftLine,
    number: &str,
    now: &str,
) -> Result<String, PurchaseError> {
    let (checked, normalized) = prepare_batch(BatchInput {
        batch_number: number.to_owned(),
        manufactured_on: None,
        expires_on: line.new_batch_expires_on.clone(),
        mrp_paise: line.new_batch_mrp_paise,
    })
    .map_err(|_| PurchaseError::BatchConflict)?;
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
         expires_on,mrp_paise,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&line.product_pack_id)
    .bind(&checked.batch_number)
    .bind(&normalized)
    .bind(&checked.expires_on)
    .bind(checked.mrp_paise)
    .bind(now)
    .bind(now)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(id)
}

/// The semantic payload a replay must match: the document, the supplier, the invoice identity, and
/// each line's commercial facts. Presentation and timestamps are deliberately excluded.
fn posting_fingerprint(
    id: &str,
    supplier_id: &str,
    invoice_date: &str,
    lines: &[DraftLine],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(supplier_id.as_bytes());
    hasher.update(invoice_date.as_bytes());
    for line in lines {
        hasher.update(line.product_id.as_bytes());
        hasher.update(line.product_pack_id.as_bytes());
        // Deliberately excluding batch_id and new_batch_number: posting rewrites both, replacing a
        // proposal with the lot it materialised. Hashing them made the fingerprint change as a
        // side effect of the very posting it describes, so a retry after a successful post was
        // reported as "already attempted with different details" instead of returning the original
        // document — inviting the operator to enter the invoice twice. What stays here is what the
        // operator actually chose, which posting never alters.
        hasher.update(line.quantity_packs.to_le_bytes());
        hasher.update(line.rate_per_pack_paise.to_le_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------------------------

/// The header facts posting needs, read under the write lock.
#[derive(Debug, FromRow)]
struct PostingHeader {
    status: String,
    revision: i64,
    supplier_party_id: String,
    invoice_date: String,
    store_id: String,
    posting_idempotency_key: Option<String>,
    posting_fingerprint: Option<String>,
}

/// The Store's authoritative tax geography, re-read at posting rather than taken from the draft.
#[derive(Debug, FromRow)]
struct StoreTaxSource {
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    gst_registration_status: String,
}

/// The supplier facts snapshotted onto the posted document.
#[derive(Debug, FromRow)]
struct SupplierTaxSource {
    display_name: String,
    legal_name: Option<String>,
    gst_registration_status: String,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    status: String,
}

#[derive(Debug, FromRow)]
struct DraftLine {
    id: String,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    new_batch_number: Option<String>,
    new_batch_expires_on: Option<String>,
    new_batch_mrp_paise: Option<i64>,
    quantity_packs: i64,
    rate_per_pack_paise: i64,
    quantity_atoms: i64,
}

async fn load_lines(
    connection: &mut PoolConnection<Sqlite>,
    document_id: &str,
) -> Result<Vec<DraftLine>, PurchaseError> {
    sqlx::query_as::<_, DraftLine>(
        "SELECT id,product_id,product_pack_id,batch_id,new_batch_number,new_batch_expires_on,\
         new_batch_mrp_paise,quantity_packs,rate_per_pack_paise,quantity_atoms \
         FROM purchase_lines WHERE purchase_document_id=? ORDER BY line_number",
    )
    .bind(document_id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)
}

async fn state_code(
    connection: &mut PoolConnection<Sqlite>,
    state_id: &str,
) -> Result<String, PurchaseError> {
    sqlx::query_scalar("SELECT state_code FROM state_codes WHERE id=?")
        .bind(state_id)
        .fetch_optional(&mut **connection)
        .await
        .map_err(map_database_error)?
        .ok_or(PurchaseError::Internal)
}

async fn draft_state(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(i64, String), PurchaseError> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM purchase_documents WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = row.ok_or(PurchaseError::NotFound)?;
    if status != "draft" {
        return Err(PurchaseError::NotDraft);
    }
    Ok((revision, status))
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), PurchaseError> {
    if current.0 != expected {
        return Err(PurchaseError::Revision {
            expected,
            current: current.0,
        });
    }
    Ok(())
}

async fn require_active_supplier(pool: &SqlitePool, party_id: &str) -> Result<(), PurchaseError> {
    let eligible: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM parties party JOIN party_roles role ON role.party_id=party.id \
         WHERE party.id=? AND party.status='active' AND role.role='supplier' \
           AND role.status='active' LIMIT 1",
    )
    .bind(party_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;
    eligible
        .map(|_| ())
        .ok_or(PurchaseError::SupplierNotEligible)
}

async fn line_document(pool: &SqlitePool, line_id: &str) -> Result<String, PurchaseError> {
    sqlx::query_scalar("SELECT purchase_document_id FROM purchase_lines WHERE id=?")
        .bind(line_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(PurchaseError::NotFound)
}

async fn bump_draft(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    current: i64,
    now: &str,
) -> Result<(), PurchaseError> {
    sqlx::query(
        "UPDATE purchase_documents SET revision=?,updated_at_utc=? WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(current + 1)
    .bind(now)
    .bind(id)
    .bind(current)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_line(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    document_id: &str,
    line_number: i64,
    line: &PreparedLine,
    now: &str,
) -> Result<(), PurchaseError> {
    sqlx::query(
        "INSERT INTO purchase_lines (id,purchase_document_id,line_number,product_id,product_pack_id,\
         batch_id,new_batch_number,new_batch_expires_on,new_batch_mrp_paise,quantity_packs,\
         rate_per_pack_paise,quantity_atoms,taxable_value_paise,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(document_id)
    .bind(line_number)
    .bind(&line.product_id)
    .bind(&line.product_pack_id)
    .bind(&line.batch_id)
    .bind(&line.new_batch_number)
    .bind(&line.new_batch_expires_on)
    .bind(line.new_batch_mrp_paise)
    .bind(line.quantity_packs)
    .bind(line.rate_per_pack_paise)
    .bind(line.quantity_atoms)
    .bind(line.taxable_value_paise)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<String, PurchaseError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

async fn audit(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    entity_id: &str,
    revision: i64,
    action: &str,
    payload: &Value,
    actor_id: &str,
) -> Result<(), PurchaseError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'purchase_document',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_id)
    .bind(revision)
    .bind(action)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn fetch_detail(
    pool: &SqlitePool,
    id: &str,
) -> Result<PurchaseDetailResponse, PurchaseError> {
    let purchase = sqlx::query_as::<_, PurchaseHeaderResponse>(&format!(
        "SELECT {HEADER_COLUMNS} FROM purchase_documents WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PurchaseError::NotFound)?;
    let lines = sqlx::query_as::<_, PurchaseLineResponse>(&format!(
        "SELECT {LINE_COLUMNS} FROM purchase_lines WHERE purchase_document_id=? ORDER BY line_number"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    Ok(PurchaseDetailResponse { purchase, lines })
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    const TABLET: &str = "01997000-0000-7000-8000-000000000001";
    const STRIP: &str = "01997000-0000-7000-8000-000000000004";
    const MAHARASHTRA: &str = "01997300-0000-7000-8000-000000000027";
    const KARNATAKA: &str = "01997300-0000-7000-8000-000000000029";
    const OWNER: &str = "purchase-owner-session-token";
    const CASHIER: &str = "purchase-cashier-session-token";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        owner_id: String,
        supplier_id: String,
        product_id: String,
        pack_id: String,
        category_id: String,
    }

    /// A store in Maharashtra, a supplier in Maharashtra, and one taxable product at 6% + 6%.
    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("purchase.sqlite3"))
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
        let owner_id = insert_session(&pool, "owner_admin", OWNER).await;
        insert_session(&pool, "cashier", CASHIER).await;

        let supplier_id = insert_supplier(&pool, "Sharma Medicals", Some(MAHARASHTRA)).await;
        let category_id = insert_category(&pool, "gst-12").await;
        insert_rate(&pool, &category_id, "2020-01-01", None, 600).await;
        let hsn_id = insert_hsn(&pool, "30049099").await;
        let (product_id, pack_id) = insert_product(&pool, "Azithral 500", 10).await;
        sqlx::query("UPDATE products SET hsn_code_id=?,tax_category_id=? WHERE id=?")
            .bind(&hsn_id)
            .bind(&category_id)
            .bind(&product_id)
            .execute(&pool)
            .await
            .unwrap();

        Fixture {
            _temp: temp,
            pool,
            owner_id,
            supplier_id,
            product_id,
            pack_id,
            category_id,
        }
    }

    async fn insert_supplier(pool: &SqlitePool, name: &str, state: Option<&str>) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             place_of_supply_state_id,created_at_utc,updated_at_utc) \
             VALUES (?,?,?, 'unregistered',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(name)
        .bind(name.to_lowercase())
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,'supplier',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_product(pool: &SqlitePool, name: &str, pack_atoms: i64) -> (String, String) {
        let product_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO products (id,product_kind,base_unit_id,quantity_scale,display_name,\
             normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,'general_pharmacy_item',?,0,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&product_id)
        .bind(TABLET)
        .bind(name)
        .bind(name.to_lowercase())
        .execute(pool)
        .await
        .unwrap();
        let pack_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,\
             display_label,created_at_utc,updated_at_utc) VALUES (?,?,?,?,'Strip',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&pack_id)
        .bind(&product_id)
        .bind(STRIP)
        .bind(pack_atoms)
        .execute(pool)
        .await
        .unwrap();
        (product_id, pack_id)
    }

    async fn insert_category(pool: &SqlitePool, code: &str) -> String {
        insert_category_with(pool, code, "taxable").await
    }

    async fn insert_category_with(pool: &SqlitePool, code: &str, treatment: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_categories (id,jurisdiction,category_code,display_name,tax_treatment,\
             created_at_utc,updated_at_utc) VALUES (?,'IN',?,?,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .bind(code)
        .bind(treatment)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_rate(
        pool: &SqlitePool,
        category_id: &str,
        from: &str,
        to: Option<&str>,
        half: i64,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_rate_versions (id,tax_category_id,effective_from,effective_to,\
             cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
             created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(category_id)
        .bind(from)
        .bind(to)
        .bind(half)
        .bind(half)
        .bind(half * 2)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_hsn(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO hsn_codes (id,jurisdiction,hsn_code,description,created_at_utc,updated_at_utc) \
             VALUES (?,'IN',?,'Medicaments',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    fn draft_body(supplier: &str, invoice: &str) -> Value {
        json!({
            "supplierPartyId": supplier,
            "supplierInvoiceNumber": invoice,
            "invoiceDate": "2026-04-01"
        })
    }

    fn line_body(revision: i64, product: &str, pack: &str, packs: i64, rate: i64) -> Value {
        json!({
            "expectedRevision": revision,
            "productId": product,
            "productPackId": pack,
            "quantityPacks": packs,
            "ratePerPackPaise": rate
        })
    }

    async fn draft_with_line(f: &Fixture, invoice: &str) -> String {
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, invoice),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, withline) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(1, &f.product_id, &f.pack_id, 5, 8000),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{withline}");
        id
    }

    async fn post_draft(f: &Fixture, id: &str, revision: i64, key: &str) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/post"),
            json!({ "expectedRevision": revision, "idempotencyKey": key }),
        )
        .await
    }

    // -------------------------------------------------------------------------------------
    // Draft
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_draft_has_no_inventory_effect_and_derives_its_own_atoms() {
        let f = fixture().await;
        let id = draft_with_line(&f, "INV-001").await;
        let (_, detail) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(detail["status"], "draft");
        // 5 strips of 10 tablets is 50 atoms, and 5 x 80.00 is 400.00 taxable — both derived.
        assert_eq!(detail["lines"][0]["quantityAtoms"], 50);
        assert_eq!(detail["lines"][0]["taxableValuePaise"], 40000);
        // A draft computes no tax and touches no stock.
        assert_eq!(detail["lines"][0]["lineTotalPaise"], 0);
        assert_eq!(detail["taxTreatment"], Value::Null);
        let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(movements, 0, "a draft must not move stock");
    }

    #[tokio::test]
    async fn a_stale_revision_cannot_change_a_draft() {
        let f = fixture().await;
        let id = draft_with_line(&f, "INV-002").await;
        let (status, conflict) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(1, &f.product_id, &f.pack_id, 1, 100),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
    }

    #[tokio::test]
    async fn the_same_supplier_invoice_is_refused_but_another_supplier_may_reuse_the_number() {
        let f = fixture().await;
        draft_with_line(&f, "INV/2026/007").await;
        let (status, duplicate) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "inv/2026/007"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
        assert_eq!(duplicate["code"], "duplicate_supplier_invoice");

        // Punctuation is preserved, so a differently punctuated number is a different document.
        let (status, punctuated) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "INV-2026-007"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{punctuated}");

        // A different supplier may legitimately use the same invoice number.
        let other = insert_supplier(&f.pool, "Bharat Distributors", Some(MAHARASHTRA)).await;
        let (status, elsewhere) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&other, "INV/2026/007"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{elsewhere}");
    }

    // -------------------------------------------------------------------------------------
    // Posting and GST
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn an_intra_state_purchase_posts_cgst_and_sgst_and_moves_exact_stock() {
        let f = fixture().await;
        let id = draft_with_line(&f, "INV-100").await;
        let (status, posted) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["status"], "posted");
        assert_eq!(posted["taxTreatment"], "intra_state");

        // 400.00 taxable at 6% + 6%.
        assert_eq!(posted["taxableValuePaise"], 40000);
        assert_eq!(posted["cgstPaise"], 2400);
        assert_eq!(posted["sgstPaise"], 2400);
        assert_eq!(posted["igstPaise"], 0, "intra-state must not charge IGST");
        assert_eq!(posted["grandTotalPaise"], 44800);

        // Snapshots are written, so later master edits cannot rewrite this document.
        assert_eq!(posted["supplierDisplayName"], "Sharma Medicals");
        assert_eq!(posted["supplierStateCode"], "27");
        assert_eq!(posted["storeStateCode"], "27");
        assert_eq!(posted["lines"][0]["hsnCode"], "30049099");
        assert_eq!(posted["lines"][0]["cgstBasisPoints"], 600);
        assert!(posted["lines"][0]["taxRateVersionId"].is_string());

        // Exactly one inward movement, carrying real provenance.
        let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
            "SELECT movement_type,quantity_delta_atoms,purchase_line_id FROM inventory_movements",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "purchase");
        assert_eq!(rows[0].1, 50);
        assert_eq!(
            rows[0].2.as_deref(),
            Some(posted["lines"][0]["id"].as_str().unwrap()),
            "the ledger knows which purchase line this came from"
        );

        // The derived balance reflects it.
        let (_, stock) = request(
            f.pool.clone(),
            "GET",
            "/api/v1/inventory/stock",
            Value::Null,
        )
        .await;
        assert_eq!(stock[0]["balanceAtoms"], 50);
    }

    #[tokio::test]
    async fn an_inter_state_purchase_posts_igst_only() {
        let f = fixture().await;
        let karnataka_supplier =
            insert_supplier(&f.pool, "Bengaluru Traders", Some(KARNATAKA)).await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&karnataka_supplier, "KA-1"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(1, &f.product_id, &f.pack_id, 5, 8000),
        )
        .await;
        let (status, posted) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["taxTreatment"], "inter_state");
        assert_eq!(posted["igstPaise"], 4800);
        assert_eq!(posted["cgstPaise"], 0, "inter-state must not charge CGST");
        assert_eq!(posted["sgstPaise"], 0, "inter-state must not charge SGST");
        assert_eq!(
            posted["grandTotalPaise"], 44800,
            "same tax, different split"
        );
    }

    #[tokio::test]
    async fn exempt_nil_and_non_gst_post_zero_tax_by_classification() {
        for treatment in ["exempt", "nil_rated", "non_gst"] {
            let f = fixture().await;
            let category = insert_category_with(&f.pool, "zero", treatment).await;
            // Deliberately no rate version: a zero-rate class asserts zero, it does not look one up.
            sqlx::query("UPDATE products SET tax_category_id=? WHERE id=?")
                .bind(&category)
                .bind(&f.product_id)
                .execute(&f.pool)
                .await
                .unwrap();
            let id = draft_with_line(&f, "ZERO-1").await;
            let (status, posted) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
            assert_eq!(status, StatusCode::OK, "{treatment}: {posted}");
            assert_eq!(posted["taxableValuePaise"], 40000);
            assert_eq!(posted["cgstPaise"], 0);
            assert_eq!(posted["igstPaise"], 0);
            assert_eq!(posted["grandTotalPaise"], 40000);
            assert_eq!(posted["lines"][0]["taxTreatmentKind"], treatment);
            // A zero-rate line names no rate version, because none was needed.
            assert_eq!(posted["lines"][0]["taxRateVersionId"], Value::Null);
        }
    }

    #[tokio::test]
    async fn missing_tax_facts_are_errors_and_never_silently_zero() {
        // No Product Tax Category.
        let f = fixture().await;
        sqlx::query("UPDATE products SET tax_category_id=NULL WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let id = draft_with_line(&f, "MISS-1").await;
        let (status, error) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "product_tax_classification_incomplete");

        // No rate in force on the invoice date.
        let f = fixture().await;
        sqlx::query(
            "UPDATE tax_rate_versions SET effective_from='2030-01-01' WHERE tax_category_id=?",
        )
        .bind(&f.category_id)
        .execute(&f.pool)
        .await
        .unwrap();
        let id = draft_with_line(&f, "MISS-2").await;
        let (status, error) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "tax_rate_not_found");

        // No Store place of supply.
        let f = fixture().await;
        sqlx::query(
            "UPDATE store_identity SET gst_registration_status='unknown',gstin=NULL,\
                     normalized_gstin=NULL,place_of_supply_state_id=NULL",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        let id = draft_with_line(&f, "MISS-3").await;
        let (status, error) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "store_tax_profile_incomplete");

        // No Supplier place of supply.
        let f = fixture().await;
        let stateless = insert_supplier(&f.pool, "Unknown State Traders", None).await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&stateless, "MISS-4"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(1, &f.product_id, &f.pack_id, 1, 100),
        )
        .await;
        let (status, error) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "supplier_tax_profile_incomplete");
    }

    #[tokio::test]
    async fn the_rate_is_resolved_on_the_invoice_date_not_today() {
        let f = fixture().await;
        // 2.50% each until 2026-04-01, then 9.00% each from that date.
        sqlx::query("DELETE FROM tax_rate_versions")
            .execute(&f.pool)
            .await
            .unwrap();
        insert_rate(
            &f.pool,
            &f.category_id,
            "2025-01-01",
            Some("2026-04-01"),
            250,
        )
        .await;
        insert_rate(&f.pool, &f.category_id, "2026-04-01", None, 900).await;

        // The fixture invoice date is exactly the boundary, which belongs to the NEXT version.
        let id = draft_with_line(&f, "DATE-1").await;
        let (status, posted) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["lines"][0]["cgstBasisPoints"], 900);
        assert_eq!(posted["cgstPaise"], 3600);

        // A day earlier resolves the earlier version.
        let (_, earlier) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            json!({
                "supplierPartyId": f.supplier_id,
                "supplierInvoiceNumber": "DATE-2",
                "invoiceDate": "2026-03-31"
            }),
        )
        .await;
        let earlier_id = earlier["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{earlier_id}/lines"),
            line_body(1, &f.product_id, &f.pack_id, 5, 8000),
        )
        .await;
        let (_, posted_earlier) = post_draft(&f, &earlier_id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(posted_earlier["lines"][0]["cgstBasisPoints"], 250);
        assert_eq!(posted_earlier["cgstPaise"], 1000);
    }

    // -------------------------------------------------------------------------------------
    // Immutability, idempotency, atomicity
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_posted_purchase_cannot_be_changed_by_the_service_or_by_direct_sql() {
        let f = fixture().await;
        let id = draft_with_line(&f, "IMM-1").await;
        post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        // The service refuses.
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(3, &f.product_id, &f.pack_id, 1, 100),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "purchase_not_draft");

        // And so does the database, independently.
        let direct = sqlx::query("UPDATE purchase_documents SET grand_total_paise=1 WHERE id=?")
            .bind(&id)
            .execute(&f.pool)
            .await;
        assert!(direct.is_err(), "a posted header must be frozen");

        let line_id: String =
            sqlx::query_scalar("SELECT id FROM purchase_lines WHERE purchase_document_id=?")
                .bind(&id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let direct_line = sqlx::query("UPDATE purchase_lines SET cgst_paise=0 WHERE id=?")
            .bind(&line_id)
            .execute(&f.pool)
            .await;
        assert!(direct_line.is_err(), "a posted line must be frozen");

        // Deleting the lines first, so that a later header delete cannot be refused merely by the
        // child foreign key. Asserting only `is_err()` let the delete-protection trigger be removed
        // without any test noticing: the foreign key was refusing it instead.
        let line_delete = sqlx::query("DELETE FROM purchase_lines WHERE id=?")
            .bind(&line_id)
            .execute(&f.pool)
            .await
            .expect_err("a posted line must not be deletable");
        assert!(
            line_delete
                .to_string()
                .contains("purchase_document_is_posted"),
            "the posted-line guard must be what refuses the delete, not something incidental: \
             {line_delete}"
        );

        let deleted = sqlx::query("DELETE FROM purchase_documents WHERE id=?")
            .bind(&id)
            .execute(&f.pool)
            .await
            .expect_err("a posted document must not be deletable");
        assert!(
            deleted.to_string().contains("purchase_document_is_posted"),
            "the posted-header guard must be what refuses the delete: {deleted}"
        );
    }

    #[tokio::test]
    async fn replaying_the_same_posting_key_does_not_double_stock() {
        let f = fixture().await;
        let id = draft_with_line(&f, "IDEM-1").await;
        let key = Uuid::now_v7().to_string();
        let (status, first) = post_draft(&f, &id, 2, &key).await;
        assert_eq!(status, StatusCode::OK, "{first}");

        // The identical replay returns the original document rather than posting again.
        let (status, replay) = post_draft(&f, &id, 2, &key).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["id"], first["id"]);
        assert_eq!(replay["grandTotalPaise"], first["grandTotalPaise"]);

        let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(movements, 1, "a replay must not move stock twice");

        // A different key against an already-posted document is not a replay.
        let (status, conflict) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "purchase_not_draft");
    }

    /// Provenance is a database invariant, not a convention the service happens to follow. A
    /// purchase movement must name the line that caused it, and only a purchase movement may name
    /// one — so a later code path cannot quietly post unattributed stock.
    #[tokio::test]
    async fn a_purchase_movement_must_carry_its_line_and_no_other_movement_may() {
        let f = fixture().await;
        let id = draft_with_line(&f, "PROV-1").await;
        post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        let (line_id, store_id): (String, String) = sqlx::query_as(
            "SELECT l.id,d.store_id FROM purchase_lines l \
             JOIN purchase_documents d ON d.id=l.purchase_document_id \
             WHERE l.purchase_document_id=?",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();

        // What posting actually wrote.
        let recorded: Option<String> = sqlx::query_scalar(
            "SELECT purchase_line_id FROM inventory_movements WHERE movement_type='purchase'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(recorded.as_deref(), Some(line_id.as_str()));

        let insert = |movement_type: &'static str, provenance: Option<String>| {
            let pool = f.pool.clone();
            let store = store_id.clone();
            let product = f.product_id.clone();
            let pack = f.pack_id.clone();
            async move {
                sqlx::query(
                    "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,\
                     movement_type,quantity_delta_atoms,occurred_on,purchase_line_id,\
                     idempotency_key,posted_by_user_id,posted_at_utc) \
                     VALUES (?,?,?,?,?,1,'2026-04-01',?,?,\
                     (SELECT id FROM users LIMIT 1),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(store)
                .bind(product)
                .bind(pack)
                .bind(movement_type)
                .bind(provenance)
                .bind(Uuid::now_v7().to_string())
                .execute(&pool)
                .await
            }
        };

        // A purchase movement with no line behind it.
        let unattributed = insert("purchase", None).await;
        assert!(
            unattributed.is_err(),
            "a purchase movement without its line must be refused"
        );
        // And an adjustment that pretends to come from a purchase.
        let borrowed = insert("adjustment", Some(line_id.clone())).await;
        assert!(
            borrowed.is_err(),
            "only a purchase movement may carry a purchase line"
        );
        // The honest shape still works, so the constraints are not simply rejecting everything.
        let honest = insert("adjustment", None).await;
        assert!(honest.is_ok(), "an ordinary adjustment must still post");
    }

    /// A posted purchase is a historical record. Once it is posted, changing the supplier, the
    /// store's own registration, or the product's tax classification must not alter a single figure
    /// or label on it — otherwise last year's invoice silently restates itself.
    #[tokio::test]
    async fn a_posted_purchase_keeps_its_snapshots_when_the_masters_change() {
        let f = fixture().await;
        let id = draft_with_line(&f, "SNAP-1").await;
        post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        let (_, before) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(before["supplierDisplayName"], "Sharma Medicals");
        assert_eq!(before["taxTreatment"], "intra_state");

        // The supplier is renamed, deregistered, and moved to another State.
        sqlx::query(
            "UPDATE parties SET display_name='Renamed Later',gst_registration_status='unregistered',\
             normalized_gstin=NULL,gstin=NULL,place_of_supply_state_id=NULL WHERE id=?",
        )
        .bind(&f.supplier_id)
        .execute(&f.pool)
        .await
        .unwrap();
        // The store deregisters and moves too.
        sqlx::query(
            "UPDATE store_identity SET gst_registration_status='unregistered',gstin=NULL,\
             normalized_gstin=NULL,place_of_supply_state_id=NULL",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        // And the product is reclassified into a different category with a different rate.
        let other = insert_category_with(&f.pool, "gst-28", "taxable").await;
        insert_rate(&f.pool, &other, "2020-01-01", None, 1400).await;
        sqlx::query("UPDATE products SET tax_category_id=?,hsn_code_id=NULL WHERE id=?")
            .bind(&other)
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();

        let (_, after) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{id}"),
            Value::Null,
        )
        .await;
        for field in [
            "supplierDisplayName",
            "supplierGstRegistrationStatus",
            "supplierNormalizedGstin",
            "supplierPlaceOfSupplyStateId",
            "supplierStateCode",
            "storeGstRegistrationStatus",
            "storeNormalizedGstin",
            "storePlaceOfSupplyStateId",
            "storeStateCode",
            "taxTreatment",
            "taxableValuePaise",
            "cgstPaise",
            "sgstPaise",
            "igstPaise",
            "grandTotalPaise",
        ] {
            assert_eq!(
                after[field], before[field],
                "{field} must be the value recorded at posting, not today's"
            );
        }
        for field in [
            "hsnCodeId",
            "hsnCode",
            "taxCategoryId",
            "taxTreatmentKind",
            "taxRateVersionId",
            "cgstBasisPoints",
            "sgstBasisPoints",
            "igstBasisPoints",
            "cgstPaise",
            "lineTotalPaise",
        ] {
            assert_eq!(
                after["lines"][0][field], before["lines"][0][field],
                "line {field} must be the value recorded at posting"
            );
        }
        // And the list view tells the same story as the detail view.
        let (_, listed) = request(f.pool.clone(), "GET", "/api/v1/purchases", Value::Null).await;
        assert_eq!(listed[0]["supplierDisplayName"], "Sharma Medicals");
    }

    /// Two operators posting the same invoice at the same moment. Whatever the interleaving, the
    /// document may post once and the stock may move once; the loser must be refused, not queued
    /// behind the winner and applied afterwards.
    #[tokio::test]
    async fn two_simultaneous_postings_of_one_draft_move_stock_exactly_once() {
        let f = fixture().await;
        let id = draft_with_line(&f, "RACE-1").await;

        let first = {
            let pool = f.pool.clone();
            let id = id.clone();
            let key = Uuid::now_v7().to_string();
            tokio::spawn(async move {
                request(
                    pool,
                    "POST",
                    &format!("/api/v1/purchases/{id}/post"),
                    json!({ "expectedRevision": 2, "idempotencyKey": key }),
                )
                .await
            })
        };
        let second = {
            let pool = f.pool.clone();
            let id = id.clone();
            let key = Uuid::now_v7().to_string();
            tokio::spawn(async move {
                request(
                    pool,
                    "POST",
                    &format!("/api/v1/purchases/{id}/post"),
                    json!({ "expectedRevision": 2, "idempotencyKey": key }),
                )
                .await
            })
        };
        let (left, right) = (first.await.unwrap(), second.await.unwrap());
        let winners = [&left, &right]
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(
            winners, 1,
            "exactly one posting may win: {left:?} {right:?}"
        );

        let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(movements, 1, "a race must not move the stock twice");
        let posted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM purchase_documents WHERE id=? AND status='posted'",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(posted, 1);
    }

    #[tokio::test]
    async fn a_failed_posting_leaves_no_stock_and_no_orphan_batch() {
        let f = fixture().await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "ROLL-1"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        // A good line proposing a new Batch, then a line whose product has no classification.
        // Resolution happens before any write, so this proves an early refusal writes nothing; the
        // second scenario below is the one that fails half-way through writing.
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "productId": f.product_id,
                "productPackId": f.pack_id,
                "newBatchNumber": "ROLLBACK-LOT",
                "newBatchExpiresOn": "2029-12-31",
                "quantityPacks": 2,
                "ratePerPackPaise": 5000
            }),
        )
        .await;
        let (unclassified, unclassified_pack) =
            insert_product(&f.pool, "Unclassified item", 5).await;
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(2, &unclassified, &unclassified_pack, 1, 100),
        )
        .await;

        let (status, failure) = post_draft(&f, &id, 3, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{failure}");
        assert_eq!(failure["code"], "product_tax_classification_incomplete");

        // Everything the partial posting did is gone.
        let batches: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM product_batches WHERE normalized_batch_number='ROLLBACKLOT'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(batches, 0, "no orphan Batch may survive a failed posting");
        let movements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(movements, 0, "no stock may survive a failed posting");
        let (_, still_draft) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(still_draft["status"], "draft");

        // A failure part-way through writing, which is the case that actually needs the rollback:
        // two lines on one pack propose the same lot, so the second materialisation collides only
        // after the first has already inserted its Batch and its movement.
        let (_, second) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "ROLL-2"),
        )
        .await;
        let collide = second["id"].as_str().unwrap().to_owned();
        for revision in 1..=2 {
            request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/purchases/{collide}/lines"),
                json!({
                    "expectedRevision": revision,
                    "productId": f.product_id,
                    "productPackId": f.pack_id,
                    "newBatchNumber": "COLLIDING-LOT",
                    "quantityPacks": 3,
                    "ratePerPackPaise": 2000
                }),
            )
            .await;
        }
        let (status, refused) = post_draft(&f, &collide, 3, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "batch_conflict");

        // The first line's Batch and movement were written inside the transaction and must be gone.
        let orphans: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM product_batches WHERE normalized_batch_number='COLLIDINGLOT'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(orphans, 0, "a half-written posting must leave no Batch");
        let moved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(moved, 0, "a half-written posting must leave no movement");
        let (_, unposted) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{collide}"),
            Value::Null,
        )
        .await;
        assert_eq!(unposted["status"], "draft");
        assert_eq!(unposted["taxTreatment"], Value::Null);
        assert_eq!(unposted["lines"][0]["batchId"], Value::Null);
    }

    #[tokio::test]
    async fn a_new_batch_is_created_only_at_posting_and_carries_its_lot_facts() {
        let f = fixture().await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "BATCH-1"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "productId": f.product_id,
                "productPackId": f.pack_id,
                "newBatchNumber": " ab-123 ",
                "newBatchExpiresOn": "2029-12-31",
                "newBatchMrpPaise": 12550,
                "quantityPacks": 3,
                "ratePerPackPaise": 9000
            }),
        )
        .await;
        // Nothing exists yet: an abandoned draft would leave no Batch behind.
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_batches")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(before, 0);

        let (status, posted) = post_draft(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let batch: (String, String, Option<i64>) = sqlx::query_as(
            "SELECT batch_number,normalized_batch_number,mrp_paise FROM product_batches",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        // Normalisation comes from the frozen validator, not a second copy of the rules.
        assert_eq!(batch.0, "ab-123");
        assert_eq!(batch.1, "AB-123");
        assert_eq!(batch.2, Some(12550));
        assert!(posted["lines"][0]["batchId"].is_string());
    }

    #[tokio::test]
    async fn a_pack_from_another_product_and_a_batch_from_another_pack_are_refused() {
        let f = fixture().await;
        let (other_product, other_pack) = insert_product(&f.pool, "Other item", 4).await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "MIS-1"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, mismatch) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            line_body(1, &f.product_id, &other_pack, 1, 100),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
        assert_eq!(mismatch["code"], "product_pack_mismatch");

        // A batch belonging to a different pack is refused too.
        let batch_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
             created_at_utc,updated_at_utc) VALUES (?,?,'X-1','X-1',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&batch_id)
        .bind(&other_pack)
        .execute(&f.pool)
        .await
        .unwrap();
        let mut body = line_body(1, &f.product_id, &f.pack_id, 1, 100);
        body["batchId"] = json!(batch_id);
        let (status, wrong_batch) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{wrong_batch}");
        assert_eq!(wrong_batch["code"], "batch_pack_mismatch");
        let _ = other_product;
    }

    // -------------------------------------------------------------------------------------
    // Security
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn the_browser_cannot_dictate_tax_totals_atoms_store_or_actor() {
        let f = fixture().await;
        let (_, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "SPOOF-1"),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_owned();
        // Every one of these is a value the server owns. They must be ignored, not honoured.
        let mut spoofed = line_body(1, &f.product_id, &f.pack_id, 5, 8000);
        spoofed["quantityAtoms"] = json!(999_999);
        spoofed["taxableValuePaise"] = json!(1);
        spoofed["cgstPaise"] = json!(0);
        spoofed["sgstPaise"] = json!(0);
        spoofed["igstPaise"] = json!(0);
        spoofed["lineTotalPaise"] = json!(1);
        spoofed["cgstBasisPoints"] = json!(0);
        spoofed["taxRateVersionId"] = json!(Uuid::now_v7().to_string());
        spoofed["hsnCode"] = json!("00000000");
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            spoofed,
        )
        .await;

        let mut spoofed_post = json!({
            "expectedRevision": 2,
            "idempotencyKey": Uuid::now_v7().to_string(),
        });
        spoofed_post["taxTreatment"] = json!("inter_state");
        spoofed_post["storeStateCode"] = json!("29");
        spoofed_post["grandTotalPaise"] = json!(1);
        spoofed_post["actorId"] = json!(Uuid::now_v7().to_string());
        let (status, posted) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/post"),
            spoofed_post,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        // The server's own figures won, every time.
        assert_eq!(posted["lines"][0]["quantityAtoms"], 50);
        assert_eq!(posted["lines"][0]["taxableValuePaise"], 40000);
        assert_eq!(posted["lines"][0]["cgstBasisPoints"], 600);
        assert_eq!(posted["lines"][0]["hsnCode"], "30049099");
        assert_eq!(
            posted["taxTreatment"], "intra_state",
            "not the spoofed value"
        );
        assert_eq!(posted["storeStateCode"], "27");
        assert_eq!(posted["grandTotalPaise"], 44800);
        assert_eq!(posted["postedByUserId"], f.owner_id, "actor is the session");
    }

    #[tokio::test]
    async fn reads_need_a_session_and_writes_need_owner_admin() {
        let f = fixture().await;
        let (status, anonymous) = request_as(
            f.pool.clone(),
            "GET",
            "/api/v1/purchases",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{anonymous}");

        let (status, readable) = request_as(
            f.pool.clone(),
            "GET",
            "/api/v1/purchases",
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{readable}");

        let (status, denied) = request_as(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "DENY-1"),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert_eq!(denied["code"], "authorization_denied");
        let drafts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM purchase_documents")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(drafts, 0);
    }

    #[tokio::test]
    async fn a_party_without_an_active_supplier_role_cannot_be_purchased_from() {
        let f = fixture().await;
        let customer_only = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,'Not A Supplier','not a supplier',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&customer_only)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&customer_only, "NOSUP-1"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "supplier_not_eligible");
    }

    #[tokio::test]
    async fn purchase_errors_stay_free_of_database_detail() {
        let f = fixture().await;
        draft_with_line(&f, "LEAK-1").await;
        let (_, duplicate) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/purchases",
            draft_body(&f.supplier_id, "LEAK-1"),
        )
        .await;
        let (_, missing) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{}", Uuid::now_v7()),
            Value::Null,
        )
        .await;
        for body in [duplicate, missing] {
            let text = body.to_string().to_ascii_lowercase();
            for leak in [
                "sqlite",
                "constraint failed",
                "insert into",
                "raise(",
                "trigger",
                "_uq",
            ] {
                assert!(!text.contains(leak), "leaked {leak} in {text}");
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // Harness
    // -------------------------------------------------------------------------------------

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
        .bind(format!("{role} purchase user"))
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
    ) -> (StatusCode, Value) {
        request_as(pool, method, uri, body, Some(OWNER)).await
    }

    async fn request_as(
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
}
