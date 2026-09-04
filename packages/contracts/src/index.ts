import { z } from "zod";

export const HealthResponseSchema = z.object({
  status: z.literal("ok"),
  apiVersion: z.literal("v1"),
  applicationVersion: z.string()
});

export const SystemInfoResponseSchema = z.object({
  status: z.literal("ok"),
  apiVersion: z.literal("v1"),
  applicationVersion: z.string(),
  compatibility: z.object({
    minimumWebVersion: z.string(),
    maximumWebMajorVersion: z.number().int().nonnegative()
  })
});

export type HealthResponse = z.infer<typeof HealthResponseSchema>;
export type SystemInfoResponse = z.infer<typeof SystemInfoResponseSchema>;

export const ReferenceKindSchema = z.enum([
  "units",
  "dosage-forms",
  "companies",
  "company-identifiers",
  "brands",
  "hsn-codes",
  "tax-categories",
  "tax-rate-versions",
  "regulatory-categories"
]);

export const MasterStatusSchema = z.enum(["active", "archived"]);
export const VerificationStateSchema = z.enum(["unverified", "verified", "rejected"]);

export const UnitAttributesSchema = z.object({
  canonicalCode: z.string(),
  displayName: z.string(),
  dimension: z.enum(["count", "container", "volume", "mass"]),
  isDiscrete: z.boolean(),
  allowedScale: z.number().int()
});

export const DosageFormAttributesSchema = z.object({
  canonicalCode: z.string(),
  displayName: z.string(),
  description: z.string().nullable().optional(),
  routeHint: z.string().nullable().optional(),
  releaseHint: z.string().nullable().optional()
});

export const CompanyAttributesSchema = z.object({
  displayName: z.string(),
  legalName: z.string().nullable().optional(),
  normalizedSearchName: z.string().optional(),
  city: z.string().nullable().optional(),
  state: z.string().nullable().optional(),
  countryCode: z.string().nullable().optional()
});

export const CompanyIdentifierAttributesSchema = z.object({
  companyId: z.string(),
  namespace: z.string(),
  normalizedValue: z.string(),
  verificationState: VerificationStateSchema
});

export const BrandAttributesSchema = z.object({
  displayName: z.string(),
  normalizedSearchName: z.string().optional(),
  brandOwnerCompanyId: z.string().nullable().optional()
});

export const HsnAttributesSchema = z.object({
  jurisdiction: z.string(),
  hsnCode: z.string(),
  description: z.string()
});

export const TaxCategoryAttributesSchema = z.object({
  jurisdiction: z.string(),
  categoryCode: z.string(),
  displayName: z.string(),
  taxTreatment: z.enum(["taxable", "exempt", "nil_rated", "non_gst"])
});

export const TaxRateVersionAttributesSchema = z.object({
  taxCategoryId: z.string(),
  effectiveFrom: z.string(),
  effectiveTo: z.string().nullable().optional(),
  cgstBasisPoints: z.number().int(),
  sgstBasisPoints: z.number().int(),
  igstBasisPoints: z.number().int(),
  cessBasisPoints: z.number().int()
});

export const RegulatoryCategoryAttributesSchema = z.object({
  jurisdiction: z.string(),
  categorySystem: z.string(),
  categoryCode: z.string(),
  displayName: z.string(),
  effectiveFrom: z.string().nullable().optional(),
  effectiveTo: z.string().nullable().optional(),
  sourceReference: z.string().nullable().optional(),
  verificationState: VerificationStateSchema
});

export const ReferenceAttributesSchema = z.union([
  UnitAttributesSchema,
  DosageFormAttributesSchema,
  CompanyAttributesSchema,
  CompanyIdentifierAttributesSchema,
  BrandAttributesSchema,
  HsnAttributesSchema,
  TaxCategoryAttributesSchema,
  TaxRateVersionAttributesSchema,
  RegulatoryCategoryAttributesSchema
]);

export const ReferenceMasterResponseSchema = z.object({
  id: z.string(),
  kind: ReferenceKindSchema,
  revision: z.number().int(),
  status: MasterStatusSchema,
  attributes: ReferenceAttributesSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const CreateReferenceRequestSchema = z.object({
  attributes: ReferenceAttributesSchema,
  reason: z.string().nullable().optional()
});

export const UpdateReferenceRequestSchema = z.object({
  expectedRevision: z.number().int(),
  attributes: ReferenceAttributesSchema,
  reason: z.string().nullable().optional()
});

export const ReferenceLifecycleRequestSchema = z.object({
  expectedRevision: z.number().int(),
  reason: z.string()
});

export const ReferenceErrorResponseSchema = z.object({
  code: z.enum([
    "validation_failed",
    "duplicate_conflict",
    "revision_conflict",
    "not_found",
    "archived_conflict",
    "effective_date_overlap",
    "internal_error"
  ]),
  message: z.string(),
  issues: z.array(z.object({ field: z.string(), message: z.string() })),
  expectedRevision: z.number().int().nullable(),
  currentRevision: z.number().int().nullable()
});

export type ReferenceKind = z.infer<typeof ReferenceKindSchema>;
export type ReferenceMasterResponse = z.infer<typeof ReferenceMasterResponseSchema>;
export type CreateReferenceRequest = z.infer<typeof CreateReferenceRequestSchema>;
export type UpdateReferenceRequest = z.infer<typeof UpdateReferenceRequestSchema>;
export type ReferenceLifecycleRequest = z.infer<typeof ReferenceLifecycleRequestSchema>;
export type ReferenceErrorResponse = z.infer<typeof ReferenceErrorResponseSchema>;

export const ProductKindSchema = z.enum([
  "medicine",
  "device",
  "general_pharmacy_item"
]);

export const ProductFieldsSchema = z.object({
  productKind: ProductKindSchema,
  brandId: z.string().nullable().optional(),
  dosageFormId: z.string().nullable().optional(),
  baseUnitId: z.string(),
  quantityScale: z.number().int().min(0).max(6),
  formulationDescriptor: z.string().nullable().optional(),
  routeDescriptor: z.string().nullable().optional(),
  releaseDescriptor: z.string().nullable().optional(),
  displayName: z.string()
});

export const ProductCompanyRoleFieldsSchema = z.object({
  companyId: z.string(),
  role: z.enum(["manufacturer", "marketer", "brand_owner", "importer"]),
  effectiveFrom: z.string().nullable().optional(),
  effectiveTo: z.string().nullable().optional()
});

export const ProductCompanyRoleSchema = ProductCompanyRoleFieldsSchema.extend({
  id: z.string(),
  productId: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const ProductPackFieldsSchema = z.object({
  containerUnitId: z.string(),
  baseQuantityAtoms: z.number().int().positive(),
  containedPackId: z.string().nullable().optional(),
  containedPackCount: z.number().int().positive().nullable().optional(),
  skuCode: z.string().nullable().optional(),
  skuStoreId: z.string().nullable().optional(),
  displayLabel: z.string().nullable().optional()
});

export const ProductPackSchema = ProductPackFieldsSchema.extend({
  id: z.string(),
  productId: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const StorePackPolicyFieldsSchema = z.object({
  storeId: z.string(),
  purchaseEnabled: z.boolean(),
  saleEnabled: z.boolean(),
  wholePackOnlyPurchase: z.boolean(),
  fractionalSaleAllowed: z.boolean(),
  minimumSaleIncrementAtoms: z.number().int().positive(),
  defaultPurchasePack: z.boolean(),
  defaultSalePack: z.boolean()
});

export const StorePackPolicySchema = StorePackPolicyFieldsSchema.extend({
  id: z.string(),
  productId: z.string(),
  packId: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const BarcodeFieldsSchema = z.object({
  namespace: z.string(),
  value: z.string(),
  symbology: z.string().nullable().optional(),
  scope: z.enum(["global", "store"]),
  storeId: z.string().nullable().optional()
});

export const BarcodeLookupSchema = z.object({
  namespace: z.string(),
  value: z.string(),
  scope: z.enum(["global", "store"]),
  storeId: z.string().nullable().optional()
});

export const BarcodeSchema = z.object({
  id: z.string(),
  packId: z.string(),
  namespace: z.string(),
  normalizedValue: z.string(),
  symbology: z.string().nullable(),
  scope: z.enum(["global", "store"]),
  storeId: z.string().nullable(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const ProductSchema = ProductFieldsSchema.extend({
  id: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const ProductDetailSchema = ProductSchema.extend({
  companyRoles: z.array(ProductCompanyRoleSchema),
  packs: z.array(ProductPackSchema)
});

export const AggregatePackSchema = ProductPackFieldsSchema.extend({
  clientKey: z.string(),
  containedPackClientKey: z.string().nullable().optional(),
  policy: StorePackPolicyFieldsSchema.nullable().optional(),
  barcodes: z.array(BarcodeFieldsSchema).default([])
});

export const CreateProductRequestSchema = z.object({
  product: ProductFieldsSchema,
  companyRoles: z.array(ProductCompanyRoleFieldsSchema).default([]),
  packs: z.array(AggregatePackSchema).default([]),
  reason: z.string().nullable().optional()
});

export const UpdateProductRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  product: ProductFieldsSchema,
  reason: z.string().nullable().optional()
});

export const UpdateProductPackRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  pack: ProductPackFieldsSchema,
  reason: z.string().nullable().optional()
});

export const UpdateProductCompanyRoleRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  role: ProductCompanyRoleFieldsSchema,
  reason: z.string().nullable().optional()
});

export const UpdateStorePackPolicyRequestSchema = z.object({
  expectedRevision: z.number().int().positive().nullable().optional(),
  policy: StorePackPolicyFieldsSchema,
  reason: z.string().nullable().optional()
});

export const ProductLifecycleRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  reason: z.string()
});

export const DuplicateCandidateSchema = z.object({
  candidateId: z.string(),
  score: z.number().int().nonnegative(),
  reasonCodes: z.array(z.string()),
  explanation: z.string()
});

export const CatalogErrorResponseSchema = z.object({
  code: z.enum([
    "validation_failed",
    "duplicate_conflict",
    "revision_conflict",
    "not_found",
    "archived_conflict",
    "conversion_conflict",
    "barcode_conflict",
    "default_pack_conflict",
    "internal_error"
  ]),
  message: z.string(),
  issues: z.array(z.object({ field: z.string(), message: z.string() })),
  expectedRevision: z.number().int().nullable(),
  currentRevision: z.number().int().nullable()
});

export type ProductFields = z.infer<typeof ProductFieldsSchema>;
export type Product = z.infer<typeof ProductSchema>;
export type ProductDetail = z.infer<typeof ProductDetailSchema>;
export type ProductCompanyRole = z.infer<typeof ProductCompanyRoleSchema>;
export type ProductPack = z.infer<typeof ProductPackSchema>;
export type StorePackPolicy = z.infer<typeof StorePackPolicySchema>;
export type Barcode = z.infer<typeof BarcodeSchema>;
export type DuplicateCandidate = z.infer<typeof DuplicateCandidateSchema>;
export type CreateProductRequest = z.infer<typeof CreateProductRequestSchema>;
export type CatalogErrorResponse = z.infer<typeof CatalogErrorResponseSchema>;
