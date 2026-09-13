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
  "regulatory-categories",
  "ingredients",
  "salt-forms",
  "strength-units"
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

/** The active moiety, independent of any salt form or strength. */
export const IngredientAttributesSchema = z.object({
  canonicalCode: z.string(),
  displayName: z.string(),
  normalizedSearchName: z.string().optional(),
  description: z.string().nullable().optional()
});

/** The chemical form modifier applied to an ingredient — never the ingredient itself. */
export const SaltFormAttributesSchema = z.object({
  canonicalCode: z.string(),
  displayName: z.string(),
  normalizedSearchName: z.string().optional()
});

/** Units a pharmaceutical strength may be expressed in, separate from inventory units of measure. */
export const StrengthUnitAttributesSchema = z.object({
  canonicalCode: z.string(),
  displayName: z.string(),
  dimension: z.enum(["mass", "volume", "count", "activity", "substance_equivalent"]),
  allowedScale: z.number().int()
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
  RegulatoryCategoryAttributesSchema,
  IngredientAttributesSchema,
  SaltFormAttributesSchema,
  StrengthUnitAttributesSchema
]);

const ReferenceMasterBaseSchema = z.object({
  id: z.string(),
  revision: z.number().int(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const ReferenceMasterResponseSchema = z.discriminatedUnion("kind", [
  ReferenceMasterBaseSchema.extend({ kind: z.literal("units"), attributes: UnitAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("dosage-forms"), attributes: DosageFormAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("companies"), attributes: CompanyAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("company-identifiers"), attributes: CompanyIdentifierAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("brands"), attributes: BrandAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("hsn-codes"), attributes: HsnAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("tax-categories"), attributes: TaxCategoryAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("tax-rate-versions"), attributes: TaxRateVersionAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("regulatory-categories"), attributes: RegulatoryCategoryAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("ingredients"), attributes: IngredientAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("salt-forms"), attributes: SaltFormAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("strength-units"), attributes: StrengthUnitAttributesSchema })
]);

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
    "internal_error",
    "authentication_required",
    "session_expired",
    "authorization_denied",
    "service_busy"
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

/**
 * Manufacturer-stated composition of one medicine Product. Business identity only: nothing here
 * asserts generic, therapeutic, clinical, or substitution equivalence.
 */
export const CompositionComponentFieldsSchema = z.object({
  ingredientId: z.string(),
  saltFormId: z.string().nullable().optional(),
  componentRole: z.enum(["active", "inactive"]).default("active"),
  displayOrder: z.number().int().nonnegative().nullable().optional(),
  strengthPresentation: z.enum(["absolute", "percentage"]).default("absolute"),
  strengthNumeratorAtoms: z.number().int().positive(),
  strengthNumeratorScale: z.number().int().min(0).max(6),
  strengthNumeratorUnitId: z.string(),
  strengthDenominatorAtoms: z.number().int().positive().nullable().optional(),
  strengthDenominatorScale: z.number().int().min(0).max(6).nullable().optional(),
  strengthDenominatorUnitId: z.string().nullable().optional()
});

export const CompositionComponentSchema = CompositionComponentFieldsSchema.extend({
  id: z.string(),
  productId: z.string(),
  displayOrder: z.number().int().nonnegative(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const UpdateCompositionComponentRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  component: CompositionComponentFieldsSchema,
  reason: z.string().nullable().optional()
});

export const ProductDetailSchema = ProductSchema.extend({
  companyRoles: z.array(ProductCompanyRoleSchema),
  packs: z.array(ProductPackSchema),
  composition: z.array(CompositionComponentSchema).default([])
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

export const CatalogContextSchema = z.object({
  storeId: z.string()
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
    "composition_conflict",
    "authentication_required",
    "session_expired",
    "authorization_denied",
    "service_busy",
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
export type ProductCompanyRoleFields = z.infer<typeof ProductCompanyRoleFieldsSchema>;
export type ProductPack = z.infer<typeof ProductPackSchema>;
export type ProductPackFields = z.infer<typeof ProductPackFieldsSchema>;
export type StorePackPolicy = z.infer<typeof StorePackPolicySchema>;
export type CompositionComponent = z.infer<typeof CompositionComponentSchema>;
export type CompositionComponentFields = z.infer<typeof CompositionComponentFieldsSchema>;
export type StorePackPolicyFields = z.infer<typeof StorePackPolicyFieldsSchema>;
export type Barcode = z.infer<typeof BarcodeSchema>;
export type BarcodeLookup = z.infer<typeof BarcodeLookupSchema>;
export type DuplicateCandidate = z.infer<typeof DuplicateCandidateSchema>;
export type CatalogContext = z.infer<typeof CatalogContextSchema>;
export type CreateProductRequest = z.infer<typeof CreateProductRequestSchema>;
export type CatalogErrorResponse = z.infer<typeof CatalogErrorResponseSchema>;

export const UserRoleSchema = z.enum(["owner_admin", "pharmacist", "cashier"]);

export const SafeUserSchema = z.object({
  id: z.string(),
  loginIdentifier: z.string(),
  displayName: z.string(),
  role: UserRoleSchema,
  revision: z.number().int().positive()
});

export const AuthStatusResponseSchema = z.object({
  setupRequired: z.boolean(),
  authenticated: z.boolean(),
  user: SafeUserSchema.nullable(),
  storeDisplayName: z.string().nullable()
});

export const SessionResponseSchema = z.object({
  user: SafeUserSchema,
  storeDisplayName: z.string(),
  expiresAtUtc: z.string()
});

export const SetupRequestSchema = z.object({
  storeDisplayName: z.string(),
  ownerDisplayName: z.string(),
  loginIdentifier: z.string(),
  password: z.string()
});

export const LoginRequestSchema = z.object({
  loginIdentifier: z.string(),
  password: z.string()
});

export const DashboardSummarySchema = z.object({
  storeDisplayName: z.string(),
  activeProductCount: z.number().int().nonnegative(),
  activePackCount: z.number().int().nonnegative()
});

export const AuthErrorResponseSchema = z.object({
  code: z.enum([
    "validation_failed",
    "setup_unavailable",
    "invalid_credentials",
    "rate_limited",
    "authentication_required",
    "session_expired",
    "authorization_denied",
    "service_busy",
    "internal_error"
  ]),
  message: z.string(),
  issues: z.array(z.object({ field: z.string(), message: z.string() })),
  retryAfterSeconds: z.number().int().positive().nullable()
});

export type UserRole = z.infer<typeof UserRoleSchema>;
export type SafeUser = z.infer<typeof SafeUserSchema>;
export type AuthStatusResponse = z.infer<typeof AuthStatusResponseSchema>;
export type SessionResponse = z.infer<typeof SessionResponseSchema>;
export type SetupRequest = z.infer<typeof SetupRequestSchema>;
export type LoginRequest = z.infer<typeof LoginRequestSchema>;
export type DashboardSummary = z.infer<typeof DashboardSummarySchema>;
export type AuthErrorResponse = z.infer<typeof AuthErrorResponseSchema>;
