//! The canonical invoice document, assembled from posted facts alone.
//!
//! One endpoint, one shape, one rule: **every value in this document comes from the posted Sale.**
//! Not from `products`, not from `parties`, not from `store_identity`. A pharmacy that renames a
//! medicine, corrects its own address, or archives a supplier must be able to reprint a two-year-old
//! bill and get the bill it issued — and the only way to guarantee that is for the renderer to have
//! nowhere else to look.
//!
//! The one exception is explicit and labelled. A Sale posted before the seller snapshot existed
//! carries `sellerSnapshot: null` and `legacyDocument: true`; the current Store details travel in a
//! separate `currentSellerProfile` object so nothing can mistake them for history. They are never
//! merged into the snapshot, because a document that quietly showed today's address as though it
//! were yesterday's would be worse than one that admits it does not know.
//!
//! This module renders nothing. It produces the facts a renderer will need, so that when printing
//! arrives it has one source instead of six.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use super::auth::{self, AuthError};
use super::reference_masters::ReferenceState;
use crate::domain::catalog::validate_uuid_v7;

#[derive(Debug)]
pub(crate) enum InvoiceError {
    Auth(AuthError),
    Validation(&'static str, &'static str),
    NotFound,
    /// A draft has no invoice number and is not a document. Asking for one is a programming error
    /// in the caller, not a business outcome.
    NotPosted,
    /// The posted line totals, the tax components and the header disagree. This cannot be corrected
    /// here — the posted row is the record — so it is surfaced rather than papered over.
    InvariantFailed(#[allow(dead_code)] &'static str),
    Internal,
}

impl From<AuthError> for InvoiceError {
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
    }
}

impl IntoResponse for InvoiceError {
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
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("invoice_not_found", "That sale no longer exists."),
            ),
            Self::NotPosted => (
                StatusCode::CONFLICT,
                simple(
                    "invoice_not_posted",
                    "This sale has not been posted, so it has no invoice yet.",
                ),
            ),
            Self::InvariantFailed(_) => (
                StatusCode::CONFLICT,
                simple(
                    "invoice_invariant_failed",
                    "This invoice does not add up and cannot be shown. Report it before using it.",
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

pub fn routes() -> Router<ReferenceState> {
    Router::new().route("/api/v1/sales/{id}/invoice", get(get_invoice))
}

// ---------------------------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------------------------

/// What kind of document this Sale actually is, decided from posted facts.
///
/// `Unresolved` is a real answer, not a failure. A registered seller supplying a mixed taxable and
/// exempt basket to a REGISTERED recipient falls outside Rule 46A, which is written for unregistered
/// recipients, and this project does not guess through a tax question. The Sale still reads; what is
/// withheld is the claim about which statutory document it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    TaxInvoice,
    BillOfSupply,
    InvoiceCumBillOfSupply,
    /// The seller holds no GST registration, so nothing here is a GST document. The Drugs Rules
    /// memo obligation is unaffected and still applies.
    RetailCashMemo,
    DocumentClassificationUnresolved,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentSection {
    document_number: String,
    business_date: String,
    financial_year: String,
    series_code: String,
    posted_at_utc: String,
    document_type: DocumentType,
    /// Present only when the type is unresolved, saying plainly what could not be decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    document_type_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SellerSnapshotSection {
    legal_name: String,
    trade_name: Option<String>,
    address_line1: String,
    address_line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_name: Option<String>,
    state_code: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    licence_text: String,
    gst_registration_status: Option<String>,
    gstin: Option<String>,
}

/// The Store as it stands today. Only ever populated for a legacy document, and never merged into
/// the snapshot above.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentSellerProfile {
    display_name: String,
    legal_name: Option<String>,
    address_line1: Option<String>,
    city: Option<String>,
    licence_text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecipientSection {
    /// True when nobody was identified: the counter customer who wanted no name on the bill.
    walk_in: bool,
    /// The name as it was recorded on the document, whether from a party or typed at the counter.
    name: Option<String>,
    gst_registration_status: Option<String>,
    gstin: Option<String>,
    /// The customer's place-of-supply State code frozen by Phase 1H. Not the address State below.
    state_code: Option<String>,
    /// 0: posted before recipient particulars were evaluated. The address and delivery facts below
    /// are then UNKNOWN — not absent — and are reported as null without consulting the Party master.
    /// 1: every field below is the fact frozen at posting.
    snapshot_version: i64,
    /// Rule 46(f), as the operator recorded it. Null on a version-0 Sale.
    particulars_requested: Option<bool>,
    /// Present exactly when a Rule 46 particular required an address and one was frozen.
    address: Option<RecipientAddress>,
    /// Present exactly when Rule 46(e)/(f) asked for an address of delivery.
    delivery: Option<DeliverySection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecipientAddress {
    /// `party` (the customer's record at posting) or `counter` (typed for a walk-in).
    source: String,
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_name: Option<String>,
    state_code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliverySection {
    same_as_recipient: bool,
    /// Present only when delivery was to a different address.
    address: Option<DeliveryAddress>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryAddress {
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    state_name: Option<String>,
    state_code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InvoiceLine {
    line_number: i64,
    description: String,
    pack_label: Option<String>,
    batch_number: String,
    expires_on: Option<String>,
    hsn_code: Option<String>,
    quantity_text: String,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    quantity_scale: Option<i64>,
    unit_label: Option<String>,
    mrp_paise: Option<i64>,
    selling_rate_paise: i64,
    tax_treatment_kind: Option<String>,
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

/// One row of the tax summary: every line sharing a treatment and a rate, added up.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaxSummaryRow {
    tax_treatment_kind: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TotalsSection {
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TenderRow {
    method: String,
    amount_paise: i64,
    reference_text: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegulatorySection {
    /// No seller snapshot was recorded, because the Sale predates them.
    legacy_document: bool,
    seller_snapshot_version: i64,
    /// Whether the seller held a GST registration at the moment of sale.
    seller_registered: bool,
    tax_treatment: Option<String>,
    /// Deliberately absent from this product: no e-invoice, no IRN, no QR. Stated so a renderer
    /// cannot mistake absence for "not yet fetched".
    einvoice_applicable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceResponse {
    sale_id: String,
    document: DocumentSection,
    seller_snapshot: Option<SellerSnapshotSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_seller_profile: Option<CurrentSellerProfile>,
    recipient: RecipientSection,
    lines: Vec<InvoiceLine>,
    tax_summary: Vec<TaxSummaryRow>,
    totals: TotalsSection,
    tender: Vec<TenderRow>,
    regulatory: RegulatorySection,
}

// ---------------------------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------------------------

#[derive(Debug, FromRow)]
struct HeaderRow {
    id: String,
    status: String,
    business_date: String,
    series_code: Option<String>,
    financial_year: Option<String>,
    document_number: Option<String>,
    posted_at_utc: Option<String>,
    store_gst_registration_status: Option<String>,
    store_normalized_gstin: Option<String>,
    store_state_code: Option<String>,
    customer_party_id: Option<String>,
    customer_name_text: Option<String>,
    customer_display_name: Option<String>,
    customer_gst_registration_status: Option<String>,
    customer_normalized_gstin: Option<String>,
    customer_state_code: Option<String>,
    tax_treatment: Option<String>,
    taxable_value_paise: i64,
    cgst_paise: i64,
    sgst_paise: i64,
    igst_paise: i64,
    cess_paise: i64,
    grand_total_paise: i64,
    seller_snapshot_version: i64,
    seller_legal_name: Option<String>,
    seller_trade_name: Option<String>,
    seller_address_line1: Option<String>,
    seller_address_line2: Option<String>,
    seller_city: Option<String>,
    seller_postal_code: Option<String>,
    seller_state_name: Option<String>,
    seller_phone: Option<String>,
    seller_email: Option<String>,
    seller_licence_text: Option<String>,
    recipient_snapshot_version: i64,
    recipient_particulars_requested: Option<bool>,
    recipient_address_source: Option<String>,
    recipient_address_line1: Option<String>,
    recipient_address_line2: Option<String>,
    recipient_city: Option<String>,
    recipient_postal_code: Option<String>,
    recipient_state_name: Option<String>,
    recipient_state_code: Option<String>,
    delivery_same_as_recipient: Option<bool>,
    delivery_address_line1: Option<String>,
    delivery_address_line2: Option<String>,
    delivery_city: Option<String>,
    delivery_postal_code: Option<String>,
    delivery_state_name: Option<String>,
    delivery_state_code: Option<String>,
}

#[derive(Debug, FromRow)]
struct LineRow {
    line_number: i64,
    product_display_name: Option<String>,
    pack_display_label: Option<String>,
    base_unit_label: Option<String>,
    batch_number: Option<String>,
    batch_expires_on: Option<String>,
    batch_mrp_paise: Option<i64>,
    hsn_code: Option<String>,
    quantity_basis: String,
    quantity_packs: Option<i64>,
    quantity_atoms: i64,
    quantity_scale: Option<i64>,
    selling_rate_paise: i64,
    tax_treatment_kind: Option<String>,
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

async fn get_invoice(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<InvoiceResponse>, InvoiceError> {
    // Reading a posted Sale is already open to every operational role — a cashier who sold it can
    // see it — and an invoice is a view of that same Sale, so it inherits that policy rather than
    // inventing a stricter one that would stop a counter from answering "what did I charge?".
    auth::require_authenticated_actor(&state.pool, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(|_| InvoiceError::Validation("id", "must be a UUIDv7"))?;
    build(&state.pool, &id).await.map(Json)
}

pub(crate) async fn build(pool: &SqlitePool, id: &str) -> Result<InvoiceResponse, InvoiceError> {
    let header = sqlx::query_as::<_, HeaderRow>(
        "SELECT id,status,business_date,series_code,financial_year,document_number,posted_at_utc,\
         store_gst_registration_status,store_normalized_gstin,store_state_code,customer_party_id,\
         customer_name_text,customer_display_name,customer_gst_registration_status,\
         customer_normalized_gstin,customer_state_code,tax_treatment,taxable_value_paise,\
         cgst_paise,sgst_paise,igst_paise,cess_paise,grand_total_paise,seller_snapshot_version,\
         seller_legal_name,seller_trade_name,seller_address_line1,seller_address_line2,seller_city,\
         seller_postal_code,seller_state_name,seller_phone,seller_email,seller_licence_text,\
         recipient_snapshot_version,recipient_particulars_requested,recipient_address_source,\
         recipient_address_line1,recipient_address_line2,recipient_city,recipient_postal_code,\
         recipient_state_name,recipient_state_code,delivery_same_as_recipient,\
         delivery_address_line1,delivery_address_line2,delivery_city,delivery_postal_code,\
         delivery_state_name,delivery_state_code \
         FROM sale_documents WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?
    .ok_or(InvoiceError::NotFound)?;

    if header.status != "posted" {
        return Err(InvoiceError::NotPosted);
    }

    let rows = sqlx::query_as::<_, LineRow>(
        "SELECT line_number,product_display_name,pack_display_label,base_unit_label,batch_number,\
         batch_expires_on,batch_mrp_paise,hsn_code,quantity_basis,quantity_packs,quantity_atoms,\
         quantity_scale,selling_rate_paise,tax_treatment_kind,cgst_basis_points,sgst_basis_points,\
         igst_basis_points,cess_basis_points,taxable_value_paise,cgst_paise,sgst_paise,igst_paise,\
         cess_paise,line_total_paise FROM sale_lines WHERE sale_document_id=? ORDER BY line_number",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?;

    let tender = sqlx::query_as::<_, (String, i64, Option<String>)>(
        "SELECT method,amount_paise,reference_text FROM sale_tenders WHERE sale_document_id=? \
         ORDER BY created_at_utc,id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?;

    let lines: Vec<InvoiceLine> = rows.iter().map(invoice_line).collect();
    let tax_summary = summarise(&rows);
    check_invariants(&header, &lines, &tax_summary, &tender)?;

    let seller_registered = header.store_gst_registration_status.as_deref() == Some("registered");
    let (document_type, document_type_reason) = classify(&header, &rows, seller_registered);
    let legacy = header.seller_snapshot_version < 1;

    let seller_snapshot = (!legacy).then(|| SellerSnapshotSection {
        legal_name: header.seller_legal_name.clone().unwrap_or_default(),
        trade_name: header.seller_trade_name.clone(),
        address_line1: header.seller_address_line1.clone().unwrap_or_default(),
        address_line2: header.seller_address_line2.clone(),
        city: header.seller_city.clone(),
        postal_code: header.seller_postal_code.clone(),
        state_name: header.seller_state_name.clone(),
        state_code: header.store_state_code.clone(),
        phone: header.seller_phone.clone(),
        email: header.seller_email.clone(),
        licence_text: header.seller_licence_text.clone().unwrap_or_default(),
        gst_registration_status: header.store_gst_registration_status.clone(),
        gstin: header.store_normalized_gstin.clone(),
    });

    let current_seller_profile = if legacy {
        Some(current_profile(pool).await?)
    } else {
        None
    };

    Ok(InvoiceResponse {
        sale_id: header.id.clone(),
        document: DocumentSection {
            document_number: header.document_number.clone().unwrap_or_default(),
            business_date: header.business_date.clone(),
            financial_year: header.financial_year.clone().unwrap_or_default(),
            series_code: header.series_code.clone().unwrap_or_default(),
            posted_at_utc: header.posted_at_utc.clone().unwrap_or_default(),
            document_type,
            document_type_reason,
        },
        seller_snapshot,
        current_seller_profile,
        recipient: RecipientSection {
            walk_in: header.customer_party_id.is_none(),
            name: header
                .customer_display_name
                .clone()
                .or_else(|| header.customer_name_text.clone()),
            gst_registration_status: header.customer_gst_registration_status.clone(),
            gstin: header.customer_normalized_gstin.clone(),
            state_code: header.customer_state_code.clone(),
            snapshot_version: header.recipient_snapshot_version,
            particulars_requested: recipient_facts(&header, header.recipient_particulars_requested),
            address: recipient_address(&header),
            delivery: delivery_section(&header),
        },
        lines,
        tax_summary,
        totals: TotalsSection {
            taxable_value_paise: header.taxable_value_paise,
            cgst_paise: header.cgst_paise,
            sgst_paise: header.sgst_paise,
            igst_paise: header.igst_paise,
            cess_paise: header.cess_paise,
            grand_total_paise: header.grand_total_paise,
        },
        tender: tender
            .into_iter()
            .map(|(method, amount_paise, reference_text)| TenderRow {
                method,
                amount_paise,
                reference_text,
            })
            .collect(),
        regulatory: RegulatorySection {
            legacy_document: legacy,
            seller_snapshot_version: header.seller_snapshot_version,
            seller_registered,
            tax_treatment: header.tax_treatment.clone(),
            einvoice_applicable: false,
        },
    })
}

/// The Store as it stands now, for a legacy document's separate panel.
async fn current_profile(pool: &SqlitePool) -> Result<CurrentSellerProfile, InvoiceError> {
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT display_name,legal_name FROM store_identity LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?
    .ok_or(InvoiceError::Internal)?;
    let address = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT line1,city FROM store_addresses LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?;
    let licences = sqlx::query_as::<_, (String, String)>(
        "SELECT licence_type,licence_number FROM store_licences WHERE status='active'",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| InvoiceError::Internal)?;
    let licence_text = crate::domain::store_profile::licence_text(
        &licences
            .into_iter()
            .map(
                |(licence_type, licence_number)| crate::domain::store_profile::SellerLicence {
                    normalized_licence_number: crate::domain::parties::normalize_licence_comparison(
                        &licence_number,
                    ),
                    licence_type,
                    licence_number,
                },
            )
            .collect::<Vec<_>>(),
    );
    Ok(CurrentSellerProfile {
        display_name: row.0,
        legal_name: row.1,
        address_line1: address.as_ref().map(|value| value.0.clone()),
        city: address.and_then(|value| value.1),
        licence_text,
    })
}

/// Which statutory document this Sale is, from posted facts alone.
fn classify(
    header: &HeaderRow,
    rows: &[LineRow],
    seller_registered: bool,
) -> (DocumentType, Option<&'static str>) {
    if !seller_registered {
        // No registration, so no GST document. Section 31 speaks to registered persons; the Drugs
        // Rules memo obligation is separate and still applies.
        return (DocumentType::RetailCashMemo, None);
    }
    let taxable = rows
        .iter()
        .any(|row| row.tax_treatment_kind.as_deref() == Some("taxable"));
    let untaxed = rows.iter().any(|row| {
        matches!(
            row.tax_treatment_kind.as_deref(),
            Some("exempt") | Some("nil_rated") | Some("non_gst")
        )
    });
    match (taxable, untaxed) {
        (true, false) => (DocumentType::TaxInvoice, None),
        (false, true) => (DocumentType::BillOfSupply, None),
        (true, true) => {
            if header.customer_gst_registration_status.as_deref() == Some("registered") {
                // Rule 46A permits one "invoice-cum-bill of supply" where a registered person
                // supplies taxable and exempted goods TO AN UNREGISTERED PERSON. It says nothing
                // about a registered recipient, and this project does not guess through a tax
                // question. The Sale still reads; only the claim about its document type is withheld.
                (
                    DocumentType::DocumentClassificationUnresolved,
                    Some(
                        "A mixed taxable and exempt supply to a registered recipient is not covered \
                         by Rule 46A, and the document type has not been settled.",
                    ),
                )
            } else {
                (DocumentType::InvoiceCumBillOfSupply, None)
            }
        }
        // No line declared a treatment at all. That is not a classification question, it is missing
        // data, and saying "tax invoice" would be inventing one.
        (false, false) => (
            DocumentType::DocumentClassificationUnresolved,
            Some("No line on this sale records a tax treatment."),
        ),
    }
}

fn invoice_line(row: &LineRow) -> InvoiceLine {
    InvoiceLine {
        line_number: row.line_number,
        description: row.product_display_name.clone().unwrap_or_default(),
        pack_label: row.pack_display_label.clone(),
        batch_number: row.batch_number.clone().unwrap_or_default(),
        expires_on: row.batch_expires_on.clone(),
        hsn_code: row.hsn_code.clone(),
        quantity_text: quantity_text(row),
        quantity_basis: row.quantity_basis.clone(),
        quantity_packs: row.quantity_packs,
        quantity_atoms: row.quantity_atoms,
        quantity_scale: row.quantity_scale,
        unit_label: row.base_unit_label.clone(),
        mrp_paise: row.batch_mrp_paise,
        selling_rate_paise: row.selling_rate_paise,
        tax_treatment_kind: row.tax_treatment_kind.clone(),
        cgst_basis_points: row.cgst_basis_points,
        sgst_basis_points: row.sgst_basis_points,
        igst_basis_points: row.igst_basis_points,
        cess_basis_points: row.cess_basis_points,
        taxable_value_paise: row.taxable_value_paise,
        cgst_paise: row.cgst_paise,
        sgst_paise: row.sgst_paise,
        igst_paise: row.igst_paise,
        cess_paise: row.cess_paise,
        line_total_paise: row.line_total_paise,
    }
}

/// What a human should read on the line: "2 Strip of 10", "3 Tablet", "0.5 ml".
///
/// Atoms never reach the page. A fractional quantity is produced by inserting a decimal point into
/// the integer string — there is no division and no floating point anywhere in this function,
/// because a bill that reads 0.30000000000000004 ml is not a bill.
fn quantity_text(row: &LineRow) -> String {
    let unit = row.base_unit_label.as_deref().unwrap_or("units");
    if row.quantity_basis == "pack" {
        let packs = row.quantity_packs.unwrap_or(row.quantity_atoms);
        return match row.pack_display_label.as_deref() {
            Some(label) => format!("{packs} × {label}"),
            None => format!("{packs}"),
        };
    }
    format!("{} {unit}", scaled(row.quantity_atoms, row.quantity_scale))
}

/// An integer and its scale as a decimal string, by string surgery rather than arithmetic.
fn scaled(atoms: i64, scale: Option<i64>) -> String {
    // A line posted before the scale was recorded renders as whole atoms. Those Sales already
    // report themselves as legacy, so the limitation travels with a document that admits it.
    let Some(scale) = scale.filter(|value| *value > 0) else {
        return atoms.to_string();
    };
    let places = scale as usize;
    let negative = atoms < 0;
    let digits = atoms.unsigned_abs().to_string();
    let padded = if digits.len() <= places {
        format!("{}{}", "0".repeat(places - digits.len() + 1), digits)
    } else {
        digits
    };
    let split = padded.len() - places;
    let whole = &padded[..split];
    let fraction = padded[split..].trim_end_matches('0');
    let text = if fraction.is_empty() {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    };
    if negative { format!("-{text}") } else { text }
}

/// Aggregation, never recomputation.
///
/// The posted line already holds the tax that was charged. This adds identical rates together so a
/// document can show one row per rate; it does not multiply anything by anything.
fn summarise(rows: &[LineRow]) -> Vec<TaxSummaryRow> {
    let mut summary: Vec<TaxSummaryRow> = Vec::new();
    for row in rows {
        let existing = summary.iter_mut().find(|candidate| {
            candidate.tax_treatment_kind == row.tax_treatment_kind
                && candidate.cgst_basis_points == row.cgst_basis_points
                && candidate.sgst_basis_points == row.sgst_basis_points
                && candidate.igst_basis_points == row.igst_basis_points
                && candidate.cess_basis_points == row.cess_basis_points
        });
        match existing {
            Some(entry) => {
                entry.taxable_value_paise += row.taxable_value_paise;
                entry.cgst_paise += row.cgst_paise;
                entry.sgst_paise += row.sgst_paise;
                entry.igst_paise += row.igst_paise;
                entry.cess_paise += row.cess_paise;
            }
            None => summary.push(TaxSummaryRow {
                tax_treatment_kind: row.tax_treatment_kind.clone(),
                cgst_basis_points: row.cgst_basis_points,
                sgst_basis_points: row.sgst_basis_points,
                igst_basis_points: row.igst_basis_points,
                cess_basis_points: row.cess_basis_points,
                taxable_value_paise: row.taxable_value_paise,
                cgst_paise: row.cgst_paise,
                sgst_paise: row.sgst_paise,
                igst_paise: row.igst_paise,
                cess_paise: row.cess_paise,
            }),
        }
    }
    summary.sort_by(|left, right| {
        (
            left.cgst_basis_points + left.sgst_basis_points + left.igst_basis_points,
            left.cess_basis_points,
        )
            .cmp(&(
                right.cgst_basis_points + right.sgst_basis_points + right.igst_basis_points,
                right.cess_basis_points,
            ))
    });
    summary
}

/// The document must add up, and it must add up to the posted header.
///
/// Nothing here corrects anything. If a stored document disagrees with itself, that is a fact about
/// the database worth knowing, and quietly rendering a number that makes it look consistent would
/// hide exactly the corruption this checks for.
fn check_invariants(
    header: &HeaderRow,
    lines: &[InvoiceLine],
    summary: &[TaxSummaryRow],
    tender: &[(String, i64, Option<String>)],
) -> Result<(), InvoiceError> {
    fn sum(mut values: impl Iterator<Item = i64>) -> Option<i64> {
        values.try_fold(0_i64, |total, value| total.checked_add(value))
    }
    let line_taxable = sum(lines.iter().map(|line| line.taxable_value_paise))
        .ok_or(InvoiceError::InvariantFailed("line taxable overflow"))?;
    if line_taxable != header.taxable_value_paise {
        return Err(InvoiceError::InvariantFailed("lines disagree with header"));
    }
    for (component, header_value) in [
        (
            sum(lines.iter().map(|line| line.cgst_paise)),
            header.cgst_paise,
        ),
        (
            sum(lines.iter().map(|line| line.sgst_paise)),
            header.sgst_paise,
        ),
        (
            sum(lines.iter().map(|line| line.igst_paise)),
            header.igst_paise,
        ),
        (
            sum(lines.iter().map(|line| line.cess_paise)),
            header.cess_paise,
        ),
    ] {
        if component.ok_or(InvoiceError::InvariantFailed("tax overflow"))? != header_value {
            return Err(InvoiceError::InvariantFailed("tax disagrees with header"));
        }
    }
    let line_total = sum(lines.iter().map(|line| line.line_total_paise))
        .ok_or(InvoiceError::InvariantFailed("total overflow"))?;
    if line_total != header.grand_total_paise {
        return Err(InvoiceError::InvariantFailed("total disagrees with header"));
    }
    // The summary is an aggregation of the same lines, so it must reach the same place.
    let summary_taxable = sum(summary.iter().map(|row| row.taxable_value_paise))
        .ok_or(InvoiceError::InvariantFailed("summary overflow"))?;
    if summary_taxable != header.taxable_value_paise {
        return Err(InvoiceError::InvariantFailed(
            "summary disagrees with header",
        ));
    }
    // Phase 1H requires the tender to settle the invoice exactly. A document that was paid for with
    // a different amount than it charges is not one to print.
    let tendered = sum(tender.iter().map(|row| row.1))
        .ok_or(InvoiceError::InvariantFailed("tender overflow"))?;
    if tendered != header.grand_total_paise {
        return Err(InvoiceError::InvariantFailed("tender disagrees with total"));
    }
    // The database already refuses an incoherent version-1 recipient snapshot. This is the renderer
    // refusing to present one if that guard were ever bypassed: a registered recipient without the
    // Rule 46(d) address, or a "delivered elsewhere" without the address it names.
    if header.recipient_snapshot_version >= 1 {
        let has_address = header
            .recipient_address_line1
            .as_deref()
            .is_some_and(|line| !line.trim().is_empty());
        if header.customer_gst_registration_status.as_deref() == Some("registered")
            && (!has_address || header.customer_normalized_gstin.is_none())
        {
            return Err(InvoiceError::InvariantFailed(
                "registered recipient without its particulars",
            ));
        }
        if header.recipient_address_source.is_some() != has_address {
            return Err(InvoiceError::InvariantFailed(
                "recipient address without provenance",
            ));
        }
        if header.delivery_same_as_recipient == Some(false)
            && header
                .delivery_address_line1
                .as_deref()
                .is_none_or(|line| line.trim().is_empty())
        {
            return Err(InvoiceError::InvariantFailed(
                "delivery elsewhere without an address",
            ));
        }
    }
    Ok(())
}

/// A recipient fact is reported only from a version-1 snapshot. On an older Sale it is unknown, and
/// saying "false" would be inventing an answer.
fn recipient_facts<T>(header: &HeaderRow, value: Option<T>) -> Option<T> {
    if header.recipient_snapshot_version >= 1 {
        value
    } else {
        None
    }
}

fn recipient_address(header: &HeaderRow) -> Option<RecipientAddress> {
    if header.recipient_snapshot_version < 1 {
        return None;
    }
    let source = header.recipient_address_source.clone()?;
    let line1 = header.recipient_address_line1.clone()?;
    Some(RecipientAddress {
        source,
        line1,
        line2: header.recipient_address_line2.clone(),
        city: header.recipient_city.clone(),
        postal_code: header.recipient_postal_code.clone(),
        state_name: header.recipient_state_name.clone(),
        state_code: header.recipient_state_code.clone(),
    })
}

fn delivery_section(header: &HeaderRow) -> Option<DeliverySection> {
    let same_as_recipient = recipient_facts(header, header.delivery_same_as_recipient)?;
    let address = if same_as_recipient {
        None
    } else {
        header
            .delivery_address_line1
            .clone()
            .map(|line1| DeliveryAddress {
                line1,
                line2: header.delivery_address_line2.clone(),
                city: header.delivery_city.clone(),
                postal_code: header.delivery_postal_code.clone(),
                state_name: header.delivery_state_name.clone(),
                state_code: header.delivery_state_code.clone(),
            })
    };
    Some(DeliverySection {
        same_as_recipient,
        address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(scale: Option<i64>, basis: &str, packs: Option<i64>, atoms: i64) -> LineRow {
        LineRow {
            line_number: 1,
            product_display_name: Some("Crocin 500 mg Tablet".to_owned()),
            pack_display_label: Some("Strip of 10".to_owned()),
            base_unit_label: Some("Tablet".to_owned()),
            batch_number: Some("B1".to_owned()),
            batch_expires_on: Some("2027-01-31".to_owned()),
            batch_mrp_paise: Some(10000),
            hsn_code: Some("3004".to_owned()),
            quantity_basis: basis.to_owned(),
            quantity_packs: packs,
            quantity_atoms: atoms,
            quantity_scale: scale,
            selling_rate_paise: 8000,
            tax_treatment_kind: Some("taxable".to_owned()),
            cgst_basis_points: 600,
            sgst_basis_points: 600,
            igst_basis_points: 0,
            cess_basis_points: 0,
            taxable_value_paise: 8000,
            cgst_paise: 480,
            sgst_paise: 480,
            igst_paise: 0,
            cess_paise: 0,
            line_total_paise: 8960,
        }
    }

    #[test]
    fn a_pack_sale_reads_as_packs_and_never_as_atoms() {
        assert_eq!(
            quantity_text(&line(Some(0), "pack", Some(2), 20)),
            "2 × Strip of 10"
        );
    }

    #[test]
    fn a_loose_sale_reads_in_base_units() {
        assert_eq!(
            quantity_text(&line(Some(0), "base_unit", None, 3)),
            "3 Tablet"
        );
    }

    /// The whole reason the scale is snapshotted: three atoms at scale 1 is not three of anything.
    #[test]
    fn a_fractional_quantity_renders_exactly_and_without_floating_point() {
        let mut row = line(Some(1), "base_unit", None, 3);
        row.base_unit_label = Some("ml".to_owned());
        assert_eq!(quantity_text(&row), "0.3 ml");

        row.quantity_scale = Some(3);
        row.quantity_atoms = 1500;
        assert_eq!(
            quantity_text(&row),
            "1.5 ml",
            "trailing zeros must be trimmed"
        );

        row.quantity_atoms = 1000;
        assert_eq!(
            quantity_text(&row),
            "1 ml",
            "a whole value must not print as 1.000"
        );

        row.quantity_scale = Some(2);
        row.quantity_atoms = 5;
        assert_eq!(quantity_text(&row), "0.05 ml");
    }

    /// A line from before the scale existed renders as whole atoms rather than guessing.
    #[test]
    fn a_legacy_line_without_a_scale_renders_its_integer() {
        assert_eq!(quantity_text(&line(None, "base_unit", None, 3)), "3 Tablet");
    }

    #[test]
    fn the_summary_adds_identical_rates_together_and_leaves_different_ones_apart() {
        let mut first = line(Some(0), "pack", Some(1), 10);
        let mut second = line(Some(0), "pack", Some(1), 10);
        second.line_number = 2;
        let mut third = line(Some(0), "pack", Some(1), 10);
        third.line_number = 3;
        third.cgst_basis_points = 250;
        third.sgst_basis_points = 250;
        third.cgst_paise = 200;
        third.sgst_paise = 200;
        third.line_total_paise = 8400;
        first.taxable_value_paise = 8000;
        second.taxable_value_paise = 8000;
        third.taxable_value_paise = 8000;

        let summary = summarise(&[first, second, third]);
        assert_eq!(summary.len(), 2, "two rates produced one row");
        // Sorted by total rate, so the 5% row comes before the 12% row.
        assert_eq!(summary[0].cgst_basis_points, 250);
        assert_eq!(summary[0].taxable_value_paise, 8000);
        assert_eq!(summary[1].cgst_basis_points, 600);
        assert_eq!(
            summary[1].taxable_value_paise, 16000,
            "identical rates were not added together"
        );
        assert_eq!(summary[1].cgst_paise, 960);
    }

    /// The summary never multiplies: its tax is the posted tax, even where that is not the rate
    /// applied to the taxable value. Recomputing would silently overwrite a rounding decision made
    /// at posting.
    #[test]
    fn the_summary_carries_posted_tax_rather_than_recomputing_it() {
        let mut row = line(Some(0), "pack", Some(1), 10);
        row.taxable_value_paise = 333;
        row.cgst_paise = 20;
        row.sgst_paise = 20;
        let summary = summarise(&[row]);
        assert_eq!(summary[0].cgst_paise, 20);
        assert_eq!(summary[0].taxable_value_paise, 333);
    }
}
