import {
  BarcodeSchema,
  BatchSchema,
  CatalogContextSchema,
  CompositionComponentSchema,
  DuplicateCandidateSchema,
  ProductCompanyRoleSchema,
  ProductDetailSchema,
  ProductPackSchema,
  ProductSchema,
  ProductTaxClassificationSchema,
  StorePackPolicySchema,
  type ProductTaxClassification,
  type Barcode,
  type BarcodeLookup,
  type Batch,
  type BatchFields,
  type CompositionComponent,
  type CompositionComponentFields,
  type CreateProductRequest,
  type Product,
  type ProductCompanyRole,
  type ProductCompanyRoleFields,
  type ProductDetail,
  type ProductFields,
  type ProductPack,
  type ProductPackFields,
  type StorePackPolicy,
  type StorePackPolicyFields
} from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest } from "../platform/localService";

export type CatalogStatusFilter = "active" | "archived" | "all";

export async function getCatalogContext() {
  return CatalogContextSchema.parse(await localServiceRequest("/api/v1/catalog/context"));
}

export async function listProducts(search = "", status: CatalogStatusFilter = "active"): Promise<Product[]> {
  const query = new URLSearchParams({ status });
  if (search.trim()) query.set("search", search.trim());
  return ProductSchema.array().parse(await localServiceRequest(`/api/v1/products?${query}`));
}

export async function getProduct(id: string): Promise<ProductDetail> {
  return ProductDetailSchema.parse(await localServiceRequest(`/api/v1/products/${encodeURIComponent(id)}`));
}

export async function findDuplicateCandidates(request: CreateProductRequest) {
  return DuplicateCandidateSchema.array().parse(await localServiceRequest("/api/v1/products/duplicate-candidates", {
    method: "POST",
    body: JSON.stringify(request)
  }));
}

export async function createProduct(request: CreateProductRequest): Promise<ProductDetail> {
  return ProductDetailSchema.parse(await localServiceRequest("/api/v1/products", {
    method: "POST",
    body: JSON.stringify(request)
  }));
}

export async function updateProduct(record: Product, product: ProductFields): Promise<ProductDetail> {
  return ProductDetailSchema.parse(await localServiceRequest(`/api/v1/products/${record.id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: record.revision, product })
  }));
}

export async function changeProductLifecycle(record: Product, action: "archive" | "restore", reason: string): Promise<ProductDetail> {
  return ProductDetailSchema.parse(await localServiceRequest(`/api/v1/products/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export async function createCompanyRole(productId: string, role: ProductCompanyRoleFields): Promise<ProductCompanyRole> {
  return ProductCompanyRoleSchema.parse(await localServiceRequest(`/api/v1/products/${productId}/company-roles`, {
    method: "POST",
    body: JSON.stringify(role)
  }));
}

export async function updateCompanyRole(record: ProductCompanyRole, role: ProductCompanyRoleFields): Promise<ProductCompanyRole> {
  return ProductCompanyRoleSchema.parse(await localServiceRequest(`/api/v1/company-roles/${record.id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: record.revision, role })
  }));
}

export async function changeCompanyRoleLifecycle(record: ProductCompanyRole, action: "archive" | "restore", reason: string): Promise<ProductCompanyRole> {
  return ProductCompanyRoleSchema.parse(await localServiceRequest(`/api/v1/company-roles/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export async function createPack(productId: string, pack: ProductPackFields): Promise<ProductPack> {
  return ProductPackSchema.parse(await localServiceRequest(`/api/v1/products/${productId}/packs`, {
    method: "POST",
    body: JSON.stringify(pack)
  }));
}

export async function updatePack(record: ProductPack, pack: ProductPackFields): Promise<ProductPack> {
  return ProductPackSchema.parse(await localServiceRequest(`/api/v1/packs/${record.id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: record.revision, pack })
  }));
}

export async function changePackLifecycle(record: ProductPack, action: "archive" | "restore", reason: string): Promise<ProductPack> {
  return ProductPackSchema.parse(await localServiceRequest(`/api/v1/packs/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export async function getPackPolicy(packId: string): Promise<StorePackPolicy | null> {
  try {
    return StorePackPolicySchema.parse(await localServiceRequest(`/api/v1/packs/${packId}/policy`));
  } catch (error) {
    if (error instanceof LocalServiceError && error.code === "not_found") return null;
    throw error;
  }
}

export async function savePackPolicy(packId: string, current: StorePackPolicy | null, policy: StorePackPolicyFields): Promise<StorePackPolicy> {
  return StorePackPolicySchema.parse(await localServiceRequest(`/api/v1/packs/${packId}/policy`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: current?.revision ?? null, policy })
  }));
}

export async function changePackPolicyLifecycle(record: StorePackPolicy, action: "archive" | "restore", reason: string): Promise<StorePackPolicy> {
  return StorePackPolicySchema.parse(await localServiceRequest(`/api/v1/pack-policies/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export async function listBarcodes(packId: string): Promise<Barcode[]> {
  return BarcodeSchema.array().parse(await localServiceRequest(`/api/v1/packs/${packId}/barcodes`));
}

export async function createBarcode(packId: string, barcode: BarcodeLookup & { symbology?: string | null }): Promise<Barcode> {
  return BarcodeSchema.parse(await localServiceRequest(`/api/v1/packs/${packId}/barcodes`, {
    method: "POST",
    body: JSON.stringify(barcode)
  }));
}

export async function changeBarcodeLifecycle(record: Barcode, action: "archive" | "restore", reason: string): Promise<Barcode> {
  return BarcodeSchema.parse(await localServiceRequest(`/api/v1/barcodes/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export async function listBatches(packId: string): Promise<Batch[]> {
  return BatchSchema.array().parse(await localServiceRequest(`/api/v1/packs/${encodeURIComponent(packId)}/batches`));
}

export async function createBatch(packId: string, batch: BatchFields): Promise<Batch> {
  return BatchSchema.parse(await localServiceRequest(`/api/v1/packs/${packId}/batches`, {
    method: "POST",
    body: JSON.stringify(batch)
  }));
}

export async function updateBatch(record: Batch, batch: BatchFields): Promise<Batch> {
  return BatchSchema.parse(await localServiceRequest(`/api/v1/batches/${record.id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: record.revision, batch })
  }));
}

export async function changeBatchLifecycle(record: Batch, action: "archive" | "restore", reason: string): Promise<Batch> {
  return BatchSchema.parse(await localServiceRequest(`/api/v1/batches/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

/**
 * Rupees to exact integer paise using string arithmetic. `12.5` never becomes a binary float, and
 * anything beyond two decimal places is rejected rather than silently rounded.
 */
export function rupeesToPaise(value: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(value.trim());
  if (!match) return null;
  const paise = Number.parseInt(`${match[1]}${(match[2] ?? "").padEnd(2, "0")}`, 10);
  return Number.isSafeInteger(paise) && paise > 0 ? paise : null;
}

export function paiseToRupees(paise: number): string {
  const sign = paise < 0 ? "-" : "";
  const absolute = Math.abs(paise);
  return `${sign}${Math.floor(absolute / 100)}.${String(absolute % 100).padStart(2, "0")}`;
}

export async function listComposition(productId: string): Promise<CompositionComponent[]> {
  return CompositionComponentSchema.array().parse(
    await localServiceRequest(`/api/v1/products/${encodeURIComponent(productId)}/composition`)
  );
}

export async function createComponent(productId: string, component: CompositionComponentFields): Promise<CompositionComponent> {
  return CompositionComponentSchema.parse(await localServiceRequest(`/api/v1/products/${productId}/composition`, {
    method: "POST",
    body: JSON.stringify(component)
  }));
}

export async function updateComponent(record: CompositionComponent, component: CompositionComponentFields): Promise<CompositionComponent> {
  return CompositionComponentSchema.parse(await localServiceRequest(`/api/v1/composition-components/${record.id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision: record.revision, component })
  }));
}

export async function changeComponentLifecycle(record: CompositionComponent, action: "archive" | "restore", reason: string): Promise<CompositionComponent> {
  return CompositionComponentSchema.parse(await localServiceRequest(`/api/v1/composition-components/${record.id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision: record.revision, reason })
  }));
}

export function quantityToAtoms(value: string, scale: number): number | null {
  const normalized = value.trim();
  const match = /^(\d+)(?:\.(\d+))?$/.exec(normalized);
  if (!match || (match[2]?.length ?? 0) > scale) return null;
  const factor = 10 ** scale;
  const whole = Number.parseInt(match[1], 10);
  const fraction = Number.parseInt((match[2] ?? "").padEnd(scale, "0") || "0", 10);
  if (!Number.isSafeInteger(whole) || whole > Math.floor(Number.MAX_SAFE_INTEGER / factor)) return null;
  const atoms = whole * factor + fraction;
  return Number.isSafeInteger(atoms) && atoms > 0 ? atoms : null;
}

export function atomsToQuantity(atoms: number, scale: number): string {
  if (scale === 0) return String(atoms);
  const factor = 10 ** scale;
  const whole = Math.floor(atoms / factor);
  const fractional = String(atoms % factor).padStart(scale, "0").replace(/0+$/, "");
  return fractional ? `${whole}.${fractional}` : String(whole);
}

/**
 * Phase 1F tax classification.
 *
 * The Product identifies its HSN and Tax Category; the rate is resolved by the Store Service for a
 * date and is never stored on the Product. `asOf` is echoed back so the caller can always say which
 * date a displayed rate belongs to.
 */
export async function getTaxClassification(productId: string, asOf?: string): Promise<ProductTaxClassification> {
  const suffix = asOf ? `?asOf=${encodeURIComponent(asOf)}` : "";
  return ProductTaxClassificationSchema.parse(await localServiceRequest(`/api/v1/products/${productId}/tax-classification${suffix}`));
}

export async function updateTaxClassification(
  productId: string,
  expectedRevision: number,
  fields: { hsnCodeId: string | null; taxCategoryId: string | null },
  reason?: string
): Promise<ProductTaxClassification> {
  return ProductTaxClassificationSchema.parse(await localServiceRequest(`/api/v1/products/${productId}/tax-classification`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, ...fields, reason: reason ?? null })
  }));
}

/** Exact integer basis points to a display percentage. 100 basis points is 1.00%. */
export function basisPointsToPercentText(value: number): string {
  const whole = Math.trunc(value / 100);
  const fraction = Math.abs(value % 100);
  return `${whole}.${String(fraction).padStart(2, "0")}`;
}
