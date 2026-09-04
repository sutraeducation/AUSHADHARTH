use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use super::reference_masters::ReferenceState;
use crate::domain::catalog::{
    CatalogValidationIssue, MAX_BASE_QUANTITY_ATOMS, MAX_QUANTITY_SCALE, normalize_barcode,
    normalize_sku, normalized_search_name, optional_text, required_text, validate_date,
    validate_uuid_v7,
};

#[derive(Debug)]
enum CatalogError {
    Validation(Vec<CatalogValidationIssue>),
    Duplicate,
    Revision { expected: i64, current: i64 },
    NotFound,
    Archived,
    Conversion,
    Barcode,
    DefaultPack,
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

impl IntoResponse for CatalogError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
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
                simple_error(
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
                simple_error("not_found", "The requested record was not found."),
            ),
            Self::Archived => (
                StatusCode::CONFLICT,
                simple_error(
                    "archived_conflict",
                    "The operation conflicts with the lifecycle state.",
                ),
            ),
            Self::Conversion => (
                StatusCode::CONFLICT,
                simple_error(
                    "conversion_conflict",
                    "The pack or quantity conversion is invalid.",
                ),
            ),
            Self::Barcode => (
                StatusCode::CONFLICT,
                simple_error(
                    "barcode_conflict",
                    "The barcode conflicts with an active assignment.",
                ),
            ),
            Self::DefaultPack => (
                StatusCode::CONFLICT,
                simple_error(
                    "default_pack_conflict",
                    "The pack policy conflicts with another default or its enabled flags.",
                ),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple_error("internal_error", "The operation could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

fn simple_error(code: &'static str, message: &'static str) -> ErrorBody {
    ErrorBody {
        code,
        message,
        issues: Vec::new(),
        expected_revision: None,
        current_revision: None,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductInput {
    product_kind: String,
    brand_id: Option<String>,
    dosage_form_id: Option<String>,
    base_unit_id: String,
    quantity_scale: i64,
    formulation_descriptor: Option<String>,
    route_descriptor: Option<String>,
    release_descriptor: Option<String>,
    display_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompanyRoleInput {
    company_id: String,
    role: String,
    effective_from: Option<String>,
    effective_to: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackInput {
    container_unit_id: String,
    base_quantity_atoms: i64,
    contained_pack_id: Option<String>,
    contained_pack_count: Option<i64>,
    sku_code: Option<String>,
    sku_store_id: Option<String>,
    display_label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyInput {
    store_id: String,
    purchase_enabled: bool,
    sale_enabled: bool,
    whole_pack_only_purchase: bool,
    fractional_sale_allowed: bool,
    minimum_sale_increment_atoms: i64,
    default_purchase_pack: bool,
    default_sale_pack: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BarcodeInput {
    namespace: String,
    value: String,
    symbology: Option<String>,
    scope: String,
    store_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AggregatePackInput {
    client_key: String,
    contained_pack_client_key: Option<String>,
    #[serde(flatten)]
    pack: PackInput,
    policy: Option<PolicyInput>,
    #[serde(default)]
    barcodes: Vec<BarcodeInput>,
}

#[derive(Debug)]
struct PreparedAggregatePack {
    id: String,
    pack: PackInput,
    policy: Option<PolicyInput>,
    barcodes: Vec<BarcodeInput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProductRequest {
    product: ProductInput,
    #[serde(default)]
    company_roles: Vec<CompanyRoleInput>,
    #[serde(default)]
    packs: Vec<AggregatePackInput>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProductRequest {
    expected_revision: i64,
    product: ProductInput,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePackRequest {
    expected_revision: i64,
    pack: PackInput,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCompanyRoleRequest {
    expected_revision: i64,
    role: CompanyRoleInput,
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
struct UpdatePolicyRequest {
    expected_revision: Option<i64>,
    policy: PolicyInput,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    search: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BarcodeLookupQuery {
    namespace: String,
    value: String,
    scope: String,
    store_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct ProductResponse {
    id: String,
    revision: i64,
    status: String,
    product_kind: String,
    brand_id: Option<String>,
    dosage_form_id: Option<String>,
    base_unit_id: String,
    quantity_scale: i64,
    formulation_descriptor: Option<String>,
    route_descriptor: Option<String>,
    release_descriptor: Option<String>,
    display_name: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct CompanyRoleResponse {
    id: String,
    product_id: String,
    company_id: String,
    role: String,
    effective_from: Option<String>,
    effective_to: Option<String>,
    revision: i64,
    status: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PackResponse {
    id: String,
    revision: i64,
    status: String,
    product_id: String,
    container_unit_id: String,
    base_quantity_atoms: i64,
    contained_pack_id: Option<String>,
    contained_pack_count: Option<i64>,
    sku_code: Option<String>,
    sku_store_id: Option<String>,
    display_label: Option<String>,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct PolicyResponse {
    id: String,
    store_id: String,
    product_id: String,
    pack_id: String,
    purchase_enabled: bool,
    sale_enabled: bool,
    whole_pack_only_purchase: bool,
    fractional_sale_allowed: bool,
    minimum_sale_increment_atoms: i64,
    default_purchase_pack: bool,
    default_sale_pack: bool,
    revision: i64,
    status: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct BarcodeResponse {
    id: String,
    pack_id: String,
    namespace: String,
    normalized_value: String,
    symbology: Option<String>,
    scope: String,
    store_id: Option<String>,
    revision: i64,
    status: String,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductDetailResponse {
    #[serde(flatten)]
    product: ProductResponse,
    company_roles: Vec<CompanyRoleResponse>,
    packs: Vec<PackResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateCandidate {
    candidate_id: String,
    score: i64,
    reason_codes: Vec<&'static str>,
    explanation: String,
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/products", get(list_products).post(create_product))
        .route(
            "/api/v1/products/duplicate-candidates",
            post(duplicate_candidates),
        )
        .route(
            "/api/v1/products/{id}",
            get(get_product).put(update_product),
        )
        .route("/api/v1/products/{id}/archive", post(archive_product))
        .route("/api/v1/products/{id}/restore", post(restore_product))
        .route(
            "/api/v1/products/{id}/company-roles",
            get(list_company_roles).post(create_company_role),
        )
        .route(
            "/api/v1/company-roles/{id}",
            axum::routing::put(update_company_role),
        )
        .route(
            "/api/v1/company-roles/{id}/archive",
            post(archive_company_role),
        )
        .route(
            "/api/v1/company-roles/{id}/restore",
            post(restore_company_role),
        )
        .route(
            "/api/v1/products/{id}/packs",
            get(list_packs).post(create_pack),
        )
        .route("/api/v1/packs/{id}", get(get_pack).put(update_pack))
        .route("/api/v1/packs/{id}/archive", post(archive_pack))
        .route("/api/v1/packs/{id}/restore", post(restore_pack))
        .route("/api/v1/packs/{id}/policy", get(get_policy).put(put_policy))
        .route("/api/v1/pack-policies/{id}/archive", post(archive_policy))
        .route("/api/v1/pack-policies/{id}/restore", post(restore_policy))
        .route(
            "/api/v1/packs/{id}/barcodes",
            get(list_barcodes).post(create_barcode),
        )
        .route("/api/v1/barcodes/resolve", get(resolve_barcode))
        .route("/api/v1/barcodes/{id}", get(get_barcode))
        .route("/api/v1/barcodes/{id}/archive", post(archive_barcode))
        .route("/api/v1/barcodes/{id}/restore", post(restore_barcode))
}

async fn list_products(
    State(state): State<ReferenceState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<ProductResponse>>, CatalogError> {
    let status = query.status.unwrap_or_else(|| "active".to_owned());
    if !matches!(status.as_str(), "active" | "archived" | "all") {
        return Err(validation("status", "must be active, archived, or all"));
    }
    let search = normalized_search_name(query.search.as_deref().unwrap_or_default());
    let pattern = format!("%{search}%");
    let rows = sqlx::query_as::<_, ProductResponse>(
        "SELECT DISTINCT p.id,p.revision,p.status,p.product_kind,p.brand_id,p.dosage_form_id,\
         p.base_unit_id,p.quantity_scale,p.formulation_descriptor,p.route_descriptor,\
         p.release_descriptor,p.display_name,p.created_at_utc,p.updated_at_utc,p.archived_at_utc,p.archive_reason \
         FROM products p \
         LEFT JOIN brands b ON b.id=p.brand_id \
         LEFT JOIN product_company_roles role ON role.product_id=p.id AND role.status='active' \
         LEFT JOIN pharmaceutical_companies company ON company.id=role.company_id \
         LEFT JOIN product_packs pack ON pack.product_id=p.id AND pack.status='active' \
         LEFT JOIN barcodes barcode ON barcode.pack_id=pack.id AND barcode.status='active' \
         WHERE (?='all' OR p.status=?) AND (\
           ?='' OR p.normalized_search_name LIKE ? OR b.normalized_search_name LIKE ? \
           OR company.normalized_search_name LIKE ? OR pack.sku_code LIKE upper(?) \
           OR barcode.normalized_value LIKE upper(?)) \
         ORDER BY p.normalized_search_name,p.id LIMIT 200",
    )
    .bind(&status)
    .bind(&status)
    .bind(&search)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| CatalogError::Internal)?;
    Ok(Json(rows))
}

async fn get_product(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_product_detail(&state.pool, &id).await?))
}

async fn create_product(
    State(state): State<ReferenceState>,
    Json(request): Json<CreateProductRequest>,
) -> Result<(StatusCode, Json<ProductDetailResponse>), CatalogError> {
    let CreateProductRequest {
        product,
        company_roles,
        packs,
        reason,
    } = request;
    let product = prepare_product(product)?;
    let roles = company_roles
        .into_iter()
        .map(prepare_company_role)
        .collect::<Result<Vec<_>, _>>()?;
    let packs = prepare_aggregate_packs(packs)?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let product_id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_product_row(&mut transaction, &product_id, &product, &now).await?;
    audit(
        &mut transaction,
        "product",
        &product_id,
        1,
        "created",
        reason.as_deref(),
        &product,
    )
    .await?;

    for role in &roles {
        let role_id = Uuid::now_v7().to_string();
        insert_company_role_row(&mut transaction, &role_id, &product_id, role, &now).await?;
        audit(
            &mut transaction,
            "product_company_role",
            &role_id,
            1,
            "created",
            reason.as_deref(),
            role,
        )
        .await?;
    }

    for aggregate_pack in &packs {
        insert_pack_row(
            &mut transaction,
            &aggregate_pack.id,
            &product_id,
            &aggregate_pack.pack,
            &now,
        )
        .await?;
        audit(
            &mut transaction,
            "product_pack",
            &aggregate_pack.id,
            1,
            "created",
            reason.as_deref(),
            &aggregate_pack.pack,
        )
        .await?;
        if let Some(policy) = &aggregate_pack.policy {
            let policy_id = Uuid::now_v7().to_string();
            insert_policy_row(
                &mut transaction,
                &policy_id,
                &product_id,
                &aggregate_pack.id,
                policy,
                &now,
            )
            .await?;
            audit(
                &mut transaction,
                "store_pack_policy",
                &policy_id,
                1,
                "created",
                reason.as_deref(),
                policy,
            )
            .await?;
        }
        for barcode in &aggregate_pack.barcodes {
            let barcode_id = Uuid::now_v7().to_string();
            insert_barcode_row(
                &mut transaction,
                &barcode_id,
                &aggregate_pack.id,
                barcode,
                &now,
            )
            .await?;
            audit(
                &mut transaction,
                "barcode",
                &barcode_id,
                1,
                "created",
                reason.as_deref(),
                barcode,
            )
            .await?;
        }
    }
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_product_detail(&state.pool, &product_id).await?),
    ))
}

async fn update_product(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProductRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let product = prepare_product(request.product)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current = product_state(&mut transaction, &id).await?;
    require_active_revision(&current, request.expected_revision)?;
    let current_identity: (String, i64) =
        sqlx::query_as("SELECT base_unit_id,quantity_scale FROM products WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| CatalogError::Internal)?;
    if current_identity != (product.base_unit_id.clone(), product.quantity_scale) {
        let pack_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM product_packs WHERE product_id=?")
                .bind(&id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| CatalogError::Internal)?;
        if pack_count > 0 {
            return Err(CatalogError::Conversion);
        }
    }
    let now = database_now(&mut transaction).await?;
    let next_revision = current.0 + 1;
    let result = sqlx::query(
        "UPDATE products SET revision=?,product_kind=?,brand_id=?,dosage_form_id=?,base_unit_id=?,quantity_scale=?,\
         formulation_descriptor=?,route_descriptor=?,release_descriptor=?,display_name=?,normalized_search_name=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(next_revision).bind(&product.product_kind).bind(&product.brand_id).bind(&product.dosage_form_id)
    .bind(&product.base_unit_id).bind(product.quantity_scale).bind(&product.formulation_descriptor)
    .bind(&product.route_descriptor).bind(&product.release_descriptor).bind(&product.display_name)
    .bind(normalized_search_name(&product.display_name)).bind(&now).bind(&id).bind(current.0)
    .execute(&mut *transaction).await.map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit(
        &mut transaction,
        "product",
        &id,
        next_revision,
        "updated",
        request.reason.as_deref(),
        &product,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_product_detail(&state.pool, &id).await?))
}

async fn archive_product(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    lifecycle_product(&state.pool, &id, request, false).await
}

async fn restore_product(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    lifecycle_product(&state.pool, &id, request, true).await
}

async fn lifecycle_product(
    pool: &SqlitePool,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    validate_uuid_v7(id, "id").map_err(validation_issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(validation_issue)?;
    let mut transaction = pool.begin().await.map_err(|_| CatalogError::Internal)?;
    let current = product_state(&mut transaction, id).await?;
    require_revision(&current, request.expected_revision)?;
    if restoring && current.1 != "archived" || !restoring && current.1 != "active" {
        return Err(CatalogError::Archived);
    }
    if !restoring {
        let children: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM product_packs WHERE product_id=? AND status='active') + \
             (SELECT COUNT(*) FROM product_company_roles WHERE product_id=? AND status='active')",
        )
        .bind(id)
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CatalogError::Internal)?;
        if children > 0 {
            return Err(CatalogError::Archived);
        }
    }
    let now = database_now(&mut transaction).await?;
    let next = current.0 + 1;
    if restoring {
        sqlx::query("UPDATE products SET revision=?,status='active',updated_at_utc=?,archived_at_utc=NULL,archive_reason=NULL WHERE id=? AND revision=? AND status='archived'")
            .bind(next).bind(&now).bind(id).bind(current.0).execute(&mut *transaction).await.map_err(map_database_error)?;
    } else {
        sqlx::query("UPDATE products SET revision=?,status='archived',updated_at_utc=?,archived_at_utc=?,archive_reason=? WHERE id=? AND revision=? AND status='active'")
            .bind(next).bind(&now).bind(&now).bind(&reason).bind(id).bind(current.0).execute(&mut *transaction).await.map_err(map_database_error)?;
    }
    audit(
        &mut transaction,
        "product",
        id,
        next,
        if restoring { "restored" } else { "archived" },
        Some(&reason),
        &json!({}),
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_product_detail(pool, id).await?))
}

async fn fetch_product_detail(
    pool: &SqlitePool,
    id: &str,
) -> Result<ProductDetailResponse, CatalogError> {
    let product = sqlx::query_as::<_, ProductResponse>(
        "SELECT id,revision,status,product_kind,brand_id,dosage_form_id,base_unit_id,quantity_scale,\
         formulation_descriptor,route_descriptor,release_descriptor,display_name,created_at_utc,updated_at_utc,archived_at_utc,archive_reason \
         FROM products WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)?;
    let company_roles = company_roles_for(pool, id).await?;
    let packs = packs_for(pool, id).await?;
    Ok(ProductDetailResponse {
        product,
        company_roles,
        packs,
    })
}

async fn list_company_roles(
    State(state): State<ReferenceState>,
    Path(product_id): Path<String>,
) -> Result<Json<Vec<CompanyRoleResponse>>, CatalogError> {
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    ensure_product_exists(&state.pool, &product_id).await?;
    Ok(Json(company_roles_for(&state.pool, &product_id).await?))
}

async fn create_company_role(
    State(state): State<ReferenceState>,
    Path(product_id): Path<String>,
    Json(input): Json<CompanyRoleInput>,
) -> Result<(StatusCode, Json<CompanyRoleResponse>), CatalogError> {
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    let input = prepare_company_role(input)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let product = product_state(&mut transaction, &product_id).await?;
    if product.1 != "active" {
        return Err(CatalogError::Archived);
    }
    let id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_company_role_row(&mut transaction, &id, &product_id, &input, &now).await?;
    audit(
        &mut transaction,
        "product_company_role",
        &id,
        1,
        "created",
        None,
        &input,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_company_role(&state.pool, &id).await?),
    ))
}

async fn update_company_role(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateCompanyRoleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let input = prepare_company_role(request.role)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current = entity_state(&mut transaction, "product_company_roles", &id).await?;
    require_active_revision(&current, request.expected_revision)?;
    let now = database_now(&mut transaction).await?;
    let next = current.0 + 1;
    let result = sqlx::query(
        "UPDATE product_company_roles SET revision=?,company_id=?,role=?,effective_from=?,effective_to=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    ).bind(next).bind(&input.company_id).bind(&input.role).bind(&input.effective_from).bind(&input.effective_to)
      .bind(&now).bind(&id).bind(current.0).execute(&mut *transaction).await.map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit(
        &mut transaction,
        "product_company_role",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &input,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn archive_company_role(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    lifecycle_simple(
        &state.pool,
        "product_company_roles",
        "product_company_role",
        &id,
        request,
        false,
    )
    .await?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn restore_company_role(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    lifecycle_simple(
        &state.pool,
        "product_company_roles",
        "product_company_role",
        &id,
        request,
        true,
    )
    .await?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn create_pack(
    State(state): State<ReferenceState>,
    Path(product_id): Path<String>,
    Json(input): Json<PackInput>,
) -> Result<(StatusCode, Json<PackResponse>), CatalogError> {
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    let input = prepare_pack(input)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let product = product_state(&mut transaction, &product_id).await?;
    if product.1 != "active" {
        return Err(CatalogError::Archived);
    }
    let id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_pack_row(&mut transaction, &id, &product_id, &input, &now).await?;
    audit(
        &mut transaction,
        "product_pack",
        &id,
        1,
        "created",
        None,
        &input,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_pack(&state.pool, &id).await?),
    ))
}

async fn list_packs(
    State(state): State<ReferenceState>,
    Path(product_id): Path<String>,
) -> Result<Json<Vec<PackResponse>>, CatalogError> {
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    ensure_product_exists(&state.pool, &product_id).await?;
    Ok(Json(packs_for(&state.pool, &product_id).await?))
}

async fn get_pack(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
) -> Result<Json<PackResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn update_pack(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<UpdatePackRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let input = prepare_pack(request.pack)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current = entity_state(&mut transaction, "product_packs", &id).await?;
    require_active_revision(&current, request.expected_revision)?;
    let current_conversion: (String, i64, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT container_unit_id,base_quantity_atoms,contained_pack_id,contained_pack_count FROM product_packs WHERE id=?",
    ).bind(&id).fetch_one(&mut *transaction).await.map_err(|_| CatalogError::Internal)?;
    let next_conversion = (
        input.container_unit_id.clone(),
        input.base_quantity_atoms,
        input.contained_pack_id.clone(),
        input.contained_pack_count,
    );
    if current_conversion != next_conversion {
        if is_pack_conversion_locked(&mut transaction, &id).await? {
            return Err(CatalogError::Conversion);
        }
        let active_parents: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM product_packs WHERE contained_pack_id=? AND status='active'",
        )
        .bind(&id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CatalogError::Internal)?;
        if active_parents > 0 {
            return Err(CatalogError::Conversion);
        }
        let invalid_policies: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM store_pack_policies WHERE pack_id=? AND status='active' \
             AND minimum_sale_increment_atoms > ?",
        )
        .bind(&id)
        .bind(input.base_quantity_atoms)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CatalogError::Internal)?;
        if invalid_policies > 0 {
            return Err(CatalogError::Conversion);
        }
    }
    let normalized_sku = normalize_sku(input.sku_code.as_deref()).map_err(validation_issue)?;
    let label = optional_text(input.display_label.as_deref(), "displayLabel", 200)
        .map_err(validation_issue)?;
    let now = database_now(&mut transaction).await?;
    let next = current.0 + 1;
    let result = sqlx::query(
        "UPDATE product_packs SET revision=?,container_unit_id=?,base_quantity_atoms=?,contained_pack_id=?,contained_pack_count=?,\
         sku_code=?,sku_store_id=?,display_label=?,updated_at_utc=? WHERE id=? AND revision=? AND status='active'",
    ).bind(next).bind(&input.container_unit_id).bind(input.base_quantity_atoms).bind(&input.contained_pack_id)
      .bind(input.contained_pack_count).bind(normalized_sku).bind(&input.sku_store_id).bind(label).bind(&now)
      .bind(&id).bind(current.0).execute(&mut *transaction).await.map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit(
        &mut transaction,
        "product_pack",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &input,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

/// Phase 1B has no posted commercial-history tables, so metadata alone never locks
/// conversion. Stock, purchase, sale, return, opening, and transfer slices extend
/// this policy with real posted-reference checks rather than placeholder records.
async fn is_pack_conversion_locked(
    _transaction: &mut Transaction<'_, Sqlite>,
    _pack_id: &str,
) -> Result<bool, CatalogError> {
    Ok(false)
}

async fn archive_pack(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let references: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM store_pack_policies WHERE pack_id=? AND status='active') + \
         (SELECT COUNT(*) FROM barcodes WHERE pack_id=? AND status='active') + \
         (SELECT COUNT(*) FROM product_packs WHERE contained_pack_id=? AND status='active')",
    )
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| CatalogError::Internal)?;
    if references > 0 {
        return Err(CatalogError::Archived);
    }
    drop(transaction);
    lifecycle_simple(
        &state.pool,
        "product_packs",
        "product_pack",
        &id,
        request,
        false,
    )
    .await?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn restore_pack(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    lifecycle_simple(
        &state.pool,
        "product_packs",
        "product_pack",
        &id,
        request,
        true,
    )
    .await?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn get_policy(
    State(state): State<ReferenceState>,
    Path(pack_id): Path<String>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    let policy = sqlx::query_as::<_, PolicyResponse>(
        "SELECT id,store_id,product_id,pack_id,purchase_enabled,sale_enabled,whole_pack_only_purchase,\
         fractional_sale_allowed,minimum_sale_increment_atoms,default_purchase_pack,default_sale_pack,revision,status,\
         created_at_utc,updated_at_utc,archived_at_utc,archive_reason FROM store_pack_policies \
         WHERE pack_id=? ORDER BY status='active' DESC,revision DESC LIMIT 1",
    )
    .bind(pack_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| CatalogError::Internal)?
    .ok_or(CatalogError::NotFound)?;
    Ok(Json(policy))
}

async fn put_policy(
    State(state): State<ReferenceState>,
    Path(pack_id): Path<String>,
    Json(request): Json<UpdatePolicyRequest>,
) -> Result<(StatusCode, Json<PolicyResponse>), CatalogError> {
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    let policy = prepare_policy(request.policy)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let product_id: String =
        sqlx::query_scalar("SELECT product_id FROM product_packs WHERE id=? AND status='active'")
            .bind(&pack_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| CatalogError::Internal)?
            .ok_or(CatalogError::NotFound)?;
    let existing: Option<(String, i64, String)> = sqlx::query_as(
        "SELECT id,revision,status FROM store_pack_policies WHERE store_id=? AND pack_id=? ORDER BY status='active' DESC LIMIT 1",
    ).bind(&policy.store_id).bind(&pack_id).fetch_optional(&mut *transaction).await.map_err(|_| CatalogError::Internal)?;
    let now = database_now(&mut transaction).await?;
    let (id, revision, action, status) = if let Some((id, revision, lifecycle)) = existing {
        if lifecycle != "active" {
            return Err(CatalogError::Archived);
        }
        let expected = request
            .expected_revision
            .ok_or_else(|| validation("expectedRevision", "is required for update"))?;
        require_revision(&(revision, lifecycle), expected)?;
        let next = revision + 1;
        sqlx::query(
            "UPDATE store_pack_policies SET revision=?,purchase_enabled=?,sale_enabled=?,whole_pack_only_purchase=?,\
             fractional_sale_allowed=?,minimum_sale_increment_atoms=?,default_purchase_pack=?,default_sale_pack=?,updated_at_utc=? \
             WHERE id=? AND revision=? AND status='active'",
        ).bind(next).bind(policy.purchase_enabled).bind(policy.sale_enabled).bind(policy.whole_pack_only_purchase)
          .bind(policy.fractional_sale_allowed).bind(policy.minimum_sale_increment_atoms)
          .bind(policy.default_purchase_pack).bind(policy.default_sale_pack).bind(&now).bind(&id).bind(revision)
          .execute(&mut *transaction).await.map_err(map_database_error)?;
        (id, next, "updated", StatusCode::OK)
    } else {
        if request.expected_revision.is_some() {
            return Err(validation(
                "expectedRevision",
                "must be omitted when creating a policy",
            ));
        }
        let id = Uuid::now_v7().to_string();
        insert_policy_row(&mut transaction, &id, &product_id, &pack_id, &policy, &now).await?;
        (id, 1, "created", StatusCode::CREATED)
    };
    audit(
        &mut transaction,
        "store_pack_policy",
        &id,
        revision,
        action,
        request.reason.as_deref(),
        &policy,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((status, Json(fetch_policy(&state.pool, &id).await?)))
}

async fn archive_policy(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    lifecycle_simple(
        &state.pool,
        "store_pack_policies",
        "store_pack_policy",
        &id,
        request,
        false,
    )
    .await?;
    Ok(Json(fetch_policy(&state.pool, &id).await?))
}

async fn restore_policy(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    lifecycle_simple(
        &state.pool,
        "store_pack_policies",
        "store_pack_policy",
        &id,
        request,
        true,
    )
    .await?;
    Ok(Json(fetch_policy(&state.pool, &id).await?))
}

async fn list_barcodes(
    State(state): State<ReferenceState>,
    Path(pack_id): Path<String>,
) -> Result<Json<Vec<BarcodeResponse>>, CatalogError> {
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    fetch_pack(&state.pool, &pack_id).await?;
    let rows = sqlx::query_as::<_, BarcodeResponse>(
        "SELECT id,pack_id,namespace,normalized_value,symbology,scope,store_id,revision,status,\
         created_at_utc,updated_at_utc,archived_at_utc,archive_reason FROM barcodes \
         WHERE pack_id=? ORDER BY status='active' DESC,namespace,normalized_value,id",
    )
    .bind(pack_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| CatalogError::Internal)?;
    Ok(Json(rows))
}

async fn create_barcode(
    State(state): State<ReferenceState>,
    Path(pack_id): Path<String>,
    Json(input): Json<BarcodeInput>,
) -> Result<(StatusCode, Json<BarcodeResponse>), CatalogError> {
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    let input = prepare_barcode(input)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let pack = entity_state(&mut transaction, "product_packs", &pack_id).await?;
    if pack.1 != "active" {
        return Err(CatalogError::Archived);
    }
    let id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_barcode_row(&mut transaction, &id, &pack_id, &input, &now).await?;
    audit(&mut transaction, "barcode", &id, 1, "created", None, &input).await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_barcode(&state.pool, &id).await?),
    ))
}

async fn get_barcode(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn resolve_barcode(
    State(state): State<ReferenceState>,
    Query(query): Query<BarcodeLookupQuery>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    let normalized = prepare_barcode(BarcodeInput {
        namespace: query.namespace,
        value: query.value,
        symbology: None,
        scope: query.scope,
        store_id: query.store_id,
    })?;
    let barcode = if normalized.scope == "global" {
        sqlx::query_as::<_, BarcodeResponse>(
            "SELECT id,pack_id,namespace,normalized_value,symbology,scope,store_id,revision,status,created_at_utc,updated_at_utc,\
             archived_at_utc,archive_reason FROM barcodes WHERE namespace=? AND normalized_value=? AND scope='global' AND status='active'",
        ).bind(&normalized.namespace).bind(&normalized.value).fetch_optional(&state.pool).await
    } else {
        sqlx::query_as::<_, BarcodeResponse>(
            "SELECT id,pack_id,namespace,normalized_value,symbology,scope,store_id,revision,status,created_at_utc,updated_at_utc,\
             archived_at_utc,archive_reason FROM barcodes WHERE namespace=? AND normalized_value=? AND scope='store' AND store_id=? AND status='active'",
        ).bind(&normalized.namespace).bind(&normalized.value).bind(&normalized.store_id).fetch_optional(&state.pool).await
    }.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)?;
    Ok(Json(barcode))
}

async fn archive_barcode(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    lifecycle_simple(&state.pool, "barcodes", "barcode", &id, request, false).await?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn restore_barcode(
    State(state): State<ReferenceState>,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    lifecycle_simple(&state.pool, "barcodes", "barcode", &id, request, true).await?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn duplicate_candidates(
    State(state): State<ReferenceState>,
    Json(request): Json<CreateProductRequest>,
) -> Result<Json<Vec<DuplicateCandidate>>, CatalogError> {
    let product = prepare_product(request.product)?;
    let role_companies = request
        .company_roles
        .iter()
        .map(|role| role.company_id.as_str())
        .collect::<HashSet<_>>();
    let requested_packs = request
        .packs
        .iter()
        .map(|pack| (&pack.pack.container_unit_id, pack.pack.base_quantity_atoms))
        .collect::<HashSet<_>>();
    let mut requested_barcodes = HashSet::new();
    for pack in &request.packs {
        for barcode in &pack.barcodes {
            let normalized =
                normalize_barcode(&barcode.namespace, &barcode.value).map_err(validation_issue)?;
            requested_barcodes.insert(normalized);
        }
    }
    let candidates = sqlx::query_as::<_, CandidateProduct>(
        "SELECT id,brand_id,dosage_form_id,formulation_descriptor,route_descriptor,release_descriptor,display_name \
         FROM products WHERE status='active' ORDER BY id",
    ).fetch_all(&state.pool).await.map_err(|_| CatalogError::Internal)?;
    let mut results = Vec::new();
    for candidate in candidates {
        let mut score = 0_i64;
        let mut reasons = Vec::new();
        if normalized_search_name(&candidate.display_name)
            == normalized_search_name(&product.display_name)
        {
            score += 25;
            reasons.push("display_name_match");
        }
        if product.brand_id.is_some() && product.brand_id == candidate.brand_id {
            score += 25;
            reasons.push("brand_match");
        }
        if product.dosage_form_id.is_some() && product.dosage_form_id == candidate.dosage_form_id {
            score += 15;
            reasons.push("dosage_form_match");
        }
        if same_optional_text(
            &product.formulation_descriptor,
            &candidate.formulation_descriptor,
        ) {
            score += 15;
            reasons.push("formulation_descriptor_match");
        }
        if same_optional_text(&product.route_descriptor, &candidate.route_descriptor) {
            score += 5;
            reasons.push("route_descriptor_match");
        }
        if same_optional_text(&product.release_descriptor, &candidate.release_descriptor) {
            score += 5;
            reasons.push("release_descriptor_match");
        }
        if !role_companies.is_empty() {
            let companies: Vec<String> = sqlx::query_scalar("SELECT company_id FROM product_company_roles WHERE product_id=? AND status='active'")
                .bind(&candidate.id).fetch_all(&state.pool).await.map_err(|_| CatalogError::Internal)?;
            if companies
                .iter()
                .any(|company| role_companies.contains(company.as_str()))
            {
                score += 10;
                reasons.push("company_match");
            }
        }
        if !requested_packs.is_empty() {
            let packs: Vec<(String, i64)> = sqlx::query_as("SELECT container_unit_id,base_quantity_atoms FROM product_packs WHERE product_id=? AND status='active'")
                .bind(&candidate.id).fetch_all(&state.pool).await.map_err(|_| CatalogError::Internal)?;
            if packs
                .iter()
                .any(|(unit, quantity)| requested_packs.contains(&(unit, *quantity)))
            {
                score += 10;
                reasons.push("pack_conversion_match");
            }
        }
        if !requested_barcodes.is_empty() {
            let barcodes: Vec<(String, String)> = sqlx::query_as(
                "SELECT barcode.namespace,barcode.normalized_value FROM barcodes barcode JOIN product_packs pack ON pack.id=barcode.pack_id \
                 WHERE pack.product_id=? AND barcode.status='active'",
            ).bind(&candidate.id).fetch_all(&state.pool).await.map_err(|_| CatalogError::Internal)?;
            if barcodes
                .iter()
                .any(|barcode| requested_barcodes.contains(barcode))
            {
                score += 100;
                reasons.push("barcode_match");
            }
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
struct CandidateProduct {
    id: String,
    brand_id: Option<String>,
    dosage_form_id: Option<String>,
    formulation_descriptor: Option<String>,
    route_descriptor: Option<String>,
    release_descriptor: Option<String>,
    display_name: String,
}

fn prepare_product(mut input: ProductInput) -> Result<ProductInput, CatalogError> {
    input.product_kind = input.product_kind.trim().to_ascii_lowercase();
    if !matches!(
        input.product_kind.as_str(),
        "medicine" | "device" | "general_pharmacy_item"
    ) {
        return Err(validation(
            "productKind",
            "must be medicine, device, or general_pharmacy_item",
        ));
    }
    input.brand_id = validated_optional_uuid(input.brand_id, "brandId")?;
    input.dosage_form_id = validated_optional_uuid(input.dosage_form_id, "dosageFormId")?;
    input.base_unit_id =
        validate_uuid_v7(&input.base_unit_id, "baseUnitId").map_err(validation_issue)?;
    if !(0..=MAX_QUANTITY_SCALE).contains(&input.quantity_scale) {
        return Err(validation(
            "quantityScale",
            "must be an integer from 0 through 6",
        ));
    }
    if input.product_kind == "medicine" && input.dosage_form_id.is_none() {
        return Err(validation(
            "dosageFormId",
            "is required for medicine Products",
        ));
    }
    input.formulation_descriptor = optional_text(
        input.formulation_descriptor.as_deref(),
        "formulationDescriptor",
        240,
    )
    .map_err(validation_issue)?;
    input.route_descriptor =
        optional_text(input.route_descriptor.as_deref(), "routeDescriptor", 100)
            .map_err(validation_issue)?;
    input.release_descriptor = optional_text(
        input.release_descriptor.as_deref(),
        "releaseDescriptor",
        100,
    )
    .map_err(validation_issue)?;
    input.display_name =
        required_text(&input.display_name, "displayName", 240).map_err(validation_issue)?;
    Ok(input)
}

fn prepare_company_role(mut input: CompanyRoleInput) -> Result<CompanyRoleInput, CatalogError> {
    input.company_id =
        validate_uuid_v7(&input.company_id, "companyId").map_err(validation_issue)?;
    input.role = input.role.trim().to_ascii_lowercase();
    if !matches!(
        input.role.as_str(),
        "manufacturer" | "marketer" | "brand_owner" | "importer"
    ) {
        return Err(validation("role", "has an unsupported company role"));
    }
    input.effective_from = validate_date(input.effective_from.as_deref(), "effectiveFrom")
        .map_err(validation_issue)?;
    input.effective_to =
        validate_date(input.effective_to.as_deref(), "effectiveTo").map_err(validation_issue)?;
    if let (Some(from), Some(to)) = (&input.effective_from, &input.effective_to)
        && to <= from
    {
        return Err(validation(
            "effectiveTo",
            "must be later than effectiveFrom",
        ));
    }
    Ok(input)
}

fn prepare_pack(mut input: PackInput) -> Result<PackInput, CatalogError> {
    input.container_unit_id =
        validate_uuid_v7(&input.container_unit_id, "containerUnitId").map_err(validation_issue)?;
    if !(1..=MAX_BASE_QUANTITY_ATOMS).contains(&input.base_quantity_atoms) {
        return Err(validation(
            "baseQuantityAtoms",
            "must be a positive bounded integer",
        ));
    }
    input.contained_pack_id = validated_optional_uuid(input.contained_pack_id, "containedPackId")?;
    if input.contained_pack_id.is_some() != input.contained_pack_count.is_some() {
        return Err(validation(
            "containedPackCount",
            "must be supplied exactly when containedPackId is supplied",
        ));
    }
    if input.contained_pack_count.is_some_and(|count| count <= 0) {
        return Err(validation(
            "containedPackCount",
            "must be a positive integer",
        ));
    }
    input.sku_code = normalize_sku(input.sku_code.as_deref()).map_err(validation_issue)?;
    input.sku_store_id = validated_optional_uuid(input.sku_store_id, "skuStoreId")?;
    if input.sku_code.is_some() != input.sku_store_id.is_some() {
        return Err(validation(
            "skuStoreId",
            "is required exactly when skuCode is present",
        ));
    }
    input.display_label = optional_text(input.display_label.as_deref(), "displayLabel", 200)
        .map_err(validation_issue)?;
    Ok(input)
}

fn prepare_policy(mut input: PolicyInput) -> Result<PolicyInput, CatalogError> {
    input.store_id = validate_uuid_v7(&input.store_id, "storeId").map_err(validation_issue)?;
    if !(1..=MAX_BASE_QUANTITY_ATOMS).contains(&input.minimum_sale_increment_atoms) {
        return Err(validation(
            "minimumSaleIncrementAtoms",
            "must be a positive bounded integer",
        ));
    }
    if input.default_purchase_pack && !input.purchase_enabled {
        return Err(CatalogError::DefaultPack);
    }
    if input.default_sale_pack && !input.sale_enabled {
        return Err(CatalogError::DefaultPack);
    }
    Ok(input)
}

fn prepare_barcode(mut input: BarcodeInput) -> Result<BarcodeInput, CatalogError> {
    let (namespace, normalized) =
        normalize_barcode(&input.namespace, &input.value).map_err(validation_issue)?;
    input.namespace = namespace;
    input.value = normalized;
    input.scope = input.scope.trim().to_ascii_lowercase();
    if !matches!(input.scope.as_str(), "global" | "store") {
        return Err(validation("scope", "must be global or store"));
    }
    input.store_id = validated_optional_uuid(input.store_id, "storeId")?;
    if (input.scope == "store") != input.store_id.is_some() {
        return Err(validation(
            "storeId",
            "must be present only for store-scoped barcodes",
        ));
    }
    if input.namespace == "internal" && input.scope != "store" {
        return Err(validation(
            "scope",
            "internal barcodes must be store-scoped",
        ));
    }
    input.symbology = optional_text(input.symbology.as_deref(), "symbology", 32)
        .map_err(validation_issue)?
        .map(|value| value.to_ascii_lowercase());
    Ok(input)
}

fn prepare_aggregate_packs(
    packs: Vec<AggregatePackInput>,
) -> Result<Vec<PreparedAggregatePack>, CatalogError> {
    let mut indexes = HashMap::with_capacity(packs.len());
    for (index, pack) in packs.iter().enumerate() {
        let key = normalize_client_key(&pack.client_key, "clientKey")?;
        if indexes.insert(key.clone(), index).is_some() {
            return Err(validation(
                "clientKey",
                "must be unique within the aggregate",
            ));
        }
        if pack.pack.contained_pack_id.is_some() {
            return Err(validation(
                "containedPackId",
                "aggregate containment must use containedPackClientKey",
            ));
        }
    }

    let mut children = vec![None; packs.len()];
    for (index, pack) in packs.iter().enumerate() {
        match pack.contained_pack_client_key.as_deref() {
            Some(raw_child) => {
                let child_key = normalize_client_key(raw_child, "containedPackClientKey")?;
                let child = indexes.get(&child_key).copied().ok_or_else(|| {
                    validation(
                        "containedPackClientKey",
                        "does not resolve to a Pack in this aggregate",
                    )
                })?;
                if child == index {
                    return Err(CatalogError::Conversion);
                }
                let count = pack.pack.contained_pack_count.ok_or_else(|| {
                    validation(
                        "containedPackCount",
                        "is required with containedPackClientKey",
                    )
                })?;
                if count <= 0 {
                    return Err(validation(
                        "containedPackCount",
                        "must be a positive integer",
                    ));
                }
                children[index] = Some(child);
            }
            None if pack.pack.contained_pack_count.is_some() => {
                return Err(validation(
                    "containedPackClientKey",
                    "is required with containedPackCount in an aggregate",
                ));
            }
            None => {}
        }
    }

    let mut states = vec![0_u8; packs.len()];
    let mut order = Vec::with_capacity(packs.len());
    for index in 0..packs.len() {
        visit_pack_graph(index, &children, &mut states, &mut order)?;
    }

    for (index, child) in children.iter().enumerate() {
        if let Some(child) = child {
            let count = packs[index]
                .pack
                .contained_pack_count
                .expect("validated containment count");
            let expected = packs[*child]
                .pack
                .base_quantity_atoms
                .checked_mul(count)
                .ok_or(CatalogError::Conversion)?;
            if packs[index].pack.base_quantity_atoms != expected {
                return Err(CatalogError::Conversion);
            }
        }
    }

    let ids = (0..packs.len())
        .map(|_| Uuid::now_v7().to_string())
        .collect::<Vec<_>>();
    let mut inputs = packs.into_iter().map(Some).collect::<Vec<_>>();
    let mut prepared = Vec::with_capacity(inputs.len());
    for index in order {
        let aggregate = inputs[index].take().expect("each graph node visited once");
        let mut pack = aggregate.pack;
        if let Some(child) = children[index] {
            pack.contained_pack_id = Some(ids[child].clone());
        }
        prepared.push(PreparedAggregatePack {
            id: ids[index].clone(),
            pack: prepare_pack(pack)?,
            policy: aggregate.policy.map(prepare_policy).transpose()?,
            barcodes: aggregate
                .barcodes
                .into_iter()
                .map(prepare_barcode)
                .collect::<Result<Vec<_>, _>>()?,
        });
    }
    Ok(prepared)
}

fn visit_pack_graph(
    index: usize,
    children: &[Option<usize>],
    states: &mut [u8],
    order: &mut Vec<usize>,
) -> Result<(), CatalogError> {
    match states[index] {
        2 => return Ok(()),
        1 => return Err(CatalogError::Conversion),
        _ => {}
    }
    states[index] = 1;
    if let Some(child) = children[index] {
        visit_pack_graph(child, children, states, order)?;
    }
    states[index] = 2;
    order.push(index);
    Ok(())
}

fn normalize_client_key(value: &str, field: &str) -> Result<String, CatalogError> {
    let key = required_text(value, field, 64)
        .map_err(validation_issue)?
        .to_ascii_lowercase();
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(validation(
            field,
            "may contain only ASCII letters, digits, dot, underscore, or hyphen",
        ));
    }
    Ok(key)
}

fn validated_optional_uuid(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, CatalogError> {
    value
        .map(|value| validate_uuid_v7(&value, field).map_err(validation_issue))
        .transpose()
}

fn same_optional_text(left: &Option<String>, right: &Option<String>) -> bool {
    left.as_ref()
        .zip(right.as_ref())
        .is_some_and(|(left, right)| normalized_search_name(left) == normalized_search_name(right))
}

fn validation(field: &str, message: &str) -> CatalogError {
    CatalogError::Validation(vec![CatalogValidationIssue {
        field: field.to_owned(),
        message: message.to_owned(),
    }])
}

fn validation_issue(issue: CatalogValidationIssue) -> CatalogError {
    CatalogError::Validation(vec![issue])
}

async fn insert_product_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    input: &ProductInput,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO products (id,product_kind,brand_id,dosage_form_id,base_unit_id,quantity_scale,formulation_descriptor,\
         route_descriptor,release_descriptor,display_name,normalized_search_name,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    ).bind(id).bind(&input.product_kind).bind(&input.brand_id).bind(&input.dosage_form_id).bind(&input.base_unit_id)
      .bind(input.quantity_scale).bind(&input.formulation_descriptor).bind(&input.route_descriptor).bind(&input.release_descriptor)
      .bind(&input.display_name).bind(normalized_search_name(&input.display_name)).bind(now).bind(now)
      .execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

async fn insert_company_role_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    product_id: &str,
    input: &CompanyRoleInput,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO product_company_roles (id,product_id,company_id,role,effective_from,effective_to,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?)",
    ).bind(id).bind(product_id).bind(&input.company_id).bind(&input.role).bind(&input.effective_from).bind(&input.effective_to)
      .bind(now).bind(now).execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

async fn insert_pack_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    product_id: &str,
    input: &PackInput,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,contained_pack_id,contained_pack_count,\
         sku_code,sku_store_id,display_label,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
    ).bind(id).bind(product_id).bind(&input.container_unit_id).bind(input.base_quantity_atoms).bind(&input.contained_pack_id)
      .bind(input.contained_pack_count).bind(&input.sku_code).bind(&input.sku_store_id).bind(&input.display_label).bind(now).bind(now)
      .execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

async fn insert_policy_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    product_id: &str,
    pack_id: &str,
    input: &PolicyInput,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO store_pack_policies (id,store_id,product_id,pack_id,purchase_enabled,sale_enabled,whole_pack_only_purchase,\
         fractional_sale_allowed,minimum_sale_increment_atoms,default_purchase_pack,default_sale_pack,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
    ).bind(id).bind(&input.store_id).bind(product_id).bind(pack_id).bind(input.purchase_enabled).bind(input.sale_enabled)
      .bind(input.whole_pack_only_purchase).bind(input.fractional_sale_allowed).bind(input.minimum_sale_increment_atoms)
      .bind(input.default_purchase_pack).bind(input.default_sale_pack).bind(now).bind(now)
      .execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

async fn insert_barcode_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    pack_id: &str,
    input: &BarcodeInput,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO barcodes (id,pack_id,namespace,normalized_value,symbology,scope,store_id,created_at_utc,updated_at_utc) \
         VALUES (?,?,?,?,?,?,?,?,?)",
    ).bind(id).bind(pack_id).bind(&input.namespace).bind(&input.value).bind(&input.symbology).bind(&input.scope)
      .bind(&input.store_id).bind(now).bind(now).execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

async fn fetch_company_role(
    pool: &SqlitePool,
    id: &str,
) -> Result<CompanyRoleResponse, CatalogError> {
    sqlx::query_as(
        "SELECT id,product_id,company_id,role,effective_from,effective_to,revision,status,created_at_utc,updated_at_utc,\
         archived_at_utc,archive_reason FROM product_company_roles WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)
}

async fn company_roles_for(
    pool: &SqlitePool,
    product_id: &str,
) -> Result<Vec<CompanyRoleResponse>, CatalogError> {
    sqlx::query_as(
        "SELECT id,product_id,company_id,role,effective_from,effective_to,revision,status,created_at_utc,updated_at_utc,\
         archived_at_utc,archive_reason FROM product_company_roles WHERE product_id=? ORDER BY status='active' DESC,role,id",
    ).bind(product_id).fetch_all(pool).await.map_err(|_| CatalogError::Internal)
}

async fn fetch_pack(pool: &SqlitePool, id: &str) -> Result<PackResponse, CatalogError> {
    sqlx::query_as(
        "SELECT id,revision,status,product_id,container_unit_id,base_quantity_atoms,contained_pack_id,contained_pack_count,\
         sku_code,sku_store_id,display_label,created_at_utc,updated_at_utc,archived_at_utc,archive_reason FROM product_packs WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)
}

async fn packs_for(pool: &SqlitePool, product_id: &str) -> Result<Vec<PackResponse>, CatalogError> {
    sqlx::query_as(
        "SELECT id,revision,status,product_id AS product_id,container_unit_id,base_quantity_atoms,contained_pack_id,contained_pack_count,\
         sku_code,sku_store_id,display_label,created_at_utc,updated_at_utc,archived_at_utc,archive_reason FROM product_packs WHERE product_id=?",
    ).bind(product_id).fetch_all(pool).await.map_err(|_| CatalogError::Internal)
}

async fn fetch_policy(pool: &SqlitePool, id: &str) -> Result<PolicyResponse, CatalogError> {
    sqlx::query_as(
        "SELECT id,store_id,product_id,pack_id,purchase_enabled,sale_enabled,whole_pack_only_purchase,fractional_sale_allowed,\
         minimum_sale_increment_atoms,default_purchase_pack,default_sale_pack,revision,status,created_at_utc,updated_at_utc,\
         archived_at_utc,archive_reason FROM store_pack_policies WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)
}

async fn fetch_barcode(pool: &SqlitePool, id: &str) -> Result<BarcodeResponse, CatalogError> {
    sqlx::query_as(
        "SELECT id,pack_id,namespace,normalized_value,symbology,scope,store_id,revision,status,created_at_utc,updated_at_utc,\
         archived_at_utc,archive_reason FROM barcodes WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)
}

async fn ensure_product_exists(pool: &SqlitePool, id: &str) -> Result<(), CatalogError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM products WHERE id=?)")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|_| CatalogError::Internal)?;
    if exists {
        Ok(())
    } else {
        Err(CatalogError::NotFound)
    }
}

async fn product_state(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
) -> Result<(i64, String), CatalogError> {
    entity_state(transaction, "products", id).await
}

async fn entity_state(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
) -> Result<(i64, String), CatalogError> {
    let sql = format!("SELECT revision,status FROM {table} WHERE id=?");
    sqlx::query_as(&sql)
        .bind(id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| CatalogError::Internal)?
        .ok_or(CatalogError::NotFound)
}

fn require_revision(current: &(i64, String), expected: i64) -> Result<(), CatalogError> {
    if current.0 == expected {
        Ok(())
    } else {
        Err(CatalogError::Revision {
            expected,
            current: current.0,
        })
    }
}

fn require_active_revision(current: &(i64, String), expected: i64) -> Result<(), CatalogError> {
    require_revision(current, expected)?;
    if current.1 == "active" {
        Ok(())
    } else {
        Err(CatalogError::Archived)
    }
}

async fn lifecycle_simple(
    pool: &SqlitePool,
    table: &str,
    entity_type: &str,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
) -> Result<(), CatalogError> {
    validate_uuid_v7(id, "id").map_err(validation_issue)?;
    let reason = required_text(&request.reason, "reason", 500).map_err(validation_issue)?;
    let mut transaction = pool.begin().await.map_err(|_| CatalogError::Internal)?;
    let current = entity_state(&mut transaction, table, id).await?;
    require_revision(&current, request.expected_revision)?;
    let expected_status = if restoring { "archived" } else { "active" };
    if current.1 != expected_status {
        return Err(CatalogError::Archived);
    }
    let next = current.0 + 1;
    let now = database_now(&mut transaction).await?;
    let sql = if restoring {
        format!(
            "UPDATE {table} SET revision=?,status='active',updated_at_utc=?,archived_at_utc=NULL,archive_reason=NULL WHERE id=? AND revision=? AND status='archived'"
        )
    } else {
        format!(
            "UPDATE {table} SET revision=?,status='archived',updated_at_utc=?,archived_at_utc=?,archive_reason=? WHERE id=? AND revision=? AND status='active'"
        )
    };
    let result = if restoring {
        sqlx::query(&sql)
            .bind(next)
            .bind(&now)
            .bind(id)
            .bind(current.0)
            .execute(&mut *transaction)
            .await
    } else {
        sqlx::query(&sql)
            .bind(next)
            .bind(&now)
            .bind(&now)
            .bind(&reason)
            .bind(id)
            .bind(current.0)
            .execute(&mut *transaction)
            .await
    }
    .map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit_value(
        &mut transaction,
        entity_type,
        id,
        next,
        if restoring { "restored" } else { "archived" },
        Some(&reason),
        &json!({}),
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)
}

async fn database_now(transaction: &mut Transaction<'_, Sqlite>) -> Result<String, CatalogError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| CatalogError::Internal)
}

async fn audit<T: Serialize>(
    transaction: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    reason: Option<&str>,
    payload: &T,
) -> Result<(), CatalogError> {
    let value = serde_json::to_value(payload).map_err(|_| CatalogError::Internal)?;
    audit_value(
        transaction,
        entity_type,
        entity_id,
        revision,
        action,
        reason,
        &value,
    )
    .await
}

async fn audit_value(
    transaction: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    reason: Option<&str>,
    payload: &Value,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,reason,payload_schema_version,change_payload) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?)",
    ).bind(Uuid::now_v7().to_string()).bind(entity_type).bind(entity_id).bind(revision).bind(action).bind(reason)
      .bind(payload.to_string()).execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

fn map_database_error(error: sqlx::Error) -> CatalogError {
    let message = error.to_string();
    if message.contains("pack_conversion_conflict")
        || message.contains("product_quantity_scale_conflict")
    {
        CatalogError::Conversion
    } else if message.contains("product_reference_conflict")
        || message.contains("product_company_role_reference_conflict")
    {
        validation("record", "references an inactive or missing catalog master")
    } else if message.contains("barcode")
        && (message.contains("UNIQUE") || message.contains("conflict"))
    {
        CatalogError::Barcode
    } else if message.contains("store_pack_policies") || message.contains("pack_policy_conflict") {
        CatalogError::DefaultPack
    } else if message.contains("UNIQUE constraint failed") {
        CatalogError::Duplicate
    } else if message.contains("FOREIGN KEY constraint failed")
        || message.contains("CHECK constraint failed")
    {
        validation("record", "violates a catalog integrity constraint")
    } else {
        CatalogError::Internal
    }
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
    const BOX_UNIT: &str = "01997000-0000-7000-8000-000000000005";
    const ML: &str = "01997000-0000-7000-8000-000000000011";
    const LITRE: &str = "01997000-0000-7000-8000-000000000012";

    async fn test_pool() -> (tempfile::TempDir, SqlitePool, String) {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("catalog.sqlite3"))
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
        (temp, pool, store_id)
    }

    async fn request_json(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = crate::api::router(pool, None)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
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

    fn product_fields(name: &str, unit: &str, scale: i64) -> Value {
        json!({
            "productKind":"general_pharmacy_item", "baseUnitId":unit,
            "quantityScale":scale, "displayName":name
        })
    }

    async fn create_product(pool: &SqlitePool, name: &str) -> Value {
        let (status, body) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":product_fields(name,TABLET,0)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    async fn create_pack(
        pool: &SqlitePool,
        product_id: &str,
        unit: &str,
        atoms: i64,
        contained: Option<(&str, i64)>,
    ) -> Value {
        let mut body = json!({"containerUnitId":unit,"baseQuantityAtoms":atoms});
        if let Some((id, count)) = contained {
            body["containedPackId"] = json!(id);
            body["containedPackCount"] = json!(count);
        }
        let (status, response) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
        response
    }

    async fn insert_company(pool: &SqlitePool, name: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO pharmaceutical_companies (id,display_name,normalized_search_name,created_at_utc,updated_at_utc) \
             VALUES (?,?,lower(?),strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        ).bind(&id).bind(name).bind(name).execute(pool).await.unwrap();
        id
    }

    #[tokio::test]
    async fn phase_1b_migration_is_restart_safe_and_has_exact_scope() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("migration.sqlite3");
        let pool = crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table'")
                .fetch_all(&pool)
                .await
                .unwrap();
        for expected in [
            "products",
            "product_company_roles",
            "product_packs",
            "store_pack_policies",
            "barcodes",
        ] {
            assert!(tables.iter().any(|table| table == expected));
        }
        for deferred in [
            "ingredients",
            "compositions",
            "batches",
            "stock",
            "stock_ledger",
            "prices",
        ] {
            assert!(!tables.iter().any(|table| table == deferred));
        }
        let editable_stock_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('products') WHERE lower(name) LIKE '%stock%' OR lower(name) LIKE '%quantity_on_hand%'",
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(editable_stock_columns, 0);
        pool.close().await;
        crate::infrastructure::database::connect(&path)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn product_identity_quantity_rules_and_optimistic_lifecycle_work() {
        let (_temp, pool, _store) = test_pool().await;
        let product = create_product(&pool, "Unbranded item").await;
        let id = product["id"].as_str().unwrap();
        assert_eq!(Uuid::parse_str(id).unwrap().get_version_num(), 7);
        assert!(product["brandId"].is_null());

        let (status, invalid_discrete) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({
                "product":product_fields("Invalid discrete",TABLET,1)
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(invalid_discrete["code"], "conversion_conflict");

        for (name, unit, scale) in [("Measured ml", ML, 3), ("Measured litre", LITRE, 6)] {
            let (status, _) = request_json(
                pool.clone(),
                "POST",
                "/api/v1/products",
                json!({
                    "product":product_fields(name,unit,scale)
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let (status, _) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({
                "product":product_fields("Too precise ml",ML,4)
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let update =
            json!({"expectedRevision":1,"product":product_fields("Renamed item",TABLET,0)});
        let (status, updated) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/products/{id}"),
            update.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["revision"], 2);
        let (status, stale) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/products/{id}"),
            update,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["code"], "revision_conflict");

        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{id}/archive"),
            json!({"expectedRevision":2,"reason":"Duplicate"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(archived["status"], "archived");
        let (status, restored) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{id}/restore"),
            json!({"expectedRevision":3,"reason":"Required"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["revision"], 4);
    }

    #[tokio::test]
    async fn product_references_and_company_role_conflicts_are_enforced() {
        let (_temp, pool, _store) = test_pool().await;
        let company_a = insert_company(&pool, "Maker A").await;
        let company_b = insert_company(&pool, "Marketer B").await;
        let product = create_product(&pool, "Role item").await;
        let product_id = product["id"].as_str().unwrap();
        let mut role_ids = Vec::new();
        for (company, role) in [(&company_a, "manufacturer"), (&company_b, "marketer")] {
            let (status, role_body) = request_json(
                pool.clone(),
                "POST",
                &format!("/api/v1/products/{product_id}/company-roles"),
                json!({"companyId":company,"role":role}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{role_body}");
            role_ids.push(role_body["id"].as_str().unwrap().to_owned());
        }
        let (status, duplicate) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/company-roles"),
            json!({"companyId":company_a,"role":"manufacturer"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(duplicate["code"], "duplicate_conflict");

        let update_role =
            json!({"expectedRevision":1,"role":{"companyId":company_a,"role":"importer"}});
        let (status, updated_role) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/company-roles/{}", role_ids[0]),
            update_role.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated_role}");
        assert_eq!(updated_role["revision"], 2);
        let (status, stale_role) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/company-roles/{}", role_ids[0]),
            update_role,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale_role["code"], "revision_conflict");
        let (status, _) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/company-roles/{}/archive", role_ids[0]),
            json!({"expectedRevision":2,"reason":"Inactive assignment"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, restored_role) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/company-roles/{}/restore", role_ids[0]),
            json!({"expectedRevision":3,"reason":"Restored assignment"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored_role}");

        let (status, invalid_fk) = request_json(pool.clone(), "POST", "/api/v1/products", json!({"product":{
            "productKind":"general_pharmacy_item","baseUnitId":TABLET,"brandId":Uuid::now_v7().to_string(),
            "quantityScale":0,"displayName":"Missing brand"
        }})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid_fk["code"], "validation_failed");

        let (status, invalid_unit) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":{
                "productKind":"general_pharmacy_item","baseUnitId":Uuid::now_v7().to_string(),
                "quantityScale":0,"displayName":"Missing unit"
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(invalid_unit["code"], "conversion_conflict");

        let (status, invalid_form) = request_json(pool.clone(), "POST", "/api/v1/products", json!({"product":{
            "productKind":"medicine","baseUnitId":TABLET,"dosageFormId":Uuid::now_v7().to_string(),
            "quantityScale":0,"displayName":"Missing form"
        }})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid_form["code"], "validation_failed");
    }

    #[tokio::test]
    async fn direct_pack_conversions_containment_graph_and_pack_revisions_are_safe() {
        let (_temp, pool, store_id) = test_pool().await;
        let product = create_product(&pool, "Tablet conversion item").await;
        let product_id = product["id"].as_str().unwrap();
        let tablet = create_pack(&pool, product_id, TABLET, 1, None).await;
        let tablet_id = tablet["id"].as_str().unwrap();
        assert_eq!(Uuid::parse_str(tablet_id).unwrap().get_version_num(), 7);
        let strip = create_pack(&pool, product_id, STRIP, 15, Some((tablet_id, 15))).await;
        let strip_id = strip["id"].as_str().unwrap();
        let box_pack = create_pack(&pool, product_id, BOX_UNIT, 150, Some((strip_id, 10))).await;
        let box_id = box_pack["id"].as_str().unwrap();

        let (status, parent_guard) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{tablet_id}"),
            json!({"expectedRevision":1,"pack":{
                "containerUnitId":STRIP,"baseQuantityAtoms":15
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(parent_guard["code"], "conversion_conflict");

        let (status, mismatch) = request_json(pool.clone(), "POST", &format!("/api/v1/products/{product_id}/packs"),
            json!({"containerUnitId":BOX_UNIT,"baseQuantityAtoms":149,"containedPackId":strip_id,"containedPackCount":10})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(mismatch["code"], "conversion_conflict");

        let other = create_product(&pool, "Other product").await;
        let (status, _) = request_json(pool.clone(), "POST", &format!("/api/v1/products/{}/packs", other["id"].as_str().unwrap()),
            json!({"containerUnitId":STRIP,"baseQuantityAtoms":15,"containedPackId":tablet_id,"containedPackCount":15})).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let direct_cycle = sqlx::query(
            "UPDATE product_packs SET contained_pack_id=?,contained_pack_count=150 WHERE id=?",
        )
        .bind(box_id)
        .bind(tablet_id)
        .execute(&pool)
        .await;
        assert!(direct_cycle.is_err());
        let self_containment = sqlx::query(
            "UPDATE product_packs SET contained_pack_id=id,contained_pack_count=1 WHERE id=?",
        )
        .bind(box_id)
        .execute(&pool)
        .await;
        assert!(self_containment.is_err());
        let non_positive = sqlx::query("UPDATE product_packs SET base_quantity_atoms=0 WHERE id=?")
            .bind(box_id)
            .execute(&pool)
            .await;
        assert!(non_positive.is_err());
        let non_integral = sqlx::query(
            "INSERT INTO product_packs (id,product_id,container_unit_id,base_quantity_atoms,contained_pack_id,contained_pack_count,created_at_utc,updated_at_utc) \
             VALUES (?,?,?,?,?,1.5,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        ).bind(Uuid::now_v7().to_string()).bind(product_id).bind(BOX_UNIT).bind(225_i64).bind(strip_id).execute(&pool).await;
        assert!(non_integral.is_err());

        let update = json!({"expectedRevision":1,"pack":{
            "containerUnitId":BOX_UNIT,"baseQuantityAtoms":150,"containedPackId":strip_id,"containedPackCount":10,
            "skuCode":" box-150 ","skuStoreId":store_id,"displayLabel":"Box of 10 strips"
        }});
        let (status, updated) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{box_id}"),
            update.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["skuCode"], "BOX-150");
        let (status, stale) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{box_id}"),
            update,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["code"], "revision_conflict");

        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{box_id}/archive"),
            json!({"expectedRevision":2,"reason":"Superseded pack"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        let (status, restored) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{box_id}/restore"),
            json!({"expectedRevision":3,"reason":"Pack required"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert_eq!(restored["revision"], 4);
    }

    #[tokio::test]
    async fn sku_barcode_normalization_scopes_and_lookup_are_enforced() {
        let (_temp, pool, store_id) = test_pool().await;
        let product = create_product(&pool, "Barcode item").await;
        let product_id = product["id"].as_str().unwrap();
        let first = create_pack(&pool, product_id, TABLET, 1, None).await;
        let first_id = first["id"].as_str().unwrap();
        let (status, second) = request_json(pool.clone(), "POST", &format!("/api/v1/products/{product_id}/packs"), json!({
            "containerUnitId":STRIP,"baseQuantityAtoms":15,"skuCode":" sku-15 ","skuStoreId":store_id
        })).await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        let second_id = second["id"].as_str().unwrap();
        assert_eq!(second["skuCode"], "SKU-15");

        let (status, blank) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            json!({
                "containerUnitId":BOX_UNIT,"baseQuantityAtoms":150,"skuCode":"   "
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{blank}");
        let (status, duplicate_sku) = request_json(pool.clone(), "POST", &format!("/api/v1/products/{product_id}/packs"), json!({
            "containerUnitId":BOX_UNIT,"baseQuantityAtoms":300,"skuCode":"SKU-15","skuStoreId":store_id
        })).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(duplicate_sku["code"], "duplicate_conflict");

        let (status, gtin) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{first_id}/barcodes"),
            json!({
                "namespace":"gtin","value":"8901 2345 6789 0","symbology":"ean13","scope":"global"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{gtin}");
        assert_eq!(gtin["normalizedValue"], "8901234567890");
        assert_eq!(
            Uuid::parse_str(gtin["id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );
        let barcode_id = gtin["id"].as_str().unwrap();
        let (status, exact) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/barcodes/{barcode_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(exact["packId"], first_id);
        let (status, resolved) = request_json(
            pool.clone(),
            "GET",
            "/api/v1/barcodes/resolve?namespace=gtin&value=8901234567890&scope=global",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resolved["id"], barcode_id);
        let (status, _archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/barcodes/{barcode_id}/archive"),
            json!({"expectedRevision":1,"reason":"Temporary retirement"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, historical) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/barcodes/{barcode_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(historical["status"], "archived");
        let (status, restored) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/barcodes/{barcode_id}/restore"),
            json!({"expectedRevision":2,"reason":"Correction"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");

        let (status, _) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{first_id}/barcodes"),
            json!({
                "namespace":"manufacturer","value":"ABC/42","scope":"global"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, global_conflict) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{second_id}/barcodes"),
            json!({
                "namespace":"manufacturer","value":" abc/42 ","scope":"global"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(global_conflict["code"], "barcode_conflict");
        let (status, store_barcode) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{second_id}/barcodes"),
            json!({
                "namespace":"manufacturer","value":"ABC/42","scope":"store","storeId":store_id
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{store_barcode}");
        let (status, store_conflict) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{first_id}/barcodes"),
            json!({
                "namespace":"manufacturer","value":"ABC/42","scope":"store","storeId":store_id
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(store_conflict["code"], "barcode_conflict");

        let (status, corrected) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{second_id}"),
            json!({
                "expectedRevision":1,"pack":{"containerUnitId":STRIP,"baseQuantityAtoms":30,
                    "skuCode":"SKU-15","skuStoreId":store_id}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{corrected}");
        assert_eq!(corrected["baseQuantityAtoms"], 30);
        let (status, attached) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/barcodes/{}", store_barcode["id"].as_str().unwrap()),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(attached["packId"], second_id);
    }

    #[tokio::test]
    async fn pack_policy_defaults_enabled_flags_and_fractional_rules_are_enforced() {
        let (_temp, pool, store_id) = test_pool().await;
        let product = create_product(&pool, "Policy item").await;
        let product_id = product["id"].as_str().unwrap();
        let first = create_pack(&pool, product_id, TABLET, 1, None).await;
        let second = create_pack(
            &pool,
            product_id,
            STRIP,
            15,
            Some((first["id"].as_str().unwrap(), 15)),
        )
        .await;
        let first_id = first["id"].as_str().unwrap();
        let second_id = second["id"].as_str().unwrap();
        let policy = |default_sale: bool| {
            json!({
                "storeId":store_id,"purchaseEnabled":true,"saleEnabled":true,"wholePackOnlyPurchase":true,
                "fractionalSaleAllowed":false,"minimumSaleIncrementAtoms":1,
                "defaultPurchasePack":default_sale,"defaultSalePack":default_sale
            })
        };
        let (status, created) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{first_id}/policy"),
            json!({"policy":policy(true)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let (status, conflict) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{second_id}/policy"),
            json!({"policy":policy(true)}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["code"], "default_pack_conflict");
        let (status, fractional) = request_json(pool.clone(), "PUT", &format!("/api/v1/packs/{second_id}/policy"), json!({"policy":{
            "storeId":store_id,"purchaseEnabled":true,"saleEnabled":true,"wholePackOnlyPurchase":false,
            "fractionalSaleAllowed":true,"minimumSaleIncrementAtoms":1,"defaultPurchasePack":false,"defaultSalePack":false
        }})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(fractional["code"], "default_pack_conflict");
        let (status, invalid_default) = request_json(pool.clone(), "PUT", &format!("/api/v1/packs/{second_id}/policy"), json!({"policy":{
            "storeId":store_id,"purchaseEnabled":false,"saleEnabled":false,"wholePackOnlyPurchase":false,
            "fractionalSaleAllowed":false,"minimumSaleIncrementAtoms":1,"defaultPurchasePack":true,"defaultSalePack":true
        }})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(invalid_default["code"], "default_pack_conflict");
        let (status, invalid_increment) = request_json(pool.clone(), "PUT", &format!("/api/v1/packs/{second_id}/policy"), json!({"policy":{
            "storeId":store_id,"purchaseEnabled":true,"saleEnabled":true,"wholePackOnlyPurchase":false,
            "fractionalSaleAllowed":false,"minimumSaleIncrementAtoms":16,"defaultPurchasePack":false,"defaultSalePack":false
        }})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(invalid_increment["code"], "default_pack_conflict");

        let policy_only_pack = create_pack(&pool, product_id, BOX_UNIT, 30, None).await;
        let policy_only_id = policy_only_pack["id"].as_str().unwrap();
        let (status, policy_only) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{policy_only_id}/policy"),
            json!({"policy":policy(false)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{policy_only}");
        let policy_id = policy_only["id"].as_str().unwrap();
        let (status, corrected) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{policy_only_id}"),
            json!({"expectedRevision":1,"pack":{
                "containerUnitId":BOX_UNIT,"baseQuantityAtoms":45
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{corrected}");
        let (status, retained_policy) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/packs/{policy_only_id}/policy"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(retained_policy["id"], policy_id);
        assert_eq!(retained_policy["packId"], policy_only_id);
    }

    #[tokio::test]
    async fn aggregate_create_is_atomic_and_audited() {
        let (_temp, pool, store_id) = test_pool().await;
        let company = insert_company(&pool, "Atomic Maker").await;
        let request = json!({
            "product":product_fields("Atomic product",TABLET,0),
            "companyRoles":[
                {"companyId":company,"role":"manufacturer"},
                {"companyId":company,"role":"manufacturer"}
            ],
            "packs":[{"clientKey":"base","containerUnitId":TABLET,"baseQuantityAtoms":1,
                "policy":{"storeId":store_id,"purchaseEnabled":true,"saleEnabled":true,"wholePackOnlyPurchase":false,
                    "fractionalSaleAllowed":false,"minimumSaleIncrementAtoms":1,"defaultPurchasePack":true,"defaultSalePack":true},
                "barcodes":[{"namespace":"internal","value":"ATOM-1","scope":"store","storeId":store_id}]
            }]
        });
        let (status, conflict) =
            request_json(pool.clone(), "POST", "/api/v1/products", request).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["code"], "duplicate_conflict");
        let product_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE display_name='Atomic product'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(product_count, 0);

        let (status, created) = request_json(pool.clone(), "POST", "/api/v1/products", json!({
            "product":product_fields("Atomic product",TABLET,0),
            "companyRoles":[{"companyId":company,"role":"manufacturer"}],
            "packs":[{"clientKey":"tablet","containerUnitId":TABLET,"baseQuantityAtoms":1,
                "policy":{"storeId":store_id,"purchaseEnabled":true,"saleEnabled":true,"wholePackOnlyPurchase":false,
                    "fractionalSaleAllowed":false,"minimumSaleIncrementAtoms":1,"defaultPurchasePack":true,"defaultSalePack":true},
                "barcodes":[{"namespace":"internal","value":"ATOM-1","scope":"store","storeId":store_id}]
            },{
                "clientKey":"strip","containedPackClientKey":"tablet","containerUnitId":STRIP,
                "baseQuantityAtoms":15,"containedPackCount":15
            },{
                "clientKey":"box","containedPackClientKey":"strip","containerUnitId":BOX_UNIT,
                "baseQuantityAtoms":150,"containedPackCount":10
            }]
        })).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let packs = created["packs"].as_array().unwrap();
        assert_eq!(packs.len(), 3);
        let tablet = packs
            .iter()
            .find(|pack| pack["baseQuantityAtoms"] == 1)
            .unwrap();
        let strip = packs
            .iter()
            .find(|pack| pack["baseQuantityAtoms"] == 15)
            .unwrap();
        let box_pack = packs
            .iter()
            .find(|pack| pack["baseQuantityAtoms"] == 150)
            .unwrap();
        for pack in packs {
            assert_eq!(
                Uuid::parse_str(pack["id"].as_str().unwrap())
                    .unwrap()
                    .get_version_num(),
                7
            );
        }
        assert_eq!(strip["containedPackId"], tablet["id"]);
        assert_eq!(strip["containedPackCount"], 15);
        assert_eq!(box_pack["containedPackId"], strip["id"]);
        assert_eq!(box_pack["containedPackCount"], 10);
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM master_change_events WHERE entity_type IN ('product','product_company_role','product_pack','store_pack_policy','barcode')")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(events, 7);
    }

    #[tokio::test]
    async fn invalid_aggregate_graph_barcode_and_policy_leave_no_orphans() {
        let (_temp, pool, store_id) = test_pool().await;
        let company = insert_company(&pool, "Rollback Maker").await;
        let scenarios = vec![
            json!({
                "product":product_fields("Bad conversion",TABLET,0),
                "companyRoles":[{"companyId":company,"role":"manufacturer"}],
                "packs":[
                    {"clientKey":"tablet","containerUnitId":TABLET,"baseQuantityAtoms":1},
                    {"clientKey":"strip","containedPackClientKey":"tablet","containerUnitId":STRIP,
                     "baseQuantityAtoms":14,"containedPackCount":15}
                ]
            }),
            json!({
                "product":product_fields("Bad barcode",TABLET,0),
                "companyRoles":[{"companyId":company,"role":"manufacturer"}],
                "packs":[{"clientKey":"tablet","containerUnitId":TABLET,"baseQuantityAtoms":1,
                    "barcodes":[{"namespace":"gtin","value":"8901234567894","scope":"global"}]}]
            }),
            json!({
                "product":product_fields("Bad policy",TABLET,0),
                "companyRoles":[{"companyId":company,"role":"manufacturer"}],
                "packs":[{"clientKey":"tablet","containerUnitId":TABLET,"baseQuantityAtoms":1,
                    "policy":{"storeId":store_id,"purchaseEnabled":false,"saleEnabled":false,
                        "wholePackOnlyPurchase":false,"fractionalSaleAllowed":false,
                        "minimumSaleIncrementAtoms":1,"defaultPurchasePack":true,"defaultSalePack":true}}]
            }),
            json!({
                "product":product_fields("Cyclic graph",TABLET,0),
                "companyRoles":[{"companyId":company,"role":"manufacturer"}],
                "packs":[
                    {"clientKey":"a","containedPackClientKey":"b","containerUnitId":STRIP,
                     "baseQuantityAtoms":1,"containedPackCount":1},
                    {"clientKey":"b","containedPackClientKey":"a","containerUnitId":BOX_UNIT,
                     "baseQuantityAtoms":1,"containedPackCount":1}
                ]
            }),
            json!({
                "product":product_fields("Missing child",TABLET,0),
                "companyRoles":[{"companyId":company,"role":"manufacturer"}],
                "packs":[{"clientKey":"box","containedPackClientKey":"missing","containerUnitId":BOX_UNIT,
                    "baseQuantityAtoms":10,"containedPackCount":10}]
            }),
            json!({
                "product":product_fields("Duplicate key",TABLET,0),
                "packs":[
                    {"clientKey":"same","containerUnitId":TABLET,"baseQuantityAtoms":1},
                    {"clientKey":"SAME","containerUnitId":STRIP,"baseQuantityAtoms":15}
                ]
            }),
            json!({
                "product":product_fields("Self containment",TABLET,0),
                "packs":[{"clientKey":"self","containedPackClientKey":"self","containerUnitId":BOX_UNIT,
                    "baseQuantityAtoms":1,"containedPackCount":1}]
            }),
        ];

        for scenario in scenarios {
            let (status, body) =
                request_json(pool.clone(), "POST", "/api/v1/products", scenario).await;
            assert!(!status.is_success(), "unexpected aggregate success: {body}");
            for table in [
                "products",
                "product_company_roles",
                "product_packs",
                "store_pack_policies",
                "barcodes",
                "master_change_events",
            ] {
                let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                assert_eq!(count, 0, "orphan row remained in {table}");
            }
        }
    }

    #[tokio::test]
    async fn search_and_soft_duplicate_candidates_use_available_nonclinical_signals() {
        let (_temp, pool, _store) = test_pool().await;
        let company = insert_company(&pool, "Searchable Labs").await;
        let brand_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO brands (id,display_name,normalized_search_name,brand_owner_company_id,created_at_utc,updated_at_utc) \
             VALUES (?,'SearchBrand','searchbrand',?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        ).bind(&brand_id).bind(&company).execute(&pool).await.unwrap();
        let create_request = json!({"product":{
            "productKind":"general_pharmacy_item","brandId":brand_id,"baseUnitId":TABLET,"quantityScale":0,
            "formulationDescriptor":"500 mg display text","displayName":"SearchBrand 500"
        },"companyRoles":[{"companyId":company,"role":"marketer"}]});
        let (status, first) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            create_request.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        let (status, _) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            create_request.clone(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "display equality is only a soft warning"
        );

        for term in ["searchbrand", "searchable"] {
            let (status, results) = request_json(
                pool.clone(),
                "GET",
                &format!("/api/v1/products?search={term}"),
                Value::Null,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(results.as_array().unwrap().len(), 2);
        }
        let (status, candidates) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products/duplicate-candidates",
            create_request,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(candidates.as_array().unwrap().len(), 2);
        assert!(
            candidates[0]["reasonCodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "brand_match")
        );
    }
}
