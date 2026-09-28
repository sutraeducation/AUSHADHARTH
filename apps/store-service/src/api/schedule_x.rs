//! Phase 1M-D2 — the Schedule X working record, over HTTP.
//!
//! What this surface is: the particulars of a Schedule X receipt, frozen at the moment the Purchase
//! was posted, readable and printable so a person can write the bound register accurately — plus the
//! two attestations that say the physical acts of rule 65(21) actually happened.
//!
//! What it is not: the statutory register. Rule 65(21)(a) requires a bound, serially page numbered
//! book, and no provision of rule 65 accommodates a database in its place. Nothing here allocates a
//! page number, and nothing here claims the software's confirmation is the signature rule
//! 65(21)(b)(x) asks for. The reference this module issues is an AUSHADHARTH reference and says so.
//!
//! What it does not do: enable a Schedule X sale. There is no endpoint here that posts anything, and
//! the sale gate is untouched.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::api::auth::{self, AuthError, AuthenticatedActor};
use crate::api::reference_masters::ReferenceState;
use crate::domain::catalog::{optional_text, required_text, validate_uuid_v7};

#[derive(Debug)]
enum ScheduleXError {
    Auth(AuthError),
    Validation(Vec<(String, String)>),
    NotFound,
    /// The entry has been closed. A finalized working record never changes again.
    Finalized,
    /// The entry was withdrawn. A void entry is history, not a draft to pick up again.
    Void,
    /// Closing an entry before the physical acts were attested would assert something untrue.
    PhysicalConfirmationRequired,
    /// Both physical acts are attested together or not at all.
    AttestationsIncomplete,
    /// Rule 65(21)(b)(x) names the person under whose supervision the transaction happened. That has
    /// to be a registered pharmacist this store actually has on record.
    PharmacistRequired,
    /// Rule 65(9)(a) asks for one attestation per prescription, and it is written once.
    DuplicateCopyAlreadyAttested,
    Internal,
}

impl From<AuthError> for ScheduleXError {
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

impl IntoResponse for ScheduleXError {
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
                        .map(|(field, message)| ErrorIssue { field, message })
                        .collect(),
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("not_found", "That working record does not exist."),
            ),
            Self::Finalized => (
                StatusCode::CONFLICT,
                simple(
                    "schedule_x_entry_finalized",
                    "This working record is closed and cannot be changed.",
                ),
            ),
            Self::Void => (
                StatusCode::CONFLICT,
                simple(
                    "schedule_x_entry_void",
                    "This working record was withdrawn.",
                ),
            ),
            Self::PhysicalConfirmationRequired => (
                StatusCode::CONFLICT,
                simple(
                    "schedule_x_physical_confirmation_required",
                    "The physical Schedule X register entry must be confirmed before this working record can be closed.",
                ),
            ),
            Self::AttestationsIncomplete => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(
                    "schedule_x_attestations_incomplete",
                    "Confirm both that the particulars were entered in the physical register and that the entry was authenticated.",
                ),
            ),
            Self::PharmacistRequired => (
                StatusCode::CONFLICT,
                simple(
                    "schedule_x_pharmacist_required",
                    "Name an active registered pharmacist on this pharmacy's record as the supervising person.",
                ),
            ),
            Self::DuplicateCopyAlreadyAttested => (
                StatusCode::CONFLICT,
                simple(
                    "schedule_x_duplicate_copy_already_attested",
                    "The retained duplicate prescription copy has already been recorded for this prescription.",
                ),
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

/// Raw database text never reaches a caller: a SQLite message can carry table and column names, and
/// an error string is not a place to widen exposure.
fn map_database_error(error: sqlx::Error) -> ScheduleXError {
    let text = error.to_string();
    if text.contains("schedule_x_register_entry_immutable") {
        return ScheduleXError::Finalized;
    }
    if text.contains("duplicate_copy_attestation_is_append_only")
        || text.contains("prescription_duplicate_copy_attestations_prescription_idx")
        || text.contains("UNIQUE constraint failed: prescription_duplicate_copy_attestations")
    {
        return ScheduleXError::DuplicateCopyAlreadyAttested;
    }
    ScheduleXError::Internal
}

fn validation(field: &str, message: &str) -> ScheduleXError {
    ScheduleXError::Validation(vec![(field.to_owned(), message.to_owned())])
}

/// A Schedule X working record's own regulatory access level: the owner and a pharmacist, never a
/// cashier. Supply entries carry a patient's name and address, so this surface follows the stricter
/// Phase 1M-B dispensing precedent rather than Purchase visibility.
async fn require_regulatory_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ScheduleXError> {
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" && actor.role != "pharmacist" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn require_regulatory_writer(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, ScheduleXError> {
    auth::validate_mutation_request(headers)?;
    require_regulatory_reader(state, headers).await
}

async fn current_store(state: &ReferenceState) -> Result<String, ScheduleXError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?
        .ok_or(ScheduleXError::NotFound)
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<String, ScheduleXError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

// ---------------------------------------------------------------------------------------------
// Reading the working record
// ---------------------------------------------------------------------------------------------

/// One working entry, entirely from its own frozen columns.
///
/// Every legally material particular here was frozen when the entry was written. Nothing is joined
/// from a Party, a Product or a Company, so renaming a supplier or archiving a manufacturer next year
/// leaves this row reading exactly as it read on the day.
#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct RegisterEntryResponse {
    id: String,
    entry_kind: String,
    /// The AUSHADHARTH reference. Not a statutory serial, and not a page number.
    reference: String,
    transaction_date: String,
    drug_name: String,
    product_id: String,
    batch_state: String,
    batch_number: Option<String>,
    manufacturer_state: String,
    manufacturer_name: Option<String>,
    quantity_atoms: i64,
    quantity_packs: Option<i64>,
    bill_number: Option<String>,
    bill_date: Option<String>,
    purchase_document_id: Option<String>,
    purchase_line_id: Option<String>,
    supplier_name: Option<String>,
    supplier_address_state: Option<String>,
    supplier_address: Option<String>,
    supplier_licence_state: Option<String>,
    supplier_licence_number: Option<String>,
    status: String,
    particulars_entered_in_physical_register: bool,
    physical_entry_authenticated: bool,
    supervising_professional_id: Option<String>,
    supervising_professional_name: Option<String>,
    confirmed_at_utc: Option<String>,
    finalized_at_utc: Option<String>,
    voided_at_utc: Option<String>,
    void_reason: Option<String>,
    created_at_utc: String,
}

/// A posted Schedule X receipt with no working record — because it was posted before this software
/// kept one. It is listed so the owner can see it, and nothing more: the particulars are NOT
/// invented, and no action here marks it compliant.
#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
struct LegacyReceiptResponse {
    purchase_document_id: String,
    purchase_line_id: String,
    invoice_date: String,
    supplier_invoice_number: String,
    /// The drug name frozen at posting, when Phase 1M-D1-A was in force. `null` for a receipt older
    /// than that — which is the truth, not a gap to be filled from today's catalogue.
    frozen_drug_name: Option<String>,
    product_id: String,
    /// The product's name as the catalogue reads it TODAY, for identification only. It is not a
    /// frozen particular and the screen says so.
    current_product_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleXRegisterResponse {
    entries: Vec<RegisterEntryResponse>,
    legacy_receipts: Vec<LegacyReceiptResponse>,
}

const ENTRY_COLUMNS: &str = "id,entry_kind,reference,transaction_date,drug_name,product_id,\
     batch_state,batch_number,manufacturer_state,manufacturer_name,quantity_atoms,quantity_packs,\
     bill_number,bill_date,purchase_document_id,purchase_line_id,supplier_name,\
     supplier_address_state,supplier_address,supplier_licence_state,supplier_licence_number,status,\
     particulars_entered_in_physical_register,physical_entry_authenticated,\
     supervising_professional_id,supervising_professional_name,confirmed_at_utc,finalized_at_utc,\
     voided_at_utc,void_reason,created_at_utc";

async fn get_register(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<ScheduleXRegisterResponse>, ScheduleXError> {
    require_regulatory_reader(&state, &headers).await?;
    let store_id = current_store(&state).await?;

    let entries: Vec<RegisterEntryResponse> = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM store_schedule_x_register_entries \
         WHERE store_id=? ORDER BY reference_value DESC"
    ))
    .bind(&store_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;

    // Posted Schedule X receipts this software never wrote a working record for. Nothing is
    // backfilled: the list exists so the gap is visible rather than silently repaired.
    let legacy_receipts: Vec<LegacyReceiptResponse> = sqlx::query_as(
        "SELECT document.id AS purchase_document_id, line.id AS purchase_line_id,\
         document.invoice_date, document.supplier_invoice_number,\
         line.drug_display_name AS frozen_drug_name, line.product_id,\
         product.display_name AS current_product_name \
         FROM purchase_lines line \
         JOIN purchase_documents document ON document.id = line.purchase_document_id \
         JOIN products product ON product.id = line.product_id \
         WHERE document.store_id=? AND document.status='posted' \
           AND EXISTS (\
             SELECT 1 FROM product_regulatory_classifications finding \
             WHERE finding.product_id = line.product_id AND finding.scheme='schedule_x' \
               AND finding.status='active' AND finding.applies=1 \
               AND finding.effective_from <= document.invoice_date \
               AND (finding.effective_to IS NULL OR finding.effective_to > document.invoice_date)) \
           AND NOT EXISTS (\
             SELECT 1 FROM store_schedule_x_register_entries entry \
             WHERE entry.entry_kind='receipt' AND entry.purchase_line_id = line.id \
               AND entry.status <> 'void') \
         ORDER BY document.invoice_date DESC, line.id",
    )
    .bind(&store_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;

    Ok(Json(ScheduleXRegisterResponse {
        entries,
        legacy_receipts,
    }))
}

// ---------------------------------------------------------------------------------------------
// The physical acts
// ---------------------------------------------------------------------------------------------

/// The operator's attestation that the two physical acts of rule 65(21) were performed.
///
/// Both flags are sent explicitly and both must be true. They are not defaulted and the screen does
/// not pre-check them, because a pre-ticked box is not an attestation — it is a guess about what
/// somebody did with a pen.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmRequest {
    supervising_professional_id: String,
    particulars_entered_in_physical_register: bool,
    physical_entry_authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VoidRequest {
    reason: String,
}

/// Loads an entry for mutation and refuses the states that cannot be moved.
async fn load_mutable(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    store_id: &str,
) -> Result<String, ScheduleXError> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM store_schedule_x_register_entries WHERE id=? AND store_id=?",
    )
    .bind(id)
    .bind(store_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    let status = status.ok_or(ScheduleXError::NotFound)?;
    match status.as_str() {
        "finalized" => Err(ScheduleXError::Finalized),
        "void" => Err(ScheduleXError::Void),
        _ => Ok(status),
    }
}

async fn record_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    reason: Option<&str>,
    payload: serde_json::Value,
    actor_id: &str,
) -> Result<(), ScheduleXError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,1,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_type)
    .bind(entity_id)
    .bind(action)
    .bind(reason)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

/// Records that the particulars were written into the bound register and that the entry was
/// authenticated there by the supervising person.
///
/// The software does not make that authentication and does not reproduce it. It records that a named
/// person, who is a registered pharmacist on this pharmacy's own record, did it — and who said so,
/// and when. A confirmation in this software is never the signature rule 65(21)(b)(x) requires.
async fn confirm_entry(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ConfirmRequest>,
) -> Result<Json<RegisterEntryResponse>, ScheduleXError> {
    let actor = require_regulatory_writer(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(|issue| validation(&issue.field, &issue.message))?;
    validate_uuid_v7(
        &request.supervising_professional_id,
        "supervisingProfessionalId",
    )
    .map_err(|issue| validation(&issue.field, &issue.message))?;
    if !request.particulars_entered_in_physical_register || !request.physical_entry_authenticated {
        return Err(ScheduleXError::AttestationsIncomplete);
    }
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let status = load_mutable(&mut transaction, &id, &store_id).await?;
    if status != "prepared" {
        // Already confirmed. Saying so beats silently re-attesting on somebody else's behalf.
        return Err(ScheduleXError::PhysicalConfirmationRequired);
    }

    // The supervising person must be a registered pharmacist this store has on record, active now.
    // A login's role is a permission in this software and is not evidence of registration.
    let professional: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT full_name,registration_number FROM store_professionals \
         WHERE id=? AND store_id=? AND status='active' AND capacity='registered_pharmacist'",
    )
    .bind(&request.supervising_professional_id)
    .bind(&store_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let Some((full_name, registration_number)) = professional else {
        return Err(ScheduleXError::PharmacistRequired);
    };
    let Some(registration_number) = registration_number else {
        return Err(ScheduleXError::PharmacistRequired);
    };

    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_schedule_x_register_entries SET status='confirmed',\
         particulars_entered_in_physical_register=1,physical_entry_authenticated=1,\
         supervising_professional_id=?,supervising_professional_name=?,\
         supervising_registration_number=?,confirmed_by_user_id=?,confirmed_at_utc=?,\
         updated_at_utc=? WHERE id=? AND store_id=?",
    )
    .bind(&request.supervising_professional_id)
    .bind(&full_name)
    .bind(&registration_number)
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .bind(&id)
    .bind(&store_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        "schedule_x_register_entry",
        &id,
        "updated",
        None,
        serde_json::json!({
            "transition": "prepared_to_confirmed",
            "particularsEnteredInPhysicalRegister": true,
            "physicalEntryAuthenticated": true,
            "supervisingProfessionalId": request.supervising_professional_id,
        }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    fetch_entry(&state, &id, &store_id).await
}

/// Closes the working entry. After this it never changes again.
async fn finalize_entry(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<RegisterEntryResponse>, ScheduleXError> {
    let actor = require_regulatory_writer(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(|issue| validation(&issue.field, &issue.message))?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let status = load_mutable(&mut transaction, &id, &store_id).await?;
    if status != "confirmed" {
        return Err(ScheduleXError::PhysicalConfirmationRequired);
    }
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_schedule_x_register_entries SET status='finalized',finalized_by_user_id=?,\
         finalized_at_utc=?,updated_at_utc=? WHERE id=? AND store_id=?",
    )
    .bind(&actor.id)
    .bind(&now)
    .bind(&now)
    .bind(&id)
    .bind(&store_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    record_event(
        &mut transaction,
        "schedule_x_register_entry",
        &id,
        "updated",
        None,
        serde_json::json!({ "transition": "confirmed_to_finalized" }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    fetch_entry(&state, &id, &store_id).await
}

/// Withdraws a working entry prepared or confirmed in error. The row and its reference are kept, so
/// the reference is never reissued and the sequence never renumbers itself.
async fn void_entry(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<VoidRequest>,
) -> Result<Json<RegisterEntryResponse>, ScheduleXError> {
    let actor = require_regulatory_writer(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(|issue| validation(&issue.field, &issue.message))?;
    let reason = required_text(&request.reason, "reason", 500)
        .map_err(|issue| validation(&issue.field, &issue.message))?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    load_mutable(&mut transaction, &id, &store_id).await?;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE store_schedule_x_register_entries SET status='void',voided_by_user_id=?,\
         voided_at_utc=?,void_reason=?,updated_at_utc=? WHERE id=? AND store_id=?",
    )
    .bind(&actor.id)
    .bind(&now)
    .bind(&reason)
    .bind(&now)
    .bind(&id)
    .bind(&store_id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    record_event(
        &mut transaction,
        "schedule_x_register_entry",
        &id,
        "updated",
        Some(&reason),
        serde_json::json!({ "transition": "void" }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    fetch_entry(&state, &id, &store_id).await
}

async fn fetch_entry(
    state: &ReferenceState,
    id: &str,
    store_id: &str,
) -> Result<Json<RegisterEntryResponse>, ScheduleXError> {
    let entry: RegisterEntryResponse = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLUMNS} FROM store_schedule_x_register_entries WHERE id=? AND store_id=?"
    ))
    .bind(id)
    .bind(store_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(entry))
}

// ---------------------------------------------------------------------------------------------
// Rule 65(9)(a) — the retained duplicate prescription
// ---------------------------------------------------------------------------------------------

/// The operator's statement about a piece of paper.
///
/// Rule 65(9)(a) requires a Schedule X prescription in duplicate with one copy retained by the
/// licensee for two years. The duplicate is paper. This records only that a person said the retained
/// copy is held — or said it is not — and nothing stored here is a substitute for it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateCopyRequest {
    retained_duplicate_prescription_copy_confirmed: bool,
    note: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct DuplicateCopyResponse {
    id: String,
    prescription_id: String,
    retained_duplicate_prescription_copy_confirmed: bool,
    attested_by_user_id: String,
    attested_at_utc: String,
    note: Option<String>,
}

async fn attest_duplicate_copy(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(prescription_id): Path<String>,
    Json(request): Json<DuplicateCopyRequest>,
) -> Result<(StatusCode, Json<DuplicateCopyResponse>), ScheduleXError> {
    let actor = require_regulatory_writer(&state, &headers).await?;
    validate_uuid_v7(&prescription_id, "prescriptionId")
        .map_err(|issue| validation(&issue.field, &issue.message))?;
    let note = optional_text(request.note.as_deref(), "note", 300)
        .map_err(|issue| validation(&issue.field, &issue.message))?;
    let store_id = current_store(&state).await?;

    let mut transaction = state.pool.begin().await.map_err(map_database_error)?;
    let exists: Option<String> =
        sqlx::query_scalar("SELECT id FROM prescriptions WHERE id=? AND store_id=?")
            .bind(&prescription_id)
            .bind(&store_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    if exists.is_none() {
        return Err(ScheduleXError::NotFound);
    }
    let already: Option<String> = sqlx::query_scalar(
        "SELECT id FROM prescription_duplicate_copy_attestations \
         WHERE store_id=? AND prescription_id=?",
    )
    .bind(&store_id)
    .bind(&prescription_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if already.is_some() {
        return Err(ScheduleXError::DuplicateCopyAlreadyAttested);
    }

    let now = database_now(&mut transaction).await?;
    let id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO prescription_duplicate_copy_attestations (id,store_id,prescription_id,\
         retained_duplicate_prescription_copy_confirmed,attested_by_user_id,attested_at_utc,note) \
         VALUES (?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&prescription_id)
    .bind(i64::from(
        request.retained_duplicate_prescription_copy_confirmed,
    ))
    .bind(&actor.id)
    .bind(&now)
    .bind(&note)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    // The payload names the prescription and the answer. It carries no patient and no prescriber.
    record_event(
        &mut transaction,
        "prescription_duplicate_copy_attestation",
        &id,
        "created",
        None,
        serde_json::json!({
            "prescriptionId": prescription_id,
            "retainedDuplicatePrescriptionCopyConfirmed":
                request.retained_duplicate_prescription_copy_confirmed,
        }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let created: DuplicateCopyResponse = sqlx::query_as(
        "SELECT id,prescription_id,retained_duplicate_prescription_copy_confirmed,\
         attested_by_user_id,attested_at_utc,note FROM prescription_duplicate_copy_attestations \
         WHERE id=?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/store/schedule-x/register", get(get_register))
        .route(
            "/api/v1/store/schedule-x/register/{id}/confirm",
            post(confirm_entry),
        )
        .route(
            "/api/v1/store/schedule-x/register/{id}/finalize",
            post(finalize_entry),
        )
        .route(
            "/api/v1/store/schedule-x/register/{id}/void",
            post(void_entry),
        )
        .route(
            "/api/v1/prescriptions/{id}/schedule-x-duplicate-copy",
            post(attest_duplicate_copy),
        )
}
