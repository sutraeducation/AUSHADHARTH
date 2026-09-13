//! Phase 1D inventory movement ledger.
//!
//! The ledger is the sole authority for quantity. Nothing here stores a balance: every on-hand
//! figure is derived by summing signed movements, and a posted movement is immutable history.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Sqlite, SqlitePool, pool::PoolConnection};
use uuid::Uuid;

use super::auth::{self, AuthError, AuthenticatedActor};
use super::reference_masters::ReferenceState;
use crate::domain::catalog::{
    CatalogValidationIssue, MAX_BASE_QUANTITY_ATOMS, optional_text, validate_date, validate_uuid_v7,
};

#[derive(Debug)]
enum InventoryError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    NotFound,
    Archived,
    BatchPackMismatch,
    InsufficientStock { available: i64 },
    ServiceBusy,
    Internal,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    issues: Vec<ErrorIssue>,
    available_atoms: Option<i64>,
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
        available_atoms: None,
    }
}

impl IntoResponse for InventoryError {
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
                    available_atoms: None,
                },
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                simple("not_found", "The record no longer exists."),
            ),
            Self::Archived => (
                StatusCode::CONFLICT,
                simple(
                    "archived_conflict",
                    "Stock cannot be posted against an archived record.",
                ),
            ),
            Self::BatchPackMismatch => (
                StatusCode::CONFLICT,
                simple(
                    "batch_pack_mismatch",
                    "The batch does not belong to the selected pack.",
                ),
            ),
            Self::InsufficientStock { available } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: "insufficient_stock",
                    message: "The posting would leave a negative balance.",
                    issues: Vec::new(),
                    available_atoms: Some(available),
                },
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

impl From<AuthError> for InventoryError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
    }
}

fn validation(field: &str, message: &str) -> InventoryError {
    InventoryError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> InventoryError {
    InventoryError::Validation(vec![issue])
}

/// Contention maps to a typed busy response; no raw database text ever reaches a client.
fn map_database_error(error: sqlx::Error) -> InventoryError {
    if let sqlx::Error::Database(database) = &error {
        let code = database.code().unwrap_or_default().to_string();
        let message = database.message().to_ascii_lowercase();
        if matches!(code.as_str(), "5" | "6" | "261" | "262" | "517")
            || message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("database is busy")
        {
            return InventoryError::ServiceBusy;
        }
        if message.contains("inventory_movement_conflict") {
            return InventoryError::Archived;
        }
        if message.contains("foreign key constraint failed") {
            return InventoryError::BatchPackMismatch;
        }
    }
    InventoryError::Internal
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PostMovementRequest {
    idempotency_key: String,
    movement_type: String,
    product_pack_id: String,
    batch_id: Option<String>,
    quantity_delta_atoms: i64,
    occurred_on: String,
    reason: Option<String>,
    reverses_movement_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct MovementResponse {
    id: String,
    store_id: String,
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    movement_type: String,
    quantity_delta_atoms: i64,
    occurred_on: String,
    reason: Option<String>,
    reverses_movement_id: Option<String>,
    idempotency_key: String,
    posted_by_user_id: String,
    posted_at_utc: String,
}

/// A derived balance. Never stored, always summed from the ledger.
#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct StockBalanceResponse {
    product_id: String,
    product_pack_id: String,
    batch_id: Option<String>,
    balance_atoms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StockQuery {
    product_id: Option<String>,
    pack_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerQuery {
    pack_id: Option<String>,
    batch_id: Option<String>,
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/inventory/stock", get(list_stock))
        .route(
            "/api/v1/inventory/movements",
            get(list_movements).post(post_movement),
        )
        .route("/api/v1/inventory/movements/{id}", get(get_movement))
}

async fn require_inventory_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, InventoryError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

/// Posting is a sensitive mutation: Owner/Admin only, with the frozen Host/Origin protection.
async fn require_inventory_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, InventoryError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

/// The Store is resolved from the installation, never accepted from the browser.
async fn current_store(pool: &SqlitePool) -> Result<String, InventoryError> {
    sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|_| InventoryError::Internal)?
        .ok_or(InventoryError::NotFound)
}

async fn list_stock(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<StockQuery>,
) -> Result<Json<Vec<StockBalanceResponse>>, InventoryError> {
    require_inventory_reader(&state, &headers).await?;
    let store_id = current_store(&state.pool).await?;
    let product_filter = optional_uuid(query.product_id, "productId")?;
    let pack_filter = optional_uuid(query.pack_id, "packId")?;
    // A balance is a sum of movements. Nothing is read from a stored quantity, because none exists.
    let rows = sqlx::query_as::<_, StockBalanceResponse>(
        "SELECT product_id,product_pack_id,batch_id,SUM(quantity_delta_atoms) AS balance_atoms \
         FROM inventory_movements \
         WHERE store_id=?1 AND (?2 IS NULL OR product_id=?2) AND (?3 IS NULL OR product_pack_id=?3) \
         GROUP BY product_id,product_pack_id,batch_id \
         HAVING SUM(quantity_delta_atoms) <> 0 \
         ORDER BY product_id,product_pack_id,batch_id",
    )
    .bind(&store_id)
    .bind(&product_filter)
    .bind(&pack_filter)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn list_movements(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<LedgerQuery>,
) -> Result<Json<Vec<MovementResponse>>, InventoryError> {
    require_inventory_reader(&state, &headers).await?;
    let store_id = current_store(&state.pool).await?;
    let pack_filter = optional_uuid(query.pack_id, "packId")?;
    let batch_filter = optional_uuid(query.batch_id, "batchId")?;
    let rows = sqlx::query_as::<_, MovementResponse>(&format!(
        "SELECT {MOVEMENT_COLUMNS} FROM inventory_movements \
         WHERE store_id=?1 AND (?2 IS NULL OR product_pack_id=?2) AND (?3 IS NULL OR batch_id=?3) \
         ORDER BY occurred_on DESC,posted_at_utc DESC,id DESC LIMIT 200"
    ))
    .bind(&store_id)
    .bind(&pack_filter)
    .bind(&batch_filter)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(rows))
}

async fn get_movement(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<MovementResponse>, InventoryError> {
    require_inventory_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    fetch_movement(&state.pool, &id).await.map(Json)
}

async fn post_movement(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<PostMovementRequest>,
) -> Result<(StatusCode, Json<MovementResponse>), InventoryError> {
    let actor = require_inventory_admin(&state, &headers).await?;
    let request = prepare_movement(request)?;
    let store_id = current_store(&state.pool).await?;

    let mut connection = state
        .pool
        .acquire()
        .await
        .map_err(|_| InventoryError::Internal)?;
    // The non-negative rule is a read-check-write, so the write lock is taken before the balance is
    // read. A second concurrent posting blocks here and then re-reads a balance that already
    // includes the first, so there is no stale-read window and no lost update.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(map_database_error)?;
    let result = post_within_transaction(&mut connection, &store_id, &request, &actor.id).await;
    match result {
        Ok(outcome) => {
            sqlx::query("COMMIT")
                .execute(&mut *connection)
                .await
                .map_err(map_database_error)?;
            drop(connection);
            let movement = fetch_movement(&state.pool, &outcome.id).await?;
            Ok((outcome.status, Json(movement)))
        }
        Err(error) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
            Err(error)
        }
    }
}

struct PostOutcome {
    id: String,
    status: StatusCode,
}

async fn post_within_transaction(
    connection: &mut PoolConnection<Sqlite>,
    store_id: &str,
    request: &PostMovementRequest,
    actor_id: &str,
) -> Result<PostOutcome, InventoryError> {
    // A replayed key returns the stored movement unconditionally. Failing to post twice is visible
    // and recoverable; duplicating stock silently is neither.
    let replay: Option<String> =
        sqlx::query_scalar("SELECT id FROM inventory_movements WHERE idempotency_key=?")
            .bind(&request.idempotency_key)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    if let Some(id) = replay {
        return Ok(PostOutcome {
            id,
            status: StatusCode::OK,
        });
    }

    let pack: Option<(String, String)> =
        sqlx::query_as("SELECT product_id,status FROM product_packs WHERE id=?")
            .bind(&request.product_pack_id)
            .fetch_optional(&mut **connection)
            .await
            .map_err(map_database_error)?;
    let (product_id, pack_status) = pack.ok_or(InventoryError::NotFound)?;
    if pack_status != "active" {
        return Err(InventoryError::Archived);
    }

    if let Some(batch_id) = &request.batch_id {
        let batch: Option<(String, String)> =
            sqlx::query_as("SELECT product_pack_id,status FROM product_batches WHERE id=?")
                .bind(batch_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
        let (batch_pack, batch_status) = batch.ok_or(InventoryError::NotFound)?;
        // Checked here for a precise message; guaranteed by the composite foreign key regardless.
        if batch_pack != request.product_pack_id {
            return Err(InventoryError::BatchPackMismatch);
        }
        if batch_status != "active" {
            return Err(InventoryError::Archived);
        }
    }

    if let Some(reversed) = &request.reverses_movement_id {
        let exists: Option<String> =
            sqlx::query_scalar("SELECT id FROM inventory_movements WHERE id=? AND store_id=?")
                .bind(reversed)
                .bind(store_id)
                .fetch_optional(&mut **connection)
                .await
                .map_err(map_database_error)?;
        if exists.is_none() {
            return Err(InventoryError::NotFound);
        }
    }

    // Negative resulting stock is prohibited in this foundation. The balance is read under the write
    // lock taken above, so this check cannot race another posting.
    if request.quantity_delta_atoms < 0 {
        let available: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements \
             WHERE store_id=? AND product_pack_id=? AND batch_id IS ?",
        )
        .bind(store_id)
        .bind(&request.product_pack_id)
        .bind(&request.batch_id)
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
        if available + request.quantity_delta_atoms < 0 {
            return Err(InventoryError::InsufficientStock { available });
        }
    }

    let id = Uuid::now_v7().to_string();
    let now: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **connection)
        .await
        .map_err(map_database_error)?;
    sqlx::query(
        "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
         movement_type,quantity_delta_atoms,occurred_on,reason,reverses_movement_id,\
         idempotency_key,posted_by_user_id,posted_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(store_id)
    .bind(&product_id)
    .bind(&request.product_pack_id)
    .bind(&request.batch_id)
    .bind(&request.movement_type)
    .bind(request.quantity_delta_atoms)
    .bind(&request.occurred_on)
    .bind(&request.reason)
    .bind(&request.reverses_movement_id)
    .bind(&request.idempotency_key)
    .bind(actor_id)
    .bind(&now)
    .execute(&mut **connection)
    .await
    .map_err(map_database_error)?;

    Ok(PostOutcome {
        id,
        status: StatusCode::CREATED,
    })
}

fn prepare_movement(
    mut request: PostMovementRequest,
) -> Result<PostMovementRequest, InventoryError> {
    request.idempotency_key =
        validate_uuid_v7(&request.idempotency_key, "idempotencyKey").map_err(validation_issue)?;
    request.product_pack_id =
        validate_uuid_v7(&request.product_pack_id, "productPackId").map_err(validation_issue)?;
    request.batch_id = match request.batch_id.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => {
            Some(validate_uuid_v7(value, "batchId").map_err(validation_issue)?)
        }
        _ => None,
    };
    request.reverses_movement_id = match request.reverses_movement_id.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => {
            Some(validate_uuid_v7(value, "reversesMovementId").map_err(validation_issue)?)
        }
        _ => None,
    };
    if !matches!(
        request.movement_type.as_str(),
        "opening_stock" | "adjustment"
    ) {
        return Err(validation(
            "movementType",
            "must be opening_stock or adjustment",
        ));
    }
    if request.quantity_delta_atoms == 0 {
        return Err(validation(
            "quantityDeltaAtoms",
            "must not be zero; a movement records a change",
        ));
    }
    if request.quantity_delta_atoms.abs() > MAX_BASE_QUANTITY_ATOMS {
        return Err(validation(
            "quantityDeltaAtoms",
            "must be a bounded integer quantity",
        ));
    }
    if request.movement_type == "opening_stock" && request.quantity_delta_atoms < 0 {
        return Err(validation(
            "quantityDeltaAtoms",
            "opening stock must be positive; correct it with an adjustment",
        ));
    }
    if request.reverses_movement_id.is_some() && request.movement_type != "adjustment" {
        return Err(validation(
            "reversesMovementId",
            "only an adjustment may reverse a movement",
        ));
    }
    request.occurred_on = validate_date(Some(&request.occurred_on), "occurredOn")
        .map_err(validation_issue)?
        .ok_or_else(|| validation("occurredOn", "is required"))?;
    request.reason =
        optional_text(request.reason.as_deref(), "reason", 500).map_err(validation_issue)?;
    Ok(request)
}

fn optional_uuid(value: Option<String>, field: &str) -> Result<Option<String>, InventoryError> {
    match value.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => Ok(Some(
            validate_uuid_v7(value, field).map_err(validation_issue)?,
        )),
        _ => Ok(None),
    }
}

const MOVEMENT_COLUMNS: &str = "id,store_id,product_id,product_pack_id,batch_id,movement_type,quantity_delta_atoms,\
     occurred_on,reason,reverses_movement_id,idempotency_key,posted_by_user_id,posted_at_utc";

async fn fetch_movement(pool: &SqlitePool, id: &str) -> Result<MovementResponse, InventoryError> {
    sqlx::query_as::<_, MovementResponse>(&format!(
        "SELECT {MOVEMENT_COLUMNS} FROM inventory_movements WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(map_database_error)?
    .ok_or(InventoryError::NotFound)
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const TABLET: &str = "01997000-0000-7000-8000-000000000001";
    const STRIP: &str = "01997000-0000-7000-8000-000000000004";
    const OWNER_TOKEN: &str = "inventory-owner-session-token";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        store_id: String,
        pack_id: String,
        other_pack_id: String,
        batch_id: String,
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("inventory.sqlite3"))
            .await
            .unwrap();
        let store_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO store_identity (store_id,display_name,business_time_zone,created_at_utc) \
             VALUES (?,'Test Store','Asia/Kolkata',strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&store_id)
        .execute(&pool)
        .await
        .unwrap();
        insert_session(&pool, "owner_admin", OWNER_TOKEN).await;

        let product = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":{"productKind":"general_pharmacy_item","baseUnitId":TABLET,
                "quantityScale":0,"displayName":"Ledger item"}}),
        )
        .await
        .1;
        let product_id = product["id"].as_str().unwrap().to_owned();
        let pack_id = create_pack(&pool, &product_id, STRIP, 10).await;
        let other_pack_id = create_pack(&pool, &product_id, TABLET, 1).await;
        let (status, batch) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{pack_id}/batches"),
            json!({"batchNumber":"LOT-1","expiresOn":"2029-12-31"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{batch}");
        Fixture {
            _temp: temp,
            pool,
            store_id,
            pack_id,
            other_pack_id,
            batch_id: batch["id"].as_str().unwrap().to_owned(),
        }
    }

    async fn create_pack(pool: &SqlitePool, product_id: &str, unit: &str, atoms: i64) -> String {
        let (status, body) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            json!({"containerUnitId":unit,"baseQuantityAtoms":atoms}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    fn opening(pack: &str, atoms: i64) -> Value {
        json!({
            "idempotencyKey": Uuid::now_v7().to_string(),
            "movementType": "opening_stock",
            "productPackId": pack,
            "quantityDeltaAtoms": atoms,
            "occurredOn": "2026-04-01"
        })
    }

    async fn post(pool: &SqlitePool, body: Value) -> (StatusCode, Value) {
        request_json(pool.clone(), "POST", "/api/v1/inventory/movements", body).await
    }

    async fn balance(pool: &SqlitePool) -> Value {
        request_json(pool.clone(), "GET", "/api/v1/inventory/stock", Value::Null)
            .await
            .1
    }

    #[tokio::test]
    async fn opening_stock_posts_exact_atoms_and_balances_derive_from_the_ledger() {
        let f = fixture().await;
        // Five strips of ten tablets is fifty base-unit atoms.
        let (status, created) = post(&f.pool, opening(&f.pack_id, 50)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["quantityDeltaAtoms"], 50);
        assert_eq!(created["storeId"], f.store_id.as_str());
        assert_eq!(created["movementType"], "opening_stock");
        assert_eq!(
            Uuid::parse_str(created["id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );

        // Issuing three loose tablets leaves forty-seven, which counting packs could not express.
        let mut issue = opening(&f.pack_id, -3);
        issue["movementType"] = json!("adjustment");
        let (status, adjusted) = post(&f.pool, issue).await;
        assert_eq!(status, StatusCode::CREATED, "{adjusted}");

        let stock = balance(&f.pool).await;
        assert_eq!(stock.as_array().unwrap().len(), 1);
        assert_eq!(stock[0]["balanceAtoms"], 47);
        assert_eq!(stock[0]["productPackId"], f.pack_id.as_str());
        assert_eq!(stock[0]["batchId"], Value::Null);

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[tokio::test]
    async fn batch_balances_are_separate_and_a_mismatched_batch_is_refused_by_the_database() {
        let f = fixture().await;
        let mut batched = opening(&f.pack_id, 30);
        batched["batchId"] = json!(f.batch_id);
        let (status, body) = post(&f.pool, batched).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        post(&f.pool, opening(&f.pack_id, 20)).await;

        let stock = balance(&f.pool).await;
        assert_eq!(stock.as_array().unwrap().len(), 2);
        let batched_row = stock
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["batchId"] == f.batch_id.as_str())
            .unwrap();
        assert_eq!(batched_row["balanceAtoms"], 30);

        // The service reports the precise mismatch.
        let mut wrong = opening(&f.other_pack_id, 5);
        wrong["batchId"] = json!(f.batch_id);
        let (status, mismatch) = post(&f.pool, wrong).await;
        assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
        assert_eq!(mismatch["code"], "batch_pack_mismatch");

        // The database refuses it too, so the guarantee does not depend on the service.
        let direct = sqlx::query(
            "INSERT INTO inventory_movements (id,store_id,product_id,product_pack_id,batch_id,\
             movement_type,quantity_delta_atoms,occurred_on,idempotency_key,posted_by_user_id,posted_at_utc) \
             SELECT ?,?,product_id,?,?,'adjustment',1,'2026-04-01',?,(SELECT id FROM users LIMIT 1),\
             strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM product_packs WHERE id=?",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&f.store_id)
        .bind(&f.other_pack_id)
        .bind(&f.batch_id)
        .bind(Uuid::now_v7().to_string())
        .bind(&f.other_pack_id)
        .execute(&f.pool)
        .await;
        assert!(direct.is_err(), "composite foreign key must reject it");
    }

    #[tokio::test]
    async fn the_ledger_is_append_only_and_replays_are_idempotent() {
        let f = fixture().await;
        let request = opening(&f.pack_id, 40);
        let (status, first) = post(&f.pool, request.clone()).await;
        assert_eq!(status, StatusCode::CREATED);
        // A retry with the same key returns the same movement instead of duplicating stock.
        let (status, replay) = post(&f.pool, request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["id"], first["id"]);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inventory_movements")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(balance(&f.pool).await[0]["balanceAtoms"], 40);

        let id = first["id"].as_str().unwrap();
        // Immutability is enforced by the database, not by service discipline.
        assert!(
            sqlx::query("UPDATE inventory_movements SET quantity_delta_atoms=999 WHERE id=?")
                .bind(id)
                .execute(&f.pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM inventory_movements WHERE id=?")
                .bind(id)
                .execute(&f.pool)
                .await
                .is_err()
        );

        // Correction is a further movement, and one movement may be reversed only once.
        let mut reversal = opening(&f.pack_id, -40);
        reversal["movementType"] = json!("adjustment");
        reversal["reversesMovementId"] = json!(id);
        let (status, reversed) = post(&f.pool, reversal.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{reversed}");
        assert_eq!(balance(&f.pool).await.as_array().unwrap().len(), 0);

        let mut again = reversal;
        again["idempotencyKey"] = json!(Uuid::now_v7().to_string());
        let (status, duplicate) = post(&f.pool, again).await;
        assert_ne!(status, StatusCode::CREATED, "{duplicate}");
    }

    #[tokio::test]
    async fn negative_balances_and_invalid_quantities_are_refused() {
        let f = fixture().await;
        post(&f.pool, opening(&f.pack_id, 10)).await;

        let mut too_much = opening(&f.pack_id, -11);
        too_much["movementType"] = json!("adjustment");
        let (status, insufficient) = post(&f.pool, too_much).await;
        assert_eq!(status, StatusCode::CONFLICT, "{insufficient}");
        assert_eq!(insufficient["code"], "insufficient_stock");
        assert_eq!(insufficient["availableAtoms"], 10);
        assert_eq!(balance(&f.pool).await[0]["balanceAtoms"], 10);

        let (status, zero) = post(&f.pool, opening(&f.pack_id, 0)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{zero}");
        assert_eq!(zero["issues"][0]["field"], "quantityDeltaAtoms");

        let (status, negative_opening) = post(&f.pool, opening(&f.pack_id, -5)).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{negative_opening}"
        );

        let mut bad_date = opening(&f.pack_id, 5);
        bad_date["occurredOn"] = json!("2026-02-30");
        let (status, invalid_date) = post(&f.pool, bad_date).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid_date}");
        assert_eq!(invalid_date["issues"][0]["field"], "occurredOn");
    }

    #[tokio::test]
    async fn archived_records_refuse_postings_while_expired_batches_accept_them() {
        let f = fixture().await;
        // An expired lot is historical fact and may still receive opening stock.
        let (status, expired_batch) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/packs/{}/batches", f.pack_id),
            json!({"batchNumber":"OLD-1","expiresOn":"2020-01-31"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{expired_batch}");
        let expired_id = expired_batch["id"].as_str().unwrap().to_owned();
        let mut historical = opening(&f.pack_id, 12);
        historical["batchId"] = json!(expired_id);
        let (status, posted) = post(&f.pool, historical).await;
        assert_eq!(status, StatusCode::CREATED, "{posted}");

        // Archiving the batch stops new postings without touching existing history.
        let (status, archived) = request_json(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/batches/{expired_id}/archive"),
            json!({"expectedRevision":1,"reason":"Lot withdrawn"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        let mut blocked = opening(&f.pack_id, 5);
        blocked["batchId"] = json!(expired_id);
        let (status, refused) = post(&f.pool, blocked).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "archived_conflict");
        assert_eq!(balance(&f.pool).await[0]["balanceAtoms"], 12);

        let (status, missing) = post(&f.pool, opening(&Uuid::now_v7().to_string(), 5)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    }

    #[tokio::test]
    async fn inventory_roles_and_server_actor_are_enforced() {
        let f = fixture().await;
        let spoofed = Uuid::now_v7().to_string();
        let mut spoofing = opening(&f.pack_id, 7);
        spoofing["postedByUserId"] = json!(spoofed);
        let (status, created) = post(&f.pool, spoofing).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let owner: String =
            sqlx::query_scalar("SELECT id FROM users WHERE role='owner_admin' LIMIT 1")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(created["postedByUserId"], owner.as_str());
        assert_ne!(created["postedByUserId"], spoofed.as_str());

        for role in ["pharmacist", "cashier"] {
            let token = format!("{role}-inventory-token");
            insert_session(&f.pool, role, &token).await;
            let (read, _) = request_json_as(
                f.pool.clone(),
                "GET",
                "/api/v1/inventory/stock",
                Value::Null,
                Some(&token),
            )
            .await;
            assert_eq!(read, StatusCode::OK, "{role} must read stock");
            let (write, body) = request_json_as(
                f.pool.clone(),
                "POST",
                "/api/v1/inventory/movements",
                opening(&f.pack_id, 3),
                Some(&token),
            )
            .await;
            assert_eq!(write, StatusCode::FORBIDDEN, "{body}");
        }

        let (status, _) = request_json_as(
            f.pool.clone(),
            "GET",
            "/api/v1/inventory/stock",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn no_mutable_stock_column_exists_on_any_catalog_table() {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("scope.sqlite3"))
            .await
            .unwrap();
        // Quantity authority belongs to the ledger alone; a mutable balance on an identity row is
        // the anti-pattern this phase exists to prevent.
        for table in ["products", "product_packs", "product_batches"] {
            let columns: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT lower(name) FROM pragma_table_info('{table}')"
            ))
            .fetch_all(&pool)
            .await
            .unwrap();
            for forbidden in [
                "quantity_on_hand",
                "current_stock",
                "stock_balance",
                "available_stock",
                "balance_atoms",
            ] {
                assert!(
                    !columns.iter().any(|column| column == forbidden),
                    "{table} must not carry {forbidden}"
                );
            }
        }
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(tables.iter().any(|table| table == "inventory_movements"));
        for deferred in [
            "prices",
            "purchases",
            "sales",
            "stock_valuation",
            "customers",
        ] {
            assert!(!tables.iter().any(|table| table == deferred));
        }
    }

    async fn insert_session(pool: &SqlitePool, role: &str, token: &str) {
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-{}", &user_id[24..32]);
        sqlx::query(
            "INSERT INTO users (id,login_identifier,normalized_login_identifier,display_name,password_hash,role,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?, '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA',?,?,?)",
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
            "INSERT INTO user_sessions (id,user_id,token_hash,created_at_utc,expires_at_utc,last_seen_at_utc) VALUES (?,?,?,?,?,?)",
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
