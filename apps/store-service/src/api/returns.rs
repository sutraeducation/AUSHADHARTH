//! Phase 1I returns: the correction mechanism for posted commercial documents.
//!
//! A posted Sale or Purchase is never mutated. It is corrected by a compensating Return that
//! references it, reverses a proportion of its frozen figures, and posts its own inventory
//! movements. Three concepts stay separate throughout, because merging them would encode a legal
//! claim the software is not entitled to make:
//!
//! - **commercial return** — what quantity and value of the original is reversed;
//! - **GST evidence** — what tax document the law attaches to that, if any;
//! - **stock disposition** — what physical state the goods enter.
//!
//! On the sales side our Store is the original supplier, so CGST s.34(1) lets *us* issue a credit
//! note when the recipient returns goods. On the purchase side we are the recipient, and s.34(3)
//! gives the debit note to the supplier — so this module never calls our purchase return a debit
//! note. Circular 72/46/2018-GST supplies the two routes it may take instead.

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
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{
        CatalogValidationIssue, optional_text, required_text, validate_date, validate_uuid_v7,
    },
    returns::{self, OriginalLineAmounts, ReturnMoneyError},
    sales,
};

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum ReturnError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    NotDraft,
    Revision {
        expected: i64,
        current: i64,
    },
    OriginalNotFound,
    OriginalNotPosted,
    OriginalLineMismatch,
    /// More was asked for than the original line has left after earlier posted returns.
    OverReturn {
        returnable: i64,
    },
    InsufficientStock {
        available: i64,
    },
    DispositionRequired,
    DispositionNotAllowed,
    /// Releasing quarantined goods to sellable is a pharmacist's decision, never a cashier's.
    DispositionDenied,
    GstRouteRequired,
    GstRouteNotAllowed,
    TaxAdjustmentRequired,
    TaxAdjustmentNotAllowed,
    SupplierCreditNoteRouteConflict,
    DuplicateSupplierCreditNote,
    ArithmeticOverflow,
    IdempotencyConflict,
    /// A replay of the same key with the same facts: the original return is returned.
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
    returnable_atoms: Option<i64>,
    available_atoms: Option<i64>,
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
        expected_revision: None,
        current_revision: None,
        returnable_atoms: None,
        available_atoms: None,
    }
}

impl IntoResponse for ReturnError {
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
                    returnable_atoms: None,
                    available_atoms: None,
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("return_not_found", "The return was not found."),
            ),
            Self::NotDraft => (
                StatusCode::CONFLICT,
                simple(
                    "return_not_draft",
                    "A posted return cannot be changed. Correct it with a later document.",
                ),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The return changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                    returnable_atoms: None,
                    available_atoms: None,
                },
            ),
            Self::OriginalNotFound => (
                StatusCode::NOT_FOUND,
                simple(
                    "original_document_not_found",
                    "The document this return corrects was not found.",
                ),
            ),
            Self::OriginalNotPosted => (
                StatusCode::CONFLICT,
                simple(
                    "original_document_not_posted",
                    "Only a posted document can be returned against.",
                ),
            ),
            Self::OriginalLineMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "original_line_mismatch",
                    "That line does not belong to the document being returned.",
                ),
            ),
            Self::OverReturn { returnable } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "over_return",
                    message: "That is more than is left to return on this line.",
                    issues: Vec::new(),
                    expected_revision: None,
                    current_revision: None,
                    returnable_atoms: Some(returnable),
                    available_atoms: None,
                },
            ),
            Self::InsufficientStock { available } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "insufficient_stock",
                    message: "There is not enough of this batch in stock to return it.",
                    issues: Vec::new(),
                    expected_revision: None,
                    current_revision: None,
                    returnable_atoms: None,
                    available_atoms: Some(available),
                },
            ),
            Self::DispositionRequired => (
                StatusCode::CONFLICT,
                simple(
                    "disposition_required",
                    "Say where the returned goods are being put before posting.",
                ),
            ),
            Self::DispositionNotAllowed => (
                StatusCode::CONFLICT,
                simple(
                    "disposition_not_allowed",
                    "Goods returned to a supplier leave the store, so they have no disposition.",
                ),
            ),
            Self::DispositionDenied => (
                StatusCode::FORBIDDEN,
                simple(
                    "disposition_denied",
                    "Only a pharmacist or the owner may release quarantined stock for sale.",
                ),
            ),
            Self::GstRouteRequired => (
                StatusCode::CONFLICT,
                simple(
                    "gst_route_required",
                    "Choose how this purchase return is being documented for GST.",
                ),
            ),
            Self::GstRouteNotAllowed => (
                StatusCode::CONFLICT,
                simple(
                    "gst_route_not_allowed",
                    "A GST route belongs to a purchase return, not a sales return.",
                ),
            ),
            Self::TaxAdjustmentRequired => (
                StatusCode::CONFLICT,
                simple(
                    "tax_adjustment_status_required",
                    "Record whether this credit note adjusts tax or is commercial only.",
                ),
            ),
            Self::TaxAdjustmentNotAllowed => (
                StatusCode::CONFLICT,
                simple(
                    "tax_adjustment_status_not_allowed",
                    "A tax-adjustment status belongs to a sales return, not a purchase return.",
                ),
            ),
            Self::SupplierCreditNoteRouteConflict => (
                StatusCode::CONFLICT,
                simple(
                    "supplier_credit_note_route_conflict",
                    "A supplier credit note can only be recorded against a posted purchase return \
                     that was returned under the supplier-credit-note route.",
                ),
            ),
            Self::DuplicateSupplierCreditNote => (
                StatusCode::CONFLICT,
                simple(
                    "duplicate_supplier_credit_note",
                    "That supplier credit note is already recorded against this return.",
                ),
            ),
            Self::ArithmeticOverflow => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(
                    "arithmetic_overflow",
                    "The amounts on this return are too large to record.",
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
                simple("posting_conflict", "This return was already posted."),
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

impl From<AuthError> for ReturnError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

impl From<ReturnMoneyError> for ReturnError {
    fn from(value: ReturnMoneyError) -> Self {
        match value {
            ReturnMoneyError::Overflow => Self::ArithmeticOverflow,
            ReturnMoneyError::InvalidReturnQuantity => {
                validation_of("quantity", "is not a usable return quantity")
            }
            ReturnMoneyError::InvalidOriginalQuantity | ReturnMoneyError::InvalidOriginalAmount => {
                Self::Internal
            }
        }
    }
}

fn validation_of(field: &str, message: &str) -> ReturnError {
    ReturnError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> ReturnError {
    ReturnError::Validation(vec![issue])
}

fn map_database_error(error: sqlx::Error) -> ReturnError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return ReturnError::ServiceBusy;
        }
        if message.contains("return_document_is_posted") {
            return ReturnError::NotDraft;
        }
        if message.contains("original_document_not_posted") {
            return ReturnError::OriginalNotPosted;
        }
        if message.contains("return_line_kind_conflict") {
            return ReturnError::OriginalLineMismatch;
        }
        if message.contains("supplier_credit_note_route_conflict") {
            return ReturnError::SupplierCreditNoteRouteConflict;
        }
        if message.contains("inventory_movement_conflict") {
            return ReturnError::OriginalLineMismatch;
        }
        // SQLite names the offending COLUMNS in a unique violation, never the index — the Phase 1G
        // lesson, which cost a release to learn.
        if message.contains("return_documents.posting_idempotency_key") {
            return ReturnError::IdempotencyConflict;
        }
        if message.contains("supplier_credit_note_evidence.normalized_credit_note_number") {
            return ReturnError::DuplicateSupplierCreditNote;
        }
        if message.contains("return_documents.document_number")
            || message.contains("return_documents.sequence_value")
        {
            return ReturnError::ServiceBusy;
        }
        if message.contains("foreign key constraint failed") {
            return ReturnError::OriginalLineMismatch;
        }
    }
    ReturnError::Internal
}

// ---------------------------------------------------------------------------------------------
// Requests and responses
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateReturnRequest {
    return_kind: String,
    original_document_id: String,
    business_date: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateReturnRequest {
    expected_revision: i64,
    business_date: String,
}

/// The only line facts a browser may supply.
///
/// Everything else — product, pack, batch, basis, rate, tax, identity — is copied from the original
/// line, because a reversal that re-resolved anything would stop being equal and opposite.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReturnLineRequest {
    expected_revision: i64,
    original_line_id: String,
    /// Read in the original line's own basis: whole packs for a pack line, atoms for a loose one.
    quantity: i64,
    /// Sales returns only. Never `sellable`: the contract cannot express it.
    disposition: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveReturnLineRequest {
    expected_revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostReturnRequest {
    expected_revision: i64,
    idempotency_key: String,
    /// Sales returns only.
    tax_adjustment_status: Option<String>,
    tax_adjustment_reason: Option<String>,
    /// Purchase returns only.
    gst_route: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListReturnsQuery {
    return_kind: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct ReturnHeaderResponse {
    id: String,
    store_id: String,
    return_kind: String,
    original_sale_document_id: Option<String>,
    original_purchase_document_id: Option<String>,
    original_document_number: Option<String>,
    original_document_date: Option<String>,
    business_date: String,
    status: String,
    revision: i64,
    series_code: Option<String>,
    financial_year: Option<String>,
    sequence_value: Option<i64>,
    document_number: Option<String>,
    store_gst_registration_status: Option<String>,
    store_normalized_gstin: Option<String>,
    store_place_of_supply_state_id: Option<String>,
    store_state_code: Option<String>,
    counterparty_party_id: Option<String>,
    counterparty_display_name: Option<String>,
    counterparty_gst_registration_status: Option<String>,
    counterparty_normalized_gstin: Option<String>,
    counterparty_state_code: Option<String>,
    tax_treatment: Option<String>,
    tax_adjustment_status: Option<String>,
    tax_adjustment_reason: Option<String>,
    gst_route: Option<String>,
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
struct ReturnLineResponse {
    id: String,
    return_document_id: String,
    line_number: i64,
    original_sale_line_id: Option<String>,
    original_purchase_line_id: Option<String>,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    disposition: Option<String>,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct SupplierCreditNoteResponse {
    id: String,
    return_document_id: String,
    credit_note_number: String,
    credit_note_date: String,
    credit_note_amount_paise: i64,
    recorded_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReturnDetailResponse {
    #[serde(flatten)]
    document: ReturnHeaderResponse,
    lines: Vec<ReturnLineResponse>,
    supplier_credit_notes: Vec<SupplierCreditNoteResponse>,
}

#[derive(Debug, FromRow)]
struct SaleReturnableHeader {
    status: String,
    document_number: Option<String>,
    business_date: String,
    customer_display_name: Option<String>,
    customer_name_text: Option<String>,
}

#[derive(Debug, FromRow)]
struct PurchaseReturnableHeader {
    status: String,
    supplier_invoice_number: String,
    invoice_date: String,
    supplier_display_name: Option<String>,
}

/// One original line with what is left to return on it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReturnableLineResponse {
    original_line_id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    quantity_basis: String,
    /// The original quantity in its own basis: packs for a pack line, atoms for a loose one.
    original_quantity: i64,
    original_quantity_atoms: i64,
    already_returned_atoms: i64,
    returnable_atoms: i64,
    /// The same figure in the basis the operator will type, so the screen never asks for atoms.
    returnable_quantity: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReturnableDocumentResponse {
    document_id: String,
    document_number: Option<String>,
    document_date: String,
    counterparty_display_name: Option<String>,
    lines: Vec<ReturnableLineResponse>,
}

const RETURN_HEADER_COLUMNS: &str = "id,store_id,return_kind,original_sale_document_id,\
     original_purchase_document_id,original_document_number,original_document_date,business_date,\
     status,revision,series_code,financial_year,sequence_value,document_number,\
     store_gst_registration_status,store_normalized_gstin,store_place_of_supply_state_id,\
     store_state_code,counterparty_party_id,counterparty_display_name,\
     counterparty_gst_registration_status,counterparty_normalized_gstin,counterparty_state_code,\
     tax_treatment,tax_adjustment_status,tax_adjustment_reason,gst_route,taxable_value_paise,\
     cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,created_by_user_id,\
     created_at_utc,updated_at_utc,posted_by_user_id,posted_at_utc";

const RETURN_LINE_COLUMNS: &str = "id,return_document_id,line_number,original_sale_line_id,\
     original_purchase_line_id,product_id,product_pack_id,batch_id,quantity_basis,quantity_packs,\
     quantity_atoms,disposition,product_display_name,pack_display_label,base_unit_label,\
     batch_number,batch_expires_on,hsn_code_id,hsn_code,tax_category_id,tax_treatment_kind,\
     tax_rate_version_id,cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
     taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise,line_total_paise";

/// The series each kind is numbered in. Both render to fourteen characters through the Phase 1H-C1
/// formatter, inside the statutory sixteen.
const SALES_RETURN_SERIES: &str = "SR";
const PURCHASE_RETURN_SERIES: &str = "PR";

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route(
            "/api/v1/sales/{id}/returnable-lines",
            get(sale_returnable_lines),
        )
        .route(
            "/api/v1/purchases/{id}/returnable-lines",
            get(purchase_returnable_lines),
        )
        .route("/api/v1/returns", get(list_returns).post(create_return))
        .route("/api/v1/returns/{id}", get(get_return).put(update_return))
        .route("/api/v1/returns/{id}/lines", post(add_return_line))
        .route("/api/v1/returns/{id}/quote", get(quote_return))
        .route("/api/v1/returns/{id}/post", post(post_return))
        .route(
            "/api/v1/returns/{id}/supplier-credit-notes",
            post(record_supplier_credit_note),
        )
        .route(
            "/api/v1/return-lines/{id}",
            delete(remove_return_line).put(update_return_line),
        )
        .route("/api/v1/stock-dispositions", post(create_stock_disposition))
}

// ---------------------------------------------------------------------------------------------
// Authorisation
//
// Deliberately not copied from the POS. Taking goods back over the counter is counter work, but
// deciding that a returned medicine may be sold to the next customer is a pharmacist's judgement,
// and sending goods back to a supplier is the owner's commercial decision.
// ---------------------------------------------------------------------------------------------

async fn require_counter(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ReturnError> {
    auth::validate_mutation_request(headers)?;
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

async fn require_pharmacist(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ReturnError> {
    let actor = require_counter(state, headers).await?;
    if !matches!(actor.role.as_str(), "owner_admin" | "pharmacist") {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn require_owner(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ReturnError> {
    let actor = require_counter(state, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ReturnError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// The Store is resolved from the installation, never accepted from the browser.
async fn current_store(pool: &SqlitePool) -> Result<String, ReturnError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(ReturnError::Internal)
}

// ---------------------------------------------------------------------------------------------
// Returnable lines
// ---------------------------------------------------------------------------------------------

/// One original line and what is left of it.
#[derive(Debug, FromRow)]
struct OriginalLine {
    id: String,
    line_number: i64,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    line_total_paise: i64,
}

/// A sale line already snapshots its own identity, so it is read as posted — with one deliberate
/// exception. The **expiry is read live from the lot**, not from the snapshot, because it is what
/// decides whether returned goods could ever go back on the shelf. A lot that expired between the
/// sale and the return is expired now, and a screen still showing the old date would offer
/// quarantine for a medicine that can never be sold again. The posted return line keeps the
/// original's own snapshot; this is the working view, not the record.
const SALE_ORIGINAL_LINE_QUERY: &str = "SELECT line.id,line.line_number,line.product_id,\
     line.product_pack_id,line.batch_id,line.quantity_basis,line.quantity_packs,line.quantity_atoms,\
     line.product_display_name,line.pack_display_label,line.base_unit_label,line.batch_number,\
     batch.expires_on AS batch_expires_on,line.hsn_code_id,line.hsn_code,line.tax_category_id,\
     line.tax_treatment_kind,line.tax_rate_version_id,line.cgst_basis_points,line.sgst_basis_points,\
     line.igst_basis_points,line.cess_basis_points,line.taxable_value_paise,line.cgst_paise,\
     line.sgst_paise,line.igst_paise,line.cess_paise,line.line_total_paise \
     FROM sale_lines line \
     LEFT JOIN product_batches batch ON batch.id = line.batch_id \
     WHERE line.sale_document_id=? ORDER BY line.line_number";

/// A purchase line snapshots no identity — Phase 1G never added one — and is always whole packs, so
/// the names are joined live from the catalogue and the basis is fixed.
const PURCHASE_ORIGINAL_LINE_QUERY: &str = "SELECT line.id,line.line_number,line.product_id,\
     line.product_pack_id,line.batch_id,'pack' AS quantity_basis,line.quantity_packs,\
     line.quantity_atoms,product.display_name AS product_display_name,\
     pack.display_label AS pack_display_label,unit.display_name AS base_unit_label,\
     batch.batch_number,batch.expires_on AS batch_expires_on,line.hsn_code_id,line.hsn_code,\
     line.tax_category_id,line.tax_treatment_kind,line.tax_rate_version_id,line.cgst_basis_points,\
     line.sgst_basis_points,line.igst_basis_points,line.cess_basis_points,line.taxable_value_paise,\
     line.cgst_paise,line.sgst_paise,line.igst_paise,line.cess_paise,line.line_total_paise \
     FROM purchase_lines line \
     LEFT JOIN products product ON product.id = line.product_id \
     LEFT JOIN product_packs pack ON pack.id = line.product_pack_id \
     LEFT JOIN units_of_measure unit ON unit.id = product.base_unit_id \
     LEFT JOIN product_batches batch ON batch.id = line.batch_id \
     WHERE line.purchase_document_id=? AND line.batch_id IS NOT NULL \
     ORDER BY line.line_number";

async fn sale_returnable_lines(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ReturnableDocumentResponse>, ReturnError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let header = sqlx::query_as::<_, SaleReturnableHeader>(
        "SELECT status,document_number,business_date,customer_display_name,customer_name_text \
         FROM sale_documents WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::OriginalNotFound)?;
    if header.status != "posted" {
        return Err(ReturnError::OriginalNotPosted);
    }
    let lines = load_original_lines(&state.pool, SALE_ORIGINAL_LINE_QUERY, &id).await?;
    let returnable = returnable_lines(&state.pool, &lines, true).await?;
    Ok(Json(ReturnableDocumentResponse {
        document_id: id,
        document_number: header.document_number,
        document_date: header.business_date,
        // A walk-in has no party, but may have asked for a name on the bill.
        counterparty_display_name: header.customer_display_name.or(header.customer_name_text),
        lines: returnable,
    }))
}

async fn purchase_returnable_lines(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ReturnableDocumentResponse>, ReturnError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let header = sqlx::query_as::<_, PurchaseReturnableHeader>(
        "SELECT status,supplier_invoice_number,invoice_date,supplier_display_name \
         FROM purchase_documents WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::OriginalNotFound)?;
    if header.status != "posted" {
        return Err(ReturnError::OriginalNotPosted);
    }
    let lines = load_original_lines(&state.pool, PURCHASE_ORIGINAL_LINE_QUERY, &id).await?;
    let returnable = returnable_lines(&state.pool, &lines, false).await?;
    Ok(Json(ReturnableDocumentResponse {
        document_id: id,
        document_number: Some(header.supplier_invoice_number),
        document_date: header.invoice_date,
        counterparty_display_name: header.supplier_display_name,
        lines: returnable,
    }))
}

async fn load_original_lines(
    pool: &SqlitePool,
    query: &str,
    document_id: &str,
) -> Result<Vec<OriginalLine>, ReturnError> {
    sqlx::query_as::<_, OriginalLine>(query)
        .bind(document_id)
        .fetch_all(pool)
        .await
        .map_err(map_database_error)
}

/// How many atoms of one original line have already come back on **posted** returns.
///
/// Drafts reserve nothing. Two operators may both draft a return for the last strip; the posting
/// transaction decides which of them gets it.
async fn already_returned_atoms<'e, E>(
    executor: E,
    original_line_id: &str,
    is_sale: bool,
) -> Result<i64, ReturnError>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let column = if is_sale {
        "original_sale_line_id"
    } else {
        "original_purchase_line_id"
    };
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(line.quantity_atoms),0) FROM return_lines line \
         JOIN return_documents document ON document.id = line.return_document_id \
         WHERE line.{column}=? AND document.status='posted'"
    ))
    .bind(original_line_id)
    .fetch_one(executor)
    .await
    .map_err(map_database_error)
}

/// What this draft already proposes to return against one original line.
///
/// Sibling lines on the same draft are as real as posted ones for the purpose of deciding what is
/// left: three strips spread over two lines is still three strips. `replacing_line_id` drops the
/// line being edited, so changing a line from two strips to one is not read as asking for three.
async fn pending_in_draft(
    pool: &SqlitePool,
    document_id: &str,
    original_line_id: &str,
    is_sale: bool,
    replacing_line_id: Option<&str>,
) -> Result<i64, ReturnError> {
    let column = if is_sale {
        "original_sale_line_id"
    } else {
        "original_purchase_line_id"
    };
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(line.quantity_atoms),0) FROM return_lines line \
         JOIN return_documents document ON document.id = line.return_document_id \
         WHERE line.return_document_id=? AND document.status='draft' \
         AND line.{column}=? AND line.id <> COALESCE(?, '')"
    ))
    .bind(document_id)
    .bind(original_line_id)
    .bind(replacing_line_id)
    .fetch_one(pool)
    .await
    .map_err(map_database_error)
}

async fn returnable_lines(
    pool: &SqlitePool,
    lines: &[OriginalLine],
    is_sale: bool,
) -> Result<Vec<ReturnableLineResponse>, ReturnError> {
    let mut rows = Vec::with_capacity(lines.len());
    for line in lines {
        let already = already_returned_atoms(pool, &line.id, is_sale).await?;
        let returnable = returns::returnable_atoms(line.quantity_atoms, already)?;
        // The operator types in the basis the original was billed in, never in atoms.
        let per_unit = atoms_per_unit(line)?;
        rows.push(ReturnableLineResponse {
            original_line_id: line.id.clone(),
            line_number: line.line_number,
            product_id: line.product_id.clone(),
            product_pack_id: line.product_pack_id.clone(),
            batch_id: line.batch_id.clone(),
            product_display_name: line.product_display_name.clone(),
            pack_display_label: line.pack_display_label.clone(),
            base_unit_label: line.base_unit_label.clone(),
            batch_number: line.batch_number.clone(),
            batch_expires_on: line.batch_expires_on.clone(),
            quantity_basis: line.quantity_basis.clone(),
            original_quantity: line.quantity_packs.unwrap_or(line.quantity_atoms),
            original_quantity_atoms: line.quantity_atoms,
            already_returned_atoms: already,
            returnable_atoms: returnable,
            returnable_quantity: returnable / per_unit,
            taxable_value_paise: line.taxable_value_paise,
            cgst_paise: line.cgst_paise,
            sgst_paise: line.sgst_paise,
            igst_paise: line.igst_paise,
            cess_paise: line.cess_paise,
            line_total_paise: line.line_total_paise,
        });
    }
    Ok(rows)
}

/// How many atoms one unit of the original line's basis is worth.
///
/// A pack line is returned in whole packs, so a unit is the pack's atoms; a loose line is returned
/// in atoms, so a unit is one atom. Deriving this from the line itself avoids re-reading the pack
/// and therefore avoids the possibility of reading a *different* pack size than was billed.
fn atoms_per_unit(line: &OriginalLine) -> Result<i64, ReturnError> {
    match line.quantity_basis.as_str() {
        "pack" => {
            let packs = line.quantity_packs.ok_or(ReturnError::Internal)?;
            if packs < 1 || line.quantity_atoms % packs != 0 {
                return Err(ReturnError::Internal);
            }
            Ok(line.quantity_atoms / packs)
        }
        "base_unit" => Ok(1),
        _ => Err(ReturnError::Internal),
    }
}

// ---------------------------------------------------------------------------------------------
// Draft handlers
// ---------------------------------------------------------------------------------------------

async fn list_returns(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListReturnsQuery>,
) -> Result<Json<Vec<ReturnHeaderResponse>>, ReturnError> {
    require_reader(&state, &headers).await?;
    let kind = query.return_kind.unwrap_or_else(|| "all".to_owned());
    if !matches!(kind.as_str(), "sales_return" | "purchase_return" | "all") {
        return Err(validation_of(
            "returnKind",
            "must be sales_return, purchase_return, or all",
        ));
    }
    let status = query.status.unwrap_or_else(|| "all".to_owned());
    if !matches!(status.as_str(), "draft" | "posted" | "all") {
        return Err(validation_of("status", "must be draft, posted, or all"));
    }
    let rows = sqlx::query_as::<_, ReturnHeaderResponse>(&format!(
        "SELECT {RETURN_HEADER_COLUMNS} FROM return_documents \
         WHERE (?1 IS NULL OR return_kind=?1) AND (?2 IS NULL OR status=?2) \
         ORDER BY business_date DESC, created_at_utc DESC LIMIT 200"
    ))
    .bind((kind != "all").then_some(kind))
    .bind((status != "all").then_some(status))
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_return(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ReturnDetailResponse>, ReturnError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

/// Starts a return against a posted original.
///
/// The kind decides which original column is filled and therefore which foreign key the database
/// enforces; the counterparty is taken from the original, never from the browser.
async fn create_return(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreateReturnRequest>,
) -> Result<(StatusCode, Json<ReturnDetailResponse>), ReturnError> {
    let actor = require_counter(&state, &headers).await?;
    let kind = request.return_kind.trim();
    if !matches!(kind, "sales_return" | "purchase_return") {
        return Err(validation_of(
            "returnKind",
            "must be sales_return or purchase_return",
        ));
    }
    validate_uuid_v7(&request.original_document_id, "originalDocumentId")
        .map_err(validation_issue)?;
    let business_date = validate_date(Some(&request.business_date), "businessDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("businessDate", "is required"))?;
    let store_id = current_store(&state.pool).await?;
    let original = load_original_document(&state.pool, kind, &request.original_document_id).await?;

    let id = Uuid::now_v7().to_string();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "INSERT INTO return_documents (id,store_id,return_kind,original_sale_document_id,\
         original_purchase_document_id,original_document_number,original_document_date,\
         business_date,counterparty_party_id,created_by_user_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(kind)
    .bind((kind == "sales_return").then(|| original.id.clone()))
    .bind((kind == "purchase_return").then(|| original.id.clone()))
    .bind(&original.document_number)
    .bind(&original.document_date)
    .bind(&business_date)
    .bind(&original.counterparty_party_id)
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
        &json!({ "returnKind": kind, "originalDocumentId": original.id }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

async fn update_return(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateReturnRequest>,
) -> Result<Json<ReturnDetailResponse>, ReturnError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let business_date = validate_date(Some(&request.business_date), "businessDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("businessDate", "is required"))?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE return_documents SET business_date=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(&business_date)
    .bind(current.0 + 1)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        &id,
        current.0 + 1,
        "updated",
        &json!({ "businessDate": business_date }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

/// A line prepared from a request: the operator's quantity, and everything else copied from the
/// original line it reverses.
struct PreparedReturnLine {
    original_line_id: String,
    is_sale: bool,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    disposition: Option<String>,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    hsn_code_id: Option<String>,
    hsn_code: Option<String>,
    tax_category_id: Option<String>,
    tax_treatment_kind: Option<String>,
    tax_rate_version_id: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    amounts: returns::ReversedAmounts,
}

async fn prepare_return_line(
    pool: &SqlitePool,
    document_id: &str,
    document: &ReturnContext,
    request: &ReturnLineRequest,
    replacing_line_id: Option<&str>,
) -> Result<PreparedReturnLine, ReturnError> {
    validate_uuid_v7(&request.original_line_id, "originalLineId").map_err(validation_issue)?;
    let is_sale = document.return_kind == "sales_return";
    let query = if is_sale {
        SALE_ORIGINAL_LINE_QUERY
    } else {
        PURCHASE_ORIGINAL_LINE_QUERY
    };
    // Reading every line of the original and picking one proves the line belongs to that document,
    // rather than trusting an identifier the browser supplied.
    let original = load_original_lines(pool, query, &document.original_document_id)
        .await?
        .into_iter()
        .find(|line| line.id == request.original_line_id)
        .ok_or(ReturnError::OriginalLineMismatch)?;

    let per_unit = atoms_per_unit(&original)?;
    if request.quantity < 1 {
        return Err(validation_of("quantity", "must be at least one"));
    }
    let quantity_atoms = request
        .quantity
        .checked_mul(per_unit)
        .ok_or(ReturnError::ArithmeticOverflow)?;

    // A friendly refusal here; the authoritative document-wide check runs inside the posting
    // transaction, where nothing can change underneath it.
    //
    // It counts what this draft already proposes as well as what has actually been returned. The
    // posting would refuse either way, but a draft that silently accumulates more than the
    // original ever held and only says so at the till is the worst possible moment to find out.
    let already = already_returned_atoms(pool, &original.id, is_sale).await?;
    let pending =
        pending_in_draft(pool, document_id, &original.id, is_sale, replacing_line_id).await?;
    let returnable =
        (returns::returnable_atoms(original.quantity_atoms, already)? - pending).max(0);
    if quantity_atoms > returnable {
        return Err(ReturnError::OverReturn { returnable });
    }

    let current_expiry: Option<String> =
        sqlx::query_scalar("SELECT expires_on FROM product_batches WHERE id=?")
            .bind(&original.batch_id)
            .fetch_optional(pool)
            .await
            .map_err(map_database_error)?
            .flatten();
    let disposition = prepare_disposition(
        is_sale,
        request.disposition.as_deref(),
        current_expiry.as_deref(),
        document,
    )?;

    let amounts = returns::reverse_line(
        &OriginalLineAmounts {
            quantity_atoms: original.quantity_atoms,
            taxable_value_paise: original.taxable_value_paise,
            cgst_paise: original.cgst_paise,
            sgst_paise: original.sgst_paise,
            igst_paise: original.igst_paise,
            cess_paise: original.cess_paise,
        },
        already,
        quantity_atoms,
    )?;

    Ok(PreparedReturnLine {
        original_line_id: original.id.clone(),
        is_sale,
        product_id: original.product_id.clone(),
        product_pack_id: original.product_pack_id.clone(),
        batch_id: original.batch_id.clone(),
        quantity_basis: original.quantity_basis.clone(),
        quantity_packs: (original.quantity_basis == "pack").then_some(request.quantity),
        quantity_atoms,
        disposition,
        product_display_name: original.product_display_name.clone(),
        pack_display_label: original.pack_display_label.clone(),
        base_unit_label: original.base_unit_label.clone(),
        batch_number: original.batch_number.clone(),
        batch_expires_on: original.batch_expires_on.clone(),
        hsn_code_id: original.hsn_code_id.clone(),
        hsn_code: original.hsn_code.clone(),
        tax_category_id: original.tax_category_id.clone(),
        tax_treatment_kind: original.tax_treatment_kind.clone(),
        tax_rate_version_id: original.tax_rate_version_id.clone(),
        cgst_basis_points: original.cgst_basis_points,
        sgst_basis_points: original.sgst_basis_points,
        igst_basis_points: original.igst_basis_points,
        cess_basis_points: original.cess_basis_points,
        amounts,
    })
}

/// Where the goods go, and what the operator is allowed to say about it.
///
/// A sales return must state a disposition and can never state `sellable`: Indian drug law gives no
/// rule permitting a retailer to resell a medicine a customer brought back, and the nearest
/// principle it does give — Schedule M, for manufacturers — destroys market returns unless quality
/// is positively assessed. So goods land in quarantine or are written off, and only a pharmacist
/// may later release them. A purchase return states nothing, because the goods leave the store.
fn prepare_disposition(
    is_sale: bool,
    requested: Option<&str>,
    current_expiry: Option<&str>,
    document: &ReturnContext,
) -> Result<Option<String>, ReturnError> {
    if !is_sale {
        if requested.is_some() {
            return Err(ReturnError::DispositionNotAllowed);
        }
        return Ok(None);
    }
    let disposition = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ReturnError::DispositionRequired)?;
    if !matches!(disposition, "quarantined" | "non_sellable") {
        return Err(validation_of(
            "disposition",
            "must be quarantined or non_sellable",
        ));
    }
    // Rule 110 of the Drugs Rules forbids selling a Schedule C substance after its expiry date, and
    // no lot that is already expired can become sellable again by any later assessment. Quarantine
    // would imply it might, so an expired lot is written off outright.
    //
    // The expiry read here is the lot's as it stands TODAY, not the one frozen onto the original
    // line: a lot that expired between the sale and the return is expired now, which is what
    // decides whether it could ever go back on the shelf.
    if current_expiry.is_some_and(|expiry| expiry < document.business_date.as_str())
        && disposition != "non_sellable"
    {
        return Err(validation_of(
            "disposition",
            "this batch has expired, so it can only be recorded as non-sellable",
        ));
    }
    Ok(Some(disposition.to_owned()))
}

async fn add_return_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ReturnLineRequest>,
) -> Result<(StatusCode, Json<ReturnDetailResponse>), ReturnError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let context = load_return_context(&state.pool, &id).await?;
    let prepared = prepare_return_line(&state.pool, &id, &context, &request, None).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let current = draft_state(&mut transaction, &id).await?;
    require_revision(&current, request.expected_revision)?;
    let next_line: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(line_number),0)+1 FROM return_lines WHERE return_document_id=?",
    )
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let line_id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_return_line(&mut transaction, &line_id, &id, next_line, &prepared, &now).await?;
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

async fn update_return_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<ReturnLineRequest>,
) -> Result<Json<ReturnDetailResponse>, ReturnError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let document_id = line_document(&state.pool, &line_id).await?;
    let context = load_return_context(&state.pool, &document_id).await?;
    let prepared = prepare_return_line(
        &state.pool,
        &document_id,
        &context,
        &request,
        Some(&line_id),
    )
    .await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE return_lines SET original_sale_line_id=?,original_purchase_line_id=?,product_id=?,\
         product_pack_id=?,batch_id=?,quantity_basis=?,quantity_packs=?,quantity_atoms=?,\
         disposition=?,taxable_value_paise=?,cgst_paise=?,sgst_paise=?,igst_paise=?,cess_paise=?,\
         line_total_paise=?,updated_at_utc=? WHERE id=?",
    )
    .bind(prepared.is_sale.then(|| prepared.original_line_id.clone()))
    .bind((!prepared.is_sale).then(|| prepared.original_line_id.clone()))
    .bind(&prepared.product_id)
    .bind(&prepared.product_pack_id)
    .bind(&prepared.batch_id)
    .bind(&prepared.quantity_basis)
    .bind(prepared.quantity_packs)
    .bind(prepared.quantity_atoms)
    .bind(&prepared.disposition)
    .bind(prepared.amounts.taxable_value_paise)
    .bind(prepared.amounts.cgst_paise)
    .bind(prepared.amounts.sgst_paise)
    .bind(prepared.amounts.igst_paise)
    .bind(prepared.amounts.cess_paise)
    .bind(prepared.amounts.line_total_paise)
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

async fn remove_return_line(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(line_id): Path<String>,
    Json(request): Json<RemoveReturnLineRequest>,
) -> Result<Json<ReturnDetailResponse>, ReturnError> {
    let actor = require_counter(&state, &headers).await?;
    validate_uuid_v7(&line_id, "id").map_err(validation_issue)?;
    let document_id = line_document(&state.pool, &line_id).await?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let current = draft_state(&mut transaction, &document_id).await?;
    require_revision(&current, request.expected_revision)?;
    sqlx::query("DELETE FROM return_lines WHERE id=?")
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

// ---------------------------------------------------------------------------------------------
// Quote
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReturnQuoteResponse {
    return_document_id: String,
    revision: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
}

/// What this return comes to, so the counter can say the amount before handing money back.
///
/// A quote resolves nothing and freezes nothing: no number, no snapshot, no movement. It sums the
/// amounts the draft lines already carry, which were computed by the same domain function posting
/// will use.
async fn quote_return(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ReturnQuoteResponse>, ReturnError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM return_documents WHERE id=?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    let revision = revision.ok_or(ReturnError::NotFound)?;
    let totals = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
        "SELECT COALESCE(SUM(taxable_value_paise),0),COALESCE(SUM(cgst_paise),0),\
         COALESCE(SUM(sgst_paise),0),COALESCE(SUM(igst_paise),0),COALESCE(SUM(cess_paise),0),\
         COALESCE(SUM(line_total_paise),0) FROM return_lines WHERE return_document_id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(ReturnQuoteResponse {
        return_document_id: id,
        revision,
        taxable_value_paise: totals.0,
        cgst_paise: totals.1,
        sgst_paise: totals.2,
        igst_paise: totals.3,
        cess_paise: totals.4,
        grand_total_paise: totals.5,
    }))
}

// ---------------------------------------------------------------------------------------------
// Posting
// ---------------------------------------------------------------------------------------------

async fn post_return(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PostReturnRequest>,
) -> Result<Json<ReturnDetailResponse>, ReturnError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;
    // The kind decides who may post: a sales return is a tax document and a disposition decision,
    // which is a pharmacist's; sending goods back to a supplier is the owner's commercial call.
    let kind: Option<String> =
        sqlx::query_scalar("SELECT return_kind FROM return_documents WHERE id=?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    let actor = match kind.as_deref().ok_or(ReturnError::NotFound)? {
        "sales_return" => require_pharmacist(&state, &headers).await?,
        _ => require_owner(&state, &headers).await?,
    };

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| ReturnError::Internal)?;
    // The frozen inventory concurrency model: take the write lock before reading any balance or any
    // returnable quantity, so two returns competing for the last strip cannot interleave.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let outcome =
        post_within_transaction(&mut connection, &id, &request, &idempotency_key, &actor.id).await;
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
            // Any failure discards the whole posting: no number, no movement, no snapshot.
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            drop(connection);
            if matches!(error, ReturnError::AlreadyPostedReplay) {
                return Ok(Json(fetch_detail(&state.pool, &id).await?));
            }
            Err(error)
        }
    }
}

async fn post_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    request: &PostReturnRequest,
    idempotency_key: &str,
    actor_id: &str,
) -> Result<(), ReturnError> {
    let header = sqlx::query_as::<_, PostingHeader>(
        "SELECT status,revision,store_id,return_kind,original_sale_document_id,\
         original_purchase_document_id,business_date,counterparty_party_id,\
         posting_idempotency_key,posting_fingerprint FROM return_documents WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::NotFound)?;

    let is_sale = header.return_kind == "sales_return";
    let original_document_id = header
        .original_sale_document_id
        .clone()
        .or_else(|| header.original_purchase_document_id.clone())
        .ok_or(ReturnError::Internal)?;

    let lines = load_draft_lines(connection, id).await?;
    let gst = prepare_gst_decision(is_sale, request)?;
    let fingerprint = posting_fingerprint(
        id,
        &original_document_id,
        &header.business_date,
        &lines,
        &gst,
    );

    // A replay of the same key with the same facts returns the original return; the same key with
    // different facts is a conflict rather than a silent second reversal.
    if header.status == "posted" {
        return match (
            header.posting_idempotency_key.as_deref(),
            header.posting_fingerprint.as_deref(),
        ) {
            (Some(key), Some(print)) if key == idempotency_key && print == fingerprint => {
                Err(ReturnError::AlreadyPostedReplay)
            }
            (Some(key), _) if key == idempotency_key => Err(ReturnError::IdempotencyConflict),
            _ => Err(ReturnError::NotDraft),
        };
    }
    if header.revision != request.expected_revision {
        return Err(ReturnError::Revision {
            expected: request.expected_revision,
            current: header.revision,
        });
    }
    if lines.is_empty() {
        return Err(validation_of("lines", "a return needs at least one line"));
    }

    // The original is re-read now, under the write lock, and its tax treatment is reused rather
    // than recomputed: a reversal must be equal and opposite even if a State code changed since.
    let original = load_original_for_posting(connection, is_sale, &original_document_id).await?;

    let store = sqlx::query_as::<_, StoreTaxSource>(
        "SELECT normalized_gstin,place_of_supply_state_id,gst_registration_status \
         FROM store_identity WHERE store_id=?",
    )
    .bind(&header.store_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::Internal)?;

    let counterparty = match header.counterparty_party_id.as_deref() {
        Some(party_id) => Some(load_counterparty(connection, party_id).await?),
        None => None,
    };

    // ---- Document-wide anti-over-return ---------------------------------------------------
    //
    // Aggregated per original line, so two draft lines pointing at one original cannot each pass a
    // check the pair of them fails. Evaluated here, under the write lock, so a concurrent return
    // cannot have taken the remainder between the read and the write.
    let mut requested: Vec<(String, i64)> = Vec::new();
    for line in &lines {
        let original_line_id = line.original_line_id().ok_or(ReturnError::Internal)?;
        match requested
            .iter_mut()
            .find(|entry| entry.0 == original_line_id)
        {
            Some(entry) => {
                entry.1 = entry
                    .1
                    .checked_add(line.quantity_atoms)
                    .ok_or(ReturnError::ArithmeticOverflow)?;
            }
            None => requested.push((original_line_id.to_owned(), line.quantity_atoms)),
        }
    }
    let mut originals = Vec::with_capacity(requested.len());
    for (original_line_id, atoms) in &requested {
        let amounts = load_original_line_amounts(connection, is_sale, original_line_id).await?;
        let already = already_returned_atoms(&mut **connection, original_line_id, is_sale).await?;
        let returnable = returns::returnable_atoms(amounts.quantity_atoms, already)?;
        if *atoms > returnable {
            return Err(ReturnError::OverReturn { returnable });
        }
        originals.push((original_line_id.clone(), amounts, already));
    }

    // ---- Recompute every amount from the original ------------------------------------------
    //
    // Never trusted from the draft: the already-returned figure may have moved since the line was
    // added. Lines are applied in order so two lines against one original reverse cumulatively.
    let mut offsets: Vec<(String, i64)> = originals
        .iter()
        .map(|(line_id, _, already)| (line_id.clone(), *already))
        .collect();
    let mut computed = Vec::with_capacity(lines.len());
    for line in &lines {
        let original_line_id = line.original_line_id().ok_or(ReturnError::Internal)?;
        let (_, amounts, _) = originals
            .iter()
            .find(|(candidate, _, _)| candidate == original_line_id)
            .ok_or(ReturnError::Internal)?;
        let offset = offsets
            .iter_mut()
            .find(|(candidate, _)| candidate == original_line_id)
            .ok_or(ReturnError::Internal)?;
        let reversed = returns::reverse_line(amounts, offset.1, line.quantity_atoms)?;
        offset.1 += line.quantity_atoms;
        computed.push(reversed);
    }

    let totals = computed.iter().try_fold(
        returns::ReversedAmounts {
            taxable_value_paise: 0,
            cgst_paise: 0,
            sgst_paise: 0,
            igst_paise: 0,
            cess_paise: 0,
            line_total_paise: 0,
        },
        |mut total, part| -> Result<returns::ReversedAmounts, ReturnError> {
            total.taxable_value_paise = total
                .taxable_value_paise
                .checked_add(part.taxable_value_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            total.cgst_paise = total
                .cgst_paise
                .checked_add(part.cgst_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            total.sgst_paise = total
                .sgst_paise
                .checked_add(part.sgst_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            total.igst_paise = total
                .igst_paise
                .checked_add(part.igst_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            total.cess_paise = total
                .cess_paise
                .checked_add(part.cess_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            total.line_total_paise = total
                .line_total_paise
                .checked_add(part.line_total_paise)
                .ok_or(ReturnError::ArithmeticOverflow)?;
            Ok(total)
        },
    )?;

    // ---- Current stock, for a purchase return only -----------------------------------------
    //
    // The original invoice once bought enough; that is not proof the atoms are still on the shelf.
    // Goods may have been sold since, so sellable availability is checked as well.
    if !is_sale {
        let mut required: Vec<(String, String, i64)> = Vec::new();
        for line in &lines {
            match required
                .iter_mut()
                .find(|entry| entry.0 == line.product_pack_id && entry.1 == line.batch_id)
            {
                Some(entry) => {
                    entry.2 = entry
                        .2
                        .checked_add(line.quantity_atoms)
                        .ok_or(ReturnError::ArithmeticOverflow)?;
                }
                None => required.push((
                    line.product_pack_id.clone(),
                    line.batch_id.clone(),
                    line.quantity_atoms,
                )),
            }
        }
        for (pack_id, batch_id, atoms) in &required {
            let available =
                sellable_balance(connection, &header.store_id, pack_id, batch_id).await?;
            if available < *atoms {
                return Err(ReturnError::InsufficientStock { available });
            }
        }
    }

    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;

    // The number is allocated inside this transaction, so a refused posting rolls it back with
    // everything else and the series has no gaps.
    let series = if is_sale {
        SALES_RETURN_SERIES
    } else {
        PURCHASE_RETURN_SERIES
    };
    let number = allocate_document_number(
        connection,
        &header.store_id,
        &header.return_kind,
        series,
        &header.business_date,
        &now,
    )
    .await?;

    for (line, amounts) in lines.iter().zip(computed.iter()) {
        sqlx::query(
            "UPDATE return_lines SET taxable_value_paise=?,cgst_paise=?,sgst_paise=?,igst_paise=?,\
             cess_paise=?,line_total_paise=?,updated_at_utc=? WHERE id=?",
        )
        .bind(amounts.taxable_value_paise)
        .bind(amounts.cgst_paise)
        .bind(amounts.sgst_paise)
        .bind(amounts.igst_paise)
        .bind(amounts.cess_paise)
        .bind(amounts.line_total_paise)
        .bind(&now)
        .bind(&line.id)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;

        // One compensating movement per line, carrying durable return provenance. A sales return
        // brings goods back into the status its disposition names — never into sellable stock. A
        // purchase return takes sellable goods out.
        let (delta, stock_status) = if is_sale {
            (
                line.quantity_atoms,
                line.disposition.clone().ok_or(ReturnError::Internal)?,
            )
        } else {
            (-line.quantity_atoms, "sellable".to_owned())
        };
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,stock_status,quantity_delta_atoms,occurred_on,return_line_id,\
             idempotency_key,posted_by_user_id,posted_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&header.store_id)
        .bind(&line.product_id)
        .bind(&line.product_pack_id)
        .bind(&line.batch_id)
        .bind(&header.return_kind)
        .bind(&stock_status)
        .bind(delta)
        .bind(&header.business_date)
        .bind(&line.id)
        .bind(Uuid::now_v7().to_string())
        .bind(actor_id)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    let next = header.revision + 1;
    sqlx::query(
        "UPDATE return_documents SET status='posted',revision=?,series_code=?,financial_year=?,\
         sequence_value=?,document_number=?,original_document_number=?,original_document_date=?,\
         store_gst_registration_status=?,store_normalized_gstin=?,store_place_of_supply_state_id=?,\
         store_state_code=?,counterparty_display_name=?,counterparty_gst_registration_status=?,\
         counterparty_normalized_gstin=?,counterparty_state_code=?,tax_treatment=?,\
         tax_adjustment_status=?,tax_adjustment_reason=?,gst_route=?,taxable_value_paise=?,\
         cgst_paise=?,sgst_paise=?,igst_paise=?,cess_paise=?,grand_total_paise=?,\
         posted_by_user_id=?,posted_at_utc=?,posting_idempotency_key=?,posting_fingerprint=?,\
         updated_at_utc=? WHERE id=? AND revision=? AND status='draft'",
    )
    .bind(next)
    .bind(&number.series_code)
    .bind(&number.financial_year)
    .bind(number.sequence_value)
    .bind(&number.document_number)
    .bind(&original.document_number)
    .bind(&original.document_date)
    .bind(&store.gst_registration_status)
    .bind(&store.normalized_gstin)
    .bind(&store.place_of_supply_state_id)
    .bind(&original.store_state_code)
    .bind(counterparty.as_ref().map(|party| &party.display_name))
    .bind(
        counterparty
            .as_ref()
            .map(|party| &party.gst_registration_status),
    )
    .bind(
        counterparty
            .as_ref()
            .and_then(|party| party.normalized_gstin.as_ref()),
    )
    .bind(
        counterparty
            .as_ref()
            .and_then(|party| party.state_code.as_ref()),
    )
    .bind(&original.tax_treatment)
    .bind(&gst.tax_adjustment_status)
    .bind(&gst.tax_adjustment_reason)
    .bind(&gst.gst_route)
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
    .bind(header.revision)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'return_document',?,?,'posted',strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(id)
    .bind(next)
    .bind(
        json!({
            "returnKind": header.return_kind,
            "documentNumber": number.document_number,
            "originalDocumentNumber": original.document_number,
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

/// The GST facts the operator asserted, validated for the kind but never decided for them.
///
/// The service checks that the combination is *allowed*; it does not conclude what the law requires.
/// Section 34(2) makes the answer depend on facts the software does not hold — whether a registered
/// recipient reversed the attributable ITC, and whether the incidence of tax was passed on — so
/// inferring `tax_adjustable` from, say, the customer being unregistered would encode a legal
/// decision on evidence this phase does not have. It is recorded, with a reason, for a later GST
/// phase to make and audit. See docs/adr/ADR-017.
struct GstDecision {
    tax_adjustment_status: Option<String>,
    tax_adjustment_reason: Option<String>,
    gst_route: Option<String>,
}

fn prepare_gst_decision(
    is_sale: bool,
    request: &PostReturnRequest,
) -> Result<GstDecision, ReturnError> {
    if is_sale {
        if request.gst_route.is_some() {
            return Err(ReturnError::GstRouteNotAllowed);
        }
        let status = request
            .tax_adjustment_status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(ReturnError::TaxAdjustmentRequired)?;
        if !matches!(status, "tax_adjustable" | "commercial_only") {
            return Err(validation_of(
                "taxAdjustmentStatus",
                "must be tax_adjustable or commercial_only",
            ));
        }
        let reason = optional_text(
            request.tax_adjustment_reason.as_deref(),
            "taxAdjustmentReason",
            500,
        )
        .map_err(validation_issue)?;
        Ok(GstDecision {
            tax_adjustment_status: Some(status.to_owned()),
            tax_adjustment_reason: reason,
            gst_route: None,
        })
    } else {
        if request.tax_adjustment_status.is_some() {
            return Err(ReturnError::TaxAdjustmentNotAllowed);
        }
        let route = request
            .gst_route
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(ReturnError::GstRouteRequired)?;
        if !matches!(route, "fresh_supply" | "supplier_credit_note") {
            return Err(validation_of(
                "gstRoute",
                "must be fresh_supply or supplier_credit_note",
            ));
        }
        Ok(GstDecision {
            tax_adjustment_status: None,
            tax_adjustment_reason: None,
            gst_route: Some(route.to_owned()),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Stock disposition: releasing quarantined goods
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StockDispositionRequest {
    idempotency_key: String,
    product_pack_id: String,
    batch_id: String,
    quantity_atoms: i64,
    from_status: String,
    to_status: String,
    reason: String,
    occurred_on: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StockDispositionResponse {
    id: String,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_atoms: i64,
    from_status: String,
    to_status: String,
    reason: String,
    occurred_on: String,
}

/// Moves quantity between stock statuses within one lot.
///
/// This is how quarantined goods become sellable, and it is deliberately not an edit: the original
/// return movement is never touched. Two append-only movements are written — one negative in the
/// status the goods leave, one positive in the status they enter — so the physical total is
/// unchanged while sellable availability rises by exactly the released quantity.
///
/// Only a pharmacist or the owner may do it. A cashier who accepted the return cannot decide that a
/// medicine which left the premises is fit to sell again.
async fn create_stock_disposition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<StockDispositionRequest>,
) -> Result<(StatusCode, Json<StockDispositionResponse>), ReturnError> {
    let actor = require_pharmacist(&state, &headers)
        .await
        .map_err(|error| match error {
            ReturnError::Auth(AuthError::AuthorizationDenied) => ReturnError::DispositionDenied,
            other => other,
        })?;
    let idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;
    validate_uuid_v7(&request.product_pack_id, "productPackId").map_err(validation_issue)?;
    validate_uuid_v7(&request.batch_id, "batchId").map_err(validation_issue)?;
    let occurred_on = validate_date(Some(&request.occurred_on), "occurredOn")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("occurredOn", "is required"))?;
    let reason = required_text(&request.reason, "reason", 500).map_err(validation_issue)?;
    let from_status = request.from_status.trim();
    let to_status = request.to_status.trim();
    // Quarantine is the only place a transfer may start. Writing stock off is a judgement that
    // it must not be sold, and undoing that judgement quietly — through the same door used to
    // release quarantine — would let written-off goods walk back onto the shelf. Correcting a
    // mistaken write-off is a deliberate act that deserves its own design, not a side effect of
    // this one.
    if from_status != "quarantined" {
        return Err(validation_of(
            "fromStatus",
            "must be quarantined, because stock written off is not transferred back",
        ));
    }
    if !matches!(to_status, "sellable" | "non_sellable") {
        return Err(validation_of(
            "toStatus",
            "must be sellable or non_sellable",
        ));
    }
    if from_status == to_status {
        return Err(validation_of(
            "toStatus",
            "must differ from the status the stock is leaving",
        ));
    }
    if request.quantity_atoms < 1 {
        return Err(validation_of("quantityAtoms", "must be at least one"));
    }
    let store_id = current_store(&state.pool).await?;
    let product_id: Option<String> =
        sqlx::query_scalar("SELECT product_id FROM product_packs WHERE id=?")
            .bind(&request.product_pack_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    let product_id = product_id.ok_or(ReturnError::NotFound)?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| ReturnError::Internal)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let id = Uuid::now_v7().to_string();
    let outcome = disposition_within_transaction(
        &mut connection,
        &id,
        &store_id,
        &product_id,
        &request,
        from_status,
        to_status,
        &reason,
        &occurred_on,
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
            Ok((
                StatusCode::CREATED,
                Json(StockDispositionResponse {
                    id,
                    product_id,
                    product_pack_id: request.product_pack_id,
                    batch_id: request.batch_id,
                    quantity_atoms: request.quantity_atoms,
                    from_status: from_status.to_owned(),
                    to_status: to_status.to_owned(),
                    reason,
                    occurred_on,
                }),
            ))
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            drop(connection);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn disposition_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    id: &str,
    store_id: &str,
    product_id: &str,
    request: &StockDispositionRequest,
    from_status: &str,
    to_status: &str,
    reason: &str,
    occurred_on: &str,
    idempotency_key: &str,
    actor_id: &str,
) -> Result<(), ReturnError> {
    // Quarantine must be a one-way door for a lot that has expired. Rule 110 of the Drugs Rules
    // forbids selling a Schedule C substance after its expiry date, and the assessment quarantined
    // stock is waiting for can no longer come back "fit to sell" once that date has passed. Without
    // this, quarantine would be a route back onto the shelf for exactly the goods it exists to keep
    // off it.
    //
    // Checked against the later of the recorded date and today, so a back-dated release cannot be
    // used to step around an expiry that has since passed, and read under this posting's write lock
    // so the batch cannot be re-dated underneath the decision.
    if to_status == "sellable" {
        let expiry: Option<String> =
            sqlx::query_scalar("SELECT expires_on FROM product_batches WHERE id=?")
                .bind(&request.batch_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?
                .flatten();
        if let Some(expiry) = expiry {
            let today: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
                .fetch_one(&mut **connection)
                .await
                .map_err(map_database_error)?;
            let effective = if occurred_on > today.as_str() {
                occurred_on
            } else {
                today.as_str()
            };
            if expiry.as_str() < effective {
                return Err(validation_of(
                    "toStatus",
                    "this batch has expired, so it can never return to sellable stock",
                ));
            }
        }
    }

    // Nothing may be released that is not there, read under the write lock this posting holds.
    let available = status_balance(
        connection,
        store_id,
        &request.product_pack_id,
        &request.batch_id,
        from_status,
    )
    .await?;
    if available < request.quantity_atoms {
        return Err(ReturnError::InsufficientStock { available });
    }

    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
    sqlx::query(
        "INSERT INTO stock_dispositions (id,store_id,product_id,product_pack_id,batch_id,\
         quantity_atoms,from_status,to_status,reason,occurred_on,authorised_by_user_id,\
         created_at_utc,idempotency_key) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(store_id)
    .bind(product_id)
    .bind(&request.product_pack_id)
    .bind(&request.batch_id)
    .bind(request.quantity_atoms)
    .bind(from_status)
    .bind(to_status)
    .bind(reason)
    .bind(occurred_on)
    .bind(actor_id)
    .bind(&now)
    .bind(idempotency_key)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    // The pair. Equal and opposite within one lot, so nothing is created or destroyed.
    for (status, delta) in [
        (from_status, -request.quantity_atoms),
        (to_status, request.quantity_atoms),
    ] {
        sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,stock_status,quantity_delta_atoms,occurred_on,reason,\
             stock_disposition_id,idempotency_key,posted_by_user_id,posted_at_utc) \
             VALUES (?,?,?,?,?,'disposition_transfer',?,?,?,?,?,?,?,?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(product_id)
        .bind(&request.product_pack_id)
        .bind(&request.batch_id)
        .bind(status)
        .bind(delta)
        .bind(occurred_on)
        .bind(reason)
        .bind(id)
        .bind(Uuid::now_v7().to_string())
        .bind(actor_id)
        .bind(&now)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }

    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'stock_disposition',?,1,'created',strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(id)
    .bind(reason)
    .bind(
        json!({
            "fromStatus": from_status,
            "toStatus": to_status,
            "quantityAtoms": request.quantity_atoms,
            "batchId": request.batch_id,
        })
        .to_string(),
    )
    .bind(actor_id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Supplier credit-note evidence
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SupplierCreditNoteRequest {
    credit_note_number: String,
    credit_note_date: String,
    credit_note_amount_paise: i64,
}

/// Records the credit note the supplier issued for goods we returned.
///
/// Under Circular 72/46/2018-GST route B the supplier issues the credit note and it reaches us
/// later, often days later. Capturing it must never require editing the posted return, so it is its
/// own append-only evidence row. This is evidence of what the supplier did — never our own tax
/// document, and never a debit note.
async fn record_supplier_credit_note(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SupplierCreditNoteRequest>,
) -> Result<(StatusCode, Json<SupplierCreditNoteResponse>), ReturnError> {
    let actor = require_owner(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let number = required_text(&request.credit_note_number, "creditNoteNumber", 64)
        .map_err(validation_issue)?;
    let date = validate_date(Some(&request.credit_note_date), "creditNoteDate")
        .map_err(validation_issue)?
        .ok_or_else(|| validation_of("creditNoteDate", "is required"))?;
    if !(0..=100_000_000_000).contains(&request.credit_note_amount_paise) {
        return Err(validation_of(
            "creditNoteAmountPaise",
            "is not a usable amount in paise",
        ));
    }

    let evidence_id = Uuid::now_v7().to_string();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| ReturnError::Internal)?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "INSERT INTO supplier_credit_note_evidence (id,return_document_id,credit_note_number,\
         normalized_credit_note_number,credit_note_date,credit_note_amount_paise,\
         recorded_by_user_id,recorded_at_utc) VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&evidence_id)
    .bind(&id)
    .bind(&number)
    .bind(number.to_uppercase())
    .bind(&date)
    .bind(request.credit_note_amount_paise)
    .bind(&actor.id)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'supplier_credit_note_evidence',?,1,'created',\
         strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&evidence_id)
    .bind(json!({ "returnDocumentId": id, "creditNoteNumber": number }).to_string())
    .bind(&actor.id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    transaction.commit().await.map_err(map_database_error)?;

    Ok((
        StatusCode::CREATED,
        Json(SupplierCreditNoteResponse {
            id: evidence_id,
            return_document_id: id,
            credit_note_number: number,
            credit_note_date: date,
            credit_note_amount_paise: request.credit_note_amount_paise,
            recorded_at_utc: now,
        }),
    ))
}

// ---------------------------------------------------------------------------------------------
// Numbering
// ---------------------------------------------------------------------------------------------

struct AllocatedNumber {
    series_code: String,
    financial_year: String,
    sequence_value: i64,
    document_number: String,
}

/// Allocates the next number in this store's series for the kind and financial year.
///
/// The same mechanism the sale uses: a counter row read and advanced under the write lock the
/// posting already holds, never `MAX(number) + 1`, and rolled back with the posting if anything
/// later fails. The rendered serial goes through the one formatter, so it obeys the statutory
/// sixteen-character limit or is refused.
async fn allocate_document_number(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    document_kind: &str,
    series_code: &str,
    business_date: &str,
    now: &str,
) -> Result<AllocatedNumber, ReturnError> {
    let financial_year = sales::indian_financial_year(business_date)
        .ok_or_else(|| validation_of("businessDate", "must be a valid YYYY-MM-DD date"))?;
    sqlx::query(
        "INSERT INTO document_number_series (id,store_id,document_kind,series_code,financial_year,\
         next_value,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,1,?,?) \
         ON CONFLICT (store_id,document_kind,series_code,financial_year) DO NOTHING",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(store_id)
    .bind(document_kind)
    .bind(series_code)
    .bind(&financial_year)
    .bind(now)
    .bind(now)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    let sequence_value: i64 = sqlx::query_scalar(
        "SELECT next_value FROM document_number_series \
         WHERE store_id=? AND document_kind=? AND series_code=? AND financial_year=?",
    )
    .bind(store_id)
    .bind(document_kind)
    .bind(series_code)
    .bind(&financial_year)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::Internal)?;

    let advanced = sqlx::query(
        "UPDATE document_number_series SET next_value=next_value+1,updated_at_utc=? \
         WHERE store_id=? AND document_kind=? AND series_code=? AND financial_year=? \
           AND next_value=?",
    )
    .bind(now)
    .bind(store_id)
    .bind(document_kind)
    .bind(series_code)
    .bind(&financial_year)
    .bind(sequence_value)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    if advanced.rows_affected() != 1 {
        return Err(ReturnError::ServiceBusy);
    }

    let document_number = sales::document_serial(series_code, &financial_year, sequence_value)
        .ok_or(ReturnError::Internal)?;
    Ok(AllocatedNumber {
        document_number,
        series_code: series_code.to_owned(),
        financial_year,
        sequence_value,
    })
}

/// The semantic payload a replay must match.
///
/// Everything that changes the commercial, inventory or GST result is in here: the original, the
/// date, each line's original line, quantity and disposition, and the GST decision. Presentation and
/// timestamps are deliberately excluded.
fn posting_fingerprint(
    id: &str,
    original_document_id: &str,
    business_date: &str,
    lines: &[DraftLine],
    gst: &GstDecision,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update(original_document_id.as_bytes());
    hasher.update(business_date.as_bytes());
    for line in lines {
        hasher.update(line.original_line_id().unwrap_or_default().as_bytes());
        hasher.update(line.batch_id.as_bytes());
        hasher.update(line.quantity_atoms.to_le_bytes());
        hasher.update(line.disposition.as_deref().unwrap_or("none").as_bytes());
    }
    hasher.update(
        gst.tax_adjustment_status
            .as_deref()
            .unwrap_or("none")
            .as_bytes(),
    );
    hasher.update(gst.gst_route.as_deref().unwrap_or("none").as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct PostingHeader {
    status: String,
    revision: i64,
    store_id: String,
    return_kind: String,
    original_sale_document_id: Option<String>,
    original_purchase_document_id: Option<String>,
    business_date: String,
    counterparty_party_id: Option<String>,
    posting_idempotency_key: Option<String>,
    posting_fingerprint: Option<String>,
}

#[derive(Debug, FromRow)]
struct StoreTaxSource {
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    gst_registration_status: String,
}

struct CounterpartySnapshot {
    display_name: String,
    gst_registration_status: String,
    normalized_gstin: Option<String>,
    state_code: Option<String>,
}

#[derive(Debug, FromRow)]
struct CounterpartySource {
    display_name: String,
    gst_registration_status: String,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
}

async fn load_counterparty(
    connection: &mut PoolConnection<Sqlite>,
    party_id: &str,
) -> Result<CounterpartySnapshot, ReturnError> {
    let party = sqlx::query_as::<_, CounterpartySource>(
        "SELECT display_name,gst_registration_status,normalized_gstin,place_of_supply_state_id \
         FROM parties WHERE id=?",
    )
    .bind(party_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::Internal)?;
    let state_code = match party.place_of_supply_state_id.as_deref() {
        Some(state_id) => sqlx::query_scalar("SELECT state_code FROM state_codes WHERE id=?")
            .bind(state_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?,
        None => None,
    };
    Ok(CounterpartySnapshot {
        display_name: party.display_name,
        gst_registration_status: party.gst_registration_status,
        normalized_gstin: party.normalized_gstin,
        state_code,
    })
}

/// What a return needs to know about the document it corrects, while still a draft.
struct ReturnContext {
    return_kind: String,
    original_document_id: String,
    business_date: String,
}

#[derive(Debug, FromRow)]
struct ReturnContextRow {
    return_kind: String,
    original_sale_document_id: Option<String>,
    original_purchase_document_id: Option<String>,
    business_date: String,
}

async fn load_return_context(pool: &SqlitePool, id: &str) -> Result<ReturnContext, ReturnError> {
    let row = sqlx::query_as::<_, ReturnContextRow>(
        "SELECT return_kind,original_sale_document_id,original_purchase_document_id,business_date \
         FROM return_documents WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::NotFound)?;
    Ok(ReturnContext {
        original_document_id: row
            .original_sale_document_id
            .or(row.original_purchase_document_id)
            .ok_or(ReturnError::Internal)?,
        return_kind: row.return_kind,
        business_date: row.business_date,
    })
}

/// The original document, as a return needs it when it is first raised.
struct OriginalDocument {
    id: String,
    document_number: Option<String>,
    document_date: String,
    counterparty_party_id: Option<String>,
}

#[derive(Debug, FromRow)]
struct OriginalDocumentRow {
    status: String,
    document_number: Option<String>,
    business_date: String,
    customer_party_id: Option<String>,
}

async fn load_original_document(
    pool: &SqlitePool,
    kind: &str,
    original_id: &str,
) -> Result<OriginalDocument, ReturnError> {
    let query = if kind == "sales_return" {
        "SELECT status,document_number,business_date,customer_party_id \
         FROM sale_documents WHERE id=?"
    } else {
        "SELECT status,supplier_invoice_number AS document_number,invoice_date AS business_date,\
         supplier_party_id AS customer_party_id FROM purchase_documents WHERE id=?"
    };
    let row = sqlx::query_as::<_, OriginalDocumentRow>(query)
        .bind(original_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(ReturnError::OriginalNotFound)?;
    if row.status != "posted" {
        return Err(ReturnError::OriginalNotPosted);
    }
    Ok(OriginalDocument {
        id: original_id.to_owned(),
        document_number: row.document_number,
        document_date: row.business_date,
        counterparty_party_id: row.customer_party_id,
    })
}

/// The frozen tax facts of the original, re-read at posting and reused rather than recomputed.
#[derive(Debug, FromRow)]
struct OriginalForPosting {
    document_number: Option<String>,
    document_date: String,
    tax_treatment: Option<String>,
    store_state_code: Option<String>,
}

async fn load_original_for_posting(
    connection: &mut PoolConnection<Sqlite>,
    is_sale: bool,
    original_id: &str,
) -> Result<OriginalForPosting, ReturnError> {
    let query = if is_sale {
        "SELECT document_number,business_date AS document_date,tax_treatment,store_state_code \
         FROM sale_documents WHERE id=?"
    } else {
        "SELECT supplier_invoice_number AS document_number,invoice_date AS document_date,\
         tax_treatment,store_state_code FROM purchase_documents WHERE id=?"
    };
    sqlx::query_as::<_, OriginalForPosting>(query)
        .bind(original_id)
        .fetch_optional(&mut **connection)
        .await
        .map_err(map_database_error)?
        .ok_or(ReturnError::OriginalNotFound)
}

#[derive(Debug, FromRow)]
struct DraftLine {
    id: String,
    original_sale_line_id: Option<String>,
    original_purchase_line_id: Option<String>,
    product_id: String,
    product_pack_id: String,
    batch_id: String,
    quantity_atoms: i64,
    disposition: Option<String>,
}

impl DraftLine {
    fn original_line_id(&self) -> Option<&str> {
        self.original_sale_line_id
            .as_deref()
            .or(self.original_purchase_line_id.as_deref())
    }
}

async fn load_draft_lines(
    connection: &mut PoolConnection<Sqlite>,
    document_id: &str,
) -> Result<Vec<DraftLine>, ReturnError> {
    sqlx::query_as::<_, DraftLine>(
        "SELECT id,original_sale_line_id,original_purchase_line_id,product_id,product_pack_id,\
         batch_id,quantity_atoms,disposition FROM return_lines \
         WHERE return_document_id=? ORDER BY line_number",
    )
    .bind(document_id)
    .fetch_all(&mut **connection)
    .await
    .map_err(map_database_error)
}

async fn load_original_line_amounts(
    connection: &mut PoolConnection<Sqlite>,
    is_sale: bool,
    original_line_id: &str,
) -> Result<OriginalLineAmounts, ReturnError> {
    let table = if is_sale {
        "sale_lines"
    } else {
        "purchase_lines"
    };
    let row: Option<(i64, i64, i64, i64, i64, i64)> = sqlx::query_as(&format!(
        "SELECT quantity_atoms,taxable_value_paise,cgst_paise,sgst_paise,igst_paise,cess_paise \
         FROM {table} WHERE id=?"
    ))
    .bind(original_line_id)
    .fetch_optional(&mut **connection)
    .await
    .map_err(map_database_error)?;
    let row = row.ok_or(ReturnError::OriginalLineMismatch)?;
    Ok(OriginalLineAmounts {
        quantity_atoms: row.0,
        taxable_value_paise: row.1,
        cgst_paise: row.2,
        sgst_paise: row.3,
        igst_paise: row.4,
        cess_paise: row.5,
    })
}

/// The sellable balance of one lot: what the counter could actually sell right now.
async fn sellable_balance(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    pack_id: &str,
    batch_id: &str,
) -> Result<i64, ReturnError> {
    status_balance(connection, store_id, pack_id, batch_id, "sellable").await
}

/// The balance of one lot in one stock status. Still derived by summing movements; nothing anywhere
/// stores a quantity.
async fn status_balance(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    pack_id: &str,
    batch_id: &str,
    stock_status: &str,
) -> Result<i64, ReturnError> {
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

async fn draft_state(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(i64, String), ReturnError> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM return_documents WHERE id=?")
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = row.ok_or(ReturnError::NotFound)?;
    if status != "draft" {
        return Err(ReturnError::NotDraft);
    }
    Ok((revision, status))
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), ReturnError> {
    if current.0 != expected {
        return Err(ReturnError::Revision {
            expected,
            current: current.0,
        });
    }
    Ok(())
}

async fn line_document(pool: &SqlitePool, line_id: &str) -> Result<String, ReturnError> {
    sqlx::query_scalar("SELECT return_document_id FROM return_lines WHERE id=?")
        .bind(line_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)?
        .ok_or(ReturnError::NotFound)
}

async fn bump_draft(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    current: i64,
    now: &str,
) -> Result<(), ReturnError> {
    sqlx::query(
        "UPDATE return_documents SET revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='draft'",
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

async fn insert_return_line(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    document_id: &str,
    line_number: i64,
    line: &PreparedReturnLine,
    now: &str,
) -> Result<(), ReturnError> {
    sqlx::query(
        "INSERT INTO return_lines (id,return_document_id,line_number,original_sale_line_id,\
         original_purchase_line_id,product_id,product_pack_id,batch_id,quantity_basis,\
         quantity_packs,quantity_atoms,disposition,product_display_name,pack_display_label,\
         base_unit_label,batch_number,batch_expires_on,hsn_code_id,hsn_code,tax_category_id,\
         tax_treatment_kind,tax_rate_version_id,cgst_basis_points,sgst_basis_points,\
         igst_basis_points,cess_basis_points,taxable_value_paise,cgst_paise,sgst_paise,igst_paise,\
         cess_paise,line_total_paise,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(document_id)
    .bind(line_number)
    .bind(line.is_sale.then(|| line.original_line_id.clone()))
    .bind((!line.is_sale).then(|| line.original_line_id.clone()))
    .bind(&line.product_id)
    .bind(&line.product_pack_id)
    .bind(&line.batch_id)
    .bind(&line.quantity_basis)
    .bind(line.quantity_packs)
    .bind(line.quantity_atoms)
    .bind(&line.disposition)
    .bind(&line.product_display_name)
    .bind(&line.pack_display_label)
    .bind(&line.base_unit_label)
    .bind(&line.batch_number)
    .bind(&line.batch_expires_on)
    .bind(&line.hsn_code_id)
    .bind(&line.hsn_code)
    .bind(&line.tax_category_id)
    .bind(&line.tax_treatment_kind)
    .bind(&line.tax_rate_version_id)
    .bind(line.cgst_basis_points)
    .bind(line.sgst_basis_points)
    .bind(line.igst_basis_points)
    .bind(line.cess_basis_points)
    .bind(line.amounts.taxable_value_paise)
    .bind(line.amounts.cgst_paise)
    .bind(line.amounts.sgst_paise)
    .bind(line.amounts.igst_paise)
    .bind(line.amounts.cess_paise)
    .bind(line.amounts.line_total_paise)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
) -> Result<String, ReturnError> {
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
) -> Result<(), ReturnError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'return_document',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),NULL,1,?,?)",
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

async fn fetch_detail(pool: &SqlitePool, id: &str) -> Result<ReturnDetailResponse, ReturnError> {
    let document = sqlx::query_as::<_, ReturnHeaderResponse>(&format!(
        "SELECT {RETURN_HEADER_COLUMNS} FROM return_documents WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(ReturnError::NotFound)?;
    let lines = sqlx::query_as::<_, ReturnLineResponse>(&format!(
        "SELECT {RETURN_LINE_COLUMNS} FROM return_lines WHERE return_document_id=? \
         ORDER BY line_number"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let supplier_credit_notes = sqlx::query_as::<_, SupplierCreditNoteResponse>(
        "SELECT id,return_document_id,credit_note_number,credit_note_date,credit_note_amount_paise,\
         recorded_at_utc FROM supplier_credit_note_evidence WHERE return_document_id=? \
         ORDER BY recorded_at_utc,id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    Ok(ReturnDetailResponse {
        document,
        lines,
        supplier_credit_notes,
    })
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
    const OWNER: &str = "return-owner-session-token";
    const PHARMACIST: &str = "return-pharmacist-session-token";
    const CASHIER: &str = "return-cashier-session-token";
    const TODAY: &str = "2026-06-15";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        store_id: String,
        owner_id: String,
        product_id: String,
        pack_id: String,
        batch_id: String,
        /// A posted sale of 2 strips at 80.00: 160.00 taxable, 9.60 + 9.60, 179.20 in all.
        sale_id: String,
        sale_line_id: String,
        /// A posted purchase of 20 strips at 60.00 that created the batch and the stock.
        purchase_id: String,
        purchase_line_id: String,
    }

    /// A store in Maharashtra with one taxable product at 6% + 6%, stock received through a real
    /// posted purchase, and one real posted sale — so every return in these tests reverses a
    /// document the service itself produced.
    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("returns.sqlite3"))
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
        record_legal_profile(&pool, &store_id).await;
        let owner_id = insert_session(&pool, "owner_admin", OWNER).await;
        insert_session(&pool, "pharmacist", PHARMACIST).await;
        insert_session(&pool, "cashier", CASHIER).await;

        let supplier_id = insert_party(&pool, "Sharma Medicals", "supplier").await;
        let customer_id = insert_party(&pool, "Rahul Deshmukh", "customer").await;
        let category_id = insert_category(&pool, "gst-12").await;
        insert_rate(&pool, &category_id, "2020-01-01", 600).await;
        let hsn_id = insert_hsn(&pool, "30049099").await;
        let (product_id, pack_id) = insert_product(&pool, "Crocin 500", 10).await;
        sqlx::query("UPDATE products SET hsn_code_id=?,tax_category_id=? WHERE id=?")
            .bind(&hsn_id)
            .bind(&category_id)
            .bind(&product_id)
            .execute(&pool)
            .await
            .unwrap();
        enable_sale(&pool, &store_id, &product_id, &pack_id).await;

        // Stock arrives the way it really does: a posted purchase, which also creates the lot.
        let (purchase_id, purchase_line_id, batch_id) =
            post_purchase(&pool, &supplier_id, &product_id, &pack_id).await;
        let (sale_id, sale_line_id) =
            post_sale(&pool, &customer_id, &product_id, &pack_id, &batch_id).await;

        Fixture {
            _temp: temp,
            pool,
            store_id,
            owner_id,
            product_id,
            pack_id,
            batch_id,
            sale_id,
            sale_line_id,
            purchase_id,
            purchase_line_id,
        }
    }

    async fn insert_party(pool: &SqlitePool, name: &str, role: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             place_of_supply_state_id,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,'unregistered',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(name)
        .bind(name.to_lowercase())
        .bind(MAHARASHTRA)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .bind(role)
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

    async fn enable_sale(pool: &SqlitePool, store_id: &str, product_id: &str, pack_id: &str) {
        sqlx::query(
            "INSERT INTO store_pack_policies (id,store_id,product_id,pack_id,purchase_enabled,\
             sale_enabled,whole_pack_only_purchase,fractional_sale_allowed,\
             minimum_sale_increment_atoms,default_purchase_pack,default_sale_pack,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,?,1,1,0,0,1,0,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(product_id)
        .bind(pack_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_category(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_categories (id,jurisdiction,category_code,display_name,tax_treatment,\
             created_at_utc,updated_at_utc) VALUES (?,'IN',?,?, 'taxable',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_rate(pool: &SqlitePool, category_id: &str, from: &str, half: i64) {
        sqlx::query(
            "INSERT INTO tax_rate_versions (id,tax_category_id,effective_from,effective_to,\
             cgst_basis_points,sgst_basis_points,igst_basis_points,cess_basis_points,\
             created_at_utc,updated_at_utc) VALUES (?,?,?,NULL,?,?,?,0,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(category_id)
        .bind(from)
        .bind(half)
        .bind(half)
        .bind(half * 2)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_hsn(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO hsn_codes (id,jurisdiction,hsn_code,description,created_at_utc,\
             updated_at_utc) VALUES (?,'IN',?,'Medicaments',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// Twenty strips at 60.00, posted through the real purchase API so the batch and the stock are
    /// produced exactly as a pharmacy would produce them.
    async fn post_purchase(
        pool: &SqlitePool,
        supplier_id: &str,
        product_id: &str,
        pack_id: &str,
    ) -> (String, String, String) {
        let (status, draft) = request(
            pool.clone(),
            "POST",
            "/api/v1/purchases",
            json!({
                "supplierPartyId": supplier_id,
                "supplierInvoiceNumber": "INV-SEED-1",
                "invoiceDate": "2026-06-01"
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let id = draft["id"].as_str().unwrap().to_owned();
        let (status, with_line) = request(
            pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "productId": product_id,
                "productPackId": pack_id,
                "newBatchNumber": "B-2601",
                "newBatchExpiresOn": "2028-03-31",
                "newBatchMrpPaise": 9550,
                "quantityPacks": 20,
                "ratePerPackPaise": 6000
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{with_line}");
        let (status, posted) = request(
            pool.clone(),
            "POST",
            &format!("/api/v1/purchases/{id}/post"),
            json!({ "expectedRevision": 2, "idempotencyKey": Uuid::now_v7().to_string() }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let line_id = posted["lines"][0]["id"].as_str().unwrap().to_owned();
        let batch_id = posted["lines"][0]["batchId"].as_str().unwrap().to_owned();
        (id, line_id, batch_id)
    }

    /// Two strips at 80.00, posted through the real sales API.
    async fn post_sale(
        pool: &SqlitePool,
        customer_id: &str,
        product_id: &str,
        pack_id: &str,
        batch_id: &str,
    ) -> (String, String) {
        let (status, draft) = request(
            pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": customer_id, "businessDate": TODAY }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let id = draft["id"].as_str().unwrap().to_owned();
        let (status, with_line) = request(
            pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "productId": product_id,
                "productPackId": pack_id,
                "batchId": batch_id,
                "quantityBasis": "pack",
                "quantity": 2,
                "sellingRatePaise": 8000
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{with_line}");
        let (status, posted) = request(
            pool.clone(),
            "POST",
            &format!("/api/v1/sales/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 17920 }]
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        (id, posted["lines"][0]["id"].as_str().unwrap().to_owned())
    }

    // -------------------------------------------------------------------------------------
    // Returnable lines
    // -------------------------------------------------------------------------------------

    /// What the counter needs before it can take anything back: the original, what is left, and the
    /// money that will be reversed — all in the basis the sale was billed in.
    #[tokio::test]
    async fn a_posted_sale_reports_what_is_left_to_return() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{}/returnable-lines", f.sale_id),
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["documentNumber"], "INV/2627/000001");
        assert_eq!(body["counterpartyDisplayName"], "Rahul Deshmukh");
        let line = &body["lines"][0];
        assert_eq!(line["productDisplayName"], "Crocin 500");
        assert_eq!(line["batchNumber"], "B-2601");
        assert_eq!(line["quantityBasis"], "pack");
        assert_eq!(line["originalQuantity"], 2);
        assert_eq!(line["originalQuantityAtoms"], 20);
        assert_eq!(line["alreadyReturnedAtoms"], 0);
        assert_eq!(line["returnableAtoms"], 20);
        // The operator types strips, never atoms.
        assert_eq!(line["returnableQuantity"], 2);
        assert_eq!(line["lineTotalPaise"], 17_920);
    }

    /// A purchase line snapshots no identity of its own, so the names are joined live — otherwise
    /// the screen would show a raw identifier, which is the defect the Phase 1H preview caught.
    #[tokio::test]
    async fn a_posted_purchase_reports_what_is_left_with_readable_names() {
        let f = fixture().await;
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/purchases/{}/returnable-lines", f.purchase_id),
            Value::Null,
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["documentNumber"], "INV-SEED-1");
        assert_eq!(body["counterpartyDisplayName"], "Sharma Medicals");
        let line = &body["lines"][0];
        assert_eq!(line["productDisplayName"], "Crocin 500");
        assert_eq!(line["packDisplayLabel"], "Strip");
        assert_eq!(line["batchNumber"], "B-2601");
        assert_eq!(line["quantityBasis"], "pack");
        assert_eq!(line["returnableQuantity"], 20);
        assert_eq!(line["returnableAtoms"], 200);
    }

    /// Only a posted document can be corrected: a draft has issued nothing and moved no stock.
    #[tokio::test]
    async fn a_draft_sale_cannot_be_returned_against() {
        let f = fixture().await;
        let (_, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": null, "businessDate": TODAY }),
            Some(OWNER),
        )
        .await;
        let draft_id = draft["id"].as_str().unwrap();
        let (status, refused) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{draft_id}/returnable-lines"),
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "original_document_not_posted");

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({
                "returnKind": "sales_return",
                "originalDocumentId": draft_id,
                "businessDate": TODAY
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "original_document_not_posted");
    }

    // -------------------------------------------------------------------------------------
    // Sales return
    // -------------------------------------------------------------------------------------

    /// The whole sales-return contract in one document: the original is reversed exactly, a number
    /// is issued from its own series, and the goods come back into quarantine rather than into
    /// sellable stock.
    #[tokio::test]
    async fn a_full_sales_return_reverses_the_invoice_and_quarantines_the_goods() {
        let f = fixture().await;
        assert_eq!(balance(&f, "sellable").await, 180);

        let id = create_sales_return(&f).await;
        let (status, added) = add_sales_line(&f, &id, 1, 2, "quarantined").await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        assert_eq!(posted["status"], "posted");
        assert_eq!(posted["documentNumber"], "SR/2627/000001");
        assert_eq!(posted["originalDocumentNumber"], "INV/2627/000001");
        assert_eq!(posted["returnKind"], "sales_return");
        assert_eq!(posted["counterpartyDisplayName"], "Rahul Deshmukh");
        // Exactly the original, reversed.
        assert_eq!(posted["taxableValuePaise"], 16_000);
        assert_eq!(posted["cgstPaise"], 960);
        assert_eq!(posted["sgstPaise"], 960);
        assert_eq!(posted["grandTotalPaise"], 17_920);
        // The treatment is the original's, reused rather than recomputed.
        assert_eq!(posted["taxTreatment"], "intra_state");
        assert_eq!(posted["taxAdjustmentStatus"], "commercial_only");
        assert_eq!(posted["gstRoute"], Value::Null);

        // The goods are back in the building but not on the shelf.
        assert_eq!(
            balance(&f, "sellable").await,
            180,
            "nothing became sellable"
        );
        assert_eq!(balance(&f, "quarantined").await, 20);

        // One compensating movement, positive, with durable provenance.
        let (kind, delta, status_text, line): (String, i64, String, String) = sqlx::query_as(
            "SELECT movement_type,quantity_delta_atoms,stock_status,return_line_id \
             FROM inventory_movements WHERE movement_type='sales_return'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(kind, "sales_return");
        assert_eq!(delta, 20);
        assert_eq!(status_text, "quarantined");
        assert_eq!(line, posted["lines"][0]["id"].as_str().unwrap());
    }

    /// Three partial returns of one line must reverse the original exactly, with no drift and no
    /// residual stranded on the last one.
    #[tokio::test]
    async fn repeated_partial_sales_returns_reverse_the_original_exactly() {
        let f = fixture().await;
        let mut taxable = 0;
        let mut cgst = 0;
        let mut total = 0;

        for expected_number in ["SR/2627/000001", "SR/2627/000002"] {
            let id = create_sales_return(&f).await;
            let (status, added) = add_sales_line(&f, &id, 1, 1, "quarantined").await;
            assert_eq!(status, StatusCode::CREATED, "{added}");
            let (status, posted) = post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
            assert_eq!(status, StatusCode::OK, "{posted}");
            assert_eq!(posted["documentNumber"], expected_number);
            taxable += posted["taxableValuePaise"].as_i64().unwrap();
            cgst += posted["cgstPaise"].as_i64().unwrap();
            total += posted["grandTotalPaise"].as_i64().unwrap();
        }

        // Two halves of the original, summing to it exactly.
        assert_eq!(taxable, 16_000);
        assert_eq!(cgst, 960);
        assert_eq!(total, 17_920);
        assert_eq!(balance(&f, "quarantined").await, 20);

        // And there is nothing left to return.
        let (_, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{}/returnable-lines", f.sale_id),
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(body["lines"][0]["alreadyReturnedAtoms"], 20);
        assert_eq!(body["lines"][0]["returnableAtoms"], 0);
    }

    #[tokio::test]
    async fn a_return_cannot_exceed_what_is_left_on_the_original_line() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        let (status, refused) = add_sales_line(&f, &id, 1, 3, "quarantined").await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "over_return");
        assert_eq!(refused["returnableAtoms"], 20);

        // And after one strip has genuinely come back, only one remains returnable.
        let first = create_sales_return(&f).await;
        add_sales_line(&f, &first, 1, 1, "quarantined").await;
        post_sales_return(&f, &first, 2, &Uuid::now_v7().to_string()).await;

        let second = create_sales_return(&f).await;
        let (status, refused) = add_sales_line(&f, &second, 1, 2, "quarantined").await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["returnableAtoms"], 10);
    }

    /// Two lines of one document pointing at the same original line are aggregated before the
    /// check, so the pair cannot pass a limit neither exceeds alone.
    #[tokio::test]
    async fn two_lines_against_one_original_are_checked_together() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        // Each line is individually returnable...
        let (status, first) = add_sales_line(&f, &id, 1, 1, "quarantined").await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let (status, second) = add_sales_line(&f, &id, 2, 1, "quarantined").await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        // ...and together they are exactly the whole line, which is allowed.
        let (status, posted) = post_sales_return(&f, &id, 3, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        // The two lines reverse cumulatively, summing to the original.
        assert_eq!(posted["grandTotalPaise"], 17_920);
        assert_eq!(posted["lines"][0]["lineTotalPaise"], 8_960);
        assert_eq!(posted["lines"][1]["lineTotalPaise"], 8_960);
        assert_eq!(balance(&f, "quarantined").await, 20);
    }

    /// The same aggregation, now exceeding the limit: a third strip does not exist to return.
    ///
    /// Two checks stand between that strip and a posting, and both are proved here. The draft
    /// counts the lines already on it, so the counter is told at entry rather than at the till; and
    /// the posting transaction aggregates the whole document under its write lock, which is the
    /// authority — shown by putting the line in behind the handler's back.
    #[tokio::test]
    async fn aggregated_lines_cannot_exceed_the_original_between_them() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        add_sales_line(&f, &id, 2, 1, "quarantined").await;

        // Two strips are already spoken for by this very draft, so the third is refused at entry.
        let (status, third) = add_sales_line(&f, &id, 3, 1, "quarantined").await;
        assert_eq!(status, StatusCode::CONFLICT, "{third}");
        assert_eq!(third["code"], "over_return");
        assert_eq!(third["returnableAtoms"], 0);

        // The posting check is the authority, so it must bite even on a line the handler never saw.
        // This is the line the draft check just refused, written straight into the table.
        let smuggled: (String, String) = sqlx::query_as(
            "SELECT original_sale_line_id,batch_id FROM return_lines WHERE return_document_id=? \
             LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO return_lines (id,return_document_id,line_number,original_sale_line_id,\
             product_id,product_pack_id,batch_id,quantity_basis,quantity_packs,quantity_atoms,\
             disposition,created_at_utc,updated_at_utc) \
             SELECT ?,return_document_id,99,original_sale_line_id,product_id,product_pack_id,\
             batch_id,quantity_basis,quantity_packs,quantity_atoms,disposition,created_at_utc,\
             updated_at_utc FROM return_lines WHERE return_document_id=? LIMIT 1",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
        let _ = smuggled;

        let (status, refused) = post_sales_return(&f, &id, 3, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "over_return");
        // Nothing was posted, so nothing came back and no number was consumed.
        assert_eq!(balance(&f, "quarantined").await, 0);
        let series: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM document_number_series WHERE document_kind='sales_return'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(series, 0);
    }

    /// Two returns competing for the last strip: exactly one may have it.
    #[tokio::test]
    async fn two_returns_cannot_both_take_the_last_returnable_strip() {
        let f = fixture().await;
        let left = create_sales_return(&f).await;
        let right = create_sales_return(&f).await;
        add_sales_line(&f, &left, 1, 2, "quarantined").await;
        add_sales_line(&f, &right, 1, 2, "quarantined").await;
        let left_key = Uuid::now_v7().to_string();
        let right_key = Uuid::now_v7().to_string();

        let (one, two) = tokio::join!(
            post_sales_return(&f, &left, 2, &left_key),
            post_sales_return(&f, &right, 2, &right_key),
        );
        let outcomes = [one, two];
        let succeeded = outcomes
            .iter()
            .filter(|(status, _)| *status == StatusCode::OK)
            .count();
        assert_eq!(succeeded, 1, "exactly one may take it: {outcomes:?}");
        for (status, body) in &outcomes {
            if *status != StatusCode::OK {
                assert!(
                    body["code"] == "over_return" || body["code"] == "service_busy",
                    "the loser must be told the truth: {body}"
                );
            }
        }
        assert_eq!(
            balance(&f, "quarantined").await,
            20,
            "only one return landed"
        );
    }

    // -------------------------------------------------------------------------------------
    // Disposition
    // -------------------------------------------------------------------------------------

    /// A sales return must say where the goods went, and can never say `sellable`.
    #[tokio::test]
    async fn a_sales_return_must_state_a_disposition_and_can_never_choose_sellable() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/lines"),
            json!({ "expectedRevision": 1, "originalLineId": f.sale_line_id, "quantity": 1 }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "disposition_required");

        // `sellable` is not a value this endpoint will accept from anyone, at any role.
        for token in [CASHIER, PHARMACIST, OWNER] {
            let (status, refused) = request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/returns/{id}/lines"),
                json!({
                    "expectedRevision": 1,
                    "originalLineId": f.sale_line_id,
                    "quantity": 1,
                    "disposition": "sellable"
                }),
                Some(token),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
            assert_eq!(refused["issues"][0]["field"], "disposition");
        }
    }

    /// Quarantined goods are in the building and in the ledger, but the counter cannot sell them.
    #[tokio::test]
    async fn quarantined_stock_is_invisible_to_the_counter() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        // The POS batch chooser offers the sellable balance only: 200 in, 20 sold, 20 quarantined.
        let (status, batches) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/packs/{}/sellable-batches?asOf={TODAY}", f.pack_id),
            Value::Null,
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{batches}");
        assert_eq!(batches[0]["availableAtoms"], 180);

        // And a sale of the whole sellable balance plus one quarantined strip is refused.
        let (_, draft) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/sales",
            json!({ "customerPartyId": null, "businessDate": TODAY }),
            Some(OWNER),
        )
        .await;
        let sale = draft["id"].as_str().unwrap();
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{sale}/lines"),
            json!({
                "expectedRevision": 1, "productId": f.product_id, "productPackId": f.pack_id,
                "batchId": f.batch_id, "quantityBasis": "pack", "quantity": 19,
                "sellingRatePaise": 100
            }),
            Some(OWNER),
        )
        .await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/sales/{sale}/post"),
            json!({
                "expectedRevision": 2, "idempotencyKey": Uuid::now_v7().to_string(),
                "tenders": [{ "method": "cash", "amountPaise": 2128 }]
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 180);
    }

    /// Releasing quarantined goods is a pharmacist's judgement, and it is a transfer rather than an
    /// edit: two movements, nothing mutated, the physical total unchanged.
    #[tokio::test]
    async fn a_pharmacist_may_release_quarantined_stock_and_a_cashier_may_not() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(balance(&f, "quarantined").await, 20);

        let release = |token: &'static str, atoms: i64| {
            let pool = f.pool.clone();
            let pack = f.pack_id.clone();
            let batch = f.batch_id.clone();
            async move {
                request(
                    pool,
                    "POST",
                    "/api/v1/stock-dispositions",
                    json!({
                        "idempotencyKey": Uuid::now_v7().to_string(),
                        "productPackId": pack,
                        "batchId": batch,
                        "quantityAtoms": atoms,
                        "fromStatus": "quarantined",
                        "toStatus": "sellable",
                        "reason": "Sealed strip, inspected and found fit for sale",
                        "occurredOn": TODAY
                    }),
                    Some(token),
                )
                .await
            }
        };

        // The person who took the return cannot decide it may be sold again.
        let (status, refused) = release(CASHIER, 10).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(refused["code"], "disposition_denied");
        assert_eq!(balance(&f, "sellable").await, 180);

        let (status, released) = release(PHARMACIST, 10).await;
        assert_eq!(status, StatusCode::CREATED, "{released}");
        // The physical total is unchanged; only its status moved.
        assert_eq!(balance(&f, "sellable").await, 190);
        assert_eq!(balance(&f, "quarantined").await, 10);

        // Two append-only movements, both naming the transfer that produced them.
        let pair: Vec<(String, i64)> = sqlx::query_as(
            "SELECT stock_status,quantity_delta_atoms FROM inventory_movements \
             WHERE movement_type='disposition_transfer' ORDER BY quantity_delta_atoms",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            pair,
            vec![("quarantined".to_owned(), -10), ("sellable".to_owned(), 10)]
        );
    }

    #[tokio::test]
    async fn nothing_can_be_released_that_is_not_in_quarantine() {
        let f = fixture().await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-dispositions",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "productPackId": f.pack_id,
                "batchId": f.batch_id,
                "quantityAtoms": 5,
                "fromStatus": "quarantined",
                "toStatus": "sellable",
                "reason": "Nothing is actually in quarantine",
                "occurredOn": TODAY
            }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 0);
    }

    /// Writing stock off is terminal.
    ///
    /// The release door exists so a pharmacist can judge quarantined goods fit to sell. Pointing it
    /// at written-off stock would turn it into an undo button for that judgement, which is how
    /// damaged goods quietly return to the shelf.
    #[tokio::test]
    async fn stock_written_off_is_never_transferred_back() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "non_sellable").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(balance(&f, "non_sellable").await, 20);

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-dispositions",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "productPackId": f.pack_id,
                "batchId": f.batch_id,
                "quantityAtoms": 10,
                "fromStatus": "non_sellable",
                "toStatus": "sellable",
                "reason": "On second thoughts the strip looked fine",
                "occurredOn": TODAY
            }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "fromStatus");
        assert_eq!(balance(&f, "non_sellable").await, 20);
        assert_eq!(balance(&f, "sellable").await, 180);

        // Anti-vacuity: the database refuses it too, so the rule does not live in the handler alone.
        let direct = sqlx::query(
            "INSERT INTO stock_dispositions (id,store_id,product_id,product_pack_id,batch_id,\
             quantity_atoms,from_status,to_status,reason,occurred_on,authorised_by_user_id,\
             created_at_utc,idempotency_key) VALUES (?,(SELECT store_id FROM store_identity),\
             ?,?,?,10,'non_sellable','sellable','bypassing the handler',?,\
             (SELECT id FROM users LIMIT 1),strftime('%Y-%m-%dT%H:%M:%fZ','now'),?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.product_id)
        .bind(&f.pack_id)
        .bind(&f.batch_id)
        .bind(TODAY)
        .bind(Uuid::now_v7().to_string())
        .execute(&f.pool)
        .await;
        assert!(
            direct.is_err(),
            "the CHECK constraint did not refuse a write-off reversal"
        );
    }

    /// Quarantine is a one-way door once the lot behind it expires.
    ///
    /// Stock quarantined while the lot was still good is the awkward case: nobody assessed it in
    /// time, and the expiry date passed while it sat there. Releasing it now would put expired
    /// medicine back on the shelf through the very mechanism that exists to keep it off, so the
    /// only move left is writing it off.
    #[tokio::test]
    async fn quarantined_stock_can_never_be_released_once_its_lot_has_expired() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(balance(&f, "quarantined").await, 20);

        let transfer = |to: &'static str, atoms: i64| {
            let pool = f.pool.clone();
            let pack = f.pack_id.clone();
            let batch = f.batch_id.clone();
            async move {
                request(
                    pool,
                    "POST",
                    "/api/v1/stock-dispositions",
                    json!({
                        "idempotencyKey": Uuid::now_v7().to_string(),
                        "productPackId": pack,
                        "batchId": batch,
                        "quantityAtoms": atoms,
                        "fromStatus": "quarantined",
                        "toStatus": to,
                        "reason": "Inspected after the customer returned it",
                        "occurredOn": TODAY
                    }),
                    Some(PHARMACIST),
                )
                .await
            }
        };

        // Anti-vacuity: while the lot is good this exact call succeeds, so the refusal below is
        // about the expiry and nothing else.
        let (status, released) = transfer("sellable", 10).await;
        assert_eq!(status, StatusCode::CREATED, "{released}");
        assert_eq!(balance(&f, "sellable").await, 190);

        sqlx::query("UPDATE product_batches SET expires_on='2026-05-31' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();

        let (status, refused) = transfer("sellable", 10).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "toStatus");
        // Nothing moved, so the refusal cost the pharmacy nothing it has to undo.
        assert_eq!(balance(&f, "sellable").await, 190);
        assert_eq!(balance(&f, "quarantined").await, 10);

        // Writing it off remains available, which is the only correct outcome left for it.
        let (status, written_off) = transfer("non_sellable", 10).await;
        assert_eq!(status, StatusCode::CREATED, "{written_off}");
        assert_eq!(balance(&f, "non_sellable").await, 10);
        assert_eq!(balance(&f, "quarantined").await, 0);
        assert_eq!(balance(&f, "sellable").await, 190);
    }

    /// An expired lot cannot be quarantined for possible resale, because no later assessment could
    /// make it sellable: Rule 110 forbids selling a Schedule C substance after its expiry date.
    #[tokio::test]
    async fn an_expired_lot_returned_by_a_customer_can_only_be_written_off() {
        let f = fixture().await;
        sqlx::query("UPDATE product_batches SET expires_on='2026-05-31' WHERE id=?")
            .bind(&f.batch_id)
            .execute(&f.pool)
            .await
            .unwrap();
        let id = create_sales_return(&f).await;

        let (status, refused) = add_sales_line(&f, &id, 1, 1, "quarantined").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "disposition");

        let (status, accepted) = add_sales_line(&f, &id, 1, 1, "non_sellable").await;
        assert_eq!(status, StatusCode::CREATED, "{accepted}");
        let (status, posted) = post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, "non_sellable").await, 10);
        assert_eq!(balance(&f, "quarantined").await, 0);
        assert_eq!(balance(&f, "sellable").await, 180);
    }

    // -------------------------------------------------------------------------------------
    // Purchase return
    // -------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_purchase_return_sends_stock_back_and_records_its_gst_route() {
        let f = fixture().await;
        let id = create_purchase_return(&f).await;
        let (status, added) = add_purchase_line(&f, &id, 1, 5).await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        let (status, posted) = post_purchase_return(
            &f,
            &id,
            2,
            &Uuid::now_v7().to_string(),
            "supplier_credit_note",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        assert_eq!(posted["documentNumber"], "PR/2627/000001");
        assert_eq!(posted["originalDocumentNumber"], "INV-SEED-1");
        assert_eq!(posted["counterpartyDisplayName"], "Sharma Medicals");
        assert_eq!(posted["gstRoute"], "supplier_credit_note");
        // The document is a purchase return; it is never a debit note.
        assert_eq!(posted["returnKind"], "purchase_return");
        assert_eq!(posted["taxAdjustmentStatus"], Value::Null);
        // Five of twenty strips at 60.00: 300.00 taxable, 18.00 each side, 336.00 in all.
        assert_eq!(posted["taxableValuePaise"], 30_000);
        assert_eq!(posted["cgstPaise"], 1_800);
        assert_eq!(posted["grandTotalPaise"], 33_600);

        // 200 received, 20 sold, 50 returned to the supplier.
        assert_eq!(balance(&f, "sellable").await, 130);
        let (delta, status_text): (i64, String) = sqlx::query_as(
            "SELECT quantity_delta_atoms,stock_status FROM inventory_movements \
             WHERE movement_type='purchase_return'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(delta, -50);
        assert_eq!(status_text, "sellable");
    }

    /// The original invoice bought twenty strips, but two were sold. Returnability alone would let
    /// nineteen go back; the current stock says otherwise, and the current stock wins.
    #[tokio::test]
    async fn a_purchase_return_needs_the_stock_to_still_be_there() {
        let f = fixture().await;
        assert_eq!(balance(&f, "sellable").await, 180);

        let id = create_purchase_return(&f).await;
        // Nineteen strips is within what was purchased...
        let (status, added) = add_purchase_line(&f, &id, 1, 19).await;
        assert_eq!(status, StatusCode::CREATED, "{added}");
        // ...but not within what is left on the shelf.
        let (status, refused) =
            post_purchase_return(&f, &id, 2, &Uuid::now_v7().to_string(), "fresh_supply").await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "insufficient_stock");
        assert_eq!(refused["availableAtoms"], 180);
        assert_eq!(balance(&f, "sellable").await, 180, "nothing left the store");

        // Eighteen is exactly what remains, and goes back.
        let ok = create_purchase_return(&f).await;
        add_purchase_line(&f, &ok, 1, 18).await;
        let (status, posted) =
            post_purchase_return(&f, &ok, 2, &Uuid::now_v7().to_string(), "fresh_supply").await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(balance(&f, "sellable").await, 0);
    }

    /// Quarantined goods are not the supplier's to take back on a sellable-stock return either.
    #[tokio::test]
    async fn a_purchase_return_cannot_draw_on_quarantined_stock() {
        let f = fixture().await;
        // Put two strips into quarantine through a sales return, leaving 180 sellable.
        let sales = create_sales_return(&f).await;
        add_sales_line(&f, &sales, 1, 2, "quarantined").await;
        post_sales_return(&f, &sales, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(balance(&f, "sellable").await, 180);
        assert_eq!(balance(&f, "quarantined").await, 20);

        let id = create_purchase_return(&f).await;
        add_purchase_line(&f, &id, 1, 19).await;
        let (status, refused) =
            post_purchase_return(&f, &id, 2, &Uuid::now_v7().to_string(), "fresh_supply").await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        // 190 atoms are wanted and only 180 are sellable, even though 200 are in the building.
        assert_eq!(refused["availableAtoms"], 180);
    }

    #[tokio::test]
    async fn a_purchase_return_states_no_disposition_because_the_goods_leave() {
        let f = fixture().await;
        let id = create_purchase_return(&f).await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/lines"),
            json!({
                "expectedRevision": 1,
                "originalLineId": f.purchase_line_id,
                "quantity": 1,
                "disposition": "quarantined"
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "disposition_not_allowed");
    }

    // -------------------------------------------------------------------------------------
    // GST evidence
    // -------------------------------------------------------------------------------------

    /// The service validates the combination; it never decides the law from the customer's
    /// registration status. See ADR-017.
    #[tokio::test]
    async fn the_gst_fields_belong_to_their_own_side_and_cannot_cross_over() {
        let f = fixture().await;

        let sale_return = create_sales_return(&f).await;
        add_sales_line(&f, &sale_return, 1, 1, "quarantined").await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{sale_return}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "gstRoute": "fresh_supply"
            }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "gst_route_not_allowed");

        // And a sales return must say which it is, rather than having it guessed.
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{sale_return}/post"),
            json!({ "expectedRevision": 2, "idempotencyKey": Uuid::now_v7().to_string() }),
            Some(PHARMACIST),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "tax_adjustment_status_required");

        let purchase_return = create_purchase_return(&f).await;
        add_purchase_line(&f, &purchase_return, 1, 1).await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{purchase_return}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "taxAdjustmentStatus": "tax_adjustable"
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "tax_adjustment_status_not_allowed");
    }

    /// The supplier's credit note arrives days later and is recorded without touching the posted
    /// return. It is evidence of what the supplier did — never our own tax document.
    #[tokio::test]
    async fn a_supplier_credit_note_is_recorded_as_separate_evidence() {
        let f = fixture().await;
        let id = create_purchase_return(&f).await;
        add_purchase_line(&f, &id, 1, 5).await;
        post_purchase_return(
            &f,
            &id,
            2,
            &Uuid::now_v7().to_string(),
            "supplier_credit_note",
        )
        .await;

        let evidence = json!({
            "creditNoteNumber": "SUPP-CN-9001",
            "creditNoteDate": "2026-06-20",
            "creditNoteAmountPaise": 33_600
        });
        let (status, recorded) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/supplier-credit-notes"),
            evidence.clone(),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{recorded}");

        let body = detail(&f, &id).await;
        assert_eq!(
            body["supplierCreditNotes"][0]["creditNoteNumber"],
            "SUPP-CN-9001"
        );
        // The posted return itself is untouched.
        assert_eq!(body["status"], "posted");
        assert_eq!(body["grandTotalPaise"], 33_600);

        // The same note twice is a duplicate, not a second credit.
        let (status, duplicate) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/supplier-credit-notes"),
            evidence,
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
        assert_eq!(duplicate["code"], "duplicate_supplier_credit_note");
    }

    /// Evidence of a supplier credit note makes no sense against a return that elected the other
    /// route, or against a sales return, and the database refuses both.
    #[tokio::test]
    async fn supplier_credit_note_evidence_belongs_only_to_that_route() {
        let f = fixture().await;
        let id = create_purchase_return(&f).await;
        add_purchase_line(&f, &id, 1, 5).await;
        post_purchase_return(&f, &id, 2, &Uuid::now_v7().to_string(), "fresh_supply").await;

        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/supplier-credit-notes"),
            json!({
                "creditNoteNumber": "SUPP-CN-1",
                "creditNoteDate": "2026-06-20",
                "creditNoteAmountPaise": 100
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "supplier_credit_note_route_conflict");
    }

    // -------------------------------------------------------------------------------------
    // Snapshots, numbering, idempotency, immutability
    // -------------------------------------------------------------------------------------

    /// A reversal reuses the original's frozen tax facts. If it resolved today's rate instead, a
    /// rate change between sale and return would make the reversal unequal to the sale.
    #[tokio::test]
    async fn a_return_reuses_the_original_rate_even_after_the_rate_changes() {
        let f = fixture().await;
        // The tax the world now charges is 28%, and it must make no difference at all.
        sqlx::query("UPDATE tax_rate_versions SET cgst_basis_points=1400,sgst_basis_points=1400")
            .execute(&f.pool)
            .await
            .unwrap();

        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "quarantined").await;
        let (status, posted) = post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        // The original's 6% + 6%, not today's 14% + 14%.
        assert_eq!(posted["cgstPaise"], 960);
        assert_eq!(posted["sgstPaise"], 960);
        assert_eq!(posted["grandTotalPaise"], 17_920);
        assert_eq!(posted["lines"][0]["cgstBasisPoints"], 600);
    }

    /// And the identity snapshot is the original's too, so a posted return reprints as issued.
    #[tokio::test]
    async fn a_posted_return_keeps_its_own_snapshot_after_the_catalogue_moves_on() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        sqlx::query("UPDATE products SET display_name='Crocin 500 (renamed)' WHERE id=?")
            .bind(&f.product_id)
            .execute(&f.pool)
            .await
            .unwrap();

        let body = detail(&f, &id).await;
        assert_eq!(body["lines"][0]["productDisplayName"], "Crocin 500");
        assert_eq!(body["lines"][0]["batchNumber"], "B-2601");
        assert_eq!(body["lines"][0]["hsnCode"], "30049099");
    }

    /// The two kinds number independently, and each restarts per financial year.
    #[tokio::test]
    async fn each_return_kind_numbers_in_its_own_series() {
        let f = fixture().await;
        let sales = create_sales_return(&f).await;
        add_sales_line(&f, &sales, 1, 1, "quarantined").await;
        let (_, posted_sales) = post_sales_return(&f, &sales, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(posted_sales["documentNumber"], "SR/2627/000001");

        let purchase = create_purchase_return(&f).await;
        add_purchase_line(&f, &purchase, 1, 1).await;
        let (_, posted_purchase) = post_purchase_return(
            &f,
            &purchase,
            2,
            &Uuid::now_v7().to_string(),
            "fresh_supply",
        )
        .await;
        // Its own series, starting at one, not continuing the sales-return count.
        assert_eq!(posted_purchase["documentNumber"], "PR/2627/000001");

        // Both obey the statutory sixteen-character limit.
        for number in [
            posted_sales["documentNumber"].as_str().unwrap(),
            posted_purchase["documentNumber"].as_str().unwrap(),
        ] {
            assert!(number.len() <= 16, "{number} exceeds the statutory limit");
        }
    }

    #[tokio::test]
    async fn a_replay_of_the_same_posting_returns_the_original_return() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        let key = Uuid::now_v7().to_string();
        let (status, first) = post_sales_return(&f, &id, 2, &key).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, replay) = post_sales_return(&f, &id, 2, &key).await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["documentNumber"], first["documentNumber"]);
        assert_eq!(replay["revision"], first["revision"]);
        // Nothing was duplicated.
        assert_eq!(balance(&f, "quarantined").await, 10);
        let movements: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inventory_movements WHERE movement_type='sales_return'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(movements, 1);
    }

    #[tokio::test]
    async fn the_same_key_with_different_facts_is_refused() {
        let f = fixture().await;
        let key = Uuid::now_v7().to_string();
        let first = create_sales_return(&f).await;
        add_sales_line(&f, &first, 1, 1, "quarantined").await;
        let (status, posted) = post_sales_return(&f, &first, 2, &key).await;
        assert_eq!(status, StatusCode::OK, "{posted}");

        let second = create_sales_return(&f).await;
        add_sales_line(&f, &second, 1, 1, "quarantined").await;
        let (status, refused) = post_sales_return(&f, &second, 2, &key).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "idempotency_conflict");

        // And the refused posting left nothing behind: no number, no movement, no snapshot.
        let after = detail(&f, &second).await;
        assert_eq!(after["status"], "draft");
        assert_eq!(after["documentNumber"], Value::Null);
        assert_eq!(balance(&f, "quarantined").await, 10);
        let next_value: i64 = sqlx::query_scalar(
            "SELECT next_value FROM document_number_series WHERE document_kind='sales_return'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(next_value, 2, "a refused posting consumes no number");

        // The next valid return takes the very next number.
        let third = create_sales_return(&f).await;
        add_sales_line(&f, &third, 1, 1, "quarantined").await;
        let (status, posted) = post_sales_return(&f, &third, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        assert_eq!(posted["documentNumber"], "SR/2627/000002");
    }

    #[tokio::test]
    async fn a_posted_return_cannot_be_changed_by_any_route() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        let (status, posted) = post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let line_id = posted["lines"][0]["id"].as_str().unwrap().to_owned();
        let revision = posted["revision"].as_i64().unwrap();

        let (status, refused) = add_sales_line(&f, &id, revision, 1, "quarantined").await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "return_not_draft");

        let (status, refused) = request(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/returns/{id}"),
            json!({ "expectedRevision": revision, "businessDate": TODAY }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "return_not_draft");

        let (status, refused) = request(
            f.pool.clone(),
            "DELETE",
            &format!("/api/v1/return-lines/{line_id}"),
            json!({ "expectedRevision": revision }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "return_not_draft");
    }

    /// The original document is never touched by returning against it.
    #[tokio::test]
    async fn returning_never_mutates_the_original_sale() {
        let f = fixture().await;
        let (_, before) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{}", f.sale_id),
            Value::Null,
            Some(OWNER),
        )
        .await;

        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 2, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        let (_, after) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/sales/{}", f.sale_id),
            Value::Null,
            Some(OWNER),
        )
        .await;
        assert_eq!(before, after, "the original invoice must be untouched");
    }

    #[tokio::test]
    async fn a_posting_from_a_stale_view_is_refused() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        let (status, refused) = post_sales_return(&f, &id, 1, &Uuid::now_v7().to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
        assert_eq!(refused["currentRevision"], 2);
        assert_eq!(balance(&f, "quarantined").await, 0);
    }

    // -------------------------------------------------------------------------------------
    // Authorisation and audit
    // -------------------------------------------------------------------------------------

    /// Taking a return is counter work; deciding its tax character and its disposition is not.
    #[tokio::test]
    async fn posting_a_sales_return_needs_a_pharmacist() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{id}/post"),
            json!({
                "expectedRevision": 2,
                "idempotencyKey": Uuid::now_v7().to_string(),
                "taxAdjustmentStatus": "commercial_only"
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(balance(&f, "quarantined").await, 0);
    }

    /// Sending goods back to a supplier is the owner's commercial decision.
    #[tokio::test]
    async fn posting_a_purchase_return_needs_the_owner() {
        let f = fixture().await;
        let id = create_purchase_return(&f).await;
        add_purchase_line(&f, &id, 1, 1).await;
        for token in [CASHIER, PHARMACIST] {
            let (status, refused) = request(
                f.pool.clone(),
                "POST",
                &format!("/api/v1/returns/{id}/post"),
                json!({
                    "expectedRevision": 2,
                    "idempotencyKey": Uuid::now_v7().to_string(),
                    "gstRoute": "fresh_supply"
                }),
                Some(token),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        }
        assert_eq!(balance(&f, "sellable").await, 180);
    }

    #[tokio::test]
    async fn an_unauthenticated_request_can_do_nothing() {
        let f = fixture().await;
        let (status, refused) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({
                "returnKind": "sales_return",
                "originalDocumentId": f.sale_id,
                "businessDate": TODAY
            }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{refused}");
    }

    /// Every posting writes an audit row naming what happened and who did it.
    #[tokio::test]
    async fn a_posted_return_leaves_an_audit_trail() {
        let f = fixture().await;
        let id = create_sales_return(&f).await;
        add_sales_line(&f, &id, 1, 1, "quarantined").await;
        post_sales_return(&f, &id, 2, &Uuid::now_v7().to_string()).await;

        let (action, payload): (String, String) = sqlx::query_as(
            "SELECT action,change_payload FROM master_change_events \
             WHERE entity_type='return_document' AND entity_id=? AND action='posted'",
        )
        .bind(&id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(action, "posted");
        assert!(payload.contains("SR/2627/000001"), "{payload}");
        assert!(payload.contains("INV/2627/000001"), "{payload}");

        // And the disposition release writes its own.
        request(
            f.pool.clone(),
            "POST",
            "/api/v1/stock-dispositions",
            json!({
                "idempotencyKey": Uuid::now_v7().to_string(),
                "productPackId": f.pack_id,
                "batchId": f.batch_id,
                "quantityAtoms": 10,
                "fromStatus": "quarantined",
                "toStatus": "sellable",
                "reason": "Inspected and found fit",
                "occurredOn": TODAY
            }),
            Some(PHARMACIST),
        )
        .await;
        let dispositions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM master_change_events WHERE entity_type='stock_disposition'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(dispositions, 1);
        let _ = f.store_id;
        let _ = f.owner_id;
    }

    // -------------------------------------------------------------------------------------
    // Harness
    // -------------------------------------------------------------------------------------

    async fn create_sales_return(f: &Fixture) -> String {
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({
                "returnKind": "sales_return",
                "originalDocumentId": f.sale_id,
                "businessDate": TODAY
            }),
            Some(CASHIER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["id"].as_str().unwrap().to_owned()
    }

    async fn create_purchase_return(f: &Fixture) -> String {
        let (status, created) = request(
            f.pool.clone(),
            "POST",
            "/api/v1/returns",
            json!({
                "returnKind": "purchase_return",
                "originalDocumentId": f.purchase_id,
                "businessDate": TODAY
            }),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created["id"].as_str().unwrap().to_owned()
    }

    async fn add_sales_line(
        f: &Fixture,
        return_id: &str,
        revision: i64,
        quantity: i64,
        disposition: &str,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": revision,
                "originalLineId": f.sale_line_id,
                "quantity": quantity,
                "disposition": disposition
            }),
            Some(CASHIER),
        )
        .await
    }

    async fn add_purchase_line(
        f: &Fixture,
        return_id: &str,
        revision: i64,
        quantity: i64,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/lines"),
            json!({
                "expectedRevision": revision,
                "originalLineId": f.purchase_line_id,
                "quantity": quantity
            }),
            Some(OWNER),
        )
        .await
    }

    async fn post_sales_return(
        f: &Fixture,
        return_id: &str,
        revision: i64,
        key: &str,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": key,
                "taxAdjustmentStatus": "commercial_only",
                "taxAdjustmentReason": "Tax was passed on to a walk-in customer"
            }),
            Some(PHARMACIST),
        )
        .await
    }

    async fn post_purchase_return(
        f: &Fixture,
        return_id: &str,
        revision: i64,
        key: &str,
        route: &str,
    ) -> (StatusCode, Value) {
        request(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/returns/{return_id}/post"),
            json!({
                "expectedRevision": revision,
                "idempotencyKey": key,
                "gstRoute": route
            }),
            Some(OWNER),
        )
        .await
    }

    async fn detail(f: &Fixture, id: &str) -> Value {
        let (status, body) = request(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/returns/{id}"),
            Value::Null,
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// The balance of one lot in one stock status, derived exactly as the service derives it.
    async fn balance(f: &Fixture, status: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
             WHERE batch_id=? AND stock_status=?",
        )
        .bind(&f.batch_id)
        .bind(status)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    }

    /// The seller facts a pharmacy must hold before it may issue a memo: a registered name, an
    /// operating address, and an active drug sale licence. Recorded here because a Sale cannot be
    /// posted without them — see `store_legal_profile_incomplete`.
    async fn record_legal_profile(pool: &SqlitePool, store_id: &str) {
        sqlx::query(
            "UPDATE store_identity SET legal_name='Care Pharmacy Private Limited',\
             primary_phone='02012345678',primary_email='care@example.test' WHERE store_id=?",
        )
        .bind(store_id)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO store_addresses (id,store_id,line1,city,state_id,postal_code,\
             created_at_utc,updated_at_utc) VALUES (?,?,'12 Market Road','Pune',?,'411001',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .bind(MAHARASHTRA)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO store_licences (id,store_id,licence_type,licence_number,\
             normalized_licence_number,created_at_utc,updated_at_utc,include_on_retail_memo) \
             VALUES (?,?,'Form 20','MH-20-1234','MH201234',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'),1)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(store_id)
        .execute(pool)
        .await
        .unwrap();
        // Phase 1L-A3: the facts a registered pharmacy records before selling. Turnover never
        // crossed the e-invoicing threshold, and it was up to Rs 5 crore in the year before the
        // fixture's financial year — so HSN is required on B2B invoices only.
        sqlx::query(
            "UPDATE store_identity SET rule46s_declaration_applicability='not_applicable',\
             einvoice_applicability='not_required',\
             dynamic_qr_applicability='not_required',\
             hsn_turnover_band='up_to_5_crore',hsn_turnover_financial_year='2026-27' \
             WHERE store_id=?",
        )
        .bind(store_id)
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
        .bind(format!("{role} return user"))
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
}
