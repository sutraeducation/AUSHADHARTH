//! Phase 1M-B — prescriptions and the prescribers written on them.
//!
//! A prescription here is the structured record of a paper a customer brought to the counter: the
//! date, the prescriber, the person it is for (or the owner of the animal), and each item with its
//! total quantity and dose, as rule 65(10) of the Drugs Rules, 1945 requires. The paper itself stays
//! with the pharmacy; nothing here stores an image of it.
//!
//! Who may do what, and why:
//!
//! * `owner_admin` and `pharmacist` may enter prescriptions and prescribers, and read them — that is
//!   the counter's dispensing work. `pharmacist` is this software's permission, not evidence of a
//!   registration; the registered pharmacist who supervised a supply is recorded separately.
//! * only `owner_admin` may edit or archive a prescriber, because a correction there reaches every
//!   later prescription entered from it.
//! * a `cashier` may do none of it. Patient particulars are personal information, and operating the
//!   till is not a reason to browse them. The counter sees a prescription's reference and quantities
//!   on the Sale it is working, never the patient.
//!
//! Audit payloads name identifiers and references, never a patient's name or address.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Sqlite, pool::PoolConnection};
use uuid::Uuid;

use crate::api::auth::{self, AuthError, AuthenticatedActor};
use crate::api::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text, required_text, validate_uuid_v7},
    prescriptions::RepeatAuthority,
};

#[derive(Debug)]
enum PrescriptionError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    Revision {
        expected: i64,
        current: i64,
    },
    /// Dispensed prescriptions are history: archive and enter a new one instead.
    Dispensed,
    /// A rule 65(3)(1) entry is not in a state that allows the change.
    RecordState,
    Archived,
    Internal,
}

impl From<AuthError> for PrescriptionError {
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

impl IntoResponse for PrescriptionError {
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
            Self::Dispensed => (
                StatusCode::CONFLICT,
                simple(
                    "prescription_is_dispensed",
                    "This prescription has been dispensed and can no longer be corrected. Archive it and enter a new one.",
                ),
            ),
            Self::RecordState => (
                StatusCode::CONFLICT,
                simple(
                    "prescription_record_state_conflict",
                    "This prescription-supply entry can no longer be changed that way.",
                ),
            ),
            Self::Archived => (
                StatusCode::CONFLICT,
                simple("record_archived", "This record is archived."),
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

fn map_database_error(error: sqlx::Error) -> PrescriptionError {
    if error.to_string().contains("prescription_is_dispensed") {
        return PrescriptionError::Dispensed;
    }
    PrescriptionError::Internal
}

fn validation(field: &str, message: &str) -> PrescriptionError {
    PrescriptionError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn issue(error: CatalogValidationIssue) -> PrescriptionError {
    PrescriptionError::Validation(vec![error])
}

fn validate_date(value: &str, field: &str) -> Result<String, PrescriptionError> {
    time::Date::parse(
        value.trim(),
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map(|_| value.trim().to_owned())
    .map_err(|_| validation(field, "must be a real calendar date such as 2026-09-21"))
}

/// The counter's dispensing roles. A cashier is refused.
async fn require_dispenser_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PrescriptionError> {
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" && actor.role != "pharmacist" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn require_dispenser(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PrescriptionError> {
    auth::validate_mutation_request(headers)?;
    require_dispenser_reader(state, headers).await
}

async fn require_owner(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PrescriptionError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn current_store(state: &ReferenceState) -> Result<String, PrescriptionError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?
        .ok_or(PrescriptionError::NotFound)
}

/// One audited change: which record, at which revision, and what happened to it.
struct Change<'a> {
    entity_type: &'a str,
    entity_id: &'a str,
    revision: i64,
    action: &'a str,
    reason: Option<&'a str>,
}

async fn record_event(
    connection: &mut PoolConnection<Sqlite>,
    change: Change<'_>,
    payload: serde_json::Value,
    actor_id: &str,
) -> Result<(), PrescriptionError> {
    let Change {
        entity_type,
        entity_id,
        revision,
        action,
        reason,
    } = change;
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_type)
    .bind(entity_id)
    .bind(revision)
    .bind(action)
    .bind(reason)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn begin_immediate(
    state: &ReferenceState,
) -> Result<PoolConnection<Sqlite>, PrescriptionError> {
    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| PrescriptionError::Internal)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    Ok(connection)
}

async fn finish<T>(
    mut connection: PoolConnection<Sqlite>,
    outcome: Result<T, PrescriptionError>,
) -> Result<T, PrescriptionError> {
    match outcome {
        Ok(value) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Prescribers
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PrescriberResponse {
    id: String,
    revision: i64,
    status: String,
    full_name: String,
    address_text: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrescriberRequest {
    expected_revision: Option<i64>,
    full_name: String,
    address_text: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveRequest {
    expected_revision: i64,
    reason: String,
}

const PRESCRIBER_COLUMNS: &str = "id,revision,status,full_name,address_text,registration_number,\
     registering_authority";

struct PreparedPrescriber {
    full_name: String,
    address_text: String,
    registration_number: Option<String>,
    registering_authority: Option<String>,
}

fn prepare_prescriber(
    request: &PrescriberRequest,
) -> Result<PreparedPrescriber, PrescriptionError> {
    Ok(PreparedPrescriber {
        full_name: required_text(&request.full_name, "fullName", 120).map_err(issue)?,
        address_text: required_text(&request.address_text, "addressText", 300).map_err(issue)?,
        registration_number: optional_text(
            request.registration_number.as_deref(),
            "registrationNumber",
            60,
        )
        .map_err(issue)?,
        registering_authority: optional_text(
            request.registering_authority.as_deref(),
            "registeringAuthority",
            160,
        )
        .map_err(issue)?,
    })
}

async fn list_prescribers(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PrescriberResponse>>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    let rows = sqlx::query_as::<_, PrescriberResponse>(&format!(
        "SELECT {PRESCRIBER_COLUMNS} FROM prescribers ORDER BY status, full_name"
    ))
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn create_prescriber(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<PrescriberRequest>,
) -> Result<(StatusCode, Json<PrescriberResponse>), PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    let prepared = prepare_prescriber(&request)?;
    let store_id = current_store(&state).await?;
    let id = Uuid::now_v7().to_string();
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        sqlx::query(
            "INSERT INTO prescribers (id,store_id,full_name,address_text,registration_number,\
             registering_authority,created_by_user_id,revision,status,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,?,?,?,?,1,'active',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(&store_id)
        .bind(&prepared.full_name)
        .bind(&prepared.address_text)
        .bind(&prepared.registration_number)
        .bind(&prepared.registering_authority)
        .bind(&actor.id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescriber",
                entity_id: &id,
                revision: 1,
                action: "created",
                reason: None,
            },
            serde_json::json!({ "prescriberId": id }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_prescriber(&state, &id).await?),
    ))
}

async fn fetch_prescriber(
    state: &ReferenceState,
    id: &str,
) -> Result<PrescriberResponse, PrescriptionError> {
    sqlx::query_as::<_, PrescriberResponse>(&format!(
        "SELECT {PRESCRIBER_COLUMNS} FROM prescribers WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PrescriptionError::NotFound)
}

/// A correction to the master reaches only prescriptions entered after it: every prescription
/// already written carries its own copy of what the prescriber's details were.
async fn update_prescriber(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PrescriberRequest>,
) -> Result<Json<PrescriberResponse>, PrescriptionError> {
    let actor = require_owner(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let prepared = prepare_prescriber(&request)?;
    let expected = request
        .expected_revision
        .ok_or_else(|| validation("expectedRevision", "is required"))?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(i64, String)> =
            sqlx::query_as("SELECT revision,status FROM prescribers WHERE id=?")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_database_error)?;
        let (revision, status) = current.ok_or(PrescriptionError::NotFound)?;
        if status == "archived" {
            return Err(PrescriptionError::Archived);
        }
        if revision != expected {
            return Err(PrescriptionError::Revision {
                expected,
                current: revision,
            });
        }
        sqlx::query(
            "UPDATE prescribers SET full_name=?,address_text=?,registration_number=?,\
             registering_authority=?,revision=?,updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
             WHERE id=?",
        )
        .bind(&prepared.full_name)
        .bind(&prepared.address_text)
        .bind(&prepared.registration_number)
        .bind(&prepared.registering_authority)
        .bind(revision + 1)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescriber",
                entity_id: &id,
                revision: revision + 1,
                action: "updated",
                reason: None,
            },
            serde_json::json!({ "prescriberId": id }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_prescriber(&state, &id).await?))
}

async fn archive_prescriber(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<PrescriberResponse>, PrescriptionError> {
    let actor = require_owner(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(i64, String)> =
            sqlx::query_as("SELECT revision,status FROM prescribers WHERE id=?")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_database_error)?;
        let (revision, status) = current.ok_or(PrescriptionError::NotFound)?;
        if status == "archived" {
            return Err(PrescriptionError::Archived);
        }
        if revision != request.expected_revision {
            return Err(PrescriptionError::Revision {
                expected: request.expected_revision,
                current: revision,
            });
        }
        sqlx::query(
            "UPDATE prescribers SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason=?,revision=?,\
             updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&reason)
        .bind(revision + 1)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescriber",
                entity_id: &id,
                revision: revision + 1,
                action: "archived",
                reason: Some(&reason),
            },
            serde_json::json!({ "prescriberId": id }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_prescriber(&state, &id).await?))
}

// ---------------------------------------------------------------------------------------------
// Prescriptions
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrescriptionItemRequest {
    product_id: String,
    written_description: String,
    prescribed_quantity_atoms: i64,
    dose_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrescriptionRequest {
    expected_revision: Option<i64>,
    prescribed_on: String,
    prescriber_id: Option<String>,
    prescriber_name: String,
    prescriber_address: String,
    prescriber_registration_number: Option<String>,
    prescriber_registering_authority: Option<String>,
    subject_kind: String,
    subject_name: String,
    subject_address: String,
    directions_text: Option<String>,
    repeat_authority: String,
    repeat_times: Option<i64>,
    repeat_interval_days: Option<i64>,
    written_signed_dated_attested: bool,
    items: Vec<PrescriptionItemRequest>,
}

struct PreparedPrescription {
    prescribed_on: String,
    prescriber_id: Option<String>,
    prescriber_name: String,
    prescriber_address: String,
    prescriber_registration_number: Option<String>,
    prescriber_registering_authority: Option<String>,
    subject_kind: String,
    subject_name: String,
    subject_address: String,
    directions_text: Option<String>,
    repeat_authority: String,
    repeat_times: Option<i64>,
    repeat_interval_days: Option<i64>,
    items: Vec<PreparedItem>,
}

struct PreparedItem {
    product_id: String,
    written_description: String,
    prescribed_quantity_atoms: i64,
    dose_text: String,
}

fn prepare_prescription(
    request: &PrescriptionRequest,
) -> Result<PreparedPrescription, PrescriptionError> {
    // Rule 65(10)(a): in writing, signed and dated by the prescriber. The person entering this
    // attests they saw it; a prescription entered without that attestation is not accepted.
    if !request.written_signed_dated_attested {
        return Err(validation(
            "writtenSignedDatedAttested",
            "confirm the prescription is in writing, signed and dated by the prescriber",
        ));
    }
    let subject_kind = request.subject_kind.trim().to_ascii_lowercase();
    if subject_kind != "human" && subject_kind != "animal" {
        return Err(validation("subjectKind", "must be human or animal"));
    }
    let repeat_authority = request.repeat_authority.trim().to_ascii_lowercase();
    if RepeatAuthority::parse(&repeat_authority, request.repeat_times).is_none() {
        return Err(validation(
            "repeatAuthority",
            "must be once; stated_times with a number of 2 or more; or stated_without_count",
        ));
    }
    if repeat_authority == "once" && request.repeat_interval_days.is_some() {
        return Err(validation(
            "repeatIntervalDays",
            "applies only to a prescription that may be dispensed more than once",
        ));
    }
    if let Some(days) = request.repeat_interval_days
        && !(1..=366).contains(&days)
    {
        return Err(validation(
            "repeatIntervalDays",
            "must be between 1 and 366 days",
        ));
    }
    if request.items.is_empty() {
        return Err(validation(
            "items",
            "a prescription names at least one item",
        ));
    }
    let prescriber_id = match request.prescriber_id.as_deref() {
        Some(value) if !value.trim().is_empty() => Some(
            validate_uuid_v7(value, "prescriberId")
                .map_err(issue)?
                .to_owned(),
        ),
        _ => None,
    };
    let mut items = Vec::with_capacity(request.items.len());
    for (index, item) in request.items.iter().enumerate() {
        let field = |name: &str| format!("items.{index}.{name}");
        validate_uuid_v7(&item.product_id, &field("productId")).map_err(issue)?;
        if item.prescribed_quantity_atoms < 1 {
            return Err(validation(
                &field("prescribedQuantityAtoms"),
                "the total amount to be supplied must be at least one unit",
            ));
        }
        items.push(PreparedItem {
            product_id: item.product_id.clone(),
            written_description: required_text(
                &item.written_description,
                &field("writtenDescription"),
                200,
            )
            .map_err(issue)?,
            prescribed_quantity_atoms: item.prescribed_quantity_atoms,
            // Rule 65(10)(c): the dose to be taken.
            dose_text: required_text(&item.dose_text, &field("doseText"), 200).map_err(issue)?,
        });
    }
    Ok(PreparedPrescription {
        prescribed_on: validate_date(&request.prescribed_on, "prescribedOn")?,
        prescriber_id,
        prescriber_name: required_text(&request.prescriber_name, "prescriberName", 120)
            .map_err(issue)?,
        prescriber_address: required_text(&request.prescriber_address, "prescriberAddress", 300)
            .map_err(issue)?,
        prescriber_registration_number: optional_text(
            request.prescriber_registration_number.as_deref(),
            "prescriberRegistrationNumber",
            60,
        )
        .map_err(issue)?,
        prescriber_registering_authority: optional_text(
            request.prescriber_registering_authority.as_deref(),
            "prescriberRegisteringAuthority",
            160,
        )
        .map_err(issue)?,
        subject_kind,
        subject_name: required_text(&request.subject_name, "subjectName", 120).map_err(issue)?,
        subject_address: required_text(&request.subject_address, "subjectAddress", 300)
            .map_err(issue)?,
        directions_text: optional_text(request.directions_text.as_deref(), "directionsText", 500)
            .map_err(issue)?,
        repeat_authority,
        repeat_times: request.repeat_times,
        repeat_interval_days: request.repeat_interval_days,
        items,
    })
}

async fn validate_references(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    prepared: &PreparedPrescription,
) -> Result<(), PrescriptionError> {
    if let Some(prescriber_id) = prepared.prescriber_id.as_deref() {
        let found: Option<String> =
            sqlx::query_scalar("SELECT id FROM prescribers WHERE id=? AND store_id=?")
                .bind(prescriber_id)
                .bind(store_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
        if found.is_none() {
            return Err(validation(
                "prescriberId",
                "is not a prescriber of this pharmacy",
            ));
        }
    }
    let today: String =
        sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes','+1 day')")
            .fetch_one(&mut **connection)
            .await
            .map_err(map_database_error)?;
    // A prescription is not written in the future. One day of grace covers any time zone.
    if prepared.prescribed_on.as_str() > today.as_str() {
        return Err(validation("prescribedOn", "cannot be in the future"));
    }
    for (index, item) in prepared.items.iter().enumerate() {
        let found: Option<String> = sqlx::query_scalar("SELECT id FROM products WHERE id=?")
            .bind(&item.product_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
        if found.is_none() {
            return Err(validation(
                &format!("items.{index}.productId"),
                "is not a product",
            ));
        }
    }
    Ok(())
}

async fn insert_items(
    connection: &mut PoolConnection<Sqlite>,
    prescription_id: &str,
    items: &[PreparedItem],
) -> Result<(), PrescriptionError> {
    for (index, item) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO prescription_items (id,prescription_id,line_number,product_id,\
             written_description,prescribed_quantity_atoms,dose_text,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(prescription_id)
        .bind(index as i64 + 1)
        .bind(&item.product_id)
        .bind(&item.written_description)
        .bind(item.prescribed_quantity_atoms)
        .bind(&item.dose_text)
        .execute(&mut **connection)
        .await
        .map_err(map_database_error)?;
    }
    Ok(())
}

async fn create_prescription(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<PrescriptionRequest>,
) -> Result<(StatusCode, Json<PrescriptionDetail>), PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    let prepared = prepare_prescription(&request)?;
    let store_id = current_store(&state).await?;
    let id = Uuid::now_v7().to_string();
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        validate_references(&mut connection, &store_id, &prepared).await?;
        // The reference is allocated under the write lock, so two counters never take one number.
        let last: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(CAST(substr(reference, 4) AS INTEGER)) FROM prescriptions WHERE store_id=?",
        )
        .bind(&store_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let reference = format!("RX-{:06}", last.unwrap_or(0) + 1);
        sqlx::query(
            "INSERT INTO prescriptions (id,store_id,reference,prescribed_on,prescriber_id,\
             prescriber_name,prescriber_address,prescriber_registration_number,\
             prescriber_registering_authority,subject_kind,subject_name,subject_address,\
             directions_text,repeat_authority,repeat_times,repeat_interval_days,\
             written_signed_dated_attested,created_by_user_id,revision,status,created_at_utc,\
             updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,1,?,1,'active',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(&store_id)
        .bind(&reference)
        .bind(&prepared.prescribed_on)
        .bind(&prepared.prescriber_id)
        .bind(&prepared.prescriber_name)
        .bind(&prepared.prescriber_address)
        .bind(&prepared.prescriber_registration_number)
        .bind(&prepared.prescriber_registering_authority)
        .bind(&prepared.subject_kind)
        .bind(&prepared.subject_name)
        .bind(&prepared.subject_address)
        .bind(&prepared.directions_text)
        .bind(&prepared.repeat_authority)
        .bind(prepared.repeat_times)
        .bind(prepared.repeat_interval_days)
        .bind(&actor.id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        insert_items(&mut connection, &id, &prepared.items).await?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription",
                entity_id: &id,
                revision: 1,
                action: "created",
                reason: None,
            },
            serde_json::json!({ "prescriptionId": id, "reference": reference }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_prescription(&state, &id).await?),
    ))
}

/// A correction before the first dispensing replaces the prescription's facts and items, audited.
/// After any dispensing it is refused — by this handler and by the database — because the authority
/// a dispensing relied on must not change beneath it.
async fn correct_prescription(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PrescriptionRequest>,
) -> Result<Json<PrescriptionDetail>, PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let prepared = prepare_prescription(&request)?;
    let expected = request
        .expected_revision
        .ok_or_else(|| validation("expectedRevision", "is required"))?;
    let store_id = current_store(&state).await?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(i64, String, String)> =
            sqlx::query_as("SELECT revision,status,store_id FROM prescriptions WHERE id=?")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_database_error)?;
        let (revision, status, owner) = current.ok_or(PrescriptionError::NotFound)?;
        if owner != store_id {
            return Err(PrescriptionError::NotFound);
        }
        if status == "archived" {
            return Err(PrescriptionError::Archived);
        }
        if revision != expected {
            return Err(PrescriptionError::Revision {
                expected,
                current: revision,
            });
        }
        let dispensed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM prescription_dispensings WHERE prescription_id=?",
        )
        .bind(&id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_database_error)?;
        if dispensed > 0 {
            return Err(PrescriptionError::Dispensed);
        }
        // A draft Sale line still pointing at an item would lose its target; unlink them first.
        let linked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sale_lines line JOIN prescription_items item \
             ON item.id=line.prescription_item_id WHERE item.prescription_id=?",
        )
        .bind(&id)
        .fetch_one(&mut *connection)
        .await
        .map_err(map_database_error)?;
        if linked > 0 {
            return Err(validation(
                "items",
                "is linked to a Sale line; remove the link on the Sale before correcting it",
            ));
        }
        validate_references(&mut connection, &store_id, &prepared).await?;
        sqlx::query(
            "UPDATE prescriptions SET prescribed_on=?,prescriber_id=?,prescriber_name=?,\
             prescriber_address=?,prescriber_registration_number=?,\
             prescriber_registering_authority=?,subject_kind=?,subject_name=?,subject_address=?,\
             directions_text=?,repeat_authority=?,repeat_times=?,repeat_interval_days=?,\
             revision=?,updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&prepared.prescribed_on)
        .bind(&prepared.prescriber_id)
        .bind(&prepared.prescriber_name)
        .bind(&prepared.prescriber_address)
        .bind(&prepared.prescriber_registration_number)
        .bind(&prepared.prescriber_registering_authority)
        .bind(&prepared.subject_kind)
        .bind(&prepared.subject_name)
        .bind(&prepared.subject_address)
        .bind(&prepared.directions_text)
        .bind(&prepared.repeat_authority)
        .bind(prepared.repeat_times)
        .bind(prepared.repeat_interval_days)
        .bind(revision + 1)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        sqlx::query("DELETE FROM prescription_items WHERE prescription_id=?")
            .bind(&id)
            .execute(&mut *connection)
            .await
            .map_err(map_database_error)?;
        insert_items(&mut connection, &id, &prepared.items).await?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription",
                entity_id: &id,
                revision: revision + 1,
                action: "updated",
                reason: Some("corrected before dispensing"),
            },
            serde_json::json!({ "prescriptionId": id }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_prescription(&state, &id).await?))
}

/// Archiving stops further dispensing. It is the one change left to a dispensed prescription.
async fn archive_prescription(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ArchiveRequest>,
) -> Result<Json<PrescriptionDetail>, PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;
    let store_id = current_store(&state).await?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(i64, String, String)> =
            sqlx::query_as("SELECT revision,status,store_id FROM prescriptions WHERE id=?")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_database_error)?;
        let (revision, status, owner) = current.ok_or(PrescriptionError::NotFound)?;
        if owner != store_id {
            return Err(PrescriptionError::NotFound);
        }
        if status == "archived" {
            return Err(PrescriptionError::Archived);
        }
        if revision != request.expected_revision {
            return Err(PrescriptionError::Revision {
                expected: request.expected_revision,
                current: revision,
            });
        }
        sqlx::query(
            "UPDATE prescriptions SET status='archived',\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason=?,revision=?,\
             updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?",
        )
        .bind(&reason)
        .bind(revision + 1)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription",
                entity_id: &id,
                revision: revision + 1,
                action: "archived",
                reason: Some(&reason),
            },
            serde_json::json!({ "prescriptionId": id }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_prescription(&state, &id).await?))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PrescriptionSummary {
    id: String,
    reference: String,
    prescribed_on: String,
    prescriber_name: String,
    status: String,
    item_count: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    reference: Option<String>,
}

/// A list for the dispensing roles: references, dates and prescribers, never the patient.
async fn list_prescriptions(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<PrescriptionSummary>>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    let reference = query
        .reference
        .as_deref()
        .map(|value| value.trim().to_ascii_uppercase())
        .filter(|value| !value.is_empty());
    let rows = sqlx::query_as::<_, PrescriptionSummary>(
        "SELECT prescription.id,prescription.reference,prescription.prescribed_on,\
         prescription.prescriber_name,prescription.status,\
         (SELECT COUNT(*) FROM prescription_items item WHERE item.prescription_id=prescription.id) \
         AS item_count FROM prescriptions prescription \
         WHERE (?1 IS NULL OR prescription.reference=?1) \
         ORDER BY prescription.prescribed_on DESC, prescription.reference DESC LIMIT 200",
    )
    .bind(reference)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PrescriptionItemDetail {
    id: String,
    line_number: i64,
    product_id: String,
    product_display_name: Option<String>,
    written_description: String,
    prescribed_quantity_atoms: i64,
    dose_text: String,
    dispensed_atoms: i64,
    reinstated_atoms: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct DispensingDetail {
    id: String,
    prescription_item_id: String,
    sale_document_id: String,
    document_number: Option<String>,
    quantity_atoms: i64,
    dispensed_on: String,
    supervising_professional_name: String,
    reversed_atoms: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PrescriptionHeader {
    id: String,
    reference: String,
    revision: i64,
    status: String,
    prescribed_on: String,
    prescriber_id: Option<String>,
    prescriber_name: String,
    prescriber_address: String,
    prescriber_registration_number: Option<String>,
    prescriber_registering_authority: Option<String>,
    subject_kind: String,
    subject_name: String,
    subject_address: String,
    directions_text: Option<String>,
    repeat_authority: String,
    repeat_times: Option<i64>,
    repeat_interval_days: Option<i64>,
    archive_reason: Option<String>,
    created_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrescriptionDetail {
    #[serde(flatten)]
    header: PrescriptionHeader,
    items: Vec<PrescriptionItemDetail>,
    dispensings: Vec<DispensingDetail>,
    /// Distinct Sales that dispensed from this prescription. Returns do not reduce it.
    occasions_used: i64,
    occasions_authorised: Option<i64>,
    /// Phase 1M-B: the rule 65(3)(1) entries recording its supplies, oldest first.
    records: Vec<RecordSummary>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct RecordSummary {
    id: String,
    serial_number: String,
    record_method: String,
    date_of_supply: String,
    status: String,
}

async fn fetch_prescription(
    state: &ReferenceState,
    id: &str,
) -> Result<PrescriptionDetail, PrescriptionError> {
    let header = sqlx::query_as::<_, PrescriptionHeader>(
        "SELECT id,reference,revision,status,prescribed_on,prescriber_id,prescriber_name,\
         prescriber_address,prescriber_registration_number,prescriber_registering_authority,\
         subject_kind,subject_name,subject_address,directions_text,repeat_authority,repeat_times,\
         repeat_interval_days,archive_reason,created_at_utc FROM prescriptions WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PrescriptionError::NotFound)?;
    let items = sqlx::query_as::<_, PrescriptionItemDetail>(
        "SELECT item.id,item.line_number,item.product_id,product.display_name AS \
         product_display_name,item.written_description,item.prescribed_quantity_atoms,\
         item.dose_text,\
         (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensings \
          WHERE prescription_item_id=item.id) AS dispensed_atoms,\
         (SELECT COALESCE(SUM(reversal.quantity_atoms),0) \
          FROM prescription_dispensing_reversals reversal \
          JOIN prescription_dispensings dispensing ON dispensing.id=reversal.dispensing_id \
          WHERE dispensing.prescription_item_id=item.id) AS reinstated_atoms \
         FROM prescription_items item LEFT JOIN products product ON product.id=item.product_id \
         WHERE item.prescription_id=? ORDER BY item.line_number",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let dispensings = sqlx::query_as::<_, DispensingDetail>(
        "SELECT dispensing.id,dispensing.prescription_item_id,dispensing.sale_document_id,\
         document.document_number,dispensing.quantity_atoms,dispensing.dispensed_on,\
         dispensing.supervising_professional_name,\
         (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals \
          WHERE dispensing_id=dispensing.id) AS reversed_atoms \
         FROM prescription_dispensings dispensing \
         JOIN sale_documents document ON document.id=dispensing.sale_document_id \
         WHERE dispensing.prescription_id=? ORDER BY dispensing.created_at_utc",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let occasions_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT sale_document_id) FROM prescription_dispensings \
         WHERE prescription_id=?",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    let occasions_authorised =
        RepeatAuthority::parse(&header.repeat_authority, header.repeat_times)
            .and_then(RepeatAuthority::authorised_occasions);
    let records = sqlx::query_as::<_, RecordSummary>(
        "SELECT id,serial_number,record_method,date_of_supply,status \
         FROM prescription_supply_records WHERE prescription_id=? ORDER BY prepared_at_utc,serial_value",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(PrescriptionDetail {
        header,
        items,
        dispensings,
        occasions_used,
        occasions_authorised,
        records,
    })
}

// ---------------------------------------------------------------------------------------------
// Rule 65(3)(1) entries
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct RecordHeader {
    id: String,
    serial_number: String,
    record_method: String,
    status: String,
    date_of_supply: String,
    prescriber_name: String,
    prescriber_address: String,
    subject_kind: String,
    subject_name: String,
    subject_address: String,
    supervising_professional_name: String,
    supervising_registration_number: String,
    original_container_confirmed: bool,
    manual_signature_confirmed: bool,
    serial_written_on_prescription: bool,
    prepared_by_display_name: Option<String>,
    prepared_at_utc: String,
    confirmed_by_display_name: Option<String>,
    confirmed_at_utc: Option<String>,
    finalized_at_utc: Option<String>,
    voided_by_display_name: Option<String>,
    voided_at_utc: Option<String>,
    void_reason: Option<String>,
    previous_serial_number: Option<String>,
    prescription_id: String,
    prescription_reference: String,
    sale_document_id: String,
    document_number: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct RecordLine {
    drug_name: String,
    quantity_atoms: i64,
    quantity_unit_label: Option<String>,
    manufacturer_name: String,
    batch_number: String,
    batch_expires_on: Option<String>,
    /// Returned since, as append-only reversals. The entry itself never changes.
    returned_atoms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordDetail {
    #[serde(flatten)]
    header: RecordHeader,
    lines: Vec<RecordLine>,
}

async fn fetch_record(state: &ReferenceState, id: &str) -> Result<RecordDetail, PrescriptionError> {
    let store_id = current_store(state).await?;
    let header = sqlx::query_as::<_, RecordHeader>(
        "SELECT record.id,record.serial_number,record.record_method,record.status,\
         record.date_of_supply,record.prescriber_name,record.prescriber_address,\
         record.subject_kind,record.subject_name,record.subject_address,\
         record.supervising_professional_name,record.supervising_registration_number,\
         record.original_container_confirmed=1 AS original_container_confirmed,\
         record.manual_signature_confirmed=1 AS manual_signature_confirmed,\
         record.serial_written_on_prescription=1 AS serial_written_on_prescription,\
         preparer.display_name AS prepared_by_display_name,record.prepared_at_utc,\
         confirmer.display_name AS confirmed_by_display_name,record.confirmed_at_utc,\
         record.finalized_at_utc,voider.display_name AS voided_by_display_name,\
         record.voided_at_utc,record.void_reason,\
         previous.serial_number AS previous_serial_number,record.prescription_id,\
         prescription.reference AS prescription_reference,record.sale_document_id,\
         document.document_number \
         FROM prescription_supply_records record \
         JOIN prescriptions prescription ON prescription.id=record.prescription_id \
         JOIN sale_documents document ON document.id=record.sale_document_id \
         LEFT JOIN prescription_supply_records previous ON previous.id=record.previous_record_id \
         LEFT JOIN users preparer ON preparer.id=record.prepared_by_user_id \
         LEFT JOIN users confirmer ON confirmer.id=record.confirmed_by_user_id \
         LEFT JOIN users voider ON voider.id=record.voided_by_user_id \
         WHERE record.id=? AND record.store_id=?",
    )
    .bind(id)
    .bind(&store_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PrescriptionError::NotFound)?;
    let lines = sqlx::query_as::<_, RecordLine>(
        "SELECT line.drug_name,line.quantity_atoms,line.quantity_unit_label,line.manufacturer_name,\
         line.batch_number,line.batch_expires_on,\
         (SELECT COALESCE(SUM(quantity_atoms),0) FROM prescription_dispensing_reversals \
          WHERE dispensing_id=line.dispensing_id) AS returned_atoms \
         FROM prescription_supply_record_lines line WHERE line.record_id=? \
         ORDER BY line.created_at_utc,line.id",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(RecordDetail { header, lines })
}

/// The entry, as it is to be printed, signed by hand and kept: every particular of rule 65(3)(1).
/// Dispensing roles only — it names the patient.
async fn get_record(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<RecordDetail>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    Ok(Json(fetch_record(&state, &id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmRecordRequest {
    manual_signature_confirmed: bool,
    serial_written_on_prescription: bool,
}

/// Confirms the two manual acts on a prepared entry: the registered pharmacist signed the physical
/// entry by hand (rule 65(3)(1)(g)), and its serial was written on the prescription (the rule's
/// opening words). AUSHADHARTH can do neither and never claims to; it records who confirmed them.
///
/// Only a pharmacist or the owner may confirm — a cashier may operate the till but may not attest
/// a professional act. The confirmer is recorded apart from the pharmacist named in the entry and
/// from whoever later posts the Sale. Only after this may the Sale post.
async fn confirm_record(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ConfirmRecordRequest>,
) -> Result<Json<RecordDetail>, PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    if !request.manual_signature_confirmed {
        return Err(validation(
            "manualSignatureConfirmed",
            "the registered pharmacist must sign the entry by hand before it is confirmed",
        ));
    }
    if !request.serial_written_on_prescription {
        return Err(validation(
            "serialWrittenOnPrescription",
            "the entry's serial number must be written on the prescription before it is confirmed",
        ));
    }
    let store_id = current_store(&state).await?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(String, String, String)> = sqlx::query_as(
            "SELECT record.status,record.serial_number,document.status \
             FROM prescription_supply_records record \
             JOIN sale_documents document ON document.id=record.sale_document_id \
             WHERE record.id=? AND record.store_id=?",
        )
        .bind(&id)
        .bind(&store_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let (status, serial, sale_status) = current.ok_or(PrescriptionError::NotFound)?;
        if status != "prepared" || sale_status != "draft" {
            return Err(PrescriptionError::RecordState);
        }
        sqlx::query(
            "UPDATE prescription_supply_records SET status='confirmed',\
             manual_signature_confirmed=1,serial_written_on_prescription=1,confirmed_by_user_id=?,\
             confirmed_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND status='prepared'",
        )
        .bind(&actor.id)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription_supply_record",
                entity_id: &id,
                revision: 2,
                action: "updated",
                reason: Some("manual signature and prescription serial confirmed"),
            },
            serde_json::json!({ "recordId": id, "serialNumber": serial, "status": "confirmed" }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_record(&state, &id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VoidRecordRequest {
    reason: String,
}

/// Cancels a prepared or confirmed entry before its Sale posts.
///
/// The serial may already be on paper — on the prescription, in the register — so it is never
/// given to another entry: the entry is kept, marked void with its reason, and the book's next
/// serial moves on. A void entry supports no posting. A finalized entry cannot be voided; a supply
/// that happened is corrected by a return, never by erasing its record.
async fn void_record(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<VoidRecordRequest>,
) -> Result<Json<RecordDetail>, PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(issue)?;
    let store_id = current_store(&state).await?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let current: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT record.status,record.serial_number,document.status,record.sale_document_id \
             FROM prescription_supply_records record \
             JOIN sale_documents document ON document.id=record.sale_document_id \
             WHERE record.id=? AND record.store_id=?",
        )
        .bind(&id)
        .bind(&store_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let (status, serial, sale_status, sale_id) = current.ok_or(PrescriptionError::NotFound)?;
        if !(status == "prepared" || status == "confirmed") || sale_status != "draft" {
            return Err(PrescriptionError::RecordState);
        }
        sqlx::query(
            "UPDATE prescription_supply_records SET status='void',voided_by_user_id=?,\
             voided_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),void_reason=? \
             WHERE id=? AND status IN ('prepared','confirmed')",
        )
        .bind(&actor.id)
        .bind(&reason)
        .bind(&id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        // Phase 1M-C: the Schedule H1 working entries prepared beside this entry are for the same
        // supply, which will not now happen as prepared. They are voided with it, kept, and their
        // AUSHADHARTH References are never reused.
        let h1: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,reference FROM prescription_h1_register_entries \
             WHERE supply_record_id=? AND status IN ('prepared','confirmed')",
        )
        .bind(&id)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_database_error)?;
        for (entry_id, reference) in &h1 {
            sqlx::query(
                "UPDATE prescription_h1_register_entries SET status='void',voided_by_user_id=?,\
                 voided_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),void_reason=? \
                 WHERE id=? AND status IN ('prepared','confirmed')",
            )
            .bind(&actor.id)
            .bind(&reason)
            .bind(entry_id)
            .execute(&mut *connection)
            .await
            .map_err(map_database_error)?;
            record_event(
                &mut connection,
                Change {
                    entity_type: "prescription_h1_register_entry",
                    entity_id: entry_id,
                    revision: 3,
                    action: "archived",
                    reason: Some(&reason),
                },
                serde_json::json!({ "entryId": entry_id, "reference": reference, "status": "void" }),
                &actor.id,
            )
            .await?;
        }
        // The draft's revision moves, so a screen that read it before the void must reload.
        sqlx::query(
            "UPDATE sale_documents SET revision=revision+1,\
             updated_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND status='draft'",
        )
        .bind(&sale_id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription_supply_record",
                entity_id: &id,
                revision: 3,
                action: "archived",
                reason: Some(&reason),
            },
            serde_json::json!({ "recordId": id, "serialNumber": serial, "status": "void" }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_record(&state, &id).await?))
}

async fn get_prescription(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<PrescriptionDetail>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let store_id = current_store(&state).await?;
    let owner: Option<String> = sqlx::query_scalar("SELECT store_id FROM prescriptions WHERE id=?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_database_error)?;
    if owner.as_deref() != Some(store_id.as_str()) {
        return Err(PrescriptionError::NotFound);
    }
    Ok(Json(fetch_prescription(&state, &id).await?))
}

// ---------------------------------------------------------------------------------------------
// Phase 1M-C — the Schedule H1 working record
//
// Rule 65(3)(1)(h) requires a separate register, kept by the pharmacy. AUSHADHARTH keeps a working
// record of each entry and prints its hard copy; it is not the statutory register and never says
// so. Following the 48th DCC's recommendation (an official CDSCO-hosted committee recommendation,
// not proven to amend rule 65), a working entry is confirmed only when a dispensing user records
// that the hard copy was placed in that register and the registered pharmacist authenticated it by
// hand. Every read and write here is for the dispensing roles: the entries name the patient.
// ---------------------------------------------------------------------------------------------

/// The store's legal identity and licences, printed on the hard copy and the register view.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct H1StoreContext {
    legal_name: Option<String>,
    display_name: String,
    address_line1: Option<String>,
    address_line2: Option<String>,
    city: Option<String>,
    state_name: Option<String>,
    postal_code: Option<String>,
    licences: Vec<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct H1EntryRow {
    id: String,
    /// The AUSHADHARTH Reference: internal, immutable. Not a register serial or page number.
    reference: String,
    status: String,
    sale_document_id: String,
    document_number: Option<String>,
    line_number: i64,
    date_of_supply: String,
    // Rule 65(3)(1)(h) particulars.
    prescriber_name: String,
    prescriber_address: String,
    patient_name: String,
    drug_name: String,
    quantity_atoms: i64,
    quantity_unit_label: Option<String>,
    // Internal cross-references and state.
    supervising_professional_name: String,
    supervising_registration_number: String,
    supply_record_serial: String,
    prepared_by_display_name: Option<String>,
    prepared_at_utc: String,
    hard_copy_placed_in_register: bool,
    pharmacist_authenticated_hard_copy: bool,
    confirmed_by_display_name: Option<String>,
    confirmed_at_utc: Option<String>,
    finalized_at_utc: Option<String>,
    voided_at_utc: Option<String>,
    void_reason: Option<String>,
    /// Returned since, recorded separately against the dispensing. The entry itself never changes.
    returned_atoms: i64,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct H1AnnotationRow {
    id: String,
    entry_id: String,
    note: String,
    created_by_display_name: Option<String>,
    created_at_utc: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct H1Sheet {
    store: H1StoreContext,
    entries: Vec<H1EntryRow>,
    annotations: Vec<H1AnnotationRow>,
}

const H1_ENTRY_SELECT: &str = "SELECT entry.id,entry.reference,entry.status,\
     entry.sale_document_id,document.document_number,line.line_number,entry.date_of_supply,\
     entry.prescriber_name,entry.prescriber_address,entry.patient_name,entry.drug_name,\
     entry.quantity_atoms,entry.quantity_unit_label,entry.supervising_professional_name,\
     entry.supervising_registration_number,record.serial_number AS supply_record_serial,\
     preparer.display_name AS prepared_by_display_name,entry.prepared_at_utc,\
     entry.hard_copy_placed_in_register=1 AS hard_copy_placed_in_register,\
     entry.pharmacist_authenticated_hard_copy=1 AS pharmacist_authenticated_hard_copy,\
     confirmer.display_name AS confirmed_by_display_name,entry.confirmed_at_utc,\
     entry.finalized_at_utc,entry.voided_at_utc,entry.void_reason,\
     (SELECT COALESCE(SUM(reversal.quantity_atoms),0) FROM prescription_dispensing_reversals reversal \
      WHERE reversal.dispensing_id=entry.dispensing_id) AS returned_atoms \
     FROM prescription_h1_register_entries entry \
     JOIN sale_documents document ON document.id=entry.sale_document_id \
     JOIN sale_lines line ON line.id=entry.sale_line_id \
     JOIN prescription_supply_records record ON record.id=entry.supply_record_id \
     LEFT JOIN users preparer ON preparer.id=entry.prepared_by_user_id \
     LEFT JOIN users confirmer ON confirmer.id=entry.confirmed_by_user_id";

async fn h1_store_context(
    state: &ReferenceState,
    store_id: &str,
) -> Result<H1StoreContext, PrescriptionError> {
    let identity: (Option<String>, String) =
        sqlx::query_as("SELECT legal_name,display_name FROM store_identity WHERE store_id=?")
            .bind(store_id)
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?;
    #[allow(clippy::type_complexity)]
    let address: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT address.line1,address.line2,address.city,code.display_name,address.postal_code \
         FROM store_addresses address LEFT JOIN state_codes code ON code.id=address.state_id \
         WHERE address.store_id=?",
    )
    .bind(store_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?;
    let licences: Vec<String> = sqlx::query_scalar(
        "SELECT licence_type||' '||licence_number FROM store_licences \
         WHERE store_id=? AND status='active' ORDER BY licence_type,licence_number",
    )
    .bind(store_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let (line1, line2, city, state_name, postal_code) = match address {
        Some((line1, line2, city, state_name, postal_code)) => {
            (Some(line1), line2, city, state_name, postal_code)
        }
        None => (None, None, None, None, None),
    };
    Ok(H1StoreContext {
        legal_name: identity.0,
        display_name: identity.1,
        address_line1: line1,
        address_line2: line2,
        city,
        state_name,
        postal_code,
        licences,
    })
}

async fn fetch_h1_annotations(
    state: &ReferenceState,
    entries: &[H1EntryRow],
) -> Result<Vec<H1AnnotationRow>, PrescriptionError> {
    let mut annotations = Vec::new();
    for entry in entries.iter().filter(|entry| entry.status == "finalized") {
        let mut rows: Vec<H1AnnotationRow> = sqlx::query_as(
            "SELECT annotation.id,annotation.entry_id,annotation.note,\
             author.display_name AS created_by_display_name,annotation.created_at_utc \
             FROM prescription_h1_register_annotations annotation \
             LEFT JOIN users author ON author.id=annotation.created_by_user_id \
             WHERE annotation.entry_id=? ORDER BY annotation.created_at_utc,annotation.id",
        )
        .bind(&entry.id)
        .fetch_all(&state.pool)
        .await
        .map_err(map_database_error)?;
        annotations.append(&mut rows);
    }
    Ok(annotations)
}

async fn fetch_sale_h1_sheet(
    state: &ReferenceState,
    store_id: &str,
    sale_id: &str,
) -> Result<H1Sheet, PrescriptionError> {
    let owner: Option<String> =
        sqlx::query_scalar("SELECT store_id FROM sale_documents WHERE id=?")
            .bind(sale_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    if owner.as_deref() != Some(store_id) {
        return Err(PrescriptionError::NotFound);
    }
    let entries: Vec<H1EntryRow> = sqlx::query_as(&format!(
        "{H1_ENTRY_SELECT} WHERE entry.sale_document_id=? AND entry.store_id=? \
         ORDER BY entry.reference_value"
    ))
    .bind(sale_id)
    .bind(store_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let annotations = fetch_h1_annotations(state, &entries).await?;
    Ok(H1Sheet {
        store: h1_store_context(state, store_id).await?,
        entries,
        annotations,
    })
}

/// The per-supply hard copy: every Schedule H1 working entry of one Sale, with the store's legal
/// identity, for printing and placing in the separate physical H1 register. Dispensing roles only.
async fn get_sale_h1_sheet(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<H1Sheet>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let store_id = current_store(&state).await?;
    Ok(Json(fetch_sale_h1_sheet(&state, &store_id, &id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfirmH1Request {
    hard_copy_placed_in_register: bool,
    pharmacist_authenticated_hard_copy: bool,
}

/// Confirms, for every prepared Schedule H1 working entry of one Sale at once, the two physical
/// acts of the DCC arrangement: the printed hard copy was placed in the separate H1 register, and
/// the registered pharmacist authenticated it by hand. AUSHADHARTH performs neither and never says
/// it did; it records who confirmed them and when. A pharmacist or the owner only — a cashier may
/// not attest a professional act. All entries move together, or none does.
async fn confirm_sale_h1_entries(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<ConfirmH1Request>,
) -> Result<Json<H1Sheet>, PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    if !request.hard_copy_placed_in_register {
        return Err(validation(
            "hardCopyPlacedInRegister",
            "the printed hard copy must be placed in the separate Schedule H1 register before it is confirmed",
        ));
    }
    if !request.pharmacist_authenticated_hard_copy {
        return Err(validation(
            "pharmacistAuthenticatedHardCopy",
            "the registered pharmacist must authenticate the hard copy by hand before it is confirmed",
        ));
    }
    let store_id = current_store(&state).await?;
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let sale: Option<(String, String)> =
            sqlx::query_as("SELECT store_id,status FROM sale_documents WHERE id=?")
                .bind(&id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(map_database_error)?;
        let (sale_store, sale_status) = sale.ok_or(PrescriptionError::NotFound)?;
        if sale_store != store_id {
            return Err(PrescriptionError::NotFound);
        }
        if sale_status != "draft" {
            return Err(PrescriptionError::RecordState);
        }
        let prepared: Vec<(String, String)> = sqlx::query_as(
            "SELECT id,reference FROM prescription_h1_register_entries \
             WHERE sale_document_id=? AND store_id=? AND status='prepared' ORDER BY reference_value",
        )
        .bind(&id)
        .bind(&store_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(map_database_error)?;
        if prepared.is_empty() {
            return Err(PrescriptionError::RecordState);
        }
        for (entry_id, reference) in &prepared {
            let updated = sqlx::query(
                "UPDATE prescription_h1_register_entries SET status='confirmed',\
                 hard_copy_placed_in_register=1,pharmacist_authenticated_hard_copy=1,\
                 confirmed_by_user_id=?,confirmed_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
                 WHERE id=? AND status='prepared'",
            )
            .bind(&actor.id)
            .bind(entry_id)
            .execute(&mut *connection)
            .await;
            match updated {
                Ok(done) if done.rows_affected() == 1 => {}
                // The database refuses a confirmation whose pharmacist is no longer a registered
                // pharmacist on record, or whose confirmer is not a dispensing user.
                Ok(_) | Err(_) => return Err(PrescriptionError::RecordState),
            }
            record_event(
                &mut connection,
                Change {
                    entity_type: "prescription_h1_register_entry",
                    entity_id: entry_id,
                    revision: 2,
                    action: "updated",
                    reason: Some("hard copy placed in the H1 register and authenticated by the registered pharmacist"),
                },
                serde_json::json!({ "entryId": entry_id, "reference": reference, "status": "confirmed" }),
                &actor.id,
            )
            .await?;
        }
        Ok(())
    }
    .await;
    finish(connection, outcome).await?;
    Ok(Json(fetch_sale_h1_sheet(&state, &store_id, &id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct H1RegisterQuery {
    from: Option<String>,
    to: Option<String>,
}

/// The H1 working record for a period, chronologically, for inspection. Dispensing roles only.
/// Searched by date; there is no patient-name search.
async fn list_h1_register(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<H1RegisterQuery>,
) -> Result<Json<H1Sheet>, PrescriptionError> {
    require_dispenser_reader(&state, &headers).await?;
    let store_id = current_store(&state).await?;
    let today: String =
        sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now','+5 hours','+30 minutes')")
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?;
    let from = match query.from.as_deref() {
        Some(value) => validate_date(value, "from")?,
        None => today.clone(),
    };
    let to = match query.to.as_deref() {
        Some(value) => validate_date(value, "to")?,
        None => today,
    };
    if to < from {
        return Err(validation("to", "must not be before from"));
    }
    let entries: Vec<H1EntryRow> = sqlx::query_as(&format!(
        "{H1_ENTRY_SELECT} WHERE entry.store_id=? AND entry.date_of_supply BETWEEN ? AND ? \
         ORDER BY entry.date_of_supply,entry.reference_value"
    ))
    .bind(&store_id)
    .bind(&from)
    .bind(&to)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    let annotations = fetch_h1_annotations(&state, &entries).await?;
    Ok(Json(H1Sheet {
        store: h1_store_context(&state, &store_id).await?,
        entries,
        annotations,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnnotateH1Request {
    note: String,
}

/// Appends a note to a finalized entry — a correction noticed later, a remark for the inspector.
/// The entry itself never changes. Rule 65 prescribes no correction format, and none is claimed.
async fn annotate_h1_entry(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<AnnotateH1Request>,
) -> Result<(StatusCode, Json<H1AnnotationRow>), PrescriptionError> {
    let actor = require_dispenser(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(issue)?;
    let note = required_text(&request.note, "note", 1000).map_err(issue)?;
    if note.chars().count() < 3 {
        return Err(validation("note", "must say what is being noted"));
    }
    let store_id = current_store(&state).await?;
    let annotation_id = Uuid::now_v7().to_string();
    let mut connection = begin_immediate(&state).await?;
    let outcome = async {
        let entry: Option<(String, String)> = sqlx::query_as(
            "SELECT status,reference FROM prescription_h1_register_entries WHERE id=? AND store_id=?",
        )
        .bind(&id)
        .bind(&store_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(map_database_error)?;
        let (status, reference) = entry.ok_or(PrescriptionError::NotFound)?;
        if status != "finalized" {
            return Err(PrescriptionError::RecordState);
        }
        sqlx::query(
            "INSERT INTO prescription_h1_register_annotations (id,entry_id,note,created_by_user_id,\
             created_at_utc) VALUES (?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&annotation_id)
        .bind(&id)
        .bind(&note)
        .bind(&actor.id)
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
        // The note itself may name people, so the audit carries identifiers only.
        record_event(
            &mut connection,
            Change {
                entity_type: "prescription_h1_register_annotation",
                entity_id: &annotation_id,
                revision: 1,
                action: "created",
                reason: None,
            },
            serde_json::json!({ "annotationId": annotation_id, "entryId": id, "reference": reference }),
            &actor.id,
        )
        .await
    }
    .await;
    finish(connection, outcome).await?;
    let row: H1AnnotationRow = sqlx::query_as(
        "SELECT annotation.id,annotation.entry_id,annotation.note,\
         author.display_name AS created_by_display_name,annotation.created_at_utc \
         FROM prescription_h1_register_annotations annotation \
         LEFT JOIN users author ON author.id=annotation.created_by_user_id WHERE annotation.id=?",
    )
    .bind(&annotation_id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(row)))
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route(
            "/api/v1/prescribers",
            get(list_prescribers).post(create_prescriber),
        )
        .route("/api/v1/prescribers/{id}", put(update_prescriber))
        .route("/api/v1/prescribers/{id}/archive", post(archive_prescriber))
        .route(
            "/api/v1/prescriptions",
            get(list_prescriptions).post(create_prescription),
        )
        .route(
            "/api/v1/prescriptions/{id}",
            get(get_prescription).put(correct_prescription),
        )
        .route(
            "/api/v1/prescriptions/{id}/archive",
            post(archive_prescription),
        )
        .route("/api/v1/prescription-supply-records/{id}", get(get_record))
        .route(
            "/api/v1/prescription-supply-records/{id}/confirm",
            post(confirm_record),
        )
        .route(
            "/api/v1/prescription-supply-records/{id}/void",
            post(void_record),
        )
        .route("/api/v1/sales/{id}/h1-register", get(get_sale_h1_sheet))
        .route(
            "/api/v1/sales/{id}/h1-register/confirm",
            post(confirm_sale_h1_entries),
        )
        .route("/api/v1/h1-register", get(list_h1_register))
        .route(
            "/api/v1/h1-register/{id}/annotations",
            post(annotate_h1_entry),
        )
}
