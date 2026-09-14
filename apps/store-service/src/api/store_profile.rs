//! Phase 1G-0 store tax identity.
//!
//! The Store's own GST registration and place of supply. Phase 1E gave the supplier one; this is
//! the other half of the comparison that decides `CGST + SGST` versus `IGST`, so Phase 1G can
//! resolve tax treatment from data instead of assuming it or asking the browser.
//!
//! GSTIN validation is reused from the party domain rather than re-implemented, so the two can
//! never drift apart.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::{
    catalog::{CatalogValidationIssue, optional_text},
    parties::{GST_REGISTRATION_STATUSES, gstin_state_code, normalize_gstin},
};

#[derive(Debug)]
enum StoreProfileError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    Revision { expected: i64, current: i64 },
    TaxConflict,
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
    Router::new().route(
        "/api/v1/store/tax-identity",
        get(get_tax_identity).put(put_tax_identity),
    )
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
}
