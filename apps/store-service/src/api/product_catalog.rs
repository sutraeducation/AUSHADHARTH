use std::collections::{HashMap, HashSet};

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

use super::{
    auth::{self, AuthError, AuthenticatedActor},
    reference_masters::ReferenceState,
};
use crate::domain::{
    catalog::{
        CatalogValidationIssue, MAX_BASE_QUANTITY_ATOMS, MAX_MRP_PAISE, MAX_QUANTITY_SCALE,
        normalize_barcode, normalize_batch_number, normalize_sku, normalized_search_name,
        optional_text, required_text, validate_date, validate_uuid_v7,
    },
    taxation,
};

#[derive(Debug)]
pub(crate) enum CatalogError {
    Auth(AuthError),
    Validation(Vec<CatalogValidationIssue>),
    Duplicate,
    Revision { expected: i64, current: i64 },
    NotFound,
    Archived,
    Conversion,
    Barcode,
    DefaultPack,
    Composition,
    Batch,
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

impl IntoResponse for CatalogError {
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
            Self::Composition => (
                StatusCode::CONFLICT,
                simple_error(
                    "composition_conflict",
                    "The composition conflicts with the product kind or an existing component.",
                ),
            ),
            Self::Batch => (
                StatusCode::CONFLICT,
                simple_error(
                    "batch_conflict",
                    "The batch conflicts with an existing lot or with the pack's status.",
                ),
            ),
            Self::ServiceBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorBody {
                    code: "service_busy",
                    message: "The local service is busy. Try again shortly.",
                    issues: Vec::new(),
                    expected_revision: None,
                    current_revision: None,
                },
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                simple_error("internal_error", "The operation could not be completed."),
            ),
        };
        (status, Json(body)).into_response()
    }
}

impl From<AuthError> for CatalogError {
    fn from(value: AuthError) -> Self {
        Self::Auth(value)
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
    // Phase 1F tax classification. Identity only: no rate is stored on a Product.
    hsn_code_id: Option<String>,
    tax_category_id: Option<String>,
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

/// One manufacturer-stated component of a medicine Product. Business identity only: this carries no
/// clinical, equivalence, or substitution meaning.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompositionInput {
    ingredient_id: String,
    salt_form_id: Option<String>,
    #[serde(default = "default_component_role")]
    component_role: String,
    display_order: Option<i64>,
    #[serde(default = "default_strength_presentation")]
    strength_presentation: String,
    strength_numerator_atoms: i64,
    strength_numerator_scale: i64,
    strength_numerator_unit_id: String,
    strength_denominator_atoms: Option<i64>,
    strength_denominator_scale: Option<i64>,
    strength_denominator_unit_id: Option<String>,
}

fn default_component_role() -> String {
    "active".to_owned()
}

fn default_strength_presentation() -> String {
    "absolute".to_owned()
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct CompositionResponse {
    id: String,
    revision: i64,
    status: String,
    product_id: String,
    ingredient_id: String,
    salt_form_id: Option<String>,
    component_role: String,
    display_order: i64,
    strength_presentation: String,
    strength_numerator_atoms: i64,
    strength_numerator_scale: i64,
    strength_numerator_unit_id: String,
    strength_denominator_atoms: Option<i64>,
    strength_denominator_scale: Option<i64>,
    strength_denominator_unit_id: Option<String>,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateCompositionRequest {
    expected_revision: i64,
    component: CompositionInput,
    reason: Option<String>,
}

/// One manufactured lot of one Product Pack. Identity and commercial metadata only — this carries
/// no quantity, balance, or stock figure, and recording a batch is never a stock receipt.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchInput {
    pub(crate) batch_number: String,
    pub(crate) manufactured_on: Option<String>,
    pub(crate) expires_on: Option<String>,
    pub(crate) mrp_paise: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct BatchResponse {
    id: String,
    revision: i64,
    status: String,
    product_pack_id: String,
    batch_number: String,
    normalized_batch_number: String,
    manufactured_on: Option<String>,
    expires_on: Option<String>,
    mrp_paise: Option<i64>,
    created_at_utc: String,
    updated_at_utc: String,
    archived_at_utc: Option<String>,
    archive_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBatchRequest {
    expected_revision: i64,
    batch: BatchInput,
    reason: Option<String>,
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
    composition: Vec<CompositionResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateCandidate {
    candidate_id: String,
    score: i64,
    reason_codes: Vec<&'static str>,
    explanation: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogContextResponse {
    store_id: String,
}

pub fn routes() -> Router<ReferenceState> {
    Router::new()
        .route("/api/v1/catalog/context", get(catalog_context))
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
            "/api/v1/products/{id}/tax-classification",
            get(get_tax_classification).put(put_tax_classification),
        )
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
            "/api/v1/products/{id}/composition",
            get(list_composition).post(create_composition),
        )
        .route(
            "/api/v1/composition-components/{id}",
            axum::routing::put(update_composition),
        )
        .route(
            "/api/v1/composition-components/{id}/archive",
            post(archive_composition),
        )
        .route(
            "/api/v1/composition-components/{id}/restore",
            post(restore_composition),
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
        .route(
            "/api/v1/packs/{id}/batches",
            get(list_batches).post(create_batch),
        )
        .route("/api/v1/batches/{id}", get(get_batch).put(update_batch))
        .route("/api/v1/batches/{id}/archive", post(archive_batch))
        .route("/api/v1/batches/{id}/restore", post(restore_batch))
        .route("/api/v1/barcodes/resolve", get(resolve_barcode))
        .route("/api/v1/barcodes/{id}", get(get_barcode))
        .route("/api/v1/barcodes/{id}/archive", post(archive_barcode))
        .route("/api/v1/barcodes/{id}/restore", post(restore_barcode))
}

async fn require_catalog_reader(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, CatalogError> {
    Ok(auth::require_authenticated_actor(&state.pool, headers).await?)
}

async fn require_catalog_admin(
    state: &ReferenceState,
    headers: &HeaderMap,
) -> Result<AuthenticatedActor, CatalogError> {
    auth::validate_mutation_request(headers)?;
    let actor = auth::require_authenticated_actor(&state.pool, headers).await?;
    if actor.role != "owner_admin" {
        return Err(AuthError::AuthorizationDenied.into());
    }
    Ok(actor)
}

async fn catalog_context(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
) -> Result<Json<CatalogContextResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    let store_id = sqlx::query_scalar("SELECT store_id FROM store_identity LIMIT 1")
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| CatalogError::Internal)?
        .ok_or(CatalogError::NotFound)?;
    Ok(Json(CatalogContextResponse { store_id }))
}

async fn list_products(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<ProductResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    let status = query.status.unwrap_or_else(|| "active".to_owned());
    if !matches!(status.as_str(), "active" | "archived" | "all") {
        return Err(validation("status", "must be active, archived, or all"));
    }
    let search = normalized_search_name(query.search.as_deref().unwrap_or_default());
    let pattern = format!("%{search}%");
    let rows = sqlx::query_as::<_, ProductResponse>(
        "SELECT DISTINCT p.id,p.revision,p.status,p.product_kind,p.brand_id,p.dosage_form_id,\
         p.base_unit_id,p.quantity_scale,p.formulation_descriptor,p.route_descriptor,\
         p.release_descriptor,p.display_name,p.hsn_code_id,p.tax_category_id,\
         p.created_at_utc,p.updated_at_utc,p.archived_at_utc,p.archive_reason \
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
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_product_detail(&state.pool, &id).await?))
}

async fn create_product(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreateProductRequest>,
) -> Result<(StatusCode, Json<ProductDetailResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
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
            &actor.id,
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
            &actor.id,
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
                &actor.id,
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
                &actor.id,
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
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateProductRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_product_detail(&state.pool, &id).await?))
}

async fn archive_product(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_product(&state.pool, &id, request, false, &actor.id).await
}

async fn restore_product(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<ProductDetailResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_product(&state.pool, &id, request, true, &actor.id).await
}

async fn lifecycle_product(
    pool: &SqlitePool,
    id: &str,
    request: LifecycleRequest,
    restoring: bool,
    actor_id: &str,
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
             (SELECT COUNT(*) FROM product_company_roles WHERE product_id=? AND status='active') + \
             (SELECT COUNT(*) FROM product_composition_components WHERE product_id=? AND status='active')",
        )
        .bind(id)
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
        actor_id,
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
         formulation_descriptor,route_descriptor,release_descriptor,display_name,hsn_code_id,tax_category_id,\
         created_at_utc,updated_at_utc,archived_at_utc,archive_reason \
         FROM products WHERE id=?",
    ).bind(id).fetch_optional(pool).await.map_err(|_| CatalogError::Internal)?.ok_or(CatalogError::NotFound)?;
    let company_roles = company_roles_for(pool, id).await?;
    let packs = packs_for(pool, id).await?;
    let composition = composition_for(pool, id).await?;
    Ok(ProductDetailResponse {
        product,
        company_roles,
        packs,
        composition,
    })
}

async fn list_company_roles(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(product_id): Path<String>,
) -> Result<Json<Vec<CompanyRoleResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    ensure_product_exists(&state.pool, &product_id).await?;
    Ok(Json(company_roles_for(&state.pool, &product_id).await?))
}

async fn create_company_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(product_id): Path<String>,
    Json(input): Json<CompanyRoleInput>,
) -> Result<(StatusCode, Json<CompanyRoleResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
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
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateCompanyRoleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn archive_company_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_company_roles",
        "product_company_role",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn restore_company_role(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompanyRoleResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_company_roles",
        "product_company_role",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_company_role(&state.pool, &id).await?))
}

async fn list_batches(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<Vec<BatchResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    Ok(Json(batches_for(&state.pool, &pack_id).await?))
}

async fn get_batch(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<BatchResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_batch(&state.pool, &id).await?))
}

async fn create_batch(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Json(input): Json<BatchInput>,
) -> Result<(StatusCode, Json<BatchResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&pack_id, "id").map_err(validation_issue)?;
    let (input, normalized) = prepare_batch(input)?;
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
    insert_batch_row(&mut transaction, &id, &pack_id, &input, &normalized, &now).await?;
    audit(
        &mut transaction,
        "product_batch",
        &id,
        1,
        "created",
        None,
        &input,
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_batch(&state.pool, &id).await?),
    ))
}

async fn update_batch(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateBatchRequest>,
) -> Result<Json<BatchResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let (input, normalized) = prepare_batch(request.batch)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current = entity_state(&mut transaction, "product_batches", &id).await?;
    require_active_revision(&current, request.expected_revision)?;
    let now = database_now(&mut transaction).await?;
    let next = current.0 + 1;
    let result = sqlx::query(
        "UPDATE product_batches SET revision=?,batch_number=?,normalized_batch_number=?,\
         manufactured_on=?,expires_on=?,mrp_paise=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(next)
    .bind(&input.batch_number)
    .bind(&normalized)
    .bind(&input.manufactured_on)
    .bind(&input.expires_on)
    .bind(input.mrp_paise)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit(
        &mut transaction,
        "product_batch",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &input,
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_batch(&state.pool, &id).await?))
}

async fn archive_batch(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BatchResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_batches",
        "product_batch",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_batch(&state.pool, &id).await?))
}

async fn restore_batch(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BatchResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_batches",
        "product_batch",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_batch(&state.pool, &id).await?))
}

async fn list_composition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(product_id): Path<String>,
) -> Result<Json<Vec<CompositionResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    ensure_product_exists(&state.pool, &product_id).await?;
    Ok(Json(composition_for(&state.pool, &product_id).await?))
}

async fn create_composition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(product_id): Path<String>,
    Json(input): Json<CompositionInput>,
) -> Result<(StatusCode, Json<CompositionResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    let input = prepare_composition(input)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let product = product_state(&mut transaction, &product_id).await?;
    if product.1 != "active" {
        return Err(CatalogError::Archived);
    }
    // Composition is a medicine-only concept; the database enforces this too.
    let kind: String = sqlx::query_scalar("SELECT product_kind FROM products WHERE id=?")
        .bind(&product_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| CatalogError::Internal)?;
    if kind != "medicine" {
        return Err(CatalogError::Composition);
    }
    // Appending never requires the client to compute an ordering.
    let order = match input.display_order {
        Some(order) => order,
        None => {
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(display_order), -1) + 1 FROM product_composition_components WHERE product_id=?",
            )
            .bind(&product_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| CatalogError::Internal)?
        }
    };
    let id = Uuid::now_v7().to_string();
    let now = database_now(&mut transaction).await?;
    insert_composition_row(&mut transaction, &id, &product_id, &input, order, &now).await?;
    audit(
        &mut transaction,
        "product_composition_component",
        &id,
        1,
        "created",
        None,
        &input,
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_composition(&state.pool, &id).await?),
    ))
}

async fn update_composition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateCompositionRequest>,
) -> Result<Json<CompositionResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let input = prepare_composition(request.component)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current = entity_state(&mut transaction, "product_composition_components", &id).await?;
    require_active_revision(&current, request.expected_revision)?;
    let existing_order: i64 =
        sqlx::query_scalar("SELECT display_order FROM product_composition_components WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| CatalogError::Internal)?;
    let order = input.display_order.unwrap_or(existing_order);
    let now = database_now(&mut transaction).await?;
    let next = current.0 + 1;
    let result = sqlx::query(
        "UPDATE product_composition_components SET revision=?,ingredient_id=?,salt_form_id=?,component_role=?,\
         display_order=?,strength_presentation=?,strength_numerator_atoms=?,strength_numerator_scale=?,\
         strength_numerator_unit_id=?,strength_denominator_atoms=?,strength_denominator_scale=?,\
         strength_denominator_unit_id=?,updated_at_utc=? WHERE id=? AND revision=? AND status='active'",
    )
    .bind(next)
    .bind(&input.ingredient_id)
    .bind(&input.salt_form_id)
    .bind(&input.component_role)
    .bind(order)
    .bind(&input.strength_presentation)
    .bind(input.strength_numerator_atoms)
    .bind(input.strength_numerator_scale)
    .bind(&input.strength_numerator_unit_id)
    .bind(input.strength_denominator_atoms)
    .bind(input.strength_denominator_scale)
    .bind(&input.strength_denominator_unit_id)
    .bind(&now)
    .bind(&id)
    .bind(current.0)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(CatalogError::Internal);
    }
    audit(
        &mut transaction,
        "product_composition_component",
        &id,
        next,
        "updated",
        request.reason.as_deref(),
        &input,
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok(Json(fetch_composition(&state.pool, &id).await?))
}

async fn archive_composition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompositionResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_composition_components",
        "product_composition_component",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_composition(&state.pool, &id).await?))
}

async fn restore_composition(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<CompositionResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_composition_components",
        "product_composition_component",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_composition(&state.pool, &id).await?))
}

async fn create_pack(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(product_id): Path<String>,
    Json(input): Json<PackInput>,
) -> Result<(StatusCode, Json<PackResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
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
    headers: HeaderMap,
    Path(product_id): Path<String>,
) -> Result<Json<Vec<PackResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&product_id, "id").map_err(validation_issue)?;
    ensure_product_exists(&state.pool, &product_id).await?;
    Ok(Json(packs_for(&state.pool, &product_id).await?))
}

async fn get_pack(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<PackResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn update_pack(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdatePackRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
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
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let references: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM store_pack_policies WHERE pack_id=? AND status='active') + \
         (SELECT COUNT(*) FROM barcodes WHERE pack_id=? AND status='active') + \
         (SELECT COUNT(*) FROM product_packs WHERE contained_pack_id=? AND status='active') + \
         (SELECT COUNT(*) FROM product_batches WHERE product_pack_id=? AND status='active')",
    )
    .bind(&id)
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
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn restore_pack(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PackResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "product_packs",
        "product_pack",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_pack(&state.pool, &id).await?))
}

async fn get_policy(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
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
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Json(request): Json<UpdatePolicyRequest>,
) -> Result<(StatusCode, Json<PolicyResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((status, Json(fetch_policy(&state.pool, &id).await?)))
}

async fn archive_policy(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "store_pack_policies",
        "store_pack_policy",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_policy(&state.pool, &id).await?))
}

async fn restore_policy(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<PolicyResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "store_pack_policies",
        "store_pack_policy",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_policy(&state.pool, &id).await?))
}

async fn list_barcodes(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(pack_id): Path<String>,
) -> Result<Json<Vec<BarcodeResponse>>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
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
    headers: HeaderMap,
    Path(pack_id): Path<String>,
    Json(input): Json<BarcodeInput>,
) -> Result<(StatusCode, Json<BarcodeResponse>), CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
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
    audit(
        &mut transaction,
        "barcode",
        &id,
        1,
        "created",
        None,
        &input,
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(fetch_barcode(&state.pool, &id).await?),
    ))
}

async fn get_barcode(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn resolve_barcode(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Query(query): Query<BarcodeLookupQuery>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
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
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "barcodes",
        "barcode",
        &id,
        request,
        false,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn restore_barcode(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<LifecycleRequest>,
) -> Result<Json<BarcodeResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    lifecycle_simple(
        &state.pool,
        "barcodes",
        "barcode",
        &id,
        request,
        true,
        &actor.id,
    )
    .await?;
    Ok(Json(fetch_barcode(&state.pool, &id).await?))
}

async fn duplicate_candidates(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Json(request): Json<CreateProductRequest>,
) -> Result<Json<Vec<DuplicateCandidate>>, CatalogError> {
    auth::validate_mutation_request(&headers)?;
    require_catalog_reader(&state, &headers).await?;
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

/// Validates one batch. Dates stay calendar-domain values that are really parsed, and MRP stays an
/// exact integer paise value — binary floating point is never accepted.
pub(crate) fn prepare_batch(mut input: BatchInput) -> Result<(BatchInput, String), CatalogError> {
    let (display, normalized) =
        normalize_batch_number(&input.batch_number).map_err(validation_issue)?;
    input.batch_number = display;
    input.manufactured_on = validate_date(input.manufactured_on.as_deref(), "manufacturedOn")
        .map_err(validation_issue)?;
    input.expires_on =
        validate_date(input.expires_on.as_deref(), "expiresOn").map_err(validation_issue)?;
    // An already-expired lot is historical fact and stays acceptable; only an impossible ordering is
    // rejected.
    if let (Some(manufactured), Some(expires)) = (&input.manufactured_on, &input.expires_on)
        && expires < manufactured
    {
        return Err(validation(
            "expiresOn",
            "must not be earlier than manufacturedOn",
        ));
    }
    if let Some(mrp) = input.mrp_paise
        && !(1..=MAX_MRP_PAISE).contains(&mrp)
    {
        return Err(validation(
            "mrpPaise",
            "must be a positive amount in paise within the supported range",
        ));
    }
    Ok((input, normalized))
}

/// Validates one composition component. Strength stays exact integer atoms with an explicit scale;
/// no binary floating point is ever accepted or produced.
fn prepare_composition(mut input: CompositionInput) -> Result<CompositionInput, CatalogError> {
    input.ingredient_id =
        validate_uuid_v7(&input.ingredient_id, "ingredientId").map_err(validation_issue)?;
    input.salt_form_id = validated_optional_uuid(input.salt_form_id, "saltFormId")?;
    if !matches!(input.component_role.as_str(), "active" | "inactive") {
        return Err(validation("componentRole", "must be active or inactive"));
    }
    if !matches!(
        input.strength_presentation.as_str(),
        "absolute" | "percentage"
    ) {
        return Err(validation(
            "strengthPresentation",
            "must be absolute or percentage",
        ));
    }
    if input.display_order.is_some_and(|order| order < 0) {
        return Err(validation("displayOrder", "must not be negative"));
    }
    if !(1..=MAX_BASE_QUANTITY_ATOMS).contains(&input.strength_numerator_atoms) {
        return Err(validation(
            "strengthNumeratorAtoms",
            "must be a positive bounded integer",
        ));
    }
    if !(0..=6).contains(&input.strength_numerator_scale) {
        return Err(validation(
            "strengthNumeratorScale",
            "must be between 0 and 6",
        ));
    }
    input.strength_numerator_unit_id =
        validate_uuid_v7(&input.strength_numerator_unit_id, "strengthNumeratorUnitId")
            .map_err(validation_issue)?;
    // A concentration is all three denominator parts or none of them; a half-specified denominator
    // has no meaning.
    let denominator_parts = [
        input.strength_denominator_atoms.is_some(),
        input.strength_denominator_scale.is_some(),
        input.strength_denominator_unit_id.is_some(),
    ];
    if denominator_parts.iter().any(|part| *part) && !denominator_parts.iter().all(|part| *part) {
        return Err(validation(
            "strengthDenominatorUnitId",
            "requires the denominator quantity, scale, and unit together",
        ));
    }
    if let Some(atoms) = input.strength_denominator_atoms
        && !(1..=MAX_BASE_QUANTITY_ATOMS).contains(&atoms)
    {
        return Err(validation(
            "strengthDenominatorAtoms",
            "must be a positive bounded integer",
        ));
    }
    if let Some(scale) = input.strength_denominator_scale
        && !(0..=6).contains(&scale)
    {
        return Err(validation(
            "strengthDenominatorScale",
            "must be between 0 and 6",
        ));
    }
    input.strength_denominator_unit_id = validated_optional_uuid(
        input.strength_denominator_unit_id,
        "strengthDenominatorUnitId",
    )?;
    // A percentage is an exact ratio out of one hundred, so the stored value stays exact.
    if input.strength_presentation == "percentage"
        && (input.strength_denominator_atoms != Some(100)
            || input.strength_denominator_scale != Some(0))
    {
        return Err(validation(
            "strengthDenominatorAtoms",
            "a percentage strength must be expressed per exactly 100",
        ));
    }
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

const BATCH_COLUMNS: &str = "id,revision,status,product_pack_id,batch_number,normalized_batch_number,manufactured_on,\
     expires_on,mrp_paise,created_at_utc,updated_at_utc,archived_at_utc,archive_reason";

pub(crate) async fn insert_batch_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    pack_id: &str,
    input: &BatchInput,
    normalized: &str,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO product_batches (id,product_pack_id,batch_number,normalized_batch_number,\
         manufactured_on,expires_on,mrp_paise,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(pack_id)
    .bind(&input.batch_number)
    .bind(normalized)
    .bind(&input.manufactured_on)
    .bind(&input.expires_on)
    .bind(input.mrp_paise)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn fetch_batch(pool: &SqlitePool, id: &str) -> Result<BatchResponse, CatalogError> {
    sqlx::query_as::<_, BatchResponse>(&format!(
        "SELECT {BATCH_COLUMNS} FROM product_batches WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| CatalogError::Internal)?
    .ok_or(CatalogError::NotFound)
}

/// Soonest expiry first with undated lots last. Presentation ordering only — this is not a FEFO
/// issue policy, which belongs to the deferred sales phase.
async fn batches_for(pool: &SqlitePool, pack_id: &str) -> Result<Vec<BatchResponse>, CatalogError> {
    sqlx::query_as::<_, BatchResponse>(&format!(
        "SELECT {BATCH_COLUMNS} FROM product_batches WHERE product_pack_id=? \
         ORDER BY expires_on IS NULL,expires_on,normalized_batch_number"
    ))
    .bind(pack_id)
    .fetch_all(pool)
    .await
    .map_err(|_| CatalogError::Internal)
}

const COMPOSITION_COLUMNS: &str = "id,revision,status,product_id,ingredient_id,salt_form_id,component_role,display_order,\
     strength_presentation,strength_numerator_atoms,strength_numerator_scale,strength_numerator_unit_id,\
     strength_denominator_atoms,strength_denominator_scale,strength_denominator_unit_id,\
     created_at_utc,updated_at_utc,archived_at_utc,archive_reason";

async fn insert_composition_row(
    transaction: &mut Transaction<'_, Sqlite>,
    id: &str,
    product_id: &str,
    input: &CompositionInput,
    display_order: i64,
    now: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO product_composition_components (id,product_id,ingredient_id,salt_form_id,component_role,\
         display_order,strength_presentation,strength_numerator_atoms,strength_numerator_scale,\
         strength_numerator_unit_id,strength_denominator_atoms,strength_denominator_scale,\
         strength_denominator_unit_id,created_at_utc,updated_at_utc) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(id)
    .bind(product_id)
    .bind(&input.ingredient_id)
    .bind(&input.salt_form_id)
    .bind(&input.component_role)
    .bind(display_order)
    .bind(&input.strength_presentation)
    .bind(input.strength_numerator_atoms)
    .bind(input.strength_numerator_scale)
    .bind(&input.strength_numerator_unit_id)
    .bind(input.strength_denominator_atoms)
    .bind(input.strength_denominator_scale)
    .bind(&input.strength_denominator_unit_id)
    .bind(now)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

async fn fetch_composition(
    pool: &SqlitePool,
    id: &str,
) -> Result<CompositionResponse, CatalogError> {
    sqlx::query_as::<_, CompositionResponse>(&format!(
        "SELECT {COMPOSITION_COLUMNS} FROM product_composition_components WHERE id=?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|_| CatalogError::Internal)?
    .ok_or(CatalogError::NotFound)
}

/// Deterministic ordering even when two components share a display order.
async fn composition_for(
    pool: &SqlitePool,
    product_id: &str,
) -> Result<Vec<CompositionResponse>, CatalogError> {
    sqlx::query_as::<_, CompositionResponse>(&format!(
        "SELECT {COMPOSITION_COLUMNS} FROM product_composition_components WHERE product_id=? \
         ORDER BY display_order,id"
    ))
    .bind(product_id)
    .fetch_all(pool)
    .await
    .map_err(|_| CatalogError::Internal)
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
    actor_id: &str,
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
        actor_id,
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

// The explicit audit coordinates keep every call site reviewable and prevent a
// partially populated event from crossing this accounting boundary.
#[allow(clippy::too_many_arguments)]
async fn audit<T: Serialize>(
    transaction: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    reason: Option<&str>,
    payload: &T,
    actor_id: &str,
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
        actor_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn audit_value(
    transaction: &mut Transaction<'_, Sqlite>,
    entity_type: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    reason: Option<&str>,
    payload: &Value,
    actor_id: &str,
) -> Result<(), CatalogError> {
    sqlx::query(
        "INSERT INTO master_change_events (event_id,entity_type,entity_id,entity_revision,action,occurred_at_utc,reason,payload_schema_version,change_payload,actor_id) \
         VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'),?,1,?,?)",
    ).bind(Uuid::now_v7().to_string()).bind(entity_type).bind(entity_id).bind(revision).bind(action).bind(reason)
      .bind(payload.to_string()).bind(actor_id).execute(&mut **transaction).await.map_err(map_database_error)?;
    Ok(())
}

fn map_database_error(error: sqlx::Error) -> CatalogError {
    let is_contention = match &error {
        sqlx::Error::Database(database) => {
            matches!(
                database.code().as_deref(),
                Some("5" | "6" | "261" | "262" | "517")
            ) || {
                let message = database.message().to_ascii_lowercase();
                message.contains("database is locked")
                    || message.contains("database table is locked")
                    || message.contains("database is busy")
            }
        }
        _ => false,
    };
    if is_contention {
        return CatalogError::ServiceBusy;
    }
    let message = error.to_string();
    if message.contains("composition_component_conflict")
        || message.contains("product_composition_components_active_uq")
    {
        return CatalogError::Composition;
    }
    // A duplicate lot is a duplicate like any other, and maps to the frozen `duplicate_conflict`.
    // `batch_conflict` is reserved for Pack/Product state violations raised by the trigger.
    if message.contains("product_batch_conflict") {
        return CatalogError::Batch;
    }
    // The tax-classification trigger is the database-level backstop for an inactive or missing
    // reference; the service checks first so the client gets the more precise message.
    if message.contains("product_tax_conflict") {
        return CatalogError::Archived;
    }
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

// ---------------------------------------------------------------------------------------------
// Phase 1F — Product tax classification
//
// A Product identifies its HSN and Tax Category. It never stores a rate: the rate in force is
// resolved from tax_rate_versions by date, and a future posted document snapshots what it applied.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaxClassificationQuery {
    as_of: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateTaxClassificationRequest {
    expected_revision: i64,
    hsn_code_id: Option<String>,
    tax_category_id: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplicableRateResponse {
    tax_rate_version_id: String,
    effective_from: String,
    effective_to: Option<String>,
    cgst_basis_points: i64,
    sgst_basis_points: i64,
    igst_basis_points: i64,
    cess_basis_points: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaxClassificationResponse {
    product_id: String,
    revision: i64,
    hsn_code_id: Option<String>,
    tax_category_id: Option<String>,
    /// True once the Product can be taxed on a document. Optional in this phase; Phase 1G is where
    /// an incomplete classification actually blocks something.
    complete: bool,
    /// The date the rate below was resolved for, echoed so a caller can never mistake the rate for
    /// permanent Product metadata.
    as_of: String,
    applicable_rate: Option<ApplicableRateResponse>,
}

async fn get_tax_classification(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TaxClassificationQuery>,
) -> Result<Json<TaxClassificationResponse>, CatalogError> {
    require_catalog_reader(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let as_of = match query
        .as_of
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => taxation::validate_calendar_date(value)
            .map_err(|_| validation("asOf", "must be a valid YYYY-MM-DD date"))?,
        None => business_today(&state.pool).await?,
    };
    fetch_tax_classification(&state.pool, &id, &as_of)
        .await
        .map(Json)
}

async fn put_tax_classification(
    State(state): State<ReferenceState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<UpdateTaxClassificationRequest>,
) -> Result<Json<TaxClassificationResponse>, CatalogError> {
    let actor = require_catalog_admin(&state, &headers).await?;
    validate_uuid_v7(&id, "id").map_err(validation_issue)?;
    let hsn_code_id = validated_optional_uuid(request.hsn_code_id, "hsnCodeId")?;
    let tax_category_id = validated_optional_uuid(request.tax_category_id, "taxCategoryId")?;
    let reason =
        optional_text(request.reason.as_deref(), "reason", 500).map_err(validation_issue)?;

    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| CatalogError::Internal)?;
    let current: Option<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT revision,status,hsn_code_id,tax_category_id FROM products WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(map_database_error)?;
    let (revision, status, previous_hsn, previous_category) =
        current.ok_or(CatalogError::NotFound)?;
    if revision != request.expected_revision {
        return Err(CatalogError::Revision {
            expected: request.expected_revision,
            current: revision,
        });
    }
    if status != "active" {
        return Err(CatalogError::Archived);
    }

    // Only a reference that actually changes is validated, matching the trigger. Re-checking an
    // unchanged value would trap a Product whose assigned master was archived after the fact.
    if hsn_code_id != previous_hsn
        && let Some(candidate) = hsn_code_id.as_deref()
    {
        require_assignable(&mut transaction, "hsn_codes", candidate, "hsnCodeId").await?;
    }
    if tax_category_id != previous_category
        && let Some(candidate) = tax_category_id.as_deref()
    {
        require_assignable(
            &mut transaction,
            "tax_categories",
            candidate,
            "taxCategoryId",
        )
        .await?;
    }

    let next = revision + 1;
    let now = database_now(&mut transaction).await?;
    sqlx::query(
        "UPDATE products SET hsn_code_id=?,tax_category_id=?,revision=?,updated_at_utc=? \
         WHERE id=? AND revision=? AND status='active'",
    )
    .bind(&hsn_code_id)
    .bind(&tax_category_id)
    .bind(next)
    .bind(&now)
    .bind(&id)
    .bind(revision)
    .execute(&mut *transaction)
    .await
    .map_err(map_database_error)?;

    // The audit answers which Product, from what, to what, by whom, and when. It is deliberately
    // not a tax snapshot: no rate is recorded here, because no rate was decided here.
    audit_value(
        &mut transaction,
        "product",
        &id,
        next,
        "updated",
        reason.as_deref(),
        &json!({
            "change": "tax_classification",
            "previous": { "hsnCodeId": previous_hsn, "taxCategoryId": previous_category },
            "next": { "hsnCodeId": hsn_code_id, "taxCategoryId": tax_category_id },
        }),
        &actor.id,
    )
    .await?;
    transaction.commit().await.map_err(map_database_error)?;

    let as_of = business_today(&state.pool).await?;
    fetch_tax_classification(&state.pool, &id, &as_of)
        .await
        .map(Json)
}

/// A newly assigned reference must exist and be active.
///
/// Missing and archived are reported differently because they are different mistakes: one is a bad
/// identifier, the other a deliberate lifecycle state the operator can see and undo.
async fn require_assignable(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &str,
    id: &str,
    field: &str,
) -> Result<(), CatalogError> {
    let status: Option<String> =
        sqlx::query_scalar(&format!("SELECT status FROM {table} WHERE id=?"))
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(map_database_error)?;
    match status.as_deref() {
        Some("active") => Ok(()),
        Some(_) => Err(CatalogError::Archived),
        None => Err(validation(field, "references a master that does not exist")),
    }
}

/// The fallback date used when a caller supplies none: the service's own clock, which is UTC.
///
/// This is only ever a *display* default. `asOf` selects which historical rate is shown and is
/// never persisted, so a caller choosing a date is a feature rather than a risk — the UI passes
/// the workstation's business date, because UTC runs a day behind India for the first hours of
/// each business day.
async fn business_today(pool: &SqlitePool) -> Result<String, CatalogError> {
    sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
        .fetch_one(pool)
        .await
        .map_err(map_database_error)
}

async fn fetch_tax_classification(
    pool: &SqlitePool,
    id: &str,
    as_of: &str,
) -> Result<TaxClassificationResponse, CatalogError> {
    let row: Option<(i64, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT revision,hsn_code_id,tax_category_id FROM products WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(map_database_error)?;
    let (revision, hsn_code_id, tax_category_id) = row.ok_or(CatalogError::NotFound)?;
    let applicable_rate = match tax_category_id.as_deref() {
        Some(category) => taxation::resolve_tax_rate(pool, category, as_of)
            .await
            .map_err(|error| match error {
                taxation::TaxResolutionError::InvalidDate => {
                    validation("asOf", "must be a valid YYYY-MM-DD date")
                }
                taxation::TaxResolutionError::Database(_) => CatalogError::Internal,
            })?
            .map(|rate| ApplicableRateResponse {
                tax_rate_version_id: rate.id,
                effective_from: rate.effective_from,
                effective_to: rate.effective_to,
                cgst_basis_points: rate.cgst_basis_points,
                sgst_basis_points: rate.sgst_basis_points,
                igst_basis_points: rate.igst_basis_points,
                cess_basis_points: rate.cess_basis_points,
            }),
        None => None,
    };
    Ok(TaxClassificationResponse {
        product_id: id.to_owned(),
        revision,
        complete: hsn_code_id.is_some() && tax_category_id.is_some(),
        hsn_code_id,
        tax_category_id,
        as_of: as_of.to_owned(),
        applicable_rate,
    })
}
#[cfg(test)]
mod tests {
    use std::time::{Duration as StdDuration, Instant};

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
    const OWNER_TOKEN: &str = "catalog-owner-session-token";
    /// Worst documented contention path is two sequential busy timeouts; the third allows for test
    /// scheduling on a loaded machine without letting a deadlock or retry loop pass.
    const CONTENTION_CEILING: StdDuration =
        crate::infrastructure::database::BUSY_TIMEOUT.saturating_mul(3);

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
        insert_session(&pool, "owner_admin", OWNER_TOKEN).await;
        (temp, pool, store_id)
    }

    /// A contended write must finish on the configured busy timeout, report the typed `service_busy`
    /// code with no database detail, leave nothing behind, and leave the pool usable.
    ///
    /// The wall-clock ceiling is derived from `BUSY_TIMEOUT` rather than written as a literal. A
    /// request may wait through two busy timeouts in sequence, because the session touch in
    /// `required_session` serialises behind the process-wide auth write lock and the holder of that
    /// lock may itself be inside a busy wait; `CONTENTION_CEILING` therefore allows that documented
    /// worst case plus scheduling margin, and only a deadlock or an unbounded retry can exceed it.
    #[tokio::test]
    async fn catalog_database_contention_is_bounded_and_maps_to_service_busy() {
        let (_temp, pool, _) = test_pool().await;
        let mut blocker = pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let started = Instant::now();
        let response = tokio::time::timeout(
            CONTENTION_CEILING,
            request_json(
                pool.clone(),
                "POST",
                "/api/v1/products",
                json!({"product":product_fields("Busy Product",TABLET,0)}),
            ),
        )
        .await
        .expect("catalog contention must remain bounded");
        let waited = started.elapsed();
        sqlx::query("ROLLBACK")
            .execute(&mut *blocker)
            .await
            .unwrap();
        assert_eq!(response.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.1["code"], "service_busy");
        assert_no_database_detail(&response.1);
        // The request really did wait on the contended lock rather than failing early for an
        // unrelated reason. Scheduling can only make this slower, never faster.
        assert!(
            waited >= crate::infrastructure::database::BUSY_TIMEOUT / 2,
            "contended write returned after {waited:?} without waiting on the lock"
        );

        // The abandoned attempt wrote nothing and poisoned no connection: once the lock is released
        // the identical request succeeds and exactly one Product exists.
        let (status, retried) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":product_fields("Busy Product",TABLET,0)}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{retried}");
        let products: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            products, 1,
            "the busy attempt must not have replayed a write"
        );
    }

    /// The typed `code` is the only machine detail a client may see; the human-facing text must not
    /// carry SQLite, sqlx, or Rust internals.
    fn assert_no_database_detail(body: &Value) {
        let rendered = format!("{} {}", body["message"], body["issues"]).to_lowercase();
        for leak in ["sqlite", "locked", "sqlx", "panicked", "error code"] {
            assert!(
                !rendered.contains(leak),
                "contention response leaked {leak:?}: {body}"
            );
        }
    }

    async fn insert_session(pool: &SqlitePool, role: &str, token: &str) -> String {
        let user_id = Uuid::now_v7().to_string();
        let login = format!("{role}-{}", &user_id[0..8]);
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
            // Phase 1C-B composition identity.
            "ingredients",
            "salt_forms",
            "strength_units",
            "product_composition_components",
        ] {
            assert!(tables.iter().any(|table| table == expected));
        }
        for deferred in ["batches", "stock", "stock_ledger", "prices"] {
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

        // A SKU is Store-scoped: sku_store_id must be supplied exactly when sku_code is. A client
        // that sends a SKU without its Store is rejected on create and on update alike.
        let (status, unscoped) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            json!({
                "containerUnitId":BOX_UNIT,"baseQuantityAtoms":150,"skuCode":"SKU-UNSCOPED","skuStoreId":Value::Null
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{unscoped}");
        assert_eq!(unscoped["code"], "validation_failed");
        assert_eq!(unscoped["issues"][0]["field"], "skuStoreId");
        let (status, unscoped_update) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/packs/{second_id}"),
            json!({
                "expectedRevision":1,
                "pack":{"containerUnitId":STRIP,"baseQuantityAtoms":15,"skuCode":"SKU-15","skuStoreId":Value::Null}
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{unscoped_update}"
        );
        assert_eq!(unscoped_update["issues"][0]["field"], "skuStoreId");
        // A Store without a SKU is equally invalid: the pair is all-or-nothing.
        let (status, orphan_store) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            json!({
                "containerUnitId":BOX_UNIT,"baseQuantityAtoms":150,"skuStoreId":store_id
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{orphan_store}");
        assert_eq!(orphan_store["issues"][0]["field"], "skuStoreId");
        // Whitespace normalizes away to no SKU, so it must arrive with no Store either.
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
        assert_eq!(blank["skuCode"], Value::Null);
        assert_eq!(blank["skuStoreId"], Value::Null);
        let (status, blank_with_store) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product_id}/packs"),
            json!({
                "containerUnitId":BOX_UNIT,"baseQuantityAtoms":300,"skuCode":"   ","skuStoreId":store_id
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{blank_with_store}"
        );
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

    #[tokio::test]
    async fn catalog_access_roles_context_and_server_audit_actor_are_enforced() {
        let (_temp, pool, store_id) = test_pool().await;
        let (status, _) =
            request_json_as(pool.clone(), "GET", "/api/v1/products", Value::Null, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        for (role, token) in [
            ("pharmacist", "catalog-pharmacist-token"),
            ("cashier", "catalog-cashier-token"),
        ] {
            insert_session(&pool, role, token).await;
            let (read_status, _) = request_json_as(
                pool.clone(),
                "GET",
                "/api/v1/products",
                Value::Null,
                Some(token),
            )
            .await;
            assert_eq!(read_status, StatusCode::OK);
            let (write_status, body) = request_json_as(
                pool.clone(),
                "POST",
                "/api/v1/products",
                json!({"product":product_fields(&format!("{role} product"),TABLET,0)}),
                Some(token),
            )
            .await;
            assert_eq!(write_status, StatusCode::FORBIDDEN, "{body}");
        }

        let (context_status, context) =
            request_json(pool.clone(), "GET", "/api/v1/catalog/context", Value::Null).await;
        assert_eq!(context_status, StatusCode::OK);
        assert_eq!(context["storeId"], store_id);

        let spoofed_actor = Uuid::now_v7().to_string();
        let (create_status, product) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({
                "actorId": spoofed_actor,
                "product": product_fields("Authenticated catalog product", TABLET, 0),
                "packs": [{"clientKey":"base","containerUnitId":TABLET,"baseQuantityAtoms":1}]
            }),
        )
        .await;
        assert_eq!(create_status, StatusCode::CREATED, "{product}");
        let product_id = product["id"].as_str().unwrap();
        let authoritative_actor: String = sqlx::query_scalar(
            "SELECT actor_id FROM master_change_events WHERE entity_type='product' AND entity_id=?",
        )
        .bind(product_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let owner_id: String =
            sqlx::query_scalar("SELECT id FROM users WHERE role='owner_admin' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(authoritative_actor, owner_id);
        assert_ne!(authoritative_actor, spoofed_actor);
    }

    // ---- Phase 1C-C batch identity ----

    async fn add_batch(pool: &SqlitePool, pack: &str, body: Value) -> (StatusCode, Value) {
        request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{pack}/batches"),
            body,
        )
        .await
    }

    #[tokio::test]
    async fn batch_identity_is_scoped_to_its_pack_and_preserves_the_printed_form() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_product(&pool, "Batched item").await;
        let product_id = product["id"].as_str().unwrap();
        let strip = create_pack(&pool, product_id, STRIP, 15, None).await;
        let strip_id = strip["id"].as_str().unwrap().to_owned();
        let box_pack = create_pack(&pool, product_id, BOX_UNIT, 150, None).await;
        let box_id = box_pack["id"].as_str().unwrap().to_owned();

        let (status, created) = add_batch(
            &pool,
            &strip_id,
            json!({"batchNumber":" ab-123 ","manufacturedOn":"2026-01-01","expiresOn":"2028-01-31","mrpPaise":12550}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        // The printed form survives; the normalized form is what uniqueness compares.
        assert_eq!(created["batchNumber"], "ab-123");
        assert_eq!(created["normalizedBatchNumber"], "AB-123");
        assert_eq!(created["mrpPaise"], 12550);
        assert_eq!(
            Uuid::parse_str(created["id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );

        // Case and whitespace never create a second lot on the same Pack.
        let (status, duplicate) =
            add_batch(&pool, &strip_id, json!({"batchNumber":"  Ab - 123 "})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
        assert_eq!(duplicate["code"], "duplicate_conflict");

        // Manufacturers reuse lot strings, so the same string on another Pack is legitimate.
        let (status, other_pack) = add_batch(&pool, &box_id, json!({"batchNumber":"AB-123"})).await;
        assert_eq!(status, StatusCode::CREATED, "{other_pack}");

        // A batch may not be created against an archived Pack.
        let (status, archived_pack) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/packs/{box_id}/archive"),
            json!({"expectedRevision":1,"reason":"Withdrawn presentation"}),
        )
        .await;
        // The Pack still carries an active batch, so archiving it is refused.
        assert_eq!(status, StatusCode::CONFLICT, "{archived_pack}");
        assert_eq!(archived_pack["code"], "archived_conflict");
    }

    #[tokio::test]
    async fn batch_dates_and_mrp_stay_exact_and_calendar_only() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_product(&pool, "Dated item").await;
        let pack = create_pack(&pool, product["id"].as_str().unwrap(), STRIP, 15, None).await;
        let pack_id = pack["id"].as_str().unwrap().to_owned();

        // Both dates absent is legitimate for a non-expiring general item.
        let (status, undated) = add_batch(&pool, &pack_id, json!({"batchNumber":"NODATE"})).await;
        assert_eq!(status, StatusCode::CREATED, "{undated}");
        assert_eq!(undated["manufacturedOn"], Value::Null);
        assert_eq!(undated["expiresOn"], Value::Null);
        assert_eq!(undated["mrpPaise"], Value::Null);

        // An already-expired historical lot must remain recordable.
        let (status, expired) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"OLD-1","expiresOn":"2020-03-31"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{expired}");

        // Shape alone is not validity: a GLOB-passing impossible date is still rejected.
        let (status, impossible) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"BAD-1","expiresOn":"2027-02-30"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{impossible}");
        assert_eq!(impossible["issues"][0]["field"], "expiresOn");

        let (status, reversed) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"BAD-2","manufacturedOn":"2027-05-01","expiresOn":"2027-04-30"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{reversed}");
        assert_eq!(reversed["issues"][0]["field"], "expiresOn");

        // Money is exact integer paise; nothing else is authoritative.
        let (status, zero_mrp) =
            add_batch(&pool, &pack_id, json!({"batchNumber":"BAD-3","mrpPaise":0})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{zero_mrp}");
        assert_eq!(zero_mrp["issues"][0]["field"], "mrpPaise");
        let (status, negative_mrp) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"BAD-4","mrpPaise":-100}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{negative_mrp}");

        let stored: Option<i64> = sqlx::query_scalar(
            "SELECT mrp_paise FROM product_batches WHERE normalized_batch_number='NODATE'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, None);

        // Soonest expiry first, undated lots last.
        let (status, listed) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/packs/{pack_id}/batches"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed[0]["normalizedBatchNumber"], "OLD-1");
        assert_eq!(listed[1]["normalizedBatchNumber"], "NODATE");
    }

    #[tokio::test]
    async fn batch_revision_lifecycle_and_history_are_safe() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_product(&pool, "Lifecycle item").await;
        let pack = create_pack(&pool, product["id"].as_str().unwrap(), STRIP, 15, None).await;
        let pack_id = pack["id"].as_str().unwrap().to_owned();
        let (_, created) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"LOT-1","expiresOn":"2029-06-30","mrpPaise":9900}),
        )
        .await;
        let batch_id = created["id"].as_str().unwrap().to_owned();

        let (status, updated) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/batches/{batch_id}"),
            json!({"expectedRevision":1,"batch":{"batchNumber":"LOT-1","expiresOn":"2029-07-31","mrpPaise":10500}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["revision"], 2);
        assert_eq!(updated["mrpPaise"], 10500);

        let (status, stale) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/batches/{batch_id}"),
            json!({"expectedRevision":1,"batch":{"batchNumber":"LOT-1"}}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["code"], "revision_conflict");
        assert_eq!(stale["currentRevision"], 2);

        // Archive preserves the lot and its history rather than pretending it never existed.
        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/batches/{batch_id}/archive"),
            json!({"expectedRevision":2,"reason":"Lot withdrawn"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
        let preserved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM product_batches WHERE id=?")
            .bind(&batch_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(preserved, 1);
        // ON DELETE RESTRICT points at the Pack, so a Pack carrying lots can never be deleted out
        // from under them and the batch's parent reference stays intact for future traceability.
        assert!(
            sqlx::query("DELETE FROM product_packs WHERE id=?")
                .bind(&pack_id)
                .execute(&pool)
                .await
                .is_err()
        );

        // Archiving frees the lot string for reuse on the same Pack.
        let (status, reused) = add_batch(&pool, &pack_id, json!({"batchNumber":"LOT-1"})).await;
        assert_eq!(status, StatusCode::CREATED, "{reused}");
    }

    #[tokio::test]
    async fn batch_row_carries_no_stock_and_no_pricing_columns() {
        let (_temp, pool, _) = test_pool().await;
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT lower(name) FROM pragma_table_info('product_batches')")
                .fetch_all(&pool)
                .await
                .unwrap();
        // A batch existing is not stock existing: no mutable balance may live on the identity row.
        for forbidden in [
            "quantity_on_hand",
            "current_stock",
            "stock_balance",
            "inward_quantity",
            "outward_quantity",
            "quantity",
            "purchase_rate",
            "selling_price",
        ] {
            assert!(
                !columns.iter().any(|column| column == forbidden),
                "product_batches must not carry {forbidden}"
            );
        }
        assert!(columns.iter().any(|column| column == "mrp_paise"));
        // Money is an integer column, never REAL.
        let mrp_type: String = sqlx::query_scalar(
            "SELECT lower(type) FROM pragma_table_info('product_batches') WHERE name='mrp_paise'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(mrp_type, "integer");

        let tables: Vec<String> =
            sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table'")
                .fetch_all(&pool)
                .await
                .unwrap();
        for deferred in ["stock", "stock_ledger", "prices", "purchases", "sales"] {
            assert!(!tables.iter().any(|table| table == deferred));
        }
    }

    #[tokio::test]
    async fn batch_access_roles_and_server_audit_actor_are_enforced() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_product(&pool, "Guarded item").await;
        let pack = create_pack(&pool, product["id"].as_str().unwrap(), STRIP, 15, None).await;
        let pack_id = pack["id"].as_str().unwrap().to_owned();

        let spoofed = Uuid::now_v7().to_string();
        let (status, created) = add_batch(
            &pool,
            &pack_id,
            json!({"batchNumber":"LOT-9","actorId":spoofed}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let batch_id = created["id"].as_str().unwrap();
        let actor: String = sqlx::query_scalar(
            "SELECT actor_id FROM master_change_events WHERE entity_type='product_batch' AND entity_id=?",
        )
        .bind(batch_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let owner: String =
            sqlx::query_scalar("SELECT id FROM users WHERE role='owner_admin' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(actor, owner);
        assert_ne!(actor, spoofed);

        for role in ["pharmacist", "cashier"] {
            let token = format!("{role}-batch-token");
            insert_session(&pool, role, &token).await;
            let (read_status, _) = request_json_as(
                pool.clone(),
                "GET",
                &format!("/api/v1/packs/{pack_id}/batches"),
                Value::Null,
                Some(&token),
            )
            .await;
            assert_eq!(read_status, StatusCode::OK, "{role} must read batches");
            let (write_status, body) = request_json_as(
                pool.clone(),
                "POST",
                &format!("/api/v1/packs/{pack_id}/batches"),
                json!({"batchNumber":"LOT-ROLE"}),
                Some(&token),
            )
            .await;
            assert_eq!(write_status, StatusCode::FORBIDDEN, "{body}");
        }

        let (status, _) = request_json_as(
            pool.clone(),
            "GET",
            &format!("/api/v1/packs/{pack_id}/batches"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ---- Phase 1C-B composition identity ----

    const SALT_SODIUM: &str = "01997100-0000-7000-8000-000000000001";
    const SU_MG: &str = "01997200-0000-7000-8000-000000000002";
    const SU_G: &str = "01997200-0000-7000-8000-000000000003";
    const SU_ML: &str = "01997200-0000-7000-8000-000000000004";

    async fn create_reference(pool: &SqlitePool, kind: &str, attributes: Value) -> Value {
        let (status, body) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/{kind}"),
            json!({ "attributes": attributes }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    async fn create_ingredient(pool: &SqlitePool, code: &str, name: &str) -> String {
        create_reference(
            pool,
            "ingredients",
            json!({"canonicalCode": code, "displayName": name}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn create_medicine(pool: &SqlitePool, name: &str) -> String {
        // Dosage-form codes are unique, so each medicine in a test gets its own. The suffix comes
        // from the random tail of a UUIDv7, not its millisecond prefix, which collides.
        let code = format!("tablet-{}", &Uuid::now_v7().to_string()[24..36]);
        let form = create_reference(
            pool,
            "dosage-forms",
            json!({"canonicalCode":code,"displayName":"Tablet"}),
        )
        .await;
        let (status, body) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":{
                "productKind":"medicine","baseUnitId":TABLET,
                "dosageFormId":form["id"].as_str().unwrap(),
                "quantityScale":0,"displayName":name
            }}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    fn component(ingredient: &str, atoms: i64, unit: &str) -> Value {
        json!({
            "ingredientId": ingredient,
            "strengthNumeratorAtoms": atoms,
            "strengthNumeratorScale": 0,
            "strengthNumeratorUnitId": unit
        })
    }

    async fn add_component(pool: &SqlitePool, product: &str, body: Value) -> (StatusCode, Value) {
        request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product}/composition"),
            body,
        )
        .await
    }

    #[tokio::test]
    async fn composition_identity_ordering_and_exact_strengths_are_enforced() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_medicine(&pool, "Combination tablet").await;
        let paracetamol = create_ingredient(&pool, "paracetamol", "Paracetamol").await;
        let diclofenac = create_ingredient(&pool, "diclofenac", "Diclofenac").await;

        // A single-ingredient strength is per one Product base unit and needs no denominator.
        let (status, first) =
            add_component(&pool, &product, component(&paracetamol, 500, SU_MG)).await;
        assert_eq!(status, StatusCode::CREATED, "{first}");
        assert_eq!(first["displayOrder"], 0);
        assert_eq!(first["strengthNumeratorAtoms"], 500);
        assert_eq!(first["strengthDenominatorAtoms"], Value::Null);
        assert_eq!(first["componentRole"], "active");
        assert_eq!(
            Uuid::parse_str(first["id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );

        // A combination medicine simply carries more components, appended in order.
        let mut second_body = component(&diclofenac, 50, SU_MG);
        second_body["saltFormId"] = json!(SALT_SODIUM);
        let (status, second) = add_component(&pool, &product, second_body).await;
        assert_eq!(status, StatusCode::CREATED, "{second}");
        assert_eq!(second["displayOrder"], 1);
        assert_eq!(second["saltFormId"], SALT_SODIUM);

        let (status, listed) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/products/{product}/composition"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed.as_array().unwrap().len(), 2);
        assert_eq!(listed[0]["ingredientId"], paracetamol.as_str());
        assert_eq!(listed[1]["ingredientId"], diclofenac.as_str());

        // The Product detail projection carries composition in the same deterministic order.
        let (status, detail) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/products/{product}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["composition"].as_array().unwrap().len(), 2);

        // The same ingredient with no salt cannot be recorded twice, despite SQL NULL semantics.
        let (status, duplicate) =
            add_component(&pool, &product, component(&paracetamol, 250, SU_MG)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");
        assert_eq!(duplicate["code"], "composition_conflict");

        // The same ingredient in a different salt form is a genuinely different component.
        let mut salted = component(&paracetamol, 250, SU_MG);
        salted["saltFormId"] = json!(SALT_SODIUM);
        let (status, salted_body) = add_component(&pool, &product, salted).await;
        assert_eq!(status, StatusCode::CREATED, "{salted_body}");
    }

    #[tokio::test]
    async fn composition_strength_shapes_reject_impossible_values() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_medicine(&pool, "Suspension").await;
        let azithromycin = create_ingredient(&pool, "azithromycin", "Azithromycin").await;

        // A concentration: 200 mg per 5 mL.
        let mut concentration = component(&azithromycin, 200, SU_MG);
        concentration["strengthDenominatorAtoms"] = json!(5);
        concentration["strengthDenominatorScale"] = json!(0);
        concentration["strengthDenominatorUnitId"] = json!(SU_ML);
        let (status, body) = add_component(&pool, &product, concentration).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["strengthDenominatorAtoms"], 5);

        // A half-specified denominator has no meaning and is refused.
        let clotrimazole = create_ingredient(&pool, "clotrimazole", "Clotrimazole").await;
        let mut half = component(&clotrimazole, 1, SU_G);
        half["strengthDenominatorAtoms"] = json!(100);
        let (status, body) = add_component(&pool, &product, half).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["issues"][0]["field"], "strengthDenominatorUnitId");

        // A percentage is stored as an exact ratio out of exactly one hundred.
        let mut percentage = component(&clotrimazole, 1, SU_G);
        percentage["strengthPresentation"] = json!("percentage");
        percentage["strengthDenominatorAtoms"] = json!(100);
        percentage["strengthDenominatorScale"] = json!(0);
        percentage["strengthDenominatorUnitId"] = json!(SU_G);
        let (status, body) = add_component(&pool, &product, percentage).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["strengthPresentation"], "percentage");
        assert_eq!(body["strengthDenominatorAtoms"], 100);

        let miconazole = create_ingredient(&pool, "miconazole", "Miconazole").await;
        let mut wrong_percentage = component(&miconazole, 2, SU_G);
        wrong_percentage["strengthPresentation"] = json!("percentage");
        wrong_percentage["strengthDenominatorAtoms"] = json!(50);
        wrong_percentage["strengthDenominatorScale"] = json!(0);
        wrong_percentage["strengthDenominatorUnitId"] = json!(SU_G);
        let (status, body) = add_component(&pool, &product, wrong_percentage).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["issues"][0]["field"], "strengthDenominatorAtoms");

        // Precision may never exceed the strength unit's allowed scale.
        let mut too_precise = component(&miconazole, 1, SU_MG);
        too_precise["strengthNumeratorScale"] = json!(6);
        let (status, body) = add_component(&pool, &product, too_precise).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "composition_conflict");

        // Zero and negative strengths are not strengths.
        let (status, body) = add_component(&pool, &product, component(&miconazole, 0, SU_MG)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["issues"][0]["field"], "strengthNumeratorAtoms");
    }

    #[tokio::test]
    async fn composition_is_medicine_only_and_revision_archive_safe() {
        let (_temp, pool, _) = test_pool().await;
        let ingredient = create_ingredient(&pool, "paracetamol", "Paracetamol").await;

        // A General Pharmacy Item has no composition.
        let general = create_product(&pool, "Cotton roll").await;
        let (status, body) = add_component(
            &pool,
            general["id"].as_str().unwrap(),
            component(&ingredient, 500, SU_MG),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "composition_conflict");

        let product = create_medicine(&pool, "Paracetamol tablet").await;
        let (_, created) = add_component(&pool, &product, component(&ingredient, 500, SU_MG)).await;
        let component_id = created["id"].as_str().unwrap();

        // A stale revision never overwrites a newer component.
        let mut updated = component(&ingredient, 650, SU_MG);
        updated["displayOrder"] = json!(0);
        let (status, body) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/composition-components/{component_id}"),
            json!({"expectedRevision":1,"component":updated.clone()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["revision"], 2);
        assert_eq!(body["strengthNumeratorAtoms"], 650);

        let (status, stale) = request_json(
            pool.clone(),
            "PUT",
            &format!("/api/v1/composition-components/{component_id}"),
            json!({"expectedRevision":1,"component":updated}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["code"], "revision_conflict");
        assert_eq!(stale["currentRevision"], 2);

        // A Product may not be archived while it still carries active composition.
        let (status, blocked) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/products/{product}/archive"),
            json!({"expectedRevision":1,"reason":"Duplicate entry"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
        assert_eq!(blocked["code"], "archived_conflict");

        // Archive preserves the component and its history; restore revalidates it.
        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/composition-components/{component_id}/archive"),
            json!({"expectedRevision":2,"reason":"Recorded in error"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
        let still_present: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM product_composition_components WHERE id=?")
                .bind(component_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(still_present, 1);

        let (status, restored) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/composition-components/{component_id}/restore"),
            json!({"expectedRevision":3,"reason":"Confirmed correct"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert_eq!(restored["status"], "active");
    }

    #[tokio::test]
    async fn composition_access_roles_and_server_audit_actor_are_enforced() {
        let (_temp, pool, _) = test_pool().await;
        let product = create_medicine(&pool, "Paracetamol tablet").await;
        let ingredient = create_ingredient(&pool, "paracetamol", "Paracetamol").await;

        // The browser-supplied actor is ignored; the session user is recorded.
        let spoofed = Uuid::now_v7().to_string();
        let mut spoofing = component(&ingredient, 500, SU_MG);
        spoofing["actorId"] = json!(spoofed);
        let (status, created) = add_component(&pool, &product, spoofing).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let component_id = created["id"].as_str().unwrap();
        let actor: String = sqlx::query_scalar(
            "SELECT actor_id FROM master_change_events WHERE entity_type='product_composition_component' AND entity_id=?",
        )
        .bind(component_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let owner: String =
            sqlx::query_scalar("SELECT id FROM users WHERE role='owner_admin' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(actor, owner);
        assert_ne!(actor, spoofed);

        for role in ["pharmacist", "cashier"] {
            let token = format!("{role}-composition-token");
            insert_session(&pool, role, &token).await;
            let (read_status, _, _) = (
                request_json_as(
                    pool.clone(),
                    "GET",
                    &format!("/api/v1/products/{product}/composition"),
                    Value::Null,
                    Some(&token),
                )
                .await
                .0,
                (),
                (),
            );
            assert_eq!(read_status, StatusCode::OK, "{role} must read composition");
            let (write_status, body) = request_json_as(
                pool.clone(),
                "POST",
                &format!("/api/v1/products/{product}/composition"),
                component(&ingredient, 250, SU_MG),
                Some(&token),
            )
            .await;
            assert_eq!(write_status, StatusCode::FORBIDDEN, "{body}");
        }

        let (status, _) = request_json_as(
            pool.clone(),
            "GET",
            &format!("/api/v1/products/{product}/composition"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ingredient_and_salt_form_masters_are_distinct_and_protected() {
        let (_temp, pool, _) = test_pool().await;
        let diclofenac = create_ingredient(&pool, "diclofenac", "Diclofenac").await;

        // Ingredient and salt form are separate identities, not synonyms.
        let (status, salts) = request_json(
            pool.clone(),
            "GET",
            "/api/v1/reference/salt-forms?search=sodium",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            salts
                .as_array()
                .unwrap()
                .iter()
                .any(|salt| { salt["attributes"]["canonicalCode"] == "sodium" })
        );

        let (status, units) = request_json(
            pool.clone(),
            "GET",
            "/api/v1/reference/strength-units?search=iu",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(units[0]["attributes"]["dimension"], "activity");

        // A duplicate canonical code is rejected exactly as other masters are.
        let (status, duplicate) = request_json(
            pool.clone(),
            "POST",
            "/api/v1/reference/ingredients",
            json!({"attributes":{"canonicalCode":"diclofenac","displayName":"Diclofenac again"}}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate}");

        // Archiving a referenced master is preserved, never cascaded and never blocked — the frozen
        // Phase 1A semantic. What it must prevent is a *new* component against an archived master.
        let product = create_medicine(&pool, "Diclofenac tablet").await;
        let (_, created) = add_component(&pool, &product, component(&diclofenac, 50, SU_MG)).await;
        assert_eq!(created["status"], "active");
        let (status, archived) = request_json(
            pool.clone(),
            "POST",
            &format!("/api/v1/reference/ingredients/{diclofenac}/archive"),
            json!({"expectedRevision":1,"reason":"No longer stocked"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");

        // The existing component keeps its reference as history.
        let (status, still_listed) = request_json(
            pool.clone(),
            "GET",
            &format!("/api/v1/products/{product}/composition"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(still_listed[0]["ingredientId"], diclofenac.as_str());

        // The archived ingredient may not be deleted, and may not back a new component.
        assert!(
            sqlx::query("DELETE FROM ingredients WHERE id=?")
                .bind(&diclofenac)
                .execute(&pool)
                .await
                .is_err()
        );
        let other = create_medicine(&pool, "Another diclofenac tablet").await;
        let (status, rejected) =
            add_component(&pool, &other, component(&diclofenac, 50, SU_MG)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(rejected["code"], "composition_conflict");
    }
}

/// Phase 1F Product tax classification.
///
/// Self-contained so it exercises the classification surface without depending on the Phase 1B/1C
/// catalog fixtures, which carry state these tests do not need.
#[cfg(test)]
mod tax_classification_tests {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    const TABLET: &str = "01997000-0000-7000-8000-000000000001";
    const OWNER_TOKEN: &str = "tax-owner-session-token";
    const CASHIER_TOKEN: &str = "tax-cashier-session-token";

    struct Fixture {
        _temp: tempfile::TempDir,
        pool: SqlitePool,
        owner_id: String,
        product_id: String,
        hsn_id: String,
        category_id: String,
    }

    async fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let pool = crate::infrastructure::database::connect(&temp.path().join("tax.sqlite3"))
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
        let owner_id = insert_tax_session(&pool, "owner_admin", OWNER_TOKEN).await;
        insert_tax_session(&pool, "cashier", CASHIER_TOKEN).await;

        let (status, product) = request_tax(
            pool.clone(),
            "POST",
            "/api/v1/products",
            json!({"product":{"productKind":"general_pharmacy_item","baseUnitId":TABLET,
                "quantityScale":0,"displayName":"Taxable item"}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{product}");

        let hsn_id = insert_hsn(&pool, "30049099").await;
        let category_id = insert_category(&pool, "gst-12").await;
        insert_rate(&pool, &category_id, "2020-01-01", None, 600).await;

        Fixture {
            _temp: temp,
            pool,
            owner_id,
            product_id: product["id"].as_str().unwrap().to_owned(),
            hsn_id,
            category_id,
        }
    }

    async fn insert_hsn(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO hsn_codes (id,jurisdiction,hsn_code,description,created_at_utc,updated_at_utc) \
             VALUES (?,'IN',?,'Medicaments',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_category(pool: &SqlitePool, code: &str) -> String {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO tax_categories (id,jurisdiction,category_code,display_name,tax_treatment,\
             created_at_utc,updated_at_utc) VALUES (?,'IN',?,'GST 12%','taxable',\
             strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(&id)
        .bind(code)
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

    async fn archive_reference(pool: &SqlitePool, table: &str, id: &str) {
        sqlx::query(&format!(
            "UPDATE {table} SET status='archived',revision=revision+1,\
             archived_at_utc=strftime('%Y-%m-%dT%H:%M:%fZ','now'),archive_reason='no longer used' \
             WHERE id=?"
        ))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    fn classification_uri(product_id: &str) -> String {
        format!("/api/v1/products/{product_id}/tax-classification")
    }

    async fn assign(
        pool: &SqlitePool,
        product_id: &str,
        revision: i64,
        hsn: Option<&str>,
        category: Option<&str>,
    ) -> (StatusCode, Value) {
        request_tax(
            pool.clone(),
            "PUT",
            &classification_uri(product_id),
            json!({
                "expectedRevision": revision,
                "hsnCodeId": hsn,
                "taxCategoryId": category,
                "reason": "Classified from the supplier invoice"
            }),
        )
        .await
    }

    #[tokio::test]
    async fn an_unclassified_product_reads_normally_and_reports_itself_incomplete() {
        let f = fixture().await;
        let (status, body) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["hsnCodeId"], Value::Null);
        assert_eq!(body["taxCategoryId"], Value::Null);
        assert_eq!(body["complete"], false);
        // No category means no rate to resolve — not an error.
        assert_eq!(body["applicableRate"], Value::Null);

        // The Product itself is unaffected: an unclassified Product is a normal Product.
        let (status, product) = request_tax(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{}", f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{product}");
        assert_eq!(product["hsnCodeId"], Value::Null);
        assert_eq!(product["taxCategoryId"], Value::Null);
        assert_eq!(product["revision"], 1);
    }

    #[tokio::test]
    async fn assigning_a_classification_bumps_the_product_revision_and_resolves_a_rate() {
        let f = fixture().await;
        let (status, assigned) = assign(
            &f.pool,
            &f.product_id,
            1,
            Some(&f.hsn_id),
            Some(&f.category_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{assigned}");
        assert_eq!(assigned["hsnCodeId"], f.hsn_id.as_str());
        assert_eq!(assigned["taxCategoryId"], f.category_id.as_str());
        assert_eq!(assigned["complete"], true);
        // Classification is intrinsic Product metadata, so it moves the Product's own revision.
        assert_eq!(assigned["revision"], 2);

        // The rate is resolved for a date, never stored on the Product.
        assert_eq!(assigned["applicableRate"]["cgstBasisPoints"], 600);
        assert_eq!(assigned["applicableRate"]["sgstBasisPoints"], 600);
        assert_eq!(assigned["applicableRate"]["igstBasisPoints"], 1200);
        assert_eq!(assigned["applicableRate"]["cessBasisPoints"], 0);
        assert!(assigned["asOf"].as_str().unwrap().len() == 10);

        // The Product detail carries the same identity.
        let (_, product) = request_tax(
            f.pool.clone(),
            "GET",
            &format!("/api/v1/products/{}", f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(product["taxCategoryId"], f.category_id.as_str());
        assert_eq!(product["revision"], 2);
    }

    #[tokio::test]
    async fn a_product_table_never_gains_a_rate_column() {
        let f = fixture().await;
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('products')")
                .fetch_all(&f.pool)
                .await
                .unwrap();
        for column in &columns {
            for forbidden in [
                "rate",
                "basis_points",
                "percent",
                "gst",
                "cgst",
                "sgst",
                "igst",
                "cess",
                "paise",
            ] {
                assert!(
                    !column.contains(forbidden),
                    "a Product identifies its classification and never stores a rate; found {column}"
                );
            }
        }
        assert!(columns.iter().any(|column| column == "hsn_code_id"));
        assert!(columns.iter().any(|column| column == "tax_category_id"));
    }

    #[tokio::test]
    async fn either_reference_may_be_assigned_alone_and_the_pair_may_be_cleared() {
        let f = fixture().await;
        // HSN alone is a legitimate partial classification.
        let (status, hsn_only) = assign(&f.pool, &f.product_id, 1, Some(&f.hsn_id), None).await;
        assert_eq!(status, StatusCode::OK, "{hsn_only}");
        assert_eq!(hsn_only["complete"], false);
        assert_eq!(hsn_only["applicableRate"], Value::Null);

        // Category alone resolves a rate even without an HSN.
        let (status, category_only) =
            assign(&f.pool, &f.product_id, 2, None, Some(&f.category_id)).await;
        assert_eq!(status, StatusCode::OK, "{category_only}");
        assert_eq!(category_only["hsnCodeId"], Value::Null);
        assert_eq!(category_only["complete"], false);
        assert_eq!(category_only["applicableRate"]["cgstBasisPoints"], 600);

        // Clearing both is allowed and leaves an ordinary unclassified Product.
        let (status, cleared) = assign(&f.pool, &f.product_id, 3, None, None).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert_eq!(cleared["hsnCodeId"], Value::Null);
        assert_eq!(cleared["taxCategoryId"], Value::Null);
        assert_eq!(cleared["complete"], false);
        assert_eq!(cleared["revision"], 4);
    }

    #[tokio::test]
    async fn an_archived_reference_cannot_be_newly_assigned() {
        let f = fixture().await;
        let archived_hsn = insert_hsn(&f.pool, "99999999").await;
        let archived_category = insert_category(&f.pool, "gst-old").await;
        archive_reference(&f.pool, "hsn_codes", &archived_hsn).await;
        archive_reference(&f.pool, "tax_categories", &archived_category).await;

        let (status, refused) = assign(&f.pool, &f.product_id, 1, Some(&archived_hsn), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "archived_conflict");

        let (status, refused) =
            assign(&f.pool, &f.product_id, 1, None, Some(&archived_category)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert_eq!(refused["code"], "archived_conflict");

        // Nothing was written: the revision is untouched.
        let (_, unchanged) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(unchanged["revision"], 1);

        // The database refuses it independently, so the guarantee does not rest on the service.
        let direct = sqlx::query("UPDATE products SET hsn_code_id=? WHERE id=?")
            .bind(&archived_hsn)
            .bind(&f.product_id)
            .execute(&f.pool)
            .await;
        assert!(direct.is_err(), "the trigger must refuse an archived HSN");
    }

    #[tokio::test]
    async fn a_reference_archived_after_assignment_stays_readable_and_never_traps_the_product() {
        let f = fixture().await;
        assign(
            &f.pool,
            &f.product_id,
            1,
            Some(&f.hsn_id),
            Some(&f.category_id),
        )
        .await;
        archive_reference(&f.pool, "hsn_codes", &f.hsn_id).await;

        // Still readable: archiving a master is not retroactive deletion.
        let (status, body) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["hsnCodeId"], f.hsn_id.as_str());

        // The trap this guards against: clearing only the Tax Category rewrites both columns, so a
        // rule that re-validated the unchanged archived HSN would leave the Product uneditable.
        let (status, cleared) = assign(&f.pool, &f.product_id, 2, Some(&f.hsn_id), None).await;
        assert_eq!(status, StatusCode::OK, "{cleared}");
        assert_eq!(cleared["hsnCodeId"], f.hsn_id.as_str());
        assert_eq!(cleared["taxCategoryId"], Value::Null);

        // An ordinary Product edit is likewise unaffected by the archived reference.
        let (status, edited) = request_tax(
            f.pool.clone(),
            "PUT",
            &format!("/api/v1/products/{}", f.product_id),
            json!({"expectedRevision":3,"product":{"productKind":"general_pharmacy_item",
                "baseUnitId":TABLET,"quantityScale":0,"displayName":"Renamed item"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["displayName"], "Renamed item");
        assert_eq!(edited["hsnCodeId"], f.hsn_id.as_str(), "edit preserved it");
    }

    #[tokio::test]
    async fn classification_never_blocks_product_archive_and_survives_restore() {
        let f = fixture().await;
        assign(
            &f.pool,
            &f.product_id,
            1,
            Some(&f.hsn_id),
            Some(&f.category_id),
        )
        .await;
        // Classification is intrinsic metadata, not an active child row, so it cannot deadlock the
        // Product lifecycle the way an active Pack or role does.
        let (status, archived) = request_tax(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/products/{}/archive", f.product_id),
            json!({"expectedRevision":2,"reason":"Discontinued"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{archived}");
        assert_eq!(archived["status"], "archived");
        assert_eq!(archived["taxCategoryId"], f.category_id.as_str());

        let (status, restored) = request_tax(
            f.pool.clone(),
            "POST",
            &format!("/api/v1/products/{}/restore", f.product_id),
            json!({"expectedRevision":3,"reason":"Stocked again"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{restored}");
        assert_eq!(restored["hsnCodeId"], f.hsn_id.as_str());
        assert_eq!(restored["taxCategoryId"], f.category_id.as_str());
    }

    #[tokio::test]
    async fn a_stale_revision_cannot_overwrite_a_newer_classification() {
        let f = fixture().await;
        assign(&f.pool, &f.product_id, 1, Some(&f.hsn_id), None).await;
        let (status, conflict) =
            assign(&f.pool, &f.product_id, 1, None, Some(&f.category_id)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
        assert_eq!(conflict["code"], "revision_conflict");
        assert_eq!(conflict["expectedRevision"], 1);
        assert_eq!(conflict["currentRevision"], 2);

        // The stale write changed nothing.
        let (_, current) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
        )
        .await;
        assert_eq!(current["hsnCodeId"], f.hsn_id.as_str());
        assert_eq!(current["taxCategoryId"], Value::Null);
    }

    #[tokio::test]
    async fn the_applicable_rate_follows_the_requested_date_across_a_boundary() {
        let f = fixture().await;
        let category = insert_category(&f.pool, "gst-stepped").await;
        // 2.50% each side until 2026-01-01, then 9.00% each side open-ended.
        insert_rate(&f.pool, &category, "2025-01-01", Some("2026-01-01"), 250).await;
        insert_rate(&f.pool, &category, "2026-01-01", None, 900).await;
        assign(&f.pool, &f.product_id, 1, None, Some(&category)).await;

        let rate_on = async |date: &str| {
            request_tax(
                f.pool.clone(),
                "GET",
                &format!("{}?asOf={date}", classification_uri(&f.product_id)),
                Value::Null,
            )
            .await
            .1
        };

        assert_eq!(rate_on("2024-12-31").await["applicableRate"], Value::Null);
        assert_eq!(
            rate_on("2025-01-01").await["applicableRate"]["cgstBasisPoints"],
            250
        );
        assert_eq!(
            rate_on("2025-12-31").await["applicableRate"]["cgstBasisPoints"],
            250
        );
        // Half-open: the end date belongs to the next version.
        assert_eq!(
            rate_on("2026-01-01").await["applicableRate"]["cgstBasisPoints"],
            900
        );
        assert_eq!(
            rate_on("2030-06-01").await["applicableRate"]["cgstBasisPoints"],
            900
        );
        // The echoed date makes it impossible to mistake the rate for Product metadata.
        assert_eq!(rate_on("2030-06-01").await["asOf"], "2030-06-01");

        let (status, bad) = request_tax(
            f.pool.clone(),
            "GET",
            &format!("{}?asOf=2026-13-01", classification_uri(&f.product_id)),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(bad["issues"][0]["field"], "asOf");
    }

    #[tokio::test]
    async fn the_resolver_reports_components_without_choosing_a_tax_treatment() {
        let f = fixture().await;
        assign(&f.pool, &f.product_id, 1, None, Some(&f.category_id)).await;
        let (_, body) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
        )
        .await;
        let rate = &body["applicableRate"];
        // Both the intra-state pair and the inter-state figure are present, and nothing in this
        // response selects between them: that is transaction context, which Phase 1G supplies.
        assert!(rate["cgstBasisPoints"].is_i64());
        assert!(rate["sgstBasisPoints"].is_i64());
        assert!(rate["igstBasisPoints"].is_i64());
        assert!(rate.get("appliedTreatment").is_none());
        assert!(rate.get("totalBasisPoints").is_none());
        assert!(rate.get("isInterState").is_none());
        // Integers, never a decimal fraction.
        for component in [
            "cgstBasisPoints",
            "sgstBasisPoints",
            "igstBasisPoints",
            "cessBasisPoints",
        ] {
            assert!(rate[component].as_f64().is_some());
            assert!(rate[component].is_i64(), "{component} must be an integer");
        }
    }

    #[tokio::test]
    async fn the_audit_records_the_previous_and_next_classification_and_the_session_actor() {
        let f = fixture().await;
        assign(&f.pool, &f.product_id, 1, Some(&f.hsn_id), None).await;
        assign(
            &f.pool,
            &f.product_id,
            2,
            Some(&f.hsn_id),
            Some(&f.category_id),
        )
        .await;

        let events: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT change_payload,COALESCE(actor_id,''),action FROM master_change_events \
             WHERE entity_type='product' AND entity_id=? AND entity_revision>1 \
             ORDER BY entity_revision",
        )
        .bind(&f.product_id)
        .fetch_all(&f.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 2);

        let second: Value = serde_json::from_str(&events[1].0).unwrap();
        assert_eq!(second["change"], "tax_classification");
        assert_eq!(second["previous"]["hsnCodeId"], f.hsn_id.as_str());
        assert_eq!(second["previous"]["taxCategoryId"], Value::Null);
        assert_eq!(second["next"]["taxCategoryId"], f.category_id.as_str());
        // The actor is the validated server session user, never a browser-supplied value.
        assert_eq!(events[1].1, f.owner_id);
        assert_eq!(events[1].2, "updated");
        // The audit records identity only; it is not a tax snapshot and claims no rate.
        assert!(second.get("cgstBasisPoints").is_none());
        assert!(second.get("applicableRate").is_none());
    }

    #[tokio::test]
    async fn reads_need_a_session_and_writes_need_owner_admin() {
        let f = fixture().await;
        let (status, anonymous) = request_tax_as(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{anonymous}");
        assert_eq!(anonymous["code"], "authentication_required");

        // A read-only role may look.
        let (status, readable) = request_tax_as(
            f.pool.clone(),
            "GET",
            &classification_uri(&f.product_id),
            Value::Null,
            Some(CASHIER_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{readable}");

        // But never write, and a spoofed actor in the body changes nothing.
        let (status, denied) = request_tax_as(
            f.pool.clone(),
            "PUT",
            &classification_uri(&f.product_id),
            json!({"expectedRevision":1,"hsnCodeId":f.hsn_id,"taxCategoryId":f.category_id,
                "actorId":f.owner_id,"actor":"owner_admin"}),
            Some(CASHIER_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
        assert_eq!(denied["code"], "authorization_denied");
        let written: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM products WHERE id=? AND tax_category_id IS NOT NULL",
        )
        .bind(&f.product_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(written, 0, "a spoofed actor must not grant a write");
    }

    #[tokio::test]
    async fn unknown_products_and_malformed_ids_stay_safe_and_typed() {
        let f = fixture().await;
        let missing = Uuid::now_v7().to_string();
        let (status, not_found) = request_tax(
            f.pool.clone(),
            "GET",
            &classification_uri(&missing),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{not_found}");
        assert_eq!(not_found["code"], "not_found");

        let (status, invalid) = request_tax(
            f.pool.clone(),
            "GET",
            "/api/v1/products/not-a-uuid/tax-classification",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid}");

        // A bad reference id is a validation failure, not a leak.
        let (status, bad_ref) = request_tax(
            f.pool.clone(),
            "PUT",
            &classification_uri(&f.product_id),
            json!({"expectedRevision":1,"hsnCodeId":"not-a-uuid"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad_ref}");

        // A well-formed id that does not exist is refused by referential integrity, safely.
        let (status, dangling) = assign(
            &f.pool,
            &f.product_id,
            1,
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
        assert!(
            status.is_client_error(),
            "a dangling reference must be refused: {dangling}"
        );

        for body in [not_found, invalid, bad_ref, dangling] {
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

    async fn insert_tax_session(pool: &SqlitePool, role: &str, token: &str) -> String {
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
        .bind(format!("{role} tax user"))
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

    async fn request_tax(
        pool: SqlitePool,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        request_tax_as(pool, method, uri, body, Some(OWNER_TOKEN)).await
    }

    async fn request_tax_as(
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
