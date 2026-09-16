//! Phase 1G-0 store tax identity.
//!
//! The Store's own GST registration and place of supply. Phase 1E gave the supplier one; this is
//! the other half of the comparison that decides `CGST + SGST` versus `IGST`, so Phase 1G can
//! resolve tax treatment from data instead of assuming it or asking the browser.
//!
//! GSTIN validation is reused from the party domain rather than re-implemented, so the two can
//! never drift apart.
//!
//! Phase 1L-A adds the rest of the seller: legal name, operating address, contact details and
//! pharmacy licences. Those are not tax facts — they are the particulars Rule 46(a) of the CGST
//! Rules and Rule 65(4)(3)(i) of the Drugs Rules require on the document a pharmacy hands over —
//! so they live beside the tax identity rather than in a second settings system.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text, required_text, validate_uuid_v7},
    parties::{
        GST_REGISTRATION_STATUSES, gstin_state_code, normalize_email, normalize_gstin,
        normalize_licence_comparison, normalize_phone,
    },
    store_profile,
};

#[derive(Debug)]
enum StoreProfileError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    Revision {
        expected: i64,
        current: i64,
    },
    TaxConflict,
    /// The same licence number is already recorded and active.
    LicenceConflict,
    /// An archived licence cannot be edited; restore it first.
    LicenceArchived,
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

impl IntoResponse for StoreProfileError {
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
                simple("not_found", "This workspace has no store configured yet."),
            ),
            Self::Revision { expected, current } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "revision_conflict",
                    message: "The store profile changed after it was read.",
                    issues: Vec::new(),
                    expected_revision: Some(expected),
                    current_revision: Some(current),
                },
            ),
            Self::TaxConflict => (
                StatusCode::CONFLICT,
                simple(
                    "store_tax_conflict",
                    "The tax registration does not agree with the selected State.",
                ),
            ),
            Self::LicenceConflict => (
                StatusCode::CONFLICT,
                simple(
                    "store_licence_conflict",
                    "That licence number is already recorded for this pharmacy.",
                ),
            ),
            Self::LicenceArchived => (
                StatusCode::CONFLICT,
                simple(
                    "store_licence_archived",
                    "This licence is archived. Restore it before editing it.",
                ),
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

impl From<AuthError> for StoreProfileError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

fn validation(field: &str, message: &str) -> StoreProfileError {
    StoreProfileError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn map_database_error(error: sqlx::Error) -> StoreProfileError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return StoreProfileError::ServiceBusy;
        }
        if message.contains("store_tax_conflict") {
            return StoreProfileError::TaxConflict;
        }
        if message.contains("foreign key constraint failed")
            || message.contains("check constraint failed")
        {
            return StoreProfileError::Validation(vec![CatalogValidationIssue {
                field: "storeProfile".to_owned(),
                message: "violates a store integrity constraint".to_owned(),
            }]);
        }
    }
    StoreProfileError::Internal
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStoreTaxIdentityRequest {
    expected_revision: i64,
    gst_registration_status: String,
    gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct StoredRow {
    store_id: String,
    display_name: String,
    revision: i64,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreTaxIdentityResponse {
    store_id: String,
    display_name: String,
    revision: i64,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    /// True once a place of supply is recorded — the single fact a GST-aware document needs.
    /// An unregistered store can be complete: it still supplies from somewhere.
    complete: bool,
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route(
            "/api/v1/store/tax-identity",
            get(get_tax_identity).put(put_tax_identity),
        )
        // One composite read, so the browser never assembles a legal identity from four requests.
        .route("/api/v1/store/profile", get(get_profile).put(put_profile))
        .route("/api/v1/store/address", put(put_address))
        .route("/api/v1/store/licences", post(create_licence))
        .route("/api/v1/store/licences/{id}", put(update_licence))
        .route("/api/v1/store/licences/{id}/archive", post(archive_licence))
        .route("/api/v1/store/licences/{id}/restore", post(restore_licence))
}

async fn get_tax_identity(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<StoreTaxIdentityResponse>, StoreProfileError> {
    auth::require_authenticated_actor(&state.pool, &headers).await?;
    fetch(&state.pool).await.map(Json)
}

/// Owner/Admin only, behind the frozen Host/Origin mutation protection.
async fn require_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, StoreProfileError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn put_tax_identity(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<UpdateStoreTaxIdentityRequest>,
) -> Result<Json<StoreTaxIdentityResponse>, StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;

    let registration = request.gst_registration_status.trim().to_ascii_lowercase();
    if !GST_REGISTRATION_STATUSES.contains(&registration.as_str()) {
        return Err(validation(
            "gstRegistrationStatus",
            "must be registered, unregistered, or unknown",
        ));
    }
    let raw_gstin = request
        .gstin
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // The same three checks a supplier GSTIN gets, from the same function, so they cannot drift.
    let (gstin, normalized_gstin) = match (registration.as_str(), raw_gstin) {
        ("registered", Some(value)) => {
            let (display, normalized) = normalize_gstin(value)
                .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
            (Some(display), Some(normalized))
        }
        ("registered", None) => {
            return Err(validation(
                "gstin",
                "is required when the store is recorded as registered",
            ));
        }
        (_, Some(_)) => {
            return Err(validation(
                "gstin",
                "is recorded only when the registration status is registered",
            ));
        }
        (_, None) => (None, None),
    };

    let place_of_supply_state_id = match request
        .place_of_supply_state_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => {
            let id = Uuid::parse_str(value)
                .ok()
                .filter(|id| id.get_version_num() == 7)
                .ok_or_else(|| validation("placeOfSupplyStateId", "must be a UUIDv7"))?;
            Some(id.to_string())
        }
        None => None,
    };
    if normalized_gstin.is_some() && place_of_supply_state_id.is_none() {
        return Err(validation(
            "placeOfSupplyStateId",
            "is required when a GSTIN is recorded, because the GSTIN encodes it",
        ));
    }
    let reason = optional_text(request.reason.as_deref(), "reason", 500)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let current: Option<(String, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT store_id,revision,normalized_gstin,place_of_supply_state_id FROM store_identity LIMIT 1",
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (store_id, revision, previous_gstin, previous_state) =
        current.ok_or(StoreProfileError::NotFound)?;
    if revision != request.expected_revision {
        return Err(StoreProfileError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }

    // Only a State that actually changes is checked for activity, so a State archived after it was
    // chosen keeps the store editable rather than trapping it.
    if place_of_supply_state_id != previous_state
        && let Some(candidate) = place_of_supply_state_id.as_deref()
    {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM state_codes WHERE id=?")
                .bind(candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(map_database_error)?;
        match status.as_deref() {
            Some("active") => {}
            Some(_) => {
                return Err(validation(
                    "placeOfSupplyStateId",
                    "names a State that has been archived",
                ));
            }
            None => {
                return Err(validation(
                    "placeOfSupplyStateId",
                    "names a State that does not exist",
                ));
            }
        }
    }
    // Checked here for a precise message; the trigger enforces it regardless.
    if let Some(normalized) = normalized_gstin.as_deref() {
        let state_code: Option<String> = sqlx::query_scalar(
            "SELECT state_code FROM state_codes WHERE id=? AND jurisdiction='IN'",
        )
        .bind(place_of_supply_state_id.as_deref().unwrap_or_default())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if state_code.as_deref() != Some(gstin_state_code(normalized)) {
            return Err(StoreProfileError::TaxConflict);
        }
    }

    let next = revision + 1;
    sqlx::query(
        "UPDATE store_identity SET revision=?,gst_registration_status=?,gstin=?,normalized_gstin=?,\
         place_of_supply_state_id=? WHERE store_id=? AND revision=?",
    )
    .bind(next)
    .bind(&registration)
    .bind(&gstin)
    .bind(&normalized_gstin)
    .bind(&place_of_supply_state_id)
    .bind(&store_id)
    .bind(revision)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    // The audit answers what changed, from what, to what, by whom, and when. The store id is a
    // UUIDv7 like every other audited entity, so it satisfies the frozen event shape.
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,'store_tax_identity',?,?,'updated',strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&store_id)
    .bind(next)
    .bind(&reason)
    .bind(
        json!({
            "previous": { "normalizedGstin": previous_gstin, "placeOfSupplyStateId": previous_state },
            "next": {
                "gstRegistrationStatus": registration,
                "normalizedGstin": normalized_gstin,
                "placeOfSupplyStateId": place_of_supply_state_id,
            },
        })
        .to_string(),
    )
    .bind(&actor.id)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch(&state.pool).await.map(Json)
}

async fn fetch(pool: &SqlitePool) -> Result<StoreTaxIdentityResponse, StoreProfileError> {
    let row = sqlx::query_as::<_, StoredRow>(
        "SELECT store_id,display_name,revision,gst_registration_status,gstin,normalized_gstin,\
         place_of_supply_state_id FROM store_identity LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(StoreProfileError::NotFound)?;
    Ok(StoreTaxIdentityResponse {
        complete: row.place_of_supply_state_id.is_some(),
        store_id: row.store_id,
        display_name: row.display_name,
        revision: row.revision,
        gst_registration_status: row.gst_registration_status,
        gstin: row.gstin,
        normalized_gstin: row.normalized_gstin,
        place_of_supply_state_id: row.place_of_supply_state_id,
    })
}

/// The Store's place of supply, for the phases that must decide tax treatment.
///
/// Returns `None` when it has not been recorded, so a caller can refuse to post rather than assume
/// a treatment. Phase 1G compares this with the supplier's place of supply.
pub async fn store_place_of_supply(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT place_of_supply_state_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

// ---------------------------------------------------------------------------------------------
// Phase 1L-A — the legal seller profile
//
// The tax identity above answers "how is this supply taxed". These answer "who sold it", which is
// a different question with a different authority behind it: Rule 46(a) of the CGST Rules wants the
// supplier's name, address and GSTIN, and Rule 65(4)(3)(i) of the Drugs Rules wants the dealer's
// name, address and sale licence number on every retail drug memo whether or not GST applies.
//
// One composite read and several narrow writes. The read exists so the browser never assembles a
// legal identity from four requests; the writes stay narrow so correcting a phone number cannot
// collide with somebody editing the GSTIN.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStoreProfileRequest {
    expected_revision: i64,
    display_name: String,
    legal_name: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStoreAddressRequest {
    /// Absent when recording the address for the first time. Present, and matched, afterwards.
    expected_revision: Option<i64>,
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    state_id: Option<String>,
    postal_code: Option<String>,
    country_code: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreLicenceRequest {
    expected_revision: Option<i64>,
    licence_type: String,
    licence_number: String,
    issuing_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleRequest {
    expected_revision: i64,
    reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoreAddressResponse {
    pub id: String,
    pub revision: i64,
    pub line1: String,
    pub line2: Option<String>,
    pub city: Option<String>,
    pub state_id: Option<String>,
    pub postal_code: Option<String>,
    pub country_code: String,
}

#[derive(Debug, Serialize, FromRow, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoreLicenceResponse {
    pub id: String,
    pub revision: i64,
    pub status: String,
    pub licence_type: String,
    pub licence_number: String,
    pub issuing_authority: Option<String>,
    pub valid_from: Option<String>,
    pub valid_upto: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MissingFactResponse {
    field: &'static str,
    message: &'static str,
}

/// Everything the Store Profile screen needs, in one read.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreProfileResponse {
    store_id: String,
    revision: i64,
    display_name: String,
    legal_name: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
    /// The Phase 1G-0 meaning: a place of supply is recorded, so a GST-aware document can be posted.
    tax_complete: bool,
    address: Option<StoreAddressResponse>,
    licences: Vec<StoreLicenceResponse>,
    /// True when a Sale may be posted. Distinct from `taxComplete`: an unregistered pharmacy still
    /// needs a name, an address and a licence.
    seller_complete: bool,
    missing_seller_facts: Vec<MissingFactResponse>,
}

#[derive(Debug, FromRow)]
struct ProfileRow {
    store_id: String,
    display_name: String,
    revision: i64,
    legal_name: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    place_of_supply_state_id: Option<String>,
}

async fn get_profile(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    auth::require_authenticated_actor(&state.pool, &headers).await?;
    fetch_profile(&state.pool).await.map(Json)
}

async fn fetch_profile(pool: &SqlitePool) -> Result<StoreProfileResponse, StoreProfileError> {
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT store_id,display_name,revision,legal_name,primary_phone,primary_email,\
         gst_registration_status,gstin,normalized_gstin,place_of_supply_state_id \
         FROM store_identity LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(StoreProfileError::NotFound)?;

    let address = sqlx::query_as::<_, StoreAddressResponse>(
        "SELECT id,revision,line1,line2,city,state_id,postal_code,country_code \
         FROM store_addresses WHERE store_id=?",
    )
    .bind(&row.store_id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?;

    let licences = sqlx::query_as::<_, StoreLicenceResponse>(
        "SELECT id,revision,status,licence_type,licence_number,issuing_authority,valid_from,\
         valid_upto FROM store_licences WHERE store_id=? ORDER BY status,licence_type,\
         normalized_licence_number",
    )
    .bind(&row.store_id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;

    let state_name = match address.as_ref().and_then(|value| value.state_id.as_deref()) {
        Some(id) => state_display_name(pool, id).await?,
        None => None,
    };
    let source = seller_source(&row, address.as_ref(), state_name, &licences);
    let missing = store_profile::missing_facts(&source);

    Ok(StoreProfileResponse {
        tax_complete: row.place_of_supply_state_id.is_some(),
        seller_complete: missing.is_empty(),
        missing_seller_facts: missing
            .into_iter()
            .map(|fact| MissingFactResponse {
                field: fact.field(),
                message: fact.message(),
            })
            .collect(),
        store_id: row.store_id,
        revision: row.revision,
        display_name: row.display_name,
        legal_name: row.legal_name,
        primary_phone: row.primary_phone,
        primary_email: row.primary_email,
        gst_registration_status: row.gst_registration_status,
        gstin: row.gstin,
        normalized_gstin: row.normalized_gstin,
        place_of_supply_state_id: row.place_of_supply_state_id,
        address,
        licences,
    })
}

/// Assembles the domain view the completeness rule reads, from rows that were fetched together.
fn seller_source(
    row: &ProfileRow,
    address: Option<&StoreAddressResponse>,
    state_name: Option<String>,
    licences: &[StoreLicenceResponse],
) -> store_profile::SellerProfileSource {
    store_profile::SellerProfileSource {
        legal_name: row.legal_name.clone(),
        trade_name: row.display_name.clone(),
        address_line1: address.map(|value| value.line1.clone()),
        address_line2: address.and_then(|value| value.line2.clone()),
        city: address.and_then(|value| value.city.clone()),
        postal_code: address.and_then(|value| value.postal_code.clone()),
        state_name,
        phone: row.primary_phone.clone(),
        email: row.primary_email.clone(),
        active_licences: licences
            .iter()
            .filter(|licence| licence.status == "active")
            .map(|licence| store_profile::SellerLicence {
                licence_type: licence.licence_type.clone(),
                licence_number: licence.licence_number.clone(),
                normalized_licence_number: normalize_licence_comparison(&licence.licence_number),
            })
            .collect(),
    }
}

async fn state_display_name(
    pool: &SqlitePool,
    state_id: &str,
) -> Result<Option<String>, StoreProfileError> {
    sqlx::query_scalar("SELECT display_name FROM state_codes WHERE id=?")
        .bind(state_id)
        .fetch_optional(pool)
        .await
        .map_err(map_database_error)
}

// ---------------------------------------------------------------------------------------------
// Business identity and contact
// ---------------------------------------------------------------------------------------------

async fn put_profile(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<UpdateStoreProfileRequest>,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;

    let display_name = required_text(&request.display_name, "displayName", 200)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let legal_name = optional_text(request.legal_name.as_deref(), "legalName", 250)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    // The same normalisation a party's contact details get, from the same functions, so the two
    // can never drift apart.
    let primary_phone = match blank(request.primary_phone.as_deref()) {
        Some(value) => Some(
            normalize_phone(value).map_err(|issue| StoreProfileError::Validation(vec![issue]))?,
        ),
        None => None,
    };
    let primary_email = match blank(request.primary_email.as_deref()) {
        Some(value) => Some(
            normalize_email(value).map_err(|issue| StoreProfileError::Validation(vec![issue]))?,
        ),
        None => None,
    };
    let reason = optional_text(request.reason.as_deref(), "reason", 500)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let current: Option<(String, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT store_id,revision,display_name,legal_name FROM store_identity LIMIT 1",
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (store_id, revision, previous_display, previous_legal) =
        current.ok_or(StoreProfileError::NotFound)?;
    if revision != request.expected_revision {
        return Err(StoreProfileError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }

    let next = revision + 1;
    sqlx::query(
        "UPDATE store_identity SET revision=?,display_name=?,legal_name=?,primary_phone=?,\
         primary_email=? WHERE store_id=? AND revision=?",
    )
    .bind(next)
    .bind(&display_name)
    .bind(&legal_name)
    .bind(&primary_phone)
    .bind(&primary_email)
    .bind(&store_id)
    .bind(revision)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    record_event(
        &mut transaction,
        ProfileEvent {
            entity_type: "store_profile",
            entity_id: &store_id,
            entity_revision: next,
            action: "updated",
            reason: reason.as_deref(),
            payload: json!({
                "previous": { "displayName": previous_display, "legalName": previous_legal },
                "next": { "displayName": display_name, "legalName": legal_name },
            }),
        },
        &actor.id,
    )
    .await?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch_profile(&state.pool).await.map(Json)
}

// ---------------------------------------------------------------------------------------------
// Operating address
// ---------------------------------------------------------------------------------------------

async fn put_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<UpdateStoreAddressRequest>,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;

    let line1 = required_text(&request.line1, "line1", 200)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let line2 = optional_text(request.line2.as_deref(), "line2", 200)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let city = optional_text(request.city.as_deref(), "city", 100)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let postal_code = match blank(request.postal_code.as_deref()) {
        Some(value) => {
            // The frozen `party_addresses` rule, unchanged. A six-digit Indian PIN passes, and so
            // does a code from anywhere else; tightening it here for the seller alone would make
            // the product disagree with itself about what an address is.
            let normalized = value.trim().to_ascii_uppercase();
            if !(3..=16).contains(&normalized.len())
                || !normalized
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b" -".contains(&byte))
            {
                return Err(validation(
                    "postalCode",
                    "may contain only letters, digits, spaces, and hyphens",
                ));
            }
            Some(normalized)
        }
        None => None,
    };
    let country_code = request
        .country_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("IN")
        .to_ascii_uppercase();
    if country_code.len() != 2 || !country_code.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(validation(
            "countryCode",
            "must be a two-letter ASCII country code",
        ));
    }
    let state_id = match blank(request.state_id.as_deref()) {
        Some(value) => Some(
            validate_uuid_v7(value, "stateId")
                .map_err(|issue| StoreProfileError::Validation(vec![issue]))?,
        ),
        None => None,
    };
    let reason = optional_text(request.reason.as_deref(), "reason", 500)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let store_id: Option<String> =
        sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let store_id = store_id.ok_or(StoreProfileError::NotFound)?;

    // The State on the address is the State the premises are in. It is deliberately NOT reconciled
    // with the GSTIN's State: a registration and a shop door can lawfully disagree, and silently
    // rewriting one from the other would destroy a fact the operator entered on purpose. The
    // GSTIN's own State agreement is still enforced, unchanged, by the tax-identity endpoint.
    if let Some(candidate) = state_id.as_deref() {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM state_codes WHERE id=?")
                .bind(candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(map_database_error)?;
        match status.as_deref() {
            Some("active") => {}
            Some(_) => {
                return Err(validation(
                    "stateId",
                    "names a State that has been archived",
                ));
            }
            None => return Err(validation("stateId", "names a State that does not exist")),
        }
    }

    let existing: Option<(String, i64)> =
        sqlx::query_as("SELECT id,revision FROM store_addresses WHERE store_id=?")
            .bind(&store_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let now = database_now(&mut transaction).await?;

    let (address_id, next_revision, action) = match existing {
        Some((id, revision)) => {
            let expected = request.expected_revision.unwrap_or(0);
            if expected != revision {
                return Err(StoreProfileError::Revision {
                    expected,
                    current: revision,
                });
            }
            let next = revision + 1;
            sqlx::query(
                "UPDATE store_addresses SET revision=?,line1=?,line2=?,city=?,state_id=?,\
                 postal_code=?,country_code=?,updated_at_utc=? WHERE id=? AND revision=?",
            )
            .bind(next)
            .bind(&line1)
            .bind(&line2)
            .bind(&city)
            .bind(&state_id)
            .bind(&postal_code)
            .bind(&country_code)
            .bind(&now)
            .bind(&id)
            .bind(revision)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            (id, next, "updated")
        }
        None => {
            // A revision was sent for an address that does not exist: the caller is working from a
            // stale read, which is exactly what the revision is for.
            if request.expected_revision.is_some_and(|value| value != 0) {
                return Err(StoreProfileError::Revision {
                    expected: request.expected_revision.unwrap_or_default(),
                    current: 0,
                });
            }
            let id = Uuid::now_v7().to_string();
            sqlx::query(
                "INSERT INTO store_addresses \
                 (id,store_id,revision,line1,line2,city,state_id,postal_code,country_code,\
                  created_at_utc,updated_at_utc) VALUES (?,?,1,?,?,?,?,?,?,?,?)",
            )
            .bind(&id)
            .bind(&store_id)
            .bind(&line1)
            .bind(&line2)
            .bind(&city)
            .bind(&state_id)
            .bind(&postal_code)
            .bind(&country_code)
            .bind(&now)
            .bind(&now)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
            (id, 1, "created")
        }
    };

    record_event(
        &mut transaction,
        ProfileEvent {
            entity_type: "store_address",
            entity_id: &address_id,
            entity_revision: next_revision,
            action,
            reason: reason.as_deref(),
            payload: json!({ "line1": line1, "city": city, "postalCode": postal_code, "stateId": state_id }),
        },
        &actor.id,
    )
    .await?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch_profile(&state.pool).await.map(Json)
}

// ---------------------------------------------------------------------------------------------
// Pharmacy licences
// ---------------------------------------------------------------------------------------------

struct PreparedLicence {
    licence_type: String,
    licence_number: String,
    normalized: String,
    issuing_authority: Option<String>,
    valid_from: Option<String>,
    valid_upto: Option<String>,
    reason: Option<String>,
}

fn prepare_licence(request: &StoreLicenceRequest) -> Result<PreparedLicence, StoreProfileError> {
    let licence_type = required_text(&request.licence_type, "licenceType", 60)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let licence_number = required_text(&request.licence_number, "licenceNumber", 100)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    // Comparison only. The printed number stays exactly as the operator transcribed it from the
    // certificate, because no authority publishes a format that holds across States.
    let normalized = normalize_licence_comparison(&licence_number);
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
    .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let valid_from = optional_date(request.valid_from.as_deref(), "validFrom")?;
    let valid_upto = optional_date(request.valid_upto.as_deref(), "validUpto")?;
    if let (Some(from), Some(upto)) = (valid_from.as_deref(), valid_upto.as_deref())
        && upto < from
    {
        return Err(validation(
            "validUpto",
            "cannot be before the start of validity",
        ));
    }
    let reason = optional_text(request.reason.as_deref(), "reason", 500)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    Ok(PreparedLicence {
        licence_type,
        licence_number,
        normalized,
        issuing_authority,
        valid_from,
        valid_upto,
        reason,
    })
}

fn optional_date(value: Option<&str>, field: &str) -> Result<Option<String>, StoreProfileError> {
    match blank(value) {
        Some(text) => {
            let trimmed = text.trim();
            time::Date::parse(
                trimmed,
                time::macros::format_description!("[year]-[month]-[day]"),
            )
            .map_err(|_| validation(field, "must be a date as YYYY-MM-DD"))?;
            Ok(Some(trimmed.to_owned()))
        }
        None => Ok(None),
    }
}

async fn create_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<StoreLicenceRequest>,
) -> Result<(StatusCode, Json<StoreProfileResponse>), StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;
    let prepared = prepare_licence(&request)?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let store_id: Option<String> =
        sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let store_id = store_id.ok_or(StoreProfileError::NotFound)?;
    let now = database_now(&mut transaction).await?;
    let id = Uuid::now_v7().to_string();

    sqlx::query(
        "INSERT INTO store_licences \
         (id,store_id,revision,status,licence_type,licence_number,normalized_licence_number,\
          issuing_authority,valid_from,valid_upto,created_at_utc,updated_at_utc) \
         VALUES (?,?,1,'active',?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&store_id)
    .bind(&prepared.licence_type)
    .bind(&prepared.licence_number)
    .bind(&prepared.normalized)
    .bind(&prepared.issuing_authority)
    .bind(&prepared.valid_from)
    .bind(&prepared.valid_upto)
    .bind(&now)
    .bind(&now)
    .execute(&mut *transaction)
    .await
    .map_err(map_licence_error)?;

    record_event(
        &mut transaction,
        ProfileEvent {
            entity_type: "store_licence",
            entity_id: &id,
            entity_revision: 1,
            action: "created",
            reason: prepared.reason.as_deref(),
            payload: json!({ "licenceType": prepared.licence_type, "licenceNumber": prepared.licence_number }),
        },
        &actor.id,
    )
    .await?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch_profile(&state.pool)
        .await
        .map(|body| (StatusCode::CREATED, Json(body)))
}

async fn update_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<StoreLicenceRequest>,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;
    let id =
        validate_uuid_v7(&id, "id").map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let prepared = prepare_licence(&request)?;
    let expected = request
        .expected_revision
        .ok_or_else(|| validation("expectedRevision", "is required"))?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let current: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM store_licences WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(StoreProfileError::NotFound)?;
    if revision != expected {
        return Err(StoreProfileError::Revision {
            expected,
            current: revision,
        });
    }
    if status != "active" {
        return Err(StoreProfileError::LicenceArchived);
    }
    let now = database_now(&mut transaction).await?;
    let next = revision + 1;

    sqlx::query(
        "UPDATE store_licences SET revision=?,licence_type=?,licence_number=?,\
         normalized_licence_number=?,issuing_authority=?,valid_from=?,valid_upto=?,updated_at_utc=? \
         WHERE id=? AND revision=?",
    )
    .bind(next)
    .bind(&prepared.licence_type)
    .bind(&prepared.licence_number)
    .bind(&prepared.normalized)
    .bind(&prepared.issuing_authority)
    .bind(&prepared.valid_from)
    .bind(&prepared.valid_upto)
    .bind(&now)
    .bind(&id)
    .bind(revision)
    .execute(&mut *transaction)
    .await
    .map_err(map_licence_error)?;

    record_event(
        &mut transaction,
        ProfileEvent {
            entity_type: "store_licence",
            entity_id: &id,
            entity_revision: next,
            action: "updated",
            reason: prepared.reason.as_deref(),
            payload: json!({ "licenceType": prepared.licence_type, "licenceNumber": prepared.licence_number }),
        },
        &actor.id,
    )
    .await?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch_profile(&state.pool).await.map(Json)
}

async fn archive_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    set_licence_status(state, headers, id, request, false).await
}

async fn restore_licence(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    set_licence_status(state, headers, id, request, true).await
}

/// Archiving a licence is how a surrendered or superseded one leaves the printed line without
/// erasing that it once existed. Restoring re-checks the duplicate rule, because the number may
/// have been re-entered on another row in the meantime.
async fn set_licence_status(
    state: ReferenceState,
    headers: HeaderMap,
    id: String,
    request: LifecycleRequest,
    activate: bool,
) -> Result<Json<StoreProfileResponse>, StoreProfileError> {
    let actor = require_admin(&state, &headers).await?;
    let id =
        validate_uuid_v7(&id, "id").map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    let reason = optional_text(request.reason.as_deref(), "reason", 500)
        .map_err(|issue| StoreProfileError::Validation(vec![issue]))?;
    if !activate && reason.is_none() {
        return Err(validation("reason", "is required when archiving a licence"));
    }

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| StoreProfileError::Internal)?;
    let current: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,status FROM store_licences WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?;
    let (revision, status) = current.ok_or(StoreProfileError::NotFound)?;
    if revision != request.expected_revision {
        return Err(StoreProfileError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    let wanted = if activate { "active" } else { "archived" };
    if status == wanted {
        return Err(validation("status", "is already in that state"));
    }

    let now = database_now(&mut transaction).await?;
    let next = revision + 1;
    if activate {
        sqlx::query(
            "UPDATE store_licences SET revision=?,status='active',archived_at_utc=NULL,\
             archive_reason=NULL,updated_at_utc=? WHERE id=? AND revision=?",
        )
        .bind(next)
        .bind(&now)
        .bind(&id)
        .bind(revision)
        .execute(&mut *transaction)
        .await
        .map_err(map_licence_error)?;
    } else {
        sqlx::query(
            "UPDATE store_licences SET revision=?,status='archived',archived_at_utc=?,\
             archive_reason=?,updated_at_utc=? WHERE id=? AND revision=?",
        )
        .bind(next)
        .bind(&now)
        .bind(&reason)
        .bind(&now)
        .bind(&id)
        .bind(revision)
        .execute(&mut *transaction)
        .await
        .map_err(map_licence_error)?;
    }

    record_event(
        &mut transaction,
        ProfileEvent {
            entity_type: "store_licence",
            entity_id: &id,
            entity_revision: next,
            action: if activate { "restored" } else { "archived" },
            reason: reason.as_deref(),
            payload: json!({ "status": wanted }),
        },
        &actor.id,
    )
    .await?;

    transaction.commit().await.map_err(map_database_error)?;
    fetch_profile(&state.pool).await.map(Json)
}

/// The active-number unique index is the authority; this turns its violation into the typed refusal
/// the browser can explain, instead of a generic conflict.
fn map_licence_error(error: sqlx::Error) -> StoreProfileError {
    let message = error.to_string();
    if message.contains("store_licences_active_number_uq")
        || message.contains("store_licences.normalized_licence_number")
    {
        return StoreProfileError::LicenceConflict;
    }
    map_database_error(error)
}

/// One audited change, named at the call site rather than positioned.
struct ProfileEvent<'a> {
    entity_type: &'a str,
    entity_id: &'a str,
    entity_revision: i64,
    action: &'a str,
    reason: Option<&'a str>,
    payload: serde_json::Value,
}

async fn record_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: ProfileEvent<'_>,
    actor_id: &str,
) -> Result<(), StoreProfileError> {
    let ProfileEvent {
        entity_type,
        entity_id,
        entity_revision,
        action,
        reason,
        payload,
    } = event;
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,\
         occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(entity_type)
    .bind(entity_id)
    .bind(entity_revision)
    .bind(action)
    .bind(reason)
    .bind(payload.to_string())
    .bind(actor_id)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn database_now(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<String, StoreProfileError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

fn blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// The seller as it stands right now, for the posting path and for a legacy invoice's honest
/// "these are the current details" panel.
///
/// Reads every table the seller spans in ONE call so a caller inside a transaction sees one
/// coherent profile. Taking the rows separately across transactions is what would produce an
/// invoice with last week's name and this week's address.
pub async fn seller_profile_source(
    connection: &mut sqlx::SqliteConnection,
    store_id: &str,
) -> Result<store_profile::SellerProfileSource, sqlx::Error> {
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT store_id,display_name,revision,legal_name,primary_phone,primary_email,\
         gst_registration_status,gstin,normalized_gstin,place_of_supply_state_id \
         FROM store_identity WHERE store_id=?",
    )
    .bind(store_id)
    .fetch_one(&mut *connection)
    .await?;
    let address = sqlx::query_as::<_, StoreAddressResponse>(
        "SELECT id,revision,line1,line2,city,state_id,postal_code,country_code \
         FROM store_addresses WHERE store_id=?",
    )
    .bind(store_id)
    .fetch_optional(&mut *connection)
    .await?;
    let licences = sqlx::query_as::<_, StoreLicenceResponse>(
        "SELECT id,revision,status,licence_type,licence_number,issuing_authority,valid_from,\
         valid_upto FROM store_licences WHERE store_id=? AND status='active'",
    )
    .bind(store_id)
    .fetch_all(&mut *connection)
    .await?;
    let state_name = match address.as_ref().and_then(|value| value.state_id.as_deref()) {
        Some(id) => {
            sqlx::query_scalar::<_, String>("SELECT display_name FROM state_codes WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *connection)
                .await?
        }
        None => None,
    };
    Ok(seller_source(&row, address.as_ref(), state_name, &licences))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    /// Genuine published GSTINs, so the check digit is exercised rather than merely satisfied.
    const MAHARASHTRA_GSTIN: &str = "27AAPFU0939F1ZV";
    const MAHARASHTRA_STATE: &str = "01997300-0000-7000-8000-000000000027";
    const KARNATAKA_STATE: &str = "01997300-0000-7000-8000-000000000029";
    const OWNER_TOKEN: &str = "store-owner-session-token";
    const CASHIER_TOKEN: &str = "store-cashier-session-token";
    const URI: &str = "/api/v1/store/tax-identity";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        owner_id: String,
        store_id: String,
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("store.sqlite3"))
            .await
            .unwrap();
        let store_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Care Pharmacy','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&store_id)
        .execute(&pool)
        .await
        .unwrap();
        let owner_id = insert_store_session(&pool, "owner_admin", OWNER_TOKEN).await;
        insert_store_session(&pool, "cashier", CASHIER_TOKEN).await;
        Fixture {
            _temp: temp,
            pool,
            owner_id,
            store_id,
        }
    }

    fn update(revision: i64, status: &str, gstin: Option<&str>, state: Option<&str>) -> Value {
        json!({
            "expectedRevision": revision,
            "gstRegistrationStatus": status,
            "gstin": gstin,
            "placeOfSupplyStateId": state,
            "reason": "Recorded from the registration certificate"
        })
    }

    #[tokio::test]
    async fn a_new_store_reads_as_incomplete_which_is_what_blocks_a_gst_document() {
        let f = fixture().await;
        let (status, body) = request_store(f.pool.clone(), "GET", URI, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["gstRegistrationStatus"], "unknown");
        assert_eq!(body["placeOfSupplyStateId"], Value::Null);
        assert_eq!(body["complete"], false);
        assert_eq!(body["revision"], 1);
        assert_eq!(body["displayName"], "Care Pharmacy");

        // The helper Phase 1G will call returns nothing, so it must refuse rather than assume.
        assert_eq!(store_place_of_supply(&f.pool).await.unwrap(), None);
    }

    #[tokio::test]
    async fn recording_a_registered_gstin_completes_the_store_and_bumps_the_revision() {
        let f = fixture().await;
        let (status, saved) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some(MAHARASHTRA_GSTIN),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["normalizedGstin"], MAHARASHTRA_GSTIN);
        assert_eq!(saved["placeOfSupplyStateId"], MAHARASHTRA_STATE);
        assert_eq!(saved["complete"], true);
        assert_eq!(saved["revision"], 2);

        // Phase 1G can now resolve the Store side of the treatment comparison.
        assert_eq!(
            store_place_of_supply(&f.pool).await.unwrap().as_deref(),
            Some(MAHARASHTRA_STATE)
        );
    }

    #[tokio::test]
    async fn an_unregistered_store_still_has_a_place_of_supply() {
        let f = fixture().await;
        // A store with no GSTIN still supplies from somewhere, so it can be complete.
        let (status, saved) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unregistered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["gstRegistrationStatus"], "unregistered");
        assert_eq!(saved["normalizedGstin"], Value::Null);
        assert_eq!(saved["complete"], true);
    }

    #[tokio::test]
    async fn a_gstin_must_agree_with_the_selected_state() {
        let f = fixture().await;
        // A Maharashtra GSTIN cannot belong to a Karnataka registration.
        let (status, refused) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some(MAHARASHTRA_GSTIN),
                Some(KARNATAKA_STATE),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "store_tax_conflict");

        // Nothing was written.
        let (_, unchanged) = request_store(f.pool.clone(), "GET", URI, Value::Null).await;
        assert_eq!(unchanged["revision"], 1);
        assert_eq!(unchanged["complete"], false);

        // The database refuses it independently, so the guarantee does not rest on the service.
        let direct = sqlx::query(
            "UPDATE store_identity SET gst_registration_status='registered',gstin=?,\
             normalized_gstin=?,place_of_supply_state_id=? WHERE store_id=?",
        )
        .bind(MAHARASHTRA_GSTIN)
        .bind(MAHARASHTRA_GSTIN)
        .bind(KARNATAKA_STATE)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await;
        assert!(direct.is_err(), "the trigger must reject a state mismatch");
    }

    #[tokio::test]
    async fn a_gstin_that_fails_its_check_digit_is_refused_by_the_shared_validator() {
        let f = fixture().await;
        // Well-shaped and wrong: only the checksum catches this, and it is the party validator.
        let (status, refused) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some("27AAPFU0939F1ZX"),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "gstin");
        assert!(
            refused["issues"][0]["message"]
                .as_str()
                .unwrap()
                .contains("check digit")
        );
    }

    #[tokio::test]
    async fn registration_status_and_gstin_presence_must_agree() {
        let f = fixture().await;
        // Claiming registration without a number.
        let (status, missing) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "registered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{missing}");
        assert_eq!(missing["issues"][0]["field"], "gstin");

        // Carrying a number while asserting there is none.
        let (status, contradictory) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "unregistered",
                Some(MAHARASHTRA_GSTIN),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{contradictory}");
        assert_eq!(contradictory["issues"][0]["field"], "gstin");

        // A GSTIN without a State cannot be stored, because the GSTIN encodes one.
        let (status, stateless) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "registered", Some(MAHARASHTRA_GSTIN), None),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{stateless}");
        assert_eq!(stateless["issues"][0]["field"], "placeOfSupplyStateId");

        // The database refuses the same contradiction independently.
        //
        // A valid State is supplied deliberately. Without it the write would be caught by the
        // separate "a GSTIN requires a place of supply" clause, and this assertion would pass even
        // if the registered-versus-GSTIN rule were deleted — which is exactly what a hardening
        // mutation revealed.
        let direct = sqlx::query(
            "UPDATE store_identity SET gst_registration_status='unregistered',gstin=?,\
             normalized_gstin=?,place_of_supply_state_id=? WHERE store_id=?",
        )
        .bind(MAHARASHTRA_GSTIN)
        .bind(MAHARASHTRA_GSTIN)
        .bind(MAHARASHTRA_STATE)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await;
        assert!(direct.is_err(), "the trigger must reject the contradiction");

        // And the mirror case: claiming registration while carrying no number.
        let missing_number = sqlx::query(
            "UPDATE store_identity SET gst_registration_status='registered',gstin=NULL,\
             normalized_gstin=NULL,place_of_supply_state_id=? WHERE store_id=?",
        )
        .bind(MAHARASHTRA_STATE)
        .bind(&f.store_id)
        .execute(&f.pool)
        .await;
        assert!(
            missing_number.is_err(),
            "the trigger must reject registration without a number"
        );
    }

    #[tokio::test]
    async fn an_archived_state_cannot_be_newly_chosen_but_never_traps_the_store() {
        let f = fixture().await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unregistered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        sqlx::query(
            "UPDATE state_codes SET status='archived',revision=2,\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='not used' WHERE id=?",
        )
        .bind(MAHARASHTRA_STATE)
        .execute(&f.pool)
        .await
        .unwrap();

        // Still readable: archiving a master is not retroactive deletion.
        let (_, readable) = request_store(f.pool.clone(), "GET", URI, Value::Null).await;
        assert_eq!(readable["placeOfSupplyStateId"], MAHARASHTRA_STATE);

        // Re-saving the SAME archived State is allowed, so the store never becomes uneditable.
        let (status, resaved) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(2, "unknown", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{resaved}");

        // Choosing a DIFFERENT archived State is refused.
        sqlx::query(
            "UPDATE state_codes SET status='archived',revision=2,\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='not used' WHERE id=?",
        )
        .bind(KARNATAKA_STATE)
        .execute(&f.pool)
        .await
        .unwrap();
        let (status, refused) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(3, "unknown", None, Some(KARNATAKA_STATE)),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "placeOfSupplyStateId");

        // And clearing is always possible.
        let (status, cleared) =
            request_store(f.pool.clone(), "PUT", URI, update(3, "unknown", None, None)).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert_eq!(cleared["complete"], false);
    }

    #[tokio::test]
    async fn a_stale_revision_cannot_overwrite_a_newer_profile() {
        let f = fixture().await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unregistered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        let (status, conflict) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unknown", None, Some(KARNATAKA_STATE)),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["expectedRevision"], 1);
        assert_eq!(conflict["currentRevision"], 2);

        let (_, current) = request_store(f.pool.clone(), "GET", URI, Value::Null).await;
        assert_eq!(current["placeOfSupplyStateId"], MAHARASHTRA_STATE);
    }

    #[tokio::test]
    async fn the_audit_records_the_previous_and_next_values_and_the_session_actor() {
        let f = fixture().await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unregistered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                2,
                "registered",
                Some(MAHARASHTRA_GSTIN),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;

        let events: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT change_payload,COALESCE(actor_id,''),entity_id,entity_revision \
             FROM master_change_events WHERE entity_type='store_tax_identity' ORDER BY entity_revision",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].2, f.store_id);
        assert_eq!(events[1].3, 3);
        assert_eq!(events[1].1, f.owner_id, "actor is the server session user");

        let payload: Value = serde_json::from_str(&events[1].0).unwrap();
        assert_eq!(payload["previous"]["normalizedGstin"], Value::Null);
        assert_eq!(
            payload["previous"]["placeOfSupplyStateId"],
            MAHARASHTRA_STATE
        );
        assert_eq!(payload["next"]["normalizedGstin"], MAHARASHTRA_GSTIN);
        assert_eq!(payload["next"]["gstRegistrationStatus"], "registered");
    }

    #[tokio::test]
    async fn reads_need_a_session_and_writes_need_owner_admin() {
        let f = fixture().await;
        let (status, anonymous) =
            request_store_as(f.pool.clone(), "GET", URI, Value::Null, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{anonymous}");
        assert_eq!(anonymous["code"], "authentication_required");

        // A read-only role may look: the purchase screen needs the Store's State.
        let (status, readable) =
            request_store_as(f.pool.clone(), "GET", URI, Value::Null, Some(CASHIER_TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{readable}");

        // But never write, and a spoofed actor in the body changes nothing.
        let mut spoofed = update(
            1,
            "registered",
            Some(MAHARASHTRA_GSTIN),
            Some(MAHARASHTRA_STATE),
        );
        spoofed["actorId"] = json!(f.owner_id);
        spoofed["role"] = json!("owner_admin");
        let (status, denied) =
            request_store_as(f.pool.clone(), "PUT", URI, spoofed, Some(CASHIER_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert_eq!(denied["code"], "authorization_denied");

        let (_, unchanged) = request_store(f.pool.clone(), "GET", URI, Value::Null).await;
        assert_eq!(unchanged["revision"], 1, "a spoofed actor must not write");
    }

    #[tokio::test]
    async fn store_errors_stay_free_of_database_detail() {
        let f = fixture().await;
        let (_, bad_checksum) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some("27AAPFU0939F1ZX"),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;
        let (_, mismatch) = request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some(MAHARASHTRA_GSTIN),
                Some(KARNATAKA_STATE),
            ),
        )
        .await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(1, "unregistered", None, Some(MAHARASHTRA_STATE)),
        )
        .await;
        let (_, conflict) =
            request_store(f.pool.clone(), "PUT", URI, update(1, "unknown", None, None)).await;

        for body in [bad_checksum, mismatch, conflict] {
            let text = body.to_string().to_ascii_lowercase();
            for leak in [
                "sqlite",
                "constraint failed",
                "update store_identity",
                "raise(",
                "trigger",
            ] {
                assert!(!text.contains(leak), "leaked {leak} in {text}");
            }
        }
    }

    async fn insert_store_session(pool: &SqlitePool, role: &str, token: &str) -> String {
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
        .bind(format!("{role} store user"))
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

    async fn request_store(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        request_store_as(pool, method, uri, body, Some(OWNER_TOKEN)).await
    }

    async fn request_store_as(
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

    // -----------------------------------------------------------------------------------------
    // Phase 1L-A — legal identity, address and licences
    // -----------------------------------------------------------------------------------------

    const PROFILE: &str = "/api/v1/store/profile";
    const ADDRESS: &str = "/api/v1/store/address";
    const LICENCES: &str = "/api/v1/store/licences";

    fn identity(revision: i64) -> Value {
        json!({
            "expectedRevision": revision,
            "displayName": "Care Pharmacy",
            "legalName": "Care Pharmacy Private Limited",
            "primaryPhone": "020 1234 5678",
            "primaryEmail": "Counter@Example.TEST",
            "reason": "Recorded from the registration certificate"
        })
    }

    fn address() -> Value {
        json!({
            "line1": "12 Market Road", "line2": "Near the bus stand",
            "city": "Pune", "stateId": MAHARASHTRA_STATE, "postalCode": "411001"
        })
    }

    fn licence(number: &str) -> Value {
        json!({ "licenceType": "Form 20", "licenceNumber": number })
    }

    /// Records the three particulars and returns the final profile.
    async fn complete_profile(f: &Fixture) -> Value {
        request_store(f.pool.clone(), "PUT", PROFILE, identity(1)).await;
        request_store(f.pool.clone(), "PUT", ADDRESS, address()).await;
        let (_, body) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        body
    }

    #[tokio::test]
    async fn a_new_store_cannot_yet_issue_a_memo_and_says_which_facts_are_missing() {
        let f = fixture().await;
        let (status, body) = request_store(f.pool.clone(), "GET", PROFILE, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sellerComplete"], false);
        assert!(body["address"].is_null());
        assert_eq!(body["licences"].as_array().unwrap().len(), 0);

        // The three particulars Rule 65(4)(3)(i) requires, each named so the operator is sent to a
        // field rather than told the profile is "incomplete".
        let fields: Vec<&str> = body["missingSellerFacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| fact["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, vec!["legalName", "address.line1", "licences"]);
    }

    #[tokio::test]
    async fn recording_all_three_particulars_makes_the_store_able_to_sell() {
        let f = fixture().await;
        let body = complete_profile(&f).await;
        assert_eq!(body["sellerComplete"], true);
        assert_eq!(body["missingSellerFacts"].as_array().unwrap().len(), 0);
        assert_eq!(body["legalName"], "Care Pharmacy Private Limited");
        // Contact details are normalised by the same functions a party's are.
        assert_eq!(body["primaryPhone"], "02012345678");
        assert_eq!(body["primaryEmail"], "counter@example.test");
        assert_eq!(body["address"]["line1"], "12 Market Road");
        assert_eq!(body["address"]["countryCode"], "IN");
        assert_eq!(body["licences"][0]["licenceNumber"], "MH-20-1234");
        assert_eq!(body["licences"][0]["status"], "active");
    }

    /// The trade name and the legal name are different facts and both survive.
    #[tokio::test]
    async fn a_trade_name_and_a_legal_name_are_stored_separately() {
        let f = fixture().await;
        let (status, body) = request_store(
            f.pool.clone(),
            "PUT",
            PROFILE,
            json!({
                "expectedRevision": 1,
                "displayName": "Care Chemists",
                "legalName": "Care Pharmacy Private Limited"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["displayName"], "Care Chemists");
        assert_eq!(body["legalName"], "Care Pharmacy Private Limited");
    }

    #[tokio::test]
    async fn blank_and_oversized_identity_text_is_refused() {
        let f = fixture().await;
        for (field, value) in [
            ("displayName", json!("   ")),
            ("displayName", json!("x".repeat(201))),
            ("legalName", json!("x".repeat(251))),
        ] {
            let mut body = identity(1);
            body[field] = value;
            let (status, refused) = request_store(f.pool.clone(), "PUT", PROFILE, body).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{field}: {refused}"
            );
            assert_eq!(refused["code"], "validation_failed");
        }
    }

    #[tokio::test]
    async fn a_malformed_phone_or_email_is_refused_with_the_field_named() {
        let f = fixture().await;
        for (field, value) in [
            ("primaryPhone", "12"),
            ("primaryPhone", "call me maybe"),
            ("primaryEmail", "not-an-email"),
            ("primaryEmail", "two@@example.test"),
        ] {
            let mut body = identity(1);
            body[field] = json!(value);
            let (status, refused) = request_store(f.pool.clone(), "PUT", PROFILE, body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}={value}");
            assert_eq!(refused["issues"][0]["field"], field);
        }
    }

    #[tokio::test]
    async fn an_address_is_validated_the_way_a_party_address_is() {
        let f = fixture().await;
        for (field, value) in [
            ("line1", json!("")),
            ("line1", json!("x".repeat(201))),
            ("postalCode", json!("41")),
            ("postalCode", json!("4110 01/A")),
            ("countryCode", json!("IND")),
        ] {
            let mut body = address();
            body[field] = value.clone();
            let (status, refused) = request_store(f.pool.clone(), "PUT", ADDRESS, body).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{field}={value}: {refused}"
            );
        }
        // The frozen party rule is permissive on purpose, and stays permissive here.
        let mut body = address();
        body["postalCode"] = json!("SW1A 1AA");
        let (status, saved) = request_store(f.pool.clone(), "PUT", ADDRESS, body).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["address"]["postalCode"], "SW1A 1AA");
    }

    /// The address State is the State the premises are in. It is recorded as entered and never
    /// reconciled with the GSTIN's State, because audit item U2 is unresolved and silently
    /// rewriting one from the other would destroy a fact the operator entered deliberately.
    #[tokio::test]
    async fn the_address_state_is_not_rewritten_from_the_gstin_state() {
        let f = fixture().await;
        request_store(
            f.pool.clone(),
            "PUT",
            URI,
            update(
                1,
                "registered",
                Some(MAHARASHTRA_GSTIN),
                Some(MAHARASHTRA_STATE),
            ),
        )
        .await;
        let mut body = address();
        body["stateId"] = json!(KARNATAKA_STATE);
        let (status, saved) = request_store(f.pool.clone(), "PUT", ADDRESS, body).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["address"]["stateId"], KARNATAKA_STATE);
        assert_eq!(
            saved["placeOfSupplyStateId"], MAHARASHTRA_STATE,
            "the GST place of supply was overwritten from the address"
        );
    }

    #[tokio::test]
    async fn an_address_is_created_once_and_then_updated_under_its_own_revision() {
        let f = fixture().await;
        let (_, first) = request_store(f.pool.clone(), "PUT", ADDRESS, address()).await;
        assert_eq!(first["address"]["revision"], 1);

        let mut second = address();
        second["expectedRevision"] = json!(1);
        second["line1"] = json!("99 New Road");
        let (status, saved) = request_store(f.pool.clone(), "PUT", ADDRESS, second.clone()).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["address"]["revision"], 2);
        assert_eq!(saved["address"]["line1"], "99 New Road");

        // The stale write is refused rather than silently winning.
        let (status, conflict) = request_store(f.pool.clone(), "PUT", ADDRESS, second).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["currentRevision"], 2);
    }

    /// One store, one operating address: the second one is a conflict, not a branch.
    #[tokio::test]
    async fn a_second_address_cannot_be_created_by_omitting_the_revision() {
        let f = fixture().await;
        request_store(f.pool.clone(), "PUT", ADDRESS, address()).await;
        let (status, refused) = request_store(f.pool.clone(), "PUT", ADDRESS, address()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "revision_conflict");
    }

    #[tokio::test]
    async fn a_licence_number_is_stored_exactly_as_it_was_transcribed() {
        let f = fixture().await;
        let (status, body) = request_store(
            f.pool.clone(),
            "POST",
            LICENCES,
            json!({
                "licenceType": "Form 20B/21B",
                "licenceNumber": "  20B-1234 / 21B-5678  ",
                "issuingAuthority": "FDA Maharashtra",
                "validFrom": "2026-01-01",
                "validUpto": "2031-12-31"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["licences"][0]["licenceNumber"], "20B-1234 / 21B-5678");
        assert_eq!(body["licences"][0]["issuingAuthority"], "FDA Maharashtra");
        assert_eq!(body["licences"][0]["validUpto"], "2031-12-31");
    }

    #[tokio::test]
    async fn a_blank_or_oversized_or_punctuation_only_licence_is_refused() {
        let f = fixture().await;
        for value in ["", "   ", "///", &"9".repeat(101)] {
            let (status, refused) =
                request_store(f.pool.clone(), "POST", LICENCES, licence(value)).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "licence {value:?} was accepted: {refused}"
            );
        }
        let (status, refused) = request_store(
            f.pool.clone(),
            "POST",
            LICENCES,
            json!({ "licenceType": "x".repeat(61), "licenceNumber": "MH-1" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }

    #[tokio::test]
    async fn a_validity_window_that_ends_before_it_starts_is_refused() {
        let f = fixture().await;
        let (status, refused) = request_store(
            f.pool.clone(),
            "POST",
            LICENCES,
            json!({
                "licenceType": "Form 20", "licenceNumber": "MH-1",
                "validFrom": "2027-01-01", "validUpto": "2026-01-01"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["issues"][0]["field"], "validUpto");

        let (status, refused) = request_store(
            f.pool.clone(),
            "POST",
            LICENCES,
            json!({ "licenceType": "Form 20", "licenceNumber": "MH-2", "validFrom": "01-01-2026" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }

    /// The same licence typed twice, however it is spaced or cased, is one licence.
    #[tokio::test]
    async fn the_same_active_licence_cannot_be_recorded_twice() {
        let f = fixture().await;
        request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        for repeat in ["MH-20-1234", "mh 20 1234", "MH20/1234"] {
            let (status, refused) =
                request_store(f.pool.clone(), "POST", LICENCES, licence(repeat)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{repeat}: {refused}");
            assert_eq!(refused["code"], "store_licence_conflict");
        }
    }

    /// Archiving frees the number, because a surrendered licence can be re-issued.
    #[tokio::test]
    async fn an_archived_licence_leaves_the_printed_line_and_frees_its_number() {
        let f = fixture().await;
        let (_, created) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        let id = created["licences"][0]["id"].as_str().unwrap().to_owned();

        let (status, archived) = request_store(
            f.pool.clone(),
            "POST",
            &format!("{LICENCES}/{id}/archive"),
            json!({ "expectedRevision": 1, "reason": "Surrendered on renewal" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["licences"][0]["status"], "archived");
        assert_eq!(
            archived["sellerComplete"], false,
            "an archived licence still counted as an active one"
        );

        // The number is free again.
        let (status, again) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        assert_eq!(status, StatusCode::CREATED, "{again}");
        assert_eq!(
            again["sellerComplete"], false,
            "the name and address are still missing"
        );
    }

    #[tokio::test]
    async fn archiving_requires_a_reason_and_an_archived_licence_cannot_be_edited() {
        let f = fixture().await;
        let (_, created) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        let id = created["licences"][0]["id"].as_str().unwrap().to_owned();

        let (status, refused) = request_store(
            f.pool.clone(),
            "POST",
            &format!("{LICENCES}/{id}/archive"),
            json!({ "expectedRevision": 1 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");

        request_store(
            f.pool.clone(),
            "POST",
            &format!("{LICENCES}/{id}/archive"),
            json!({ "expectedRevision": 1, "reason": "Surrendered" }),
        )
        .await;
        let (status, refused) = request_store(
            f.pool.clone(),
            "PUT",
            &format!("{LICENCES}/{id}"),
            json!({ "expectedRevision": 2, "licenceType": "Form 21", "licenceNumber": "MH-9" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "store_licence_archived");
    }

    #[tokio::test]
    async fn a_restored_licence_counts_again() {
        let f = fixture().await;
        let (_, created) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        let id = created["licences"][0]["id"].as_str().unwrap().to_owned();
        request_store(
            f.pool.clone(),
            "POST",
            &format!("{LICENCES}/{id}/archive"),
            json!({ "expectedRevision": 1, "reason": "Surrendered" }),
        )
        .await;
        let (status, restored) = request_store(
            f.pool.clone(),
            "POST",
            &format!("{LICENCES}/{id}/restore"),
            json!({ "expectedRevision": 2 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert_eq!(restored["licences"][0]["status"], "active");
        assert_eq!(restored["licences"][0]["revision"], 3);
    }

    #[tokio::test]
    async fn a_stale_licence_revision_is_refused() {
        let f = fixture().await;
        let (_, created) =
            request_store(f.pool.clone(), "POST", LICENCES, licence("MH-20-1234")).await;
        let id = created["licences"][0]["id"].as_str().unwrap().to_owned();
        request_store(
            f.pool.clone(),
            "PUT",
            &format!("{LICENCES}/{id}"),
            json!({ "expectedRevision": 1, "licenceType": "Form 21", "licenceNumber": "MH-20-1234" }),
        )
        .await;
        let (status, conflict) = request_store(
            f.pool.clone(),
            "PUT",
            &format!("{LICENCES}/{id}"),
            json!({ "expectedRevision": 1, "licenceType": "Form 21", "licenceNumber": "MH-20-1234" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["currentRevision"], 2);
    }

    #[tokio::test]
    async fn an_unknown_or_malformed_licence_is_refused_safely() {
        let f = fixture().await;
        let (status, _) = request_store(
            f.pool.clone(),
            "PUT",
            &format!("{LICENCES}/{}", Uuid::now_v7()),
            json!({ "expectedRevision": 1, "licenceType": "Form 20", "licenceNumber": "MH-1" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, refused) = request_store(
            f.pool.clone(),
            "PUT",
            &format!("{LICENCES}/not-a-uuid"),
            json!({ "expectedRevision": 1, "licenceType": "Form 20", "licenceNumber": "MH-1" }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    }

    /// Only the owner may change who the pharmacy is. Everyone else may look.
    #[tokio::test]
    async fn a_read_only_role_can_inspect_the_profile_but_never_change_it() {
        let f = fixture().await;
        complete_profile(&f).await;

        let (status, body) = request_store_as(
            f.pool.clone(),
            "GET",
            PROFILE,
            Value::Null,
            Some(CASHIER_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sellerComplete"], true);

        for (method, uri, payload) in [
            ("PUT", PROFILE, identity(2)),
            ("PUT", ADDRESS, address()),
            ("POST", LICENCES, licence("MH-99")),
        ] {
            let (status, refused) =
                request_store_as(f.pool.clone(), method, uri, payload, Some(CASHIER_TOKEN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {refused}");
            assert_eq!(refused["code"], "authorization_denied");
        }
    }

    #[tokio::test]
    async fn an_unauthenticated_caller_can_neither_read_nor_change_the_profile() {
        let f = fixture().await;
        for (method, uri, payload) in [
            ("GET", PROFILE, Value::Null),
            ("PUT", PROFILE, identity(1)),
            ("PUT", ADDRESS, address()),
            ("POST", LICENCES, licence("MH-99")),
        ] {
            let (status, _) = request_store_as(f.pool.clone(), method, uri, payload, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }

    /// The frozen Host/Origin mutation guard still applies to every new write.
    #[tokio::test]
    async fn the_mutation_guard_still_covers_the_new_writes() {
        let f = fixture().await;
        for (method, uri) in [("PUT", PROFILE), ("PUT", ADDRESS), ("POST", LICENCES)] {
            let response = crate::api::router(f.pool.clone(), None)
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("host", "evil.example.com")
                        .header("content-type", "application/json")
                        .header("cookie", format!("aushadharth_session={OWNER_TOKEN}"))
                        .body(Body::from(identity(1).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "{method} {uri} accepted a foreign Host"
            );
        }
    }

    #[tokio::test]
    async fn a_stale_profile_revision_is_refused() {
        let f = fixture().await;
        request_store(f.pool.clone(), "PUT", PROFILE, identity(1)).await;
        let (status, conflict) = request_store(f.pool.clone(), "PUT", PROFILE, identity(1)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["expectedRevision"], 1);
        assert_eq!(conflict["currentRevision"], 2);
    }

    /// Long Unicode text is stored, not mangled. Escaping belongs to whatever renders it.
    #[tokio::test]
    async fn long_unicode_identity_and_licence_text_survives_unchanged() {
        let f = fixture().await;
        let name = "श्री साईं मेडिकल स्टोर्स प्राइवेट लिमिटेड <&>";
        let licence_number = "२०बी-१२३४/MH-XYZ-9999";
        let (status, saved) = request_store(
            f.pool.clone(),
            "PUT",
            PROFILE,
            json!({ "expectedRevision": 1, "displayName": name, "legalName": name }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        assert_eq!(saved["legalName"], name);

        let (status, saved) = request_store(
            f.pool.clone(),
            "POST",
            LICENCES,
            json!({ "licenceType": "औषधि अनुज्ञप्ति", "licenceNumber": licence_number }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{saved}");
        assert_eq!(saved["licences"][0]["licenceNumber"], licence_number);
    }

    /// Every change to who the pharmacy is belongs in business history.
    #[tokio::test]
    async fn profile_address_and_licence_changes_are_audited() {
        let f = fixture().await;
        complete_profile(&f).await;
        for entity_type in ["store_profile", "store_address", "store_licence"] {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events WHERE entity_type=?")
                    .bind(entity_type)
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 1, "{entity_type} was not audited");
        }
        let actor: Option<String> = sqlx::query_scalar(
            "SELECT actor_id FROM master_change_events WHERE entity_type='store_licence'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            actor.as_deref(),
            Some(f.owner_id.as_str()),
            "the audit actor did not come from the session"
        );
    }
}
