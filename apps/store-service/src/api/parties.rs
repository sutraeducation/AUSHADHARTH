//! Phase 1E party identity, the supplier role, and postal addresses.
//!
//! A Party is an identity, never an account. Nothing here reads or writes a balance, an
//! outstanding amount, or a credit limit, because no such column exists and none may be added: a
//! future accounting ledger will reference `party_id` and derive every figure from its own
//! postings, exactly as the inventory ledger is the sole authority for quantity.

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{
        CatalogValidationIssue, normalized_search_name, optional_text, required_text,
        validate_date, validate_uuid_v7,
    },
    parties::{
        ADDRESS_ROLES, GST_REGISTRATION_STATUSES, SUPPORTED_PARTY_ROLES, gstin_pan,
        normalize_email, normalize_gstin, normalize_licence_comparison, normalize_pan,
        normalize_phone,
    },
};

#[derive(Debug)]
enum PartyError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    Duplicate,
    Revision { expected: i64, current: i64 },
    NotFound,
    Archived,
    Party,
    Role,
    Address,
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

impl IntoResponse for PartyError {
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
                simple(
                    "duplicate_conflict",
                    "A conflicting active record already exists.",
                ),
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
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("not_found", "The requested record was not found."),
            ),
            Self::Archived => (
                StatusCode::CONFLICT,
                simple(
                    "archived_conflict",
                    "The operation conflicts with the current lifecycle state.",
                ),
            ),
            Self::Party => (
                StatusCode::CONFLICT,
                simple(
                    "party_conflict",
                    "The tax registration does not agree with the selected State.",
                ),
            ),
            Self::Role => (
                StatusCode::CONFLICT,
                simple(
                    "party_role_conflict",
                    "The role conflicts with the party's current state.",
                ),
            ),
            Self::Address => (
                StatusCode::CONFLICT,
                simple(
                    "party_address_conflict",
                    "The address conflicts with the party's or the State's current state.",
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

impl From<AuthError> for PartyError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

fn validation(field: &str, message: &str) -> PartyError {
    PartyError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> PartyError {
    PartyError::Validation(vec![issue])
}

/// Typed trigger codes and contention are translated here; no raw database text reaches a client.
fn map_database_error(error: sqlx::Error) -> PartyError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return PartyError::ServiceBusy;
        }
        if message.contains("party_role_conflict") {
            return PartyError::Role;
        }
        if message.contains("party_address_conflict") {
            return PartyError::Address;
        }
        if message.contains("party_conflict") {
            return PartyError::Party;
        }
        if message.contains("unique constraint failed") {
            return PartyError::Duplicate;
        }
        if message.contains("foreign key constraint failed")
            || message.contains("check constraint failed")
        {
            return PartyError::Validation(vec![CatalogValidationIssue {
                field: "party".to_owned(),
                message: "violates a party integrity constraint".to_owned(),
            }]);
        }
    }
    PartyError::Internal
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

fn unknown_registration() -> String {
    "unknown".to_owned()
}

fn default_country() -> String {
    "IN".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartyFields {
    display_name: String,
    legal_name: Option<String>,
    #[serde(default = "unknown_registration")]
    gst_registration_status: String,
    gstin: Option<String>,
    pan: Option<String>,
    place_of_supply_state_id: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    drug_licence_number: Option<String>,
    drug_licence_valid_upto: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartyRoleFields {
    role: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartyAddressFields {
    address_role: String,
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    state_id: Option<String>,
    postal_code: Option<String>,
    #[serde(default = "default_country")]
    country_code: String,
    #[serde(default)]
    is_primary: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePartyRequest {
    party: PartyFields,
    #[serde(default)]
    roles: Vec<PartyRoleFields>,
    #[serde(default)]
    addresses: Vec<PartyAddressFields>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePartyRequest {
    expected_revision: i64,
    party: PartyFields,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRoleRequest {
    expected_revision: i64,
    role: PartyRoleFields,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAddressRequest {
    expected_revision: i64,
    address: PartyAddressFields,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleRequest {
    expected_revision: i64,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    search: Option<String>,
    status: Option<String>,
    role: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PartyResponse {
    id: String,
    revision: i64,
    status: String,
    display_name: String,
    legal_name: Option<String>,
    normalized_search_name: String,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    pan: Option<String>,
    normalized_pan: Option<String>,
    place_of_supply_state_id: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    drug_licence_number: Option<String>,
    drug_licence_valid_upto: Option<String>,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct RoleResponse {
    id: String,
    party_id: String,
    role: String,
    revision: i64,
    status: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct AddressResponse {
    id: String,
    party_id: String,
    address_role: String,
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    state_id: Option<String>,
    postal_code: Option<String>,
    country_code: String,
    is_primary: bool,
    revision: i64,
    status: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PartyDetailResponse {
    #[serde(flatten)]
    party: PartyResponse,
    roles: Vec<RoleResponse>,
    addresses: Vec<AddressResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateCandidate {
    candidate_id: String,
    score: i64,
    reason_codes: Vec<&'static str>,
    explanation: String,
}

const PARTY_COLUMNS: &str = "id,revision,status,display_name,legal_name,normalized_search_name,\
     gst_registration_status,gstin,normalized_gstin,pan,normalized_pan,place_of_supply_state_id,\
     primary_phone,primary_email,drug_licence_number,drug_licence_valid_upto,\
     created_at_utc,updated_at_utc,archived_at_utc,archive_reason";

const ROLE_COLUMNS: &str =
    "id,party_id,role,revision,status,created_at_utc,updated_at_utc,archived_at_utc,archive_reason";

const ADDRESS_COLUMNS: &str = "id,party_id,address_role,line1,line2,city,state_id,postal_code,\
     country_code,is_primary,revision,status,created_at_utc,updated_at_utc,archived_at_utc,archive_reason";

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/parties", get(list_parties).post(create_party))
        .route(
            "/api/v1/parties/duplicate-candidates",
            post(duplicate_candidates),
        )
        .route("/api/v1/parties/{id}", get(get_party).put(update_party))
        .route("/api/v1/parties/{id}/archive", post(archive_party))
        .route("/api/v1/parties/{id}/restore", post(restore_party))
        .route("/api/v1/parties/{id}/roles", post(add_role))
        .route("/api/v1/parties/{id}/addresses", post(add_address))
        .route("/api/v1/party-roles/{id}", get(get_role).put(update_role))
        .route("/api/v1/party-roles/{id}/archive", post(archive_role))
        .route("/api/v1/party-roles/{id}/restore", post(restore_role))
        .route(
            "/api/v1/party-addresses/{id}",
            get(get_address).put(update_address),
        )
        .route(
            "/api/v1/party-addresses/{id}/archive",
            post(archive_address),
        )
        .route(
            "/api/v1/party-addresses/{id}/restore",
            post(restore_address),
        )
}

async fn require_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PartyError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// Party administration is an Owner/Admin mutation with the frozen Host/Origin protection.
async fn require_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, PartyError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PreparedParty {
    display_name: String,
    legal_name: Option<String>,
    normalized_search_name: String,
    gst_registration_status: String,
    gstin: Option<String>,
    normalized_gstin: Option<String>,
    pan: Option<String>,
    normalized_pan: Option<String>,
    place_of_supply_state_id: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    drug_licence_number: Option<String>,
    drug_licence_valid_upto: Option<String>,
}

fn blank_to_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn prepare_party(fields: PartyFields) -> Result<PreparedParty, PartyError> {
    let display_name =
        required_text(&fields.display_name, "displayName", 200).map_err(validation_issue)?;
    let legal_name =
        optional_text(fields.legal_name.as_deref(), "legalName", 250).map_err(validation_issue)?;
    let registration = fields.gst_registration_status.trim().to_ascii_lowercase();
    if !GST_REGISTRATION_STATUSES.contains(&registration.as_str()) {
        return Err(validation(
            "gstRegistrationStatus",
            "must be registered, unregistered, or unknown",
        ));
    }

    let raw_gstin = blank_to_none(fields.gstin.as_deref());
    let (gstin, normalized_gstin) = match (registration.as_str(), raw_gstin) {
        ("registered", Some(value)) => {
            let (display, normalized) = normalize_gstin(value).map_err(validation_issue)?;
            (Some(display), Some(normalized))
        }
        ("registered", None) => {
            return Err(validation(
                "gstin",
                "is required when the party is recorded as registered",
            ));
        }
        // 'unregistered' asserts there is no GSTIN and 'unknown' says none has been captured;
        // storing one under either would make the status a lie.
        (_, Some(_)) => {
            return Err(validation(
                "gstin",
                "is recorded only when the registration status is registered",
            ));
        }
        (_, None) => (None, None),
    };

    let (pan, normalized_pan) = match blank_to_none(fields.pan.as_deref()) {
        Some(value) => {
            let (display, normalized) = normalize_pan(value).map_err(validation_issue)?;
            (Some(display), Some(normalized))
        }
        None => (None, None),
    };
    // Characters 3-12 of a GSTIN are the holder's PAN, so a disagreement means one was mistyped.
    if let (Some(gstin), Some(pan)) = (normalized_gstin.as_deref(), normalized_pan.as_deref())
        && gstin_pan(gstin) != pan
    {
        return Err(validation(
            "pan",
            "does not match the PAN embedded in the GSTIN",
        ));
    }

    let place_of_supply_state_id = match blank_to_none(fields.place_of_supply_state_id.as_deref()) {
        Some(value) => Some(
            validate_uuid_v7(value, "placeOfSupplyStateId").map_err(validation_issue)?,
        ),
        None => None,
    };
    if normalized_gstin.is_some() && place_of_supply_state_id.is_none() {
        return Err(validation(
            "placeOfSupplyStateId",
            "is required when a GSTIN is recorded, because the GSTIN encodes it",
        ));
    }

    let primary_phone = match blank_to_none(fields.primary_phone.as_deref()) {
        Some(value) => Some(normalize_phone(value).map_err(validation_issue)?),
        None => None,
    };
    let primary_email = match blank_to_none(fields.primary_email.as_deref()) {
        Some(value) => Some(normalize_email(value).map_err(validation_issue)?),
        None => None,
    };
    let drug_licence_number = optional_text(
        fields.drug_licence_number.as_deref(),
        "drugLicenceNumber",
        100,
    )
    .map_err(validation_issue)?;
    let drug_licence_valid_upto = validate_date(
        blank_to_none(fields.drug_licence_valid_upto.as_deref()),
        "drugLicenceValidUpto",
    )
    .map_err(validation_issue)?;

    Ok(PreparedParty {
        normalized_search_name: normalized_search_name(&display_name),
        display_name,
        legal_name,
        gst_registration_status: registration,
        gstin,
        normalized_gstin,
        pan,
        normalized_pan,
        place_of_supply_state_id,
        primary_phone,
        primary_email,
        drug_licence_number,
        drug_licence_valid_upto,
    })
}

fn prepare_role(fields: &PartyRoleFields) -> Result<String, PartyError> {
    let role = fields.role.trim().to_ascii_lowercase();
    if !SUPPORTED_PARTY_ROLES.contains(&role.as_str()) {
        // The schema names 'customer' as the other half of the closed set, but the Customer
        // workflow — sales, receivables, loyalty — is a later phase, so it is refused here.
        return Err(validation(
            "role",
            "must be supplier; the customer role arrives with the sales phase",
        ));
    }
    Ok(role)
}

#[derive(Debug, Clone)]
struct PreparedAddress {
    address_role: String,
    line1: String,
    line2: Option<String>,
    city: Option<String>,
    state_id: Option<String>,
    postal_code: Option<String>,
    country_code: String,
    is_primary: bool,
}

fn prepare_address(fields: PartyAddressFields) -> Result<PreparedAddress, PartyError> {
    let address_role = fields.address_role.trim().to_ascii_lowercase();
    if !ADDRESS_ROLES.contains(&address_role.as_str()) {
        return Err(validation("addressRole", "must be billing or shipping"));
    }
    let country_code = fields.country_code.trim().to_ascii_uppercase();
    if country_code.len() != 2 || !country_code.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(validation(
            "countryCode",
            "must be a two-letter ASCII country code",
        ));
    }
    let postal_code = match blank_to_none(fields.postal_code.as_deref()) {
        Some(value) => {
            let normalized = value.to_ascii_uppercase();
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
    Ok(PreparedAddress {
        address_role,
        line1: required_text(&fields.line1, "line1", 200).map_err(validation_issue)?,
        line2: optional_text(fields.line2.as_deref(), "line2", 200).map_err(validation_issue)?,
        city: optional_text(fields.city.as_deref(), "city", 100).map_err(validation_issue)?,
        state_id: match blank_to_none(fields.state_id.as_deref()) {
            Some(value) => Some(validate_uuid_v7(value, "stateId").map_err(validation_issue)?),
            None => None,
        },
        postal_code,
        country_code,
        is_primary: fields.is_primary,
    })
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

async fn list_parties(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<PartyResponse>>, PartyError> {
    require_reader(&state, &headers).await?;
    let status = query.status.unwrap_or_else(|| "active".to_owned());
    if !matches!(status.as_str(), "active" | "archived" | "all") {
        return Err(validation("status", "must be active, archived, or all"));
    }
    let status_filter = (status != "all").then_some(status);
    let search = query
        .search
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    let role_filter = match blank_to_none(query.role.as_deref()) {
        Some(value) => Some(prepare_role(&PartyRoleFields {
            role: value.to_owned(),
        })?),
        None => None,
    };
    let rows = sqlx::query_as::<_, PartyResponse>(&format!(
        "SELECT {PARTY_COLUMNS} FROM parties \
         WHERE (?1 IS NULL OR status=?1) \
           AND (?2 IS NULL OR lower(normalized_search_name || ' ' || display_name \
                || ' ' || COALESCE(legal_name,'') || ' ' || COALESCE(normalized_gstin,'') \
                || ' ' || COALESCE(normalized_pan,'')) LIKE '%' || ?2 || '%') \
           AND (?3 IS NULL OR EXISTS ( \
                SELECT 1 FROM party_roles WHERE party_id=parties.id AND role=?3 AND status='active')) \
         ORDER BY normalized_search_name, id LIMIT 500"
    ))
    .bind(&status_filter)
    .bind(&search)
    .bind(&role_filter)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_party(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<PartyDetailResponse>, PartyError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn create_party(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreatePartyRequest>,
) -> Result<(StatusCode, Json<PartyDetailResponse>), PartyError> {
    let actor = require_admin(&state, &headers).await?;
    let party = prepare_party(request.party)?;
    let mut roles = Vec::new();
    let mut seen = HashSet::new();
    for fields in &request.roles {
        let role = prepare_role(fields)?;
        if !seen.insert(role.clone()) {
            return Err(validation("roles", "lists the same role twice"));
        }
        roles.push(role);
    }
    let addresses = request
        .addresses
        .into_iter()
        .map(prepare_address)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_primary(&addresses)?;

    let id = Uuid::now_v7().to_string();
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let now = database_now(&mut transaction).await?;
    insert_party(&mut transaction, &id, &party, &now).await?;
    audit(
        &mut transaction,
        "party",
        &id,
        1,
        "created",
        request.reason.as_deref(),
        &party_payload(&party),
        &actor.id,
    )
    .await?;
    for role in &roles {
        let role_id = Uuid::now_v7().to_string();
        insert_role(&mut transaction, &role_id, &id, role, &now).await?;
        audit(
            &mut transaction,
            "party_role",
            &role_id,
            1,
            "created",
            request.reason.as_deref(),
            &json!({ "partyId": id, "role": role }),
            &actor.id,
        )
        .await?;
    }
    for address in &addresses {
        let address_id = Uuid::now_v7().to_string();
        insert_address(&mut transaction, &address_id, &id, address, &now).await?;
        audit(
            &mut transaction,
            "party_address",
            &address_id,
            1,
            "created",
            request.reason.as_deref(),
            &address_payload(&id, address),
            &actor.id,
        )
        .await?;
    }
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_detail(&state.pool, &id).await?),
    ))
}

fn reject_duplicate_primary(addresses: &[PreparedAddress]) -> Result<(), PartyError> {
    let mut primaries = HashSet::new();
    for address in addresses.iter().filter(|address| address.is_primary) {
        if !primaries.insert(address.address_role.clone()) {
            return Err(validation(
                "addresses",
                "names more than one primary address for the same purpose",
            ));
        }
    }
    Ok(())
}

async fn update_party(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdatePartyRequest>,
) -> Result<Json<PartyDetailResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let party = prepare_party(request.party)?;
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let current = current_state(&mut transaction, "parties", &id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != "active" {
        return Err(PartyError::Archived);
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE parties SET display_name=?,legal_name=?,normalized_search_name=?,\
         gst_registration_status=?,gstin=?,normalized_gstin=?,pan=?,normalized_pan=?,\
         place_of_supply_state_id=?,primary_phone=?,primary_email=?,drug_licence_number=?,\
         drug_licence_valid_upto=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(&party.display_name)
    .bind(&party.legal_name)
    .bind(&party.normalized_search_name)
    .bind(&party.gst_registration_status)
    .bind(&party.gstin)
    .bind(&party.normalized_gstin)
    .bind(&party.pan)
    .bind(&party.normalized_pan)
    .bind(&party.place_of_supply_state_id)
    .bind(&party.primary_phone)
    .bind(&party.primary_email)
    .bind(&party.drug_licence_number)
    .bind(&party.drug_licence_valid_upto)
    .bind(next)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        "party",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &party_payload(&party),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(&state.pool, &id).await?))
}

async fn archive_party(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PartyDetailResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    party_lifecycle(&state.pool, &id, request, false, &actor.id).await
}

async fn restore_party(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PartyDetailResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    party_lifecycle(&state.pool, &id, request, true, &actor.id).await
}

async fn party_lifecycle(
    pool: &SqlitePool,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
    actor_id: &str,
) -> Result<Json<PartyDetailResponse>, PartyError> {
    validate_uuid_v7(id, "id").map_err(validation_issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(validation_issue)?;
    let mut transaction = pool.begin().await.map_err(|_| PartyError::Internal)?;
    let current = current_state(&mut transaction, "parties", id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != if restoring { "archived" } else { "active" } {
        return Err(PartyError::Archived);
    }
    if !restoring {
        // Following lifecycle_product: archiving is refused while active children exist rather than
        // cascading, so nothing the operator never saw is archived and restore stays unambiguous.
        let children: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM party_roles WHERE party_id=? AND status='active') + \
             (SELECT COUNT(*) FROM party_addresses WHERE party_id=? AND status='active')",
        )
        .bind(id)
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
        if children > 0 {
            return Err(PartyError::Archived);
        }
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    lifecycle_update(&mut transaction, "parties", id, current.0, next, &now, &reason, restoring)
        .await?;
    audit(
        &mut transaction,
        "party",
        id,
        next,
        if restoring { "restored" } else { "archived" },
        Some(&reason),
        &json!({}),
        actor_id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_detail(pool, id).await?))
}

async fn add_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(party_id): Path<String>,
    Json(request): Json<PartyRoleFields>,
) -> Result<(StatusCode, Json<RoleResponse>), PartyError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&party_id, "id").map_err(validation_issue)?;
    let role = prepare_role(&request)?;
    let id = Uuid::now_v7().to_string();
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let now = database_now(&mut transaction).await?;
    insert_role(&mut transaction, &id, &party_id, &role, &now).await?;
    audit(
        &mut transaction,
        "party_role",
        &id,
        1,
        "created",
        None,
        &json!({ "partyId": party_id, "role": role }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_role(&state.pool, &id).await?),
    ))
}

async fn get_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<RoleResponse>, PartyError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_role(&state.pool, &id).await?))
}

async fn update_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateRoleRequest>,
) -> Result<Json<RoleResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let role = prepare_role(&request.role)?;
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let current = current_state(&mut transaction, "party_roles", &id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != "active" {
        return Err(PartyError::Archived);
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE party_roles SET role=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(&role)
    .bind(next)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    audit(
        &mut transaction,
        "party_role",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &json!({ "role": role }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_role(&state.pool, &id).await?))
}

async fn archive_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<RoleResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    child_lifecycle(&state, "party_roles", "party_role", &id, request, false, &actor.id).await?;
    Ok(Json(fetch_role(&state.pool, &id).await?))
}

async fn restore_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<RoleResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    child_lifecycle(&state, "party_roles", "party_role", &id, request, true, &actor.id).await?;
    Ok(Json(fetch_role(&state.pool, &id).await?))
}

async fn add_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(party_id): Path<String>,
    Json(request): Json<PartyAddressFields>,
) -> Result<(StatusCode, Json<AddressResponse>), PartyError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&party_id, "id").map_err(validation_issue)?;
    let address = prepare_address(request)?;
    let id = Uuid::now_v7().to_string();
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let now = database_now(&mut transaction).await?;
    insert_address(&mut transaction, &id, &party_id, &address, &now).await?;
    audit(
        &mut transaction,
        "party_address",
        &id,
        1,
        "created",
        None,
        &address_payload(&party_id, &address),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_address(&state.pool, &id).await?),
    ))
}

async fn get_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<AddressResponse>, PartyError> {
    require_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_address(&state.pool, &id).await?))
}

async fn update_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateAddressRequest>,
) -> Result<Json<AddressResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let address = prepare_address(request.address)?;
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let current = current_state(&mut transaction, "party_addresses", &id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != "active" {
        return Err(PartyError::Archived);
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE party_addresses SET address_role=?,line1=?,line2=?,city=?,state_id=?,postal_code=?,\
         country_code=?,is_primary=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(&address.address_role)
    .bind(&address.line1)
    .bind(&address.line2)
    .bind(&address.city)
    .bind(&address.state_id)
    .bind(&address.postal_code)
    .bind(&address.country_code)
    .bind(i64::from(address.is_primary))
    .bind(next)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let party_id: String = sqlx::query_scalar("SELECT party_id FROM party_addresses WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(map_database_error)?;
    audit(
        &mut transaction,
        "party_address",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &address_payload(&party_id, &address),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_address(&state.pool, &id).await?))
}

async fn archive_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<AddressResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    child_lifecycle(
        &state,
        "party_addresses",
        "party_address",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_address(&state.pool, &id).await?))
}

async fn restore_address(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<AddressResponse>, PartyError> {
    let actor = require_admin(&state, &headers).await?;
    child_lifecycle(
        &state,
        "party_addresses",
        "party_address",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_address(&state.pool, &id).await?))
}

async fn child_lifecycle(
    state: &ReferenceState,
    table: &str,
    entity_type: &str,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
    actor_id: &str,
) -> Result<(), PartyError> {
    validate_uuid_v7(id, "id").map_err(validation_issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(validation_issue)?;
    let mut transaction = state.pool.begin().await.map_err(|_| PartyError::Internal)?;
    let current = current_state(&mut transaction, table, id).await?;
    require_revision(&current, request.expected_revision)?;
    if current.1 != if restoring { "archived" } else { "active" } {
        return Err(PartyError::Archived);
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    lifecycle_update(&mut transaction, table, id, current.0, next, &now, &reason, restoring).await?;
    audit(
        &mut transaction,
        entity_type,
        id,
        next,
        if restoring { "restored" } else { "archived" },
        Some(&reason),
        &json!({}),
        actor_id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(())
}

/// Advisory only. Creation is never blocked: two pharmacies really can be called "Sharma Medicals".
async fn duplicate_candidates(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreatePartyRequest>,
) -> Result<Json<Vec<DuplicateCandidate>>, PartyError> {
    auth::validate_mutation_request(&headers)?;
    require_reader(&state, &headers).await?;
    let party = prepare_party(request.party)?;
    let legal = party.legal_name.as_deref().map(normalized_search_name);
    let licence = party
        .drug_licence_number
        .as_deref()
        .map(normalize_licence_comparison)
        .filter(|value| !value.is_empty());

    let rows = sqlx::query_as::<_, CandidateRow>(
        "SELECT id,display_name,legal_name,normalized_gstin,normalized_pan,primary_phone,\
         primary_email,drug_licence_number FROM parties WHERE status='active' ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;

    let mut results = Vec::new();
    for candidate in rows {
        let mut score = 0_i64;
        let mut reasons: Vec<&'static str> = Vec::new();
        // A GSTIN identifies exactly one registration, so a match is decisive rather than advisory.
        if party.normalized_gstin.is_some() && party.normalized_gstin == candidate.normalized_gstin {
            score += 100;
            reasons.push("gstin_match");
        }
        // A shared PAN is legitimate for one entity registered in several states, so it scores
        // high without being conclusive.
        if party.normalized_pan.is_some() && party.normalized_pan == candidate.normalized_pan {
            score += 60;
            reasons.push("pan_match");
        }
        if licence.is_some()
            && candidate
                .drug_licence_number
                .as_deref()
                .map(normalize_licence_comparison)
                .filter(|value| !value.is_empty())
                == licence
        {
            score += 40;
            reasons.push("drug_licence_match");
        }
        let candidate_names = [
            Some(normalized_search_name(&candidate.display_name)),
            candidate.legal_name.as_deref().map(normalized_search_name),
        ];
        if candidate_names
            .iter()
            .flatten()
            .any(|name| *name == party.normalized_search_name || Some(name) == legal.as_ref())
        {
            score += 25;
            reasons.push("name_match");
        }
        if party.primary_phone.is_some() && party.primary_phone == candidate.primary_phone {
            score += 20;
            reasons.push("phone_match");
        }
        if party.primary_email.is_some() && party.primary_email == candidate.primary_email {
            score += 20;
            reasons.push("email_match");
        }
        if score > 0 {
            results.push(DuplicateCandidate {
                candidate_id: candidate.id,
                score,
                explanation: reasons.join(", "),
                reason_codes: reasons,
            });
        }
    }
    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.candidate_id.cmp(&right.candidate_id))
    });
    Ok(Json(results))
}

#[derive(Debug, FromRow)]
struct CandidateRow {
    id: String,
    display_name: String,
    legal_name: Option<String>,
    normalized_gstin: Option<String>,
    normalized_pan: Option<String>,
    primary_phone: Option<String>,
    primary_email: Option<String>,
    drug_licence_number: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------------------------

fn party_payload(party: &PreparedParty) -> Value {
    json!({
        "displayName": party.display_name,
        "legalName": party.legal_name,
        "gstRegistrationStatus": party.gst_registration_status,
        "normalizedGstin": party.normalized_gstin,
        "normalizedPan": party.normalized_pan,
        "placeOfSupplyStateId": party.place_of_supply_state_id,
        "primaryPhone": party.primary_phone,
        "primaryEmail": party.primary_email,
        "drugLicenceNumber": party.drug_licence_number,
        "drugLicenceValidUpto": party.drug_licence_valid_upto,
    })
}

fn address_payload(party_id: &str, address: &PreparedAddress) -> Value {
    json!({
        "partyId": party_id,
        "addressRole": address.address_role,
        "line1": address.line1,
        "line2": address.line2,
        "city": address.city,
        "stateId": address.state_id,
        "postalCode": address.postal_code,
        "countryCode": address.country_code,
        "isPrimary": address.is_primary,
    })
}

async fn insert_party(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    party: &PreparedParty,
    now: &str,
) -> Result<(), PartyError> {
    sqlx::query(
        "INSERT INTO parties (id,display_name,legal_name,normalized_search_name,\
         gst_registration_status,gstin,normalized_gstin,pan,normalized_pan,place_of_supply_state_id,\
         primary_phone,primary_email,drug_licence_number,drug_licence_valid_upto,\
         created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(&party.display_name)
    .bind(&party.legal_name)
    .bind(&party.normalized_search_name)
    .bind(&party.gst_registration_status)
    .bind(&party.gstin)
    .bind(&party.normalized_gstin)
    .bind(&party.pan)
    .bind(&party.normalized_pan)
    .bind(&party.place_of_supply_state_id)
    .bind(&party.primary_phone)
    .bind(&party.primary_email)
    .bind(&party.drug_licence_number)
    .bind(&party.drug_licence_valid_upto)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn insert_role(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    party_id: &str,
    role: &str,
    now: &str,
) -> Result<(), PartyError> {
    sqlx::query(
        "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?)",
    )
    .bind(id)
    .bind(party_id)
    .bind(role)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn insert_address(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    party_id: &str,
    address: &PreparedAddress,
    now: &str,
) -> Result<(), PartyError> {
    sqlx::query(
        "INSERT INTO party_addresses (id,party_id,address_role,line1,line2,city,state_id,\
         postal_code,country_code,is_primary,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(party_id)
    .bind(&address.address_role)
    .bind(&address.line1)
    .bind(&address.line2)
    .bind(&address.city)
    .bind(&address.state_id)
    .bind(&address.postal_code)
    .bind(&address.country_code)
    .bind(i64::from(address.is_primary))
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn lifecycle_update(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
    current: i64,
    next: i64,
    now: &str,
    reason: &str,
    restoring: bool,
) -> Result<(), PartyError> {
    let result = if restoring {
        sqlx::query(&format!(
            "UPDATE {table} SET revision=?,status='active',updated_at_utc=?,archived_at_utc=NULL,\
             archive_reason=NULL WHERE id=? AND revision=? AND status='archived'"
        ))
        .bind(next)
        .bind(now)
        .bind(id)
        .bind(current)
        .execute(&mut **transaction)
        .await
    } else {
        sqlx::query(&format!(
            "UPDATE {table} SET revision=?,status='archived',updated_at_utc=?,archived_at_utc=?,\
             archive_reason=? WHERE id=? AND revision=? AND status='active'"
        ))
        .bind(next)
        .bind(now)
        .bind(now)
        .bind(reason)
        .bind(id)
        .bind(current)
        .execute(&mut **transaction)
        .await
    };
    if result.map_err(map_database_error)?.rows_affected() != 1 {
        return Err(PartyError::Internal);
    }
    Ok(())
}

async fn current_state(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
) -> Result<(i64, String), PartyError> {
    sqlx::query_as(&format!("SELECT revision,status FROM {table} WHERE id=?"))
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(map_database_error)?
        .ok_or(PartyError::NotFound)
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), PartyError> {
    if current.0 != expected {
        return Err(PartyError::Revision {
            expected,
            current: current.0,
        });
    }
    Ok(())
}

async fn database_now(transaction: &mut Transaction<'_, Sqlite>) -> Result<String, PartyError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_database_error)
}

#[allow(clippy::too_many_arguments)]
async fn audit(
    transaction: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    reason: Option<&str>,
    payload: &Value,
    actor_id: &str,
) -> Result<(), PartyError> {
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
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn fetch_detail(pool: &SqlitePool, id: &str) -> Result<PartyDetailResponse, PartyError> {
    let party = sqlx::query_as::<_, PartyResponse>(&format!(
        "SELECT {PARTY_COLUMNS} FROM parties WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PartyError::NotFound)?;
    let roles = sqlx::query_as::<_, RoleResponse>(&format!(
        "SELECT {ROLE_COLUMNS} FROM party_roles WHERE party_id=? ORDER BY role,id"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    let addresses = sqlx::query_as::<_, AddressResponse>(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM party_addresses WHERE party_id=? \
         ORDER BY address_role,is_primary DESC,id"
    ))
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(map_database_error)?;
    Ok(PartyDetailResponse {
        party,
        roles,
        addresses,
    })
}

async fn fetch_role(pool: &SqlitePool, id: &str) -> Result<RoleResponse, PartyError> {
    sqlx::query_as::<_, RoleResponse>(&format!(
        "SELECT {ROLE_COLUMNS} FROM party_roles WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PartyError::NotFound)
}

async fn fetch_address(pool: &SqlitePool, id: &str) -> Result<AddressResponse, PartyError> {
    sqlx::query_as::<_, AddressResponse>(&format!(
        "SELECT {ADDRESS_COLUMNS} FROM party_addresses WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(PartyError::NotFound)
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    /// Genuine published GSTINs, so the check digit is exercised rather than merely satisfied.
    const MAHARASHTRA_GSTIN: &str = "27AAPFU0939F1ZV";
    const KARNATAKA_GSTIN: &str = "29AAGCB7383J1Z4";
    const MAHARASHTRA_STATE: &str = "01997300-0000-7000-8000-000000000027";
    const KARNATAKA_STATE: &str = "01997300-0000-7000-8000-000000000029";
    const OWNER_TOKEN: &str = "party-owner-session-token";
    const CASHIER_TOKEN: &str = "party-cashier-session-token";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        owner_id: String,
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("parties.sqlite3"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Test Store','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .execute(&pool)
        .await
        .unwrap();
        let owner_id = insert_session(&pool, "owner_admin", OWNER_TOKEN).await;
        insert_session(&pool, "cashier", CASHIER_TOKEN).await;
        Fixture {
            _temp: temp,
            pool,
            owner_id,
        }
    }

    /// A minimal registered supplier in Maharashtra.
    fn supplier() -> Value {
        json!({
            "party": {
                "displayName": "Sharma Medicals",
                "legalName": "Sharma Medical And General Stores Private Limited",
                "gstRegistrationStatus": "registered",
                "gstin": MAHARASHTRA_GSTIN,
                "placeOfSupplyStateId": MAHARASHTRA_STATE
            },
            "roles": [{ "role": "supplier" }]
        })
    }

    async fn create(pool: &SqlitePool, body: Value) -> (StatusCode, Value) {
        request_json(pool.clone(), "POST", "/api/v1/parties", body).await
    }

    #[tokio::test]
    async fn a_registered_supplier_is_created_with_its_role_and_address() {
        let f = fixture().await;
        let mut body = supplier();
        body["addresses"] = json!([{
            "addressRole": "billing",
            "line1": "12 Market Road",
            "city": "Pune",
            "stateId": MAHARASHTRA_STATE,
            "postalCode": "411001",
            "isPrimary": true
        }]);
        let (status, created) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["displayName"], "Sharma Medicals");
        assert_eq!(created["normalizedGstin"], MAHARASHTRA_GSTIN);
        // The PAN is supplied by the caller, never invented by the service from the GSTIN.
        assert_eq!(created["pan"], Value::Null);
        assert_eq!(created["revision"], 1);
        assert_eq!(created["status"], "active");
        assert_eq!(created["roles"].as_array().unwrap().len(), 1);
        assert_eq!(created["roles"][0]["role"], "supplier");
        assert_eq!(created["addresses"][0]["postalCode"], "411001");
        assert_eq!(created["addresses"][0]["countryCode"], "IN");
        assert_eq!(created["addresses"][0]["isPrimary"], true);

        // A Party is an identity, never an account: no amount column exists to read.
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('parties')")
                .fetch_all(&f.pool)
                .await
                .unwrap();
        for column in &columns {
            for forbidden in ["balance", "outstanding", "credit", "amount", "paise"] {
                assert!(
                    !column.contains(forbidden),
                    "parties must carry no accounting column, found {column}"
                );
            }
        }

        // Every write appended an audit row carrying the session actor.
        let events: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT entity_type,action,COALESCE(actor_id,'') FROM master_change_events \
             WHERE entity_type LIKE 'party%' ORDER BY entity_type",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 3, "party, role, and address are each audited");
        for (_, action, actor) in &events {
            assert_eq!(action, "created");
            assert_eq!(actor, &f.owner_id, "the actor is the server session user");
        }
    }

    #[tokio::test]
    async fn a_gstin_that_fails_its_check_digit_is_refused() {
        let f = fixture().await;
        let mut body = supplier();
        // Well-shaped and wrong: only the checksum can catch this.
        body["party"]["gstin"] = json!("27AAPFU0939F1ZX");
        let (status, error) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["code"], "validation_failed");
        assert_eq!(error["issues"][0]["field"], "gstin");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM parties")
                .fetch_one(&f.pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_gstin_must_agree_with_the_place_of_supply_state() {
        let f = fixture().await;
        let mut body = supplier();
        // A Maharashtra GSTIN cannot belong to a Karnataka registration.
        body["party"]["placeOfSupplyStateId"] = json!(KARNATAKA_STATE);
        let (status, error) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        assert_eq!(error["code"], "party_conflict");

        // The database refuses it independently, so the guarantee does not rest on the service.
        let direct = sqlx::query(
            "INSERT INTO parties (id,display_name,normalized_search_name,gst_registration_status,\
             gstin,normalized_gstin,place_of_supply_state_id,created_at_utc,updated_at_utc) \
             VALUES (?,'Direct','direct','registered',?,?,?,\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(MAHARASHTRA_GSTIN)
        .bind(MAHARASHTRA_GSTIN)
        .bind(KARNATAKA_STATE)
        .execute(&f.pool)
        .await;
        assert!(direct.is_err(), "the trigger must reject a state mismatch");
    }

    #[tokio::test]
    async fn a_typed_pan_must_match_the_one_inside_the_gstin() {
        let f = fixture().await;
        let mut body = supplier();
        body["party"]["pan"] = json!("AAPFU0939F");
        let (status, created) = create(&f.pool, body.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["normalizedPan"], "AAPFU0939F");

        body["party"]["pan"] = json!("ZZZZZ1111Z");
        body["party"]["gstin"] = json!(KARNATAKA_GSTIN);
        body["party"]["placeOfSupplyStateId"] = json!(KARNATAKA_STATE);
        let (status, error) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["issues"][0]["field"], "pan");
    }

    #[tokio::test]
    async fn registration_status_and_gstin_presence_must_agree() {
        let f = fixture().await;
        // Claiming registration without a number.
        let mut missing = supplier();
        missing["party"]["gstin"] = Value::Null;
        let (status, error) = create(&f.pool, missing).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["issues"][0]["field"], "gstin");

        // Carrying a number while asserting there is none.
        let mut contradictory = supplier();
        contradictory["party"]["gstRegistrationStatus"] = json!("unregistered");
        let (status, error) = create(&f.pool, contradictory).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["issues"][0]["field"], "gstin");

        // An unregistered supplier is legitimate and needs no State.
        let (status, created) = create(
            &f.pool,
            json!({
                "party": {
                    "displayName": "Local Cash Vendor",
                    "gstRegistrationStatus": "unregistered"
                },
                "roles": [{ "role": "supplier" }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["gstRegistrationStatus"], "unregistered");
        assert_eq!(created["normalizedGstin"], Value::Null);
        assert_eq!(created["placeOfSupplyStateId"], Value::Null);

        // 'unknown' is the default and is a different fact from 'unregistered'.
        let (status, unknown) = create(
            &f.pool,
            json!({ "party": { "displayName": "Not Yet Captured" } }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{unknown}");
        assert_eq!(unknown["gstRegistrationStatus"], "unknown");
    }

    #[tokio::test]
    async fn one_active_party_may_hold_a_gstin_but_a_pan_may_legitimately_repeat() {
        let f = fixture().await;
        let (status, first) = create(&f.pool, supplier()).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");

        let mut same_gstin = supplier();
        same_gstin["party"]["displayName"] = json!("Sharma Medicals Branch");
        let (status, conflict) = create(&f.pool, same_gstin).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "duplicate_conflict");

        // The same legal entity registered in another state shares its PAN, which must be allowed.
        let (status, sibling) = create(
            &f.pool,
            json!({
                "party": {
                    "displayName": "Sharma Medicals Karnataka",
                    "gstRegistrationStatus": "unregistered",
                    "pan": "AAPFU0939F"
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{sibling}");
        assert_eq!(sibling["normalizedPan"], "AAPFU0939F");
    }

    #[tokio::test]
    async fn the_customer_role_is_named_by_the_schema_but_refused_by_this_phase() {
        let f = fixture().await;
        let mut body = supplier();
        body["roles"] = json!([{ "role": "customer" }]);
        let (status, error) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["issues"][0]["field"], "role");
        assert!(
            error["issues"][0]["message"]
                .as_str()
                .unwrap()
                .contains("supplier")
        );

        // The schema does name it, so the later phase needs no migration for the word itself.
        let (_, created) = create(&f.pool, supplier()).await;
        let party_id = created["id"].as_str().unwrap();
        let direct = sqlx::query(
            "INSERT INTO party_roles (id,party_id,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,'customer',strftime('%Y-%m-%dT%H:%M:%fZ','now'),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(party_id)
        .execute(&f.pool)
        .await;
        assert!(direct.is_ok(), "the schema must already accept the word");
    }

    #[tokio::test]
    async fn a_role_is_unique_while_active_and_may_be_archived_then_restored() {
        let f = fixture().await;
        let (_, created) = create(&f.pool, supplier()).await;
        let party_id = created["id"].as_str().unwrap().to_owned();
        let role_id = created["roles"][0]["id"].as_str().unwrap().to_owned();

        let (status, duplicate) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{party_id}/roles"),
            json!({ "role": "supplier" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
        assert_eq!(duplicate["code"], "duplicate_conflict");

        let (status, archived) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/party-roles/{role_id}/archive"),
            json!({ "expectedRevision": 1, "reason": "No longer supplies us" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
        assert_eq!(archived["revision"], 2);

        // With no active role the same role may be attached again.
        let (status, again) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{party_id}/roles"),
            json!({ "role": "supplier" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{again}");

        // Restoring the first would create a second active supplier role, so it is refused.
        let (status, restored) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/party-roles/{role_id}/restore"),
            json!({ "expectedRevision": 2, "reason": "Supplying again" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{restored}");
        assert_eq!(restored["code"], "duplicate_conflict");
    }

    #[tokio::test]
    async fn only_one_primary_address_per_purpose_is_active_at_a_time() {
        let f = fixture().await;
        let mut body = supplier();
        body["addresses"] = json!([
            { "addressRole": "billing", "line1": "12 Market Road", "isPrimary": true },
            { "addressRole": "billing", "line1": "13 Market Road", "isPrimary": true }
        ]);
        let (status, error) = create(&f.pool, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert_eq!(error["issues"][0]["field"], "addresses");

        let (_, created) = create(&f.pool, supplier()).await;
        let party_id = created["id"].as_str().unwrap().to_owned();
        let uri = format!("/api/v1/parties/{party_id}/addresses");

        let (status, first) = request_json(
            f.pool.clone(),
            "POST",
            &uri,
            json!({ "addressRole": "billing", "line1": "12 Market Road", "isPrimary": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let (status, second) = request_json(
            f.pool.clone(),
            "POST",
            &uri,
            json!({ "addressRole": "billing", "line1": "13 Market Road", "isPrimary": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{second}");
        assert_eq!(second["code"], "duplicate_conflict");
        // A non-primary address of the same purpose is fine.
        let (status, third) = request_json(
            f.pool.clone(),
            "POST",
            &uri,
            json!({ "addressRole": "billing", "line1": "14 Market Road" }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{third}");
        // A shipping primary is a different purpose and does not collide.
        let (status, shipping) = request_json(
            f.pool.clone(),
            "POST",
            &uri,
            json!({ "addressRole": "shipping", "line1": "Warehouse 4", "isPrimary": true }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{shipping}");
    }

    #[tokio::test]
    async fn an_update_requires_the_revision_that_was_read() {
        let f = fixture().await;
        let (_, created) = create(&f.pool, supplier()).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let uri = format!("/api/v1/parties/{id}");

        let renamed = json!({
            "expectedRevision": 1,
            "party": {
                "displayName": "Sharma Medicals & Sons",
                "gstRegistrationStatus": "registered",
                "gstin": MAHARASHTRA_GSTIN,
                "placeOfSupplyStateId": MAHARASHTRA_STATE
            },
            "reason": "Name corrected from the certificate"
        });
        let (status, updated) =
            request_json(f.pool.clone(), "PUT", &uri, renamed.clone()).await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["displayName"], "Sharma Medicals & Sons");
        assert_eq!(updated["normalizedSearchName"], "sharma medicals & sons");

        // Replaying the stale revision must not silently overwrite the newer one.
        let (status, conflict) = request_json(f.pool.clone(), "PUT", &uri, renamed).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["expectedRevision"], 1);
        assert_eq!(conflict["currentRevision"], 2);
    }

    #[tokio::test]
    async fn archiving_is_refused_while_active_children_remain() {
        let f = fixture().await;
        let (_, created) = create(&f.pool, supplier()).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let role_id = created["roles"][0]["id"].as_str().unwrap().to_owned();
        let archive = json!({ "expectedRevision": 1, "reason": "Closed down" });

        let (status, refused) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{id}/archive"),
            archive.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "archived_conflict");

        request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/party-roles/{role_id}/archive"),
            archive.clone(),
        )
        .await;
        let (status, archived) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{id}/archive"),
            archive,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
        assert_eq!(archived["archiveReason"], "Closed down");

        // An archived party frees its GSTIN for a replacement record.
        let (status, replacement) = create(&f.pool, supplier()).await;
        assert_eq!(status, StatusCode::CREATED, "{replacement}");

        // Restoring now would put the GSTIN back into conflict, so it is refused.
        let (status, restored) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{id}/restore"),
            json!({ "expectedRevision": 2, "reason": "Reopened" }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{restored}");
        assert_eq!(restored["code"], "duplicate_conflict");
    }

    #[tokio::test]
    async fn an_archived_party_restores_once_nothing_conflicts() {
        let f = fixture().await;
        let (_, created) = create(&f.pool, supplier()).await;
        let id = created["id"].as_str().unwrap().to_owned();
        let role_id = created["roles"][0]["id"].as_str().unwrap().to_owned();
        let reason = json!({ "expectedRevision": 1, "reason": "Temporarily closed" });

        request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/party-roles/{role_id}/archive"),
            reason.clone(),
        )
        .await;
        request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{id}/archive"),
            reason,
        )
        .await;
        let (status, restored) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/parties/{id}/restore"),
            json!({ "expectedRevision": 2, "reason": "Reopened" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert_eq!(restored["status"], "active");
        assert_eq!(restored["revision"], 3);
        assert_eq!(restored["archivedAtUtc"], Value::Null);
        // The role stayed archived: restore brings back only what was asked for.
        assert_eq!(restored["roles"][0]["status"], "archived");

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM master_change_events WHERE entity_type='party' AND entity_id=? \
             ORDER BY entity_revision",
        )
        .bind(&id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(actions, ["created", "archived", "restored"]);
    }

    #[tokio::test]
    async fn listing_filters_by_status_role_and_search_across_identifiers() {
        let f = fixture().await;
        create(&f.pool, supplier()).await;
        create(
            &f.pool,
            json!({
                "party": {
                    "displayName": "Bharat Distributors",
                    "gstRegistrationStatus": "unregistered"
                }
            }),
        )
        .await;

        let listed = |query: String| {
            let pool = f.pool.clone();
            async move {
                request_json(pool, "GET", &format!("/api/v1/parties{query}"), Value::Null)
                    .await
                    .1
            }
        };
        assert_eq!(listed(String::new()).await.as_array().unwrap().len(), 2);
        // Only the first party carries a supplier role.
        assert_eq!(
            listed("?role=supplier".to_owned())
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            listed("?search=bharat".to_owned())
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // Searching by the tax number finds the record without knowing its name.
        let by_gstin = listed("?search=27aapfu0939f1zv".to_owned()).await;
        assert_eq!(by_gstin.as_array().unwrap().len(), 1);
        assert_eq!(by_gstin[0]["displayName"], "Sharma Medicals");
        assert_eq!(
            listed("?status=archived".to_owned())
                .await
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let (status, error) = request_json(
            f.pool.clone(),
            "GET",
            "/api/v1/parties?status=deleted",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    }

    #[tokio::test]
    async fn duplicate_candidates_score_the_strongest_signal_first_and_never_block() {
        let f = fixture().await;
        let mut first = supplier();
        first["party"]["primaryPhone"] = json!("+91 98200 12345");
        first["party"]["primaryEmail"] = json!("Sales@Sharma-Medicals.CO.IN");
        first["party"]["drugLicenceNumber"] = json!("20B-1234 / 21B-5678");
        let (status, created) = create(&f.pool, first).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let existing = created["id"].as_str().unwrap().to_owned();
        // Both are stored already normalised.
        assert_eq!(created["primaryPhone"], "+919820012345");
        assert_eq!(created["primaryEmail"], "sales@sharma-medicals.co.in");

        let mut probe = supplier();
        probe["party"]["displayName"] = json!("  sharma   MEDICALS  ");
        probe["party"]["primaryPhone"] = json!("+91-98200-12345");
        let (status, candidates) = request_json(
            f.pool.clone(),
            "POST",
            "/api/v1/parties/duplicate-candidates",
            probe,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{candidates}");
        let rows = candidates.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["candidateId"], existing.as_str());
        // GSTIN 100 + name 25 + phone 20.
        assert_eq!(rows[0]["score"], 145);
        let reasons = rows[0]["reasonCodes"].as_array().unwrap();
        for expected in ["gstin_match", "name_match", "phone_match"] {
            assert!(
                reasons.iter().any(|reason| reason == expected),
                "missing {expected} in {reasons:?}"
            );
        }

        // The probe wrote nothing, and an unrelated party matches nothing.
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM parties")
                .fetch_one(&f.pool)
                .await
                .unwrap(),
            1
        );
        let (_, none) = request_json(
            f.pool.clone(),
            "POST",
            "/api/v1/parties/duplicate-candidates",
            json!({ "party": { "displayName": "Completely Unrelated Traders" } }),
        )
        .await;
        assert_eq!(none.as_array().unwrap().len(), 0);

        // Creation is never blocked by a candidate: two pharmacies may share a name.
        let (status, allowed) = create(
            &f.pool,
            json!({
                "party": {
                    "displayName": "Sharma Medicals",
                    "gstRegistrationStatus": "unregistered"
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{allowed}");
    }

    #[tokio::test]
    async fn a_read_only_role_may_look_but_never_write() {
        let f = fixture().await;
        let (_, created) = create(&f.pool, supplier()).await;
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, listed) = request_json_as(
            f.pool.clone(),
            "GET",
            "/api/v1/parties",
            Value::Null,
            Some(CASHIER_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert_eq!(listed.as_array().unwrap().len(), 1);

        for (method, uri, body) in [
            ("POST", "/api/v1/parties".to_owned(), supplier()),
            (
                "PUT",
                format!("/api/v1/parties/{id}"),
                json!({ "expectedRevision": 1, "party": { "displayName": "Renamed" } }),
            ),
            (
                "POST",
                format!("/api/v1/parties/{id}/roles"),
                json!({ "role": "supplier" }),
            ),
        ] {
            let (status, denied) =
                request_json_as(f.pool.clone(), method, &uri, body, Some(CASHIER_TOKEN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {denied}");
            assert_eq!(denied["code"], "authorization_denied");
        }

        // An anonymous caller sees nothing at all.
        let (status, anonymous) =
            request_json_as(f.pool.clone(), "GET", "/api/v1/parties", Value::Null, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{anonymous}");
        assert_eq!(anonymous["code"], "authentication_required");
    }

    #[tokio::test]
    async fn contact_and_licence_details_are_validated_and_normalised() {
        let f = fixture().await;
        for (field, value) in [
            ("primaryPhone", json!("98x0012345")),
            ("primaryEmail", json!("not-an-address")),
            ("drugLicenceValidUpto", json!("2026-13-01")),
            ("displayName", json!("   ")),
        ] {
            let mut body = supplier();
            body["party"][field] = value;
            let (status, error) = create(&f.pool, body).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field} {error}");
            assert_eq!(error["issues"][0]["field"], field);
        }

        let mut good = supplier();
        good["party"]["drugLicenceNumber"] = json!("  20B-1234 / 21B-5678  ");
        good["party"]["drugLicenceValidUpto"] = json!("2028-03-31");
        let (status, created) = create(&f.pool, good).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        // The licence is kept exactly as printed, only trimmed.
        assert_eq!(created["drugLicenceNumber"], "20B-1234 / 21B-5678");
        assert_eq!(created["drugLicenceValidUpto"], "2028-03-31");
    }

    #[tokio::test]
    async fn the_state_reference_master_is_seeded_and_administrable() {
        let f = fixture().await;
        let (status, states) = request_json(
            f.pool.clone(),
            "GET",
            "/api/v1/reference/state-codes?search=maharashtra",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{states}");
        assert_eq!(states.as_array().unwrap().len(), 1);
        assert_eq!(states[0]["id"], MAHARASHTRA_STATE);
        assert_eq!(states[0]["attributes"]["stateCode"], "27");
        assert_eq!(states[0]["attributes"]["jurisdiction"], "IN");

        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM state_codes")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(total, 39, "38 numbered states plus Other Territory");

        let (status, created) = request_json(
            f.pool.clone(),
            "POST",
            "/api/v1/reference/state-codes",
            json!({ "attributes": {
                "jurisdiction": "IN", "stateCode": "99", "displayName": "Test Territory"
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        // The same code twice in one jurisdiction is a duplicate.
        let (status, duplicate) = request_json(
            f.pool.clone(),
            "POST",
            "/api/v1/reference/state-codes",
            json!({ "attributes": {
                "jurisdiction": "IN", "stateCode": "99", "displayName": "Again"
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
    }

    #[tokio::test]
    async fn an_archived_state_cannot_be_attached_to_a_new_party() {
        let f = fixture().await;
        let (status, archived) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/reference/state-codes/{MAHARASHTRA_STATE}/archive"),
            json!({ "expectedRevision": 1, "reason": "Not used by this store" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        let (status, refused) = create(&f.pool, supplier()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "party_conflict");
    }

    #[tokio::test]
    async fn every_party_error_stays_free_of_database_detail() {
        let f = fixture().await;
        let mut broken = supplier();
        broken["party"]["gstin"] = json!("27AAPFU0939F1ZX");
        let (_, validation) = create(&f.pool, broken).await;
        create(&f.pool, supplier()).await;
        let (_, duplicate) = create(&f.pool, supplier()).await;
        let mut mismatched = supplier();
        mismatched["party"]["placeOfSupplyStateId"] = json!(KARNATAKA_STATE);
        let (_, conflict) = create(&f.pool, mismatched).await;

        for body in [validation, duplicate, conflict] {
            let text = body.to_string().to_ascii_lowercase();
            for leak in [
                "sqlite",
                "constraint failed",
                "insert into",
                "raise(",
                "trigger",
            ] {
                assert!(!text.contains(leak), "leaked {leak} in {text}");
            }
        }
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
        .bind(format!("{role} test user"))
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

    async fn request_json(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        request_json_as(pool, method, uri, body, Some(OWNER_TOKEN)).await
    }

    async fn request_json_as(
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
