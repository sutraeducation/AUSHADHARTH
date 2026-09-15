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
  "controlled-formulations",
  "price-control-versions",
  "regulatory-categories",
  "ingredients",
  "salt-forms",
  "strength-units",
  "state-codes"
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

/**
 * A notified formulation a ceiling price belongs to. `strengthText` records the notification as
 * written and is deliberately never parsed — a Product is linked by explicit assignment, never by
 * matching this text.
 */
export const ControlledFormulationAttributesSchema = z.object({
  jurisdiction: z.string(),
  formulationCode: z.string(),
  displayName: z.string(),
  dosageFormId: z.string().nullable().optional(),
  strengthText: z.string().nullable().optional(),
  verificationState: z.enum(["unverified", "verified", "rejected"]),
  sourceNote: z.string().nullable().optional()
});

/** What a ceiling price is quoted per. A per-pack ceiling is recorded but never divided. */
export const CeilingBasisSchema = z.enum(["per_base_unit", "per_pack"]);

export const PriceControlVersionAttributesSchema = z.object({
  controlledFormulationId: z.string(),
  effectiveFrom: z.string(),
  effectiveTo: z.string().nullable().optional(),
  /** Exact integer minor units per ADR-009, exclusive of GST. Never a binary float. */
  ceilingPricePaise: z.number().int().positive(),
  ceilingBasis: CeilingBasisSchema,
  ceilingBasisUnitId: z.string().nullable().optional(),
  notificationReference: z.string().nullable().optional(),
  sourceNote: z.string().nullable().optional()
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

/**
 * A jurisdiction's State. The code is the one a GSTIN carries in its first two positions, which is
 * what place-of-supply agreement is checked against.
 */
export const StateCodeAttributesSchema = z.object({
  jurisdiction: z.string(),
  stateCode: z.string(),
  displayName: z.string()
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
  ControlledFormulationAttributesSchema,
  PriceControlVersionAttributesSchema,
  RegulatoryCategoryAttributesSchema,
  IngredientAttributesSchema,
  SaltFormAttributesSchema,
  StrengthUnitAttributesSchema,
  StateCodeAttributesSchema
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
  ReferenceMasterBaseSchema.extend({ kind: z.literal("controlled-formulations"), attributes: ControlledFormulationAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("price-control-versions"), attributes: PriceControlVersionAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("regulatory-categories"), attributes: RegulatoryCategoryAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("ingredients"), attributes: IngredientAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("salt-forms"), attributes: SaltFormAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("strength-units"), attributes: StrengthUnitAttributesSchema }),
  ReferenceMasterBaseSchema.extend({ kind: z.literal("state-codes"), attributes: StateCodeAttributesSchema })
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
export type ControlledFormulationAttributes = z.infer<typeof ControlledFormulationAttributesSchema>;
export type PriceControlVersionAttributes = z.infer<typeof PriceControlVersionAttributesSchema>;
export type CeilingBasis = z.infer<typeof CeilingBasisSchema>;
export type PriceControlStatus = z.infer<typeof PriceControlStatusSchema>;
export type Comparability = z.infer<typeof ComparabilitySchema>;
export type ApplicableCeiling = z.infer<typeof ApplicableCeilingSchema>;
export type ProductPriceControl = z.infer<typeof ProductPriceControlSchema>;
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

/**
 * Phase 1F tax classification. A Product identifies *which* HSN and Tax Category apply; it never
 * carries a rate. The rate in force is resolved from the Tax Category's effective-dated versions,
 * and a future posted document snapshots what it actually applied.
 */
export const ProductTaxClassificationFieldsSchema = z.object({
  hsnCodeId: z.string().nullable().optional(),
  taxCategoryId: z.string().nullable().optional()
});

/** Every component is exact integer basis points: 100 = 1.00%. Never a floating-point value. */
export const ApplicableTaxRateSchema = z.object({
  taxRateVersionId: z.string(),
  effectiveFrom: z.string(),
  effectiveTo: z.string().nullable(),
  cgstBasisPoints: z.number().int(),
  sgstBasisPoints: z.number().int(),
  igstBasisPoints: z.number().int(),
  cessBasisPoints: z.number().int()
});

/**
 * Whether anyone has assessed this Product for price control, and what they concluded.
 *
 * `unknown` is a real state, never to be read as "not controlled": a medicine appearing
 * uncontrolled because reference data is incomplete is the failure Phase 1H-0 exists to prevent.
 */
export const PriceControlStatusSchema = z.enum(["unknown", "not_applicable", "controlled"]);

/** Whether a resolved ceiling can be compared with a selling rate at all, or why it cannot. */
export const ComparabilitySchema = z.enum([
  "comparable",
  "incomparable_pack_basis",
  "incomparable_unit"
]);

export const ApplicableCeilingSchema = z.object({
  priceControlVersionId: z.string(),
  effectiveFrom: z.string(),
  effectiveTo: z.string().nullable(),
  ceilingPricePaise: z.number().int(),
  ceilingBasis: CeilingBasisSchema,
  ceilingBasisUnitId: z.string().nullable(),
  notificationReference: z.string().nullable()
});

export const ProductPriceControlSchema = z.object({
  productId: z.string(),
  revision: z.number().int().positive(),
  priceControlStatus: PriceControlStatusSchema,
  controlledFormulationId: z.string().nullable(),
  /** The date the ceiling was resolved for, so it can never be mistaken for Product metadata. */
  asOf: z.string(),
  applicableCeiling: ApplicableCeilingSchema.nullable(),
  comparability: ComparabilitySchema.nullable(),
  /** True only when the Product is controlled AND a ceiling actually resolves on this date. */
  resolved: z.boolean()
});

export const UpdateProductPriceControlRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  priceControlStatus: PriceControlStatusSchema,
  controlledFormulationId: z.string().nullable().optional(),
  reason: z.string().nullable().optional()
});

export const ProductTaxClassificationSchema = ProductTaxClassificationFieldsSchema.extend({
  productId: z.string(),
  revision: z.number().int().positive(),
  complete: z.boolean(),
  /** The date the rate was resolved for, so it can never be mistaken for Product metadata. */
  asOf: z.string(),
  applicableRate: ApplicableTaxRateSchema.nullable()
});

export const UpdateProductTaxClassificationRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  hsnCodeId: z.string().nullable(),
  taxCategoryId: z.string().nullable(),
  reason: z.string().nullable().optional()
});

export const ProductSchema = ProductFieldsSchema.extend({
  id: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  hsnCodeId: z.string().nullable().optional(),
  taxCategoryId: z.string().nullable().optional(),
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

/**
 * One manufactured lot of one Product Pack. Identity and commercial metadata only — a batch carries
 * no quantity, balance, or stock figure, and recording one is never a stock receipt.
 * `mrpPaise` is exact integer minor units per ADR-009; binary floating-point money is never used.
 */
export const BatchFieldsSchema = z.object({
  batchNumber: z.string(),
  manufacturedOn: z.string().nullable().optional(),
  expiresOn: z.string().nullable().optional(),
  mrpPaise: z.number().int().positive().nullable().optional()
});

export const BatchSchema = BatchFieldsSchema.extend({
  id: z.string(),
  productPackId: z.string(),
  normalizedBatchNumber: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const UpdateBatchRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  batch: BatchFieldsSchema,
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
    "batch_conflict",
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
export type Batch = z.infer<typeof BatchSchema>;
export type BatchFields = z.infer<typeof BatchFieldsSchema>;
export type CompositionComponent = z.infer<typeof CompositionComponentSchema>;
export type CompositionComponentFields = z.infer<typeof CompositionComponentFieldsSchema>;
export type StorePackPolicyFields = z.infer<typeof StorePackPolicyFieldsSchema>;
export type Barcode = z.infer<typeof BarcodeSchema>;
export type BarcodeLookup = z.infer<typeof BarcodeLookupSchema>;
export type DuplicateCandidate = z.infer<typeof DuplicateCandidateSchema>;
export type CatalogContext = z.infer<typeof CatalogContextSchema>;
export type CreateProductRequest = z.infer<typeof CreateProductRequestSchema>;
export type CatalogErrorResponse = z.infer<typeof CatalogErrorResponseSchema>;
export type ProductTaxClassification = z.infer<typeof ProductTaxClassificationSchema>;
export type ProductTaxClassificationFields = z.infer<typeof ProductTaxClassificationFieldsSchema>;
export type ApplicableTaxRate = z.infer<typeof ApplicableTaxRateSchema>;
export type UpdateProductTaxClassificationRequest = z.infer<
  typeof UpdateProductTaxClassificationRequestSchema
>;

/**
 * Phase 1D inventory ledger. Quantity authority is exact integer atoms in the Product's base unit;
 * a balance is always derived by summing movements and is never stored anywhere.
 */
/**
 * `purchase` and `sale` are written only by posting their own documents, never by the movement
 * endpoint, but both must be listed here: the ledger returns them like any other movement, and an
 * enum that omits one fails to parse the whole page.
 */
/** Which stock a quantity is: what the counter may sell, versus what is merely in the building. */
export const StockStatusSchema = z.enum(["sellable", "quarantined", "non_sellable"]);

export const MovementTypeSchema = z.enum([
  "opening_stock", "adjustment", "purchase", "sale",
  "sales_return", "purchase_return", "disposition_transfer"
]);

export const PostMovementRequestSchema = z.object({
  idempotencyKey: z.string(),
  movementType: MovementTypeSchema,
  productPackId: z.string(),
  batchId: z.string().nullable().optional(),
  quantityDeltaAtoms: z.number().int(),
  occurredOn: z.string(),
  reason: z.string().nullable().optional(),
  reversesMovementId: z.string().nullable().optional()
});

export const InventoryMovementSchema = z.object({
  id: z.string(),
  storeId: z.string(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string().nullable(),
  movementType: MovementTypeSchema,
  quantityDeltaAtoms: z.number().int(),
  occurredOn: z.string(),
  reason: z.string().nullable(),
  reversesMovementId: z.string().nullable(),
  /** The purchase line that caused this inward, when one did. */
  purchaseLineId: z.string().nullable(),
  /** The sale line that caused this outward, when one did. */
  saleLineId: z.string().nullable(),
  /** The return line that caused this compensating movement, when one did. */
  returnLineId: z.string().nullable(),
  /** The disposition transfer that moved this quantity between stock statuses, when one did. */
  stockDispositionId: z.string().nullable(),
  /** Which stock this movement belongs to. The counter may only sell `sellable`. */
  stockStatus: StockStatusSchema,
  idempotencyKey: z.string(),
  postedByUserId: z.string(),
  postedAtUtc: z.string()
});

export const StockBalanceSchema = z.object({
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string().nullable(),
  /** Balances are reported per status, never merged: unavailable stock is not sellable stock. */
  stockStatus: StockStatusSchema,
  balanceAtoms: z.number().int()
});

export const InventoryErrorResponseSchema = z.object({
  code: z.enum([
    "validation_failed",
    "not_found",
    "archived_conflict",
    "batch_pack_mismatch",
    "insufficient_stock",
    "authentication_required",
    "session_expired",
    "authorization_denied",
    "service_busy",
    "internal_error"
  ]),
  message: z.string(),
  issues: z.array(z.object({ field: z.string(), message: z.string() })),
  availableAtoms: z.number().int().nullable()
});

export type MovementType = z.infer<typeof MovementTypeSchema>;
export type InventoryMovement = z.infer<typeof InventoryMovementSchema>;
export type PostMovementRequest = z.infer<typeof PostMovementRequestSchema>;
export type StockBalance = z.infer<typeof StockBalanceSchema>;
export type InventoryErrorResponse = z.infer<typeof InventoryErrorResponseSchema>;

/**
 * Phase 1E party identity.
 *
 * A Party is an identity, never an account. Nothing here carries a balance, an outstanding amount,
 * or a credit limit, and nothing may be added that does: a future accounting ledger will reference
 * the Party and derive every figure from its own postings, exactly as the inventory ledger is the
 * sole authority for quantity.
 */
export const PartyRoleNameSchema = z.enum(["supplier", "customer"]);
export const GstRegistrationStatusSchema = z.enum(["registered", "unregistered", "unknown"]);
export const AddressRoleSchema = z.enum(["billing", "shipping"]);

export const PartyFieldsSchema = z.object({
  displayName: z.string(),
  legalName: z.string().nullable().optional(),
  /** `unregistered` asserts there is no GSTIN; `unknown` means none has been captured yet. */
  gstRegistrationStatus: GstRegistrationStatusSchema.default("unknown"),
  gstin: z.string().nullable().optional(),
  pan: z.string().nullable().optional(),
  placeOfSupplyStateId: z.string().nullable().optional(),
  primaryPhone: z.string().nullable().optional(),
  primaryEmail: z.string().nullable().optional(),
  /** The licence string exactly as printed; nothing reads it programmatically. */
  drugLicenceNumber: z.string().nullable().optional(),
  drugLicenceValidUpto: z.string().nullable().optional()
});

const PartyLifecycleSchema = z.object({
  id: z.string(),
  revision: z.number().int().positive(),
  status: MasterStatusSchema,
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  archivedAtUtc: z.string().nullable(),
  archiveReason: z.string().nullable()
});

export const PartySchema = PartyFieldsSchema.extend({
  normalizedSearchName: z.string(),
  normalizedGstin: z.string().nullable(),
  normalizedPan: z.string().nullable()
}).merge(PartyLifecycleSchema);

export const PartyRoleFieldsSchema = z.object({ role: PartyRoleNameSchema });

export const PartyRoleSchema = PartyRoleFieldsSchema.extend({ partyId: z.string() }).merge(
  PartyLifecycleSchema
);

export const PartyAddressFieldsSchema = z.object({
  addressRole: AddressRoleSchema,
  line1: z.string(),
  line2: z.string().nullable().optional(),
  city: z.string().nullable().optional(),
  stateId: z.string().nullable().optional(),
  postalCode: z.string().nullable().optional(),
  countryCode: z.string().default("IN"),
  isPrimary: z.boolean().default(false)
});

export const PartyAddressSchema = PartyAddressFieldsSchema.extend({
  partyId: z.string()
}).merge(PartyLifecycleSchema);

export const PartyDetailSchema = PartySchema.extend({
  roles: z.array(PartyRoleSchema).default([]),
  addresses: z.array(PartyAddressSchema).default([])
});

export const CreatePartyRequestSchema = z.object({
  party: PartyFieldsSchema,
  roles: z.array(PartyRoleFieldsSchema).default([]),
  addresses: z.array(PartyAddressFieldsSchema).default([]),
  reason: z.string().nullable().optional()
});

export const UpdatePartyRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  party: PartyFieldsSchema,
  reason: z.string().nullable().optional()
});

export const UpdatePartyRoleRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  role: PartyRoleFieldsSchema,
  reason: z.string().nullable().optional()
});

export const UpdatePartyAddressRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  address: PartyAddressFieldsSchema,
  reason: z.string().nullable().optional()
});

export const PartyLifecycleRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  reason: z.string()
});

export const PartyErrorResponseSchema = z.object({
  code: z.enum([
    "validation_failed",
    "duplicate_conflict",
    "revision_conflict",
    "not_found",
    "archived_conflict",
    "party_conflict",
    "party_role_conflict",
    "party_address_conflict",
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

export type PartyRoleName = z.infer<typeof PartyRoleNameSchema>;
export type GstRegistrationStatus = z.infer<typeof GstRegistrationStatusSchema>;
export type AddressRole = z.infer<typeof AddressRoleSchema>;
export type PartyFields = z.infer<typeof PartyFieldsSchema>;
export type Party = z.infer<typeof PartySchema>;
export type PartyDetail = z.infer<typeof PartyDetailSchema>;
export type PartyRole = z.infer<typeof PartyRoleSchema>;
export type PartyRoleFields = z.infer<typeof PartyRoleFieldsSchema>;
export type PartyAddress = z.infer<typeof PartyAddressSchema>;
export type PartyAddressFields = z.infer<typeof PartyAddressFieldsSchema>;
export type CreatePartyRequest = z.infer<typeof CreatePartyRequestSchema>;
export type UpdatePartyRequest = z.infer<typeof UpdatePartyRequestSchema>;
export type PartyErrorResponse = z.infer<typeof PartyErrorResponseSchema>;
export type StateCodeAttributes = z.infer<typeof StateCodeAttributesSchema>;

/**
 * Phase 1G-0 store tax identity.
 *
 * The Store's own GST registration and place of supply — the half of the tax-treatment comparison
 * that Phase 1E supplied for the supplier. `complete` is true once a place of supply exists, which
 * is the single fact a GST-aware document needs; an unregistered store can still be complete.
 */
export const StoreTaxIdentitySchema = z.object({
  storeId: z.string(),
  displayName: z.string(),
  revision: z.number().int().positive(),
  gstRegistrationStatus: GstRegistrationStatusSchema,
  gstin: z.string().nullable(),
  normalizedGstin: z.string().nullable(),
  placeOfSupplyStateId: z.string().nullable(),
  complete: z.boolean()
});

export const UpdateStoreTaxIdentityRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  gstRegistrationStatus: GstRegistrationStatusSchema,
  gstin: z.string().nullable(),
  placeOfSupplyStateId: z.string().nullable(),
  reason: z.string().nullable().optional()
});

export type StoreTaxIdentity = z.infer<typeof StoreTaxIdentitySchema>;
export type UpdateStoreTaxIdentityRequest = z.infer<typeof UpdateStoreTaxIdentityRequestSchema>;

/**
 * Phase 1G purchase inward.
 *
 * A draft carries only commercial intent. Every tax figure, the inventory atoms, the Store, and the
 * actor are derived by the Store Service — a value for any of them in a request body is ignored, so
 * the draft input schemas deliberately cannot express them.
 */
export const PurchaseStatusSchema = z.enum(["draft", "posted"]);
export const PurchaseTaxTreatmentSchema = z.enum(["intra_state", "inter_state"]);
export const PurchaseLineTaxKindSchema = z.enum(["taxable", "exempt", "nil_rated", "non_gst"]);

/** Server-owned header facts. Snapshot fields are null until the document is posted. */
export const PurchaseSchema = z.object({
  id: z.string(),
  storeId: z.string(),
  supplierPartyId: z.string(),
  supplierInvoiceNumber: z.string(),
  normalizedSupplierInvoiceNumber: z.string(),
  invoiceDate: z.string(),
  status: PurchaseStatusSchema,
  revision: z.number().int().positive(),
  supplierDisplayName: z.string().nullable(),
  supplierGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  supplierNormalizedGstin: z.string().nullable(),
  supplierPlaceOfSupplyStateId: z.string().nullable(),
  supplierStateCode: z.string().nullable(),
  storeGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  storeNormalizedGstin: z.string().nullable(),
  storePlaceOfSupplyStateId: z.string().nullable(),
  storeStateCode: z.string().nullable(),
  taxTreatment: PurchaseTaxTreatmentSchema.nullable(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  grandTotalPaise: z.number().int(),
  createdByUserId: z.string(),
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  postedByUserId: z.string().nullable(),
  postedAtUtc: z.string().nullable()
});

/**
 * A line as the server returns it. `quantityAtoms` and every paise and basis-point field are
 * derived; they appear here because the UI displays them, never because it supplies them.
 */
export const PurchaseLineSchema = z.object({
  id: z.string(),
  purchaseDocumentId: z.string(),
  lineNumber: z.number().int().positive(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string().nullable(),
  newBatchNumber: z.string().nullable(),
  newBatchExpiresOn: z.string().nullable(),
  newBatchMrpPaise: z.number().int().nullable(),
  quantityPacks: z.number().int().positive(),
  ratePerPackPaise: z.number().int(),
  quantityAtoms: z.number().int(),
  taxableValuePaise: z.number().int(),
  hsnCodeId: z.string().nullable(),
  hsnCode: z.string().nullable(),
  taxCategoryId: z.string().nullable(),
  taxTreatmentKind: PurchaseLineTaxKindSchema.nullable(),
  taxRateVersionId: z.string().nullable(),
  cgstBasisPoints: z.number().int(),
  sgstBasisPoints: z.number().int(),
  igstBasisPoints: z.number().int(),
  cessBasisPoints: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  lineTotalPaise: z.number().int()
});

export const PurchaseDetailSchema = PurchaseSchema.extend({
  lines: z.array(PurchaseLineSchema).default([])
});

/** The only header facts a browser may supply. */
export const PurchaseDraftInputSchema = z.object({
  supplierPartyId: z.string(),
  supplierInvoiceNumber: z.string(),
  invoiceDate: z.string()
});

export const UpdatePurchaseDraftRequestSchema = PurchaseDraftInputSchema.extend({
  expectedRevision: z.number().int().positive()
});

/**
 * The only line facts a browser may supply: what was bought, in what pack, at what price, and
 * either an existing batch or a proposal for a new one — never both.
 */
export const PurchaseLineInputSchema = z.object({
  expectedRevision: z.number().int().positive(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string().nullable().optional(),
  newBatchNumber: z.string().nullable().optional(),
  newBatchExpiresOn: z.string().nullable().optional(),
  newBatchMrpPaise: z.number().int().positive().nullable().optional(),
  quantityPacks: z.number().int().positive(),
  ratePerPackPaise: z.number().int().nonnegative()
});

export const PostPurchaseRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  idempotencyKey: z.string()
});

/** Every code the purchase API can return, mirroring `api::purchases::PurchaseError`. */
export const PurchaseErrorCodeSchema = z.enum([
  "validation_failed",
  "purchase_not_found",
  "purchase_not_draft",
  "revision_conflict",
  "duplicate_supplier_invoice",
  "supplier_not_eligible",
  "store_tax_profile_incomplete",
  "supplier_tax_profile_incomplete",
  "product_tax_classification_incomplete",
  "tax_rate_not_found",
  "product_pack_mismatch",
  "batch_pack_mismatch",
  "batch_conflict",
  "arithmetic_overflow",
  "idempotency_conflict",
  "posting_conflict",
  "authentication_required",
  "session_expired",
  "authorization_denied",
  "service_busy",
  "internal_error"
]);

export type PurchaseStatus = z.infer<typeof PurchaseStatusSchema>;
export type PurchaseTaxTreatment = z.infer<typeof PurchaseTaxTreatmentSchema>;
export type PurchaseLineTaxKind = z.infer<typeof PurchaseLineTaxKindSchema>;
export type Purchase = z.infer<typeof PurchaseSchema>;
export type PurchaseLine = z.infer<typeof PurchaseLineSchema>;
export type PurchaseDetail = z.infer<typeof PurchaseDetailSchema>;
export type PurchaseDraftInput = z.infer<typeof PurchaseDraftInputSchema>;
export type PurchaseLineInput = z.infer<typeof PurchaseLineInputSchema>;
export type PurchaseErrorCode = z.infer<typeof PurchaseErrorCodeSchema>;

/**
 * Phase 1H sales / POS outward.
 *
 * The same discipline as Phase 1G: a draft carries only commercial intent, and every tax figure,
 * every snapshot, the invoice number, the inventory atoms, the Store and the actor are derived by
 * the Store Service. The input schemas below deliberately cannot express any of them.
 */
export const SaleStatusSchema = z.enum(["draft", "posted"]);
export const SaleTaxTreatmentSchema = z.enum(["intra_state", "inter_state"]);
export const SaleLineTaxKindSchema = z.enum(["taxable", "exempt", "nil_rated", "non_gst"]);
export const TenderMethodSchema = z.enum(["cash", "card", "upi"]);

/**
 * What a line's quantity and rate are counted in.
 *
 * `pack` is a whole-pack count at a rate per pack; `base_unit` is a count of base-unit atoms at a
 * rate per atom. There is deliberately no decimal pack quantity: three tablets out of a strip of ten
 * is `base_unit` with three atoms, not `0.3` of a pack.
 */
export const QuantityBasisSchema = z.enum(["pack", "base_unit"]);

// A posted line snapshots its price-control assessment using the Phase 1H-0 vocabulary above:
// `PriceControlStatusSchema` and `CeilingBasisSchema` are reused rather than restated, so the two
// phases can never drift apart on what 'unknown' or 'per_base_unit' means.

/** Server-owned header facts. Everything below `revision` is null until the sale is posted. */
export const SaleSchema = z.object({
  id: z.string(),
  storeId: z.string(),
  /** Null means a walk-in. There is deliberately no shared "Cash Customer" party row. */
  customerPartyId: z.string().nullable(),
  customerNameText: z.string().nullable(),
  businessDate: z.string(),
  status: SaleStatusSchema,
  revision: z.number().int().positive(),
  /** The invoice number the store itself issues, allocated at posting and never before. */
  seriesCode: z.string().nullable(),
  financialYear: z.string().nullable(),
  sequenceValue: z.number().int().positive().nullable(),
  documentNumber: z.string().nullable(),
  storeGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  storeNormalizedGstin: z.string().nullable(),
  storePlaceOfSupplyStateId: z.string().nullable(),
  storeStateCode: z.string().nullable(),
  customerDisplayName: z.string().nullable(),
  customerGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  customerNormalizedGstin: z.string().nullable(),
  customerStateCode: z.string().nullable(),
  taxTreatment: SaleTaxTreatmentSchema.nullable(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  grandTotalPaise: z.number().int(),
  createdByUserId: z.string(),
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  postedByUserId: z.string().nullable(),
  postedAtUtc: z.string().nullable()
});

/**
 * A line as the server returns it.
 *
 * `quantityAtoms` is derived from the basis and the frozen Pack model; the browser never supplies
 * atoms. `taxableValuePaise` is present on a draft (it is an exact integer product), but every tax
 * and snapshot field stays zero or null until posting resolves them.
 */
export const SaleLineSchema = z.object({
  id: z.string(),
  saleDocumentId: z.string(),
  lineNumber: z.number().int().positive(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  quantityBasis: QuantityBasisSchema,
  /** Present only for the `pack` basis, matching the schema's own CHECK. */
  quantityPacks: z.number().int().positive().nullable(),
  quantityAtoms: z.number().int().positive(),
  sellingRatePaise: z.number().int(),
  productDisplayName: z.string().nullable(),
  packDisplayLabel: z.string().nullable(),
  baseUnitLabel: z.string().nullable(),
  batchNumber: z.string().nullable(),
  batchExpiresOn: z.string().nullable(),
  batchMrpPaise: z.number().int().nullable(),
  hsnCodeId: z.string().nullable(),
  hsnCode: z.string().nullable(),
  taxCategoryId: z.string().nullable(),
  taxTreatmentKind: SaleLineTaxKindSchema.nullable(),
  taxRateVersionId: z.string().nullable(),
  cgstBasisPoints: z.number().int(),
  sgstBasisPoints: z.number().int(),
  igstBasisPoints: z.number().int(),
  cessBasisPoints: z.number().int(),
  priceControlStatus: PriceControlStatusSchema.nullable(),
  controlledFormulationId: z.string().nullable(),
  priceControlVersionId: z.string().nullable(),
  ceilingPricePaise: z.number().int().nullable(),
  ceilingBasis: CeilingBasisSchema.nullable(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  lineTotalPaise: z.number().int(),
  /**
   * What the catalogue calls these things right now.
   *
   * The snapshot fields above are null until posting freezes them, because a snapshot is what was
   * true when the invoice was issued and a draft has issued nothing. These are read live so a
   * counter can read its own bill while building it, and are never written anywhere.
   */
  currentProductDisplayName: z.string().nullable(),
  currentPackDisplayLabel: z.string().nullable(),
  currentBaseUnitLabel: z.string().nullable(),
  currentBatchNumber: z.string().nullable()
});

/** Operational evidence of payment. No cash ledger, no settlement, no receivable. */
export const SaleTenderSchema = z.object({
  id: z.string(),
  saleDocumentId: z.string(),
  method: TenderMethodSchema,
  amountPaise: z.number().int().positive(),
  referenceText: z.string().nullable()
});

export const SaleDetailSchema = SaleSchema.extend({
  lines: z.array(SaleLineSchema).default([]),
  tenders: z.array(SaleTenderSchema).default([])
});

/** The only header facts a browser may supply. */
export const SaleDraftInputSchema = z.object({
  customerPartyId: z.string().nullable().optional(),
  customerNameText: z.string().nullable().optional(),
  businessDate: z.string()
});

export const UpdateSaleDraftRequestSchema = SaleDraftInputSchema.extend({
  expectedRevision: z.number().int().positive()
});

/**
 * The only line facts a browser may supply.
 *
 * One `quantity` field, read in the basis named beside it, because a second field would let the two
 * disagree about what was sold.
 */
export const SaleLineInputSchema = z.object({
  expectedRevision: z.number().int().positive(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  quantityBasis: QuantityBasisSchema,
  quantity: z.number().int().positive(),
  sellingRatePaise: z.number().int().nonnegative()
});

export const TenderInputSchema = z.object({
  method: TenderMethodSchema,
  amountPaise: z.number().int().positive(),
  referenceText: z.string().nullable().optional()
});

export const PostSaleRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  idempotencyKey: z.string(),
  tenders: z.array(TenderInputSchema).min(1)
});

/**
 * A lot the counter may choose from, with what the ledger says is left and whether it has expired.
 * Expired lots are returned and marked rather than hidden, so the operator can see why a lot they
 * expected to use is unavailable.
 */
export const SellableBatchSchema = z.object({
  id: z.string(),
  productPackId: z.string(),
  batchNumber: z.string(),
  expiresOn: z.string().nullable(),
  mrpPaise: z.number().int().nullable(),
  availableAtoms: z.number().int(),
  expired: z.boolean()
});

/**
 * What a draft would come to if it were posted now.
 *
 * A quote, never a commitment: the Store Service resolves it with the same code posting uses, but
 * allocates no number, freezes no snapshot and moves no stock. It exists so the counter can say the
 * amount before taking money — and so a price the posting would refuse is discovered while the
 * customer is still standing there.
 */
export const SaleQuoteLineSchema = z.object({
  id: z.string(),
  lineNumber: z.number().int().positive(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  lineTotalPaise: z.number().int()
});

export const SaleQuoteSchema = z.object({
  saleDocumentId: z.string(),
  revision: z.number().int().positive(),
  taxTreatment: SaleTaxTreatmentSchema,
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  grandTotalPaise: z.number().int(),
  lines: z.array(SaleQuoteLineSchema).default([])
});

/** Every code the sale API can return, mirroring `api::sales::SaleError`. */
export const SaleErrorCodeSchema = z.enum([
  "validation_failed",
  "sale_not_found",
  "sale_not_draft",
  "revision_conflict",
  "customer_not_eligible",
  "store_tax_profile_incomplete",
  "product_tax_classification_incomplete",
  "tax_rate_not_found",
  "product_pack_mismatch",
  "pack_not_sellable",
  "batch_pack_mismatch",
  "batch_expired",
  "fractional_sale_not_allowed",
  "quantity_increment_violation",
  "insufficient_stock",
  "selling_rate_above_mrp",
  "selling_rate_above_ceiling",
  "price_control_unresolved",
  "price_control_incomparable",
  "tender_mismatch",
  "arithmetic_overflow",
  "idempotency_conflict",
  "posting_conflict",
  "authentication_required",
  "session_expired",
  "authorization_denied",
  "service_busy",
  "internal_error"
]);

export type SaleStatus = z.infer<typeof SaleStatusSchema>;
export type SaleTaxTreatment = z.infer<typeof SaleTaxTreatmentSchema>;
export type SaleLineTaxKind = z.infer<typeof SaleLineTaxKindSchema>;
export type QuantityBasis = z.infer<typeof QuantityBasisSchema>;
export type TenderMethod = z.infer<typeof TenderMethodSchema>;
export type Sale = z.infer<typeof SaleSchema>;
export type SaleLine = z.infer<typeof SaleLineSchema>;
export type SaleTender = z.infer<typeof SaleTenderSchema>;
export type SaleDetail = z.infer<typeof SaleDetailSchema>;
export type SaleDraftInput = z.infer<typeof SaleDraftInputSchema>;
export type SaleLineInput = z.infer<typeof SaleLineInputSchema>;
export type TenderInput = z.infer<typeof TenderInputSchema>;
export type SellableBatch = z.infer<typeof SellableBatchSchema>;
export type SaleQuote = z.infer<typeof SaleQuoteSchema>;
export type SaleQuoteLine = z.infer<typeof SaleQuoteLineSchema>;
export type SaleErrorCode = z.infer<typeof SaleErrorCodeSchema>;

/**
 * Phase 1I returns.
 *
 * Three concepts stay separate here exactly as they do in the schema, because merging them would
 * encode a legal claim the software is not entitled to make:
 *
 * - the **commercial return** — what part of the original is reversed;
 * - the **GST evidence** — what tax document the law attaches to it;
 * - the **stock disposition** — what physical state the goods enter.
 *
 * Note what this vocabulary deliberately does NOT contain: a `debit_note` kind. CGST s.34(3) gives
 * the debit note to "the registered person, who has supplied" — the supplier — so a pharmacy
 * returning goods never issues one. Its document is a Purchase Return carrying a GST route.
 */
export const ReturnKindSchema = z.enum(["sales_return", "purchase_return"]);
export const ReturnStatusSchema = z.enum(["draft", "posted"]);

/**
 * Where returned goods go. `sellable` is deliberately absent: a commercial return may never create
 * sellable stock, because Indian drug law gives a retailer no rule permitting resale of a medicine a
 * customer brought back. Releasing quarantined stock is a separate, separately-authorised transfer.
 */
export const ReturnDispositionSchema = z.enum(["quarantined", "non_sellable"]);

/**
 * Whether the credit note this return evidences may reduce output tax liability.
 *
 * Recorded, never inferred. Section 34(2) makes the answer depend on facts the software does not
 * hold — whether a registered recipient reversed the attributable input tax credit, and whether the
 * incidence of tax was passed on — so the operator asserts it and a later GST phase audits it.
 */
export const TaxAdjustmentStatusSchema = z.enum(["tax_adjustable", "commercial_only"]);

/** Circular 72/46/2018-GST gives a returning retailer two routes, and this is which one was used. */
export const GstRouteSchema = z.enum(["fresh_supply", "supplier_credit_note"]);

export const ReturnSchema = z.object({
  id: z.string(),
  storeId: z.string(),
  returnKind: ReturnKindSchema,
  originalSaleDocumentId: z.string().nullable(),
  originalPurchaseDocumentId: z.string().nullable(),
  originalDocumentNumber: z.string().nullable(),
  originalDocumentDate: z.string().nullable(),
  businessDate: z.string(),
  status: ReturnStatusSchema,
  revision: z.number().int().positive(),
  seriesCode: z.string().nullable(),
  financialYear: z.string().nullable(),
  sequenceValue: z.number().int().positive().nullable(),
  documentNumber: z.string().nullable(),
  storeGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  storeNormalizedGstin: z.string().nullable(),
  storePlaceOfSupplyStateId: z.string().nullable(),
  storeStateCode: z.string().nullable(),
  counterpartyPartyId: z.string().nullable(),
  counterpartyDisplayName: z.string().nullable(),
  counterpartyGstRegistrationStatus: GstRegistrationStatusSchema.nullable(),
  counterpartyNormalizedGstin: z.string().nullable(),
  counterpartyStateCode: z.string().nullable(),
  taxTreatment: SaleTaxTreatmentSchema.nullable(),
  taxAdjustmentStatus: TaxAdjustmentStatusSchema.nullable(),
  taxAdjustmentReason: z.string().nullable(),
  gstRoute: GstRouteSchema.nullable(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  grandTotalPaise: z.number().int(),
  createdByUserId: z.string(),
  createdAtUtc: z.string(),
  updatedAtUtc: z.string(),
  postedByUserId: z.string().nullable(),
  postedAtUtc: z.string().nullable()
});

/**
 * A return line. Every commercial and tax fact is copied from the original line it reverses, so a
 * reversal stays equal and opposite even if a rate, a price or a product name changed since.
 */
export const ReturnLineSchema = z.object({
  id: z.string(),
  returnDocumentId: z.string(),
  lineNumber: z.number().int().positive(),
  originalSaleLineId: z.string().nullable(),
  originalPurchaseLineId: z.string().nullable(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  quantityBasis: QuantityBasisSchema,
  quantityPacks: z.number().int().positive().nullable(),
  quantityAtoms: z.number().int().positive(),
  disposition: ReturnDispositionSchema.nullable(),
  productDisplayName: z.string().nullable(),
  packDisplayLabel: z.string().nullable(),
  baseUnitLabel: z.string().nullable(),
  batchNumber: z.string().nullable(),
  batchExpiresOn: z.string().nullable(),
  hsnCodeId: z.string().nullable(),
  hsnCode: z.string().nullable(),
  taxCategoryId: z.string().nullable(),
  taxTreatmentKind: SaleLineTaxKindSchema.nullable(),
  taxRateVersionId: z.string().nullable(),
  cgstBasisPoints: z.number().int(),
  sgstBasisPoints: z.number().int(),
  igstBasisPoints: z.number().int(),
  cessBasisPoints: z.number().int(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  lineTotalPaise: z.number().int()
});

/** Evidence of the credit note the supplier issued — never our own tax document. */
export const SupplierCreditNoteSchema = z.object({
  id: z.string(),
  returnDocumentId: z.string(),
  creditNoteNumber: z.string(),
  creditNoteDate: z.string(),
  creditNoteAmountPaise: z.number().int(),
  recordedAtUtc: z.string()
});

export const ReturnDetailSchema = ReturnSchema.extend({
  lines: z.array(ReturnLineSchema).default([]),
  supplierCreditNotes: z.array(SupplierCreditNoteSchema).default([])
});

/** One original line and what is left of it. */
export const ReturnableLineSchema = z.object({
  originalLineId: z.string(),
  lineNumber: z.number().int().positive(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  productDisplayName: z.string().nullable(),
  packDisplayLabel: z.string().nullable(),
  baseUnitLabel: z.string().nullable(),
  batchNumber: z.string().nullable(),
  batchExpiresOn: z.string().nullable(),
  quantityBasis: QuantityBasisSchema,
  originalQuantity: z.number().int(),
  originalQuantityAtoms: z.number().int(),
  alreadyReturnedAtoms: z.number().int(),
  returnableAtoms: z.number().int(),
  returnableQuantity: z.number().int(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  lineTotalPaise: z.number().int()
});

export const ReturnableDocumentSchema = z.object({
  documentId: z.string(),
  documentNumber: z.string().nullable(),
  documentDate: z.string(),
  counterpartyDisplayName: z.string().nullable(),
  lines: z.array(ReturnableLineSchema).default([])
});

export const ReturnQuoteSchema = z.object({
  returnDocumentId: z.string(),
  revision: z.number().int().positive(),
  taxableValuePaise: z.number().int(),
  cgstPaise: z.number().int(),
  sgstPaise: z.number().int(),
  igstPaise: z.number().int(),
  cessPaise: z.number().int(),
  grandTotalPaise: z.number().int()
});

/** The only facts a browser may supply when starting a return. */
export const CreateReturnInputSchema = z.object({
  returnKind: ReturnKindSchema,
  originalDocumentId: z.string(),
  businessDate: z.string()
});

/**
 * The only line facts a browser may supply: which original line, how much of it, and — for goods
 * coming back from a customer — where they were put.
 */
export const ReturnLineInputSchema = z.object({
  expectedRevision: z.number().int().positive(),
  originalLineId: z.string(),
  quantity: z.number().int().positive(),
  disposition: ReturnDispositionSchema.nullable().optional()
});

export const PostReturnRequestSchema = z.object({
  expectedRevision: z.number().int().positive(),
  idempotencyKey: z.string(),
  taxAdjustmentStatus: TaxAdjustmentStatusSchema.nullable().optional(),
  taxAdjustmentReason: z.string().nullable().optional(),
  gstRoute: GstRouteSchema.nullable().optional()
});

export const StockDispositionInputSchema = z.object({
  idempotencyKey: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  quantityAtoms: z.number().int().positive(),
  fromStatus: z.literal("quarantined"),
  toStatus: z.enum(["sellable", "non_sellable"]),
  reason: z.string(),
  occurredOn: z.string()
});

export const StockDispositionSchema = z.object({
  id: z.string(),
  productId: z.string(),
  productPackId: z.string(),
  batchId: z.string(),
  quantityAtoms: z.number().int(),
  fromStatus: z.string(),
  toStatus: z.string(),
  reason: z.string(),
  occurredOn: z.string()
});

export const SupplierCreditNoteInputSchema = z.object({
  creditNoteNumber: z.string(),
  creditNoteDate: z.string(),
  creditNoteAmountPaise: z.number().int().nonnegative()
});

/** Every code the returns API can return, mirroring `api::returns::ReturnError`. */
export const ReturnErrorCodeSchema = z.enum([
  "validation_failed",
  "return_not_found",
  "return_not_draft",
  "revision_conflict",
  "original_document_not_found",
  "original_document_not_posted",
  "original_line_mismatch",
  "over_return",
  "insufficient_stock",
  "disposition_required",
  "disposition_not_allowed",
  "disposition_denied",
  "gst_route_required",
  "gst_route_not_allowed",
  "tax_adjustment_status_required",
  "tax_adjustment_status_not_allowed",
  "supplier_credit_note_route_conflict",
  "duplicate_supplier_credit_note",
  "arithmetic_overflow",
  "idempotency_conflict",
  "posting_conflict",
  "authentication_required",
  "session_expired",
  "authorization_denied",
  "service_busy",
  "internal_error"
]);

export type ReturnKind = z.infer<typeof ReturnKindSchema>;
export type ReturnStatus = z.infer<typeof ReturnStatusSchema>;
export type ReturnDisposition = z.infer<typeof ReturnDispositionSchema>;
export type TaxAdjustmentStatus = z.infer<typeof TaxAdjustmentStatusSchema>;
export type GstRoute = z.infer<typeof GstRouteSchema>;
export type StockStatus = z.infer<typeof StockStatusSchema>;
export type ReturnDocument = z.infer<typeof ReturnSchema>;
export type ReturnLine = z.infer<typeof ReturnLineSchema>;
export type ReturnDetail = z.infer<typeof ReturnDetailSchema>;
export type SupplierCreditNote = z.infer<typeof SupplierCreditNoteSchema>;
export type ReturnableLine = z.infer<typeof ReturnableLineSchema>;
export type ReturnableDocument = z.infer<typeof ReturnableDocumentSchema>;
export type ReturnQuote = z.infer<typeof ReturnQuoteSchema>;
export type CreateReturnInput = z.infer<typeof CreateReturnInputSchema>;
export type ReturnLineInput = z.infer<typeof ReturnLineInputSchema>;
export type StockDispositionInput = z.infer<typeof StockDispositionInputSchema>;
export type StockDisposition = z.infer<typeof StockDispositionSchema>;
export type SupplierCreditNoteInput = z.infer<typeof SupplierCreditNoteInputSchema>;
export type ReturnErrorCode = z.infer<typeof ReturnErrorCodeSchema>;



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
