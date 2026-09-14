const LOCAL_SERVICE_ORIGIN = import.meta.env.VITE_LOCAL_SERVICE_ORIGIN ?? "";

export class LocalServiceError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly retryAfterSeconds?: number,
    public readonly issues: ReadonlyArray<{ field: string; message: string }> = [],
    public readonly expectedRevision: number | null = null,
    public readonly currentRevision: number | null = null
  ) {
    super(message);
  }
}

export async function fetchHealth(): Promise<unknown> {
  const response = await fetch(`${LOCAL_SERVICE_ORIGIN}/api/v1/health`, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(3_000)
  });

  if (!response.ok) {
    throw new Error(`Local service health request failed with ${response.status}`);
  }

  return response.json();
}

export async function localServiceRequest(
  path: string,
  init: RequestInit = {}
): Promise<unknown> {
  const headers = new Headers(init.headers);
  headers.set("Accept", "application/json");
  if (init.body) headers.set("Content-Type", "application/json");

  const response = await fetch(`${LOCAL_SERVICE_ORIGIN}${path}`, {
    ...init,
    headers,
    credentials: "same-origin",
    signal: init.signal ?? AbortSignal.timeout(5_000)
  });
  if (response.status === 204) return null;

  const body = await response.json().catch(() => null) as {
    code?: string;
    message?: string;
    retryAfterSeconds?: number;
    issues?: Array<{ field: string; message: string }>;
    expectedRevision?: number | null;
    currentRevision?: number | null;
  } | null;
  if (!response.ok) {
    throw new LocalServiceError(
      response.status,
      body?.code ?? "internal_error",
      safeErrorMessage(body?.code),
      body?.retryAfterSeconds,
      body?.issues ?? [],
      body?.expectedRevision ?? null,
      body?.currentRevision ?? null
    );
  }
  return body;
}

export function safeErrorMessage(code?: string): string {
  switch (code) {
    case "invalid_credentials": return "The login ID or password is incorrect.";
    case "rate_limited": return "Too many attempts. Wait briefly and try again.";
    case "setup_unavailable": return "Setup was already completed. Continue to sign in.";
    case "session_expired": return "Your session expired. Sign in again.";
    case "authentication_required": return "Sign in to continue.";
    case "authorization_denied": return "Your role does not permit this operation.";
    case "service_busy": return "The local service is busy. Try again shortly.";
    case "duplicate_conflict": return "A conflicting active record already exists.";
    case "revision_conflict": return "This record was changed after you opened it.";
    case "not_found": return "This record no longer exists.";
    case "archived_conflict": return "This action conflicts with the record's archive status.";
    case "effective_date_overlap": return "This effective period overlaps an existing active rate.";
    case "conversion_conflict": return "This Pack conversion conflicts with its containment or an active parent Pack.";
    case "barcode_conflict": return "This barcode is already assigned within the same scope.";
    case "default_pack_conflict": return "A default Pack must be enabled, and only one default is allowed for each purpose.";
    case "composition_conflict": return "This composition conflicts with the product kind or an ingredient already recorded.";
    case "batch_conflict": return "This batch conflicts with the pack's current status.";
    case "insufficient_stock": return "This would leave a negative stock balance.";
    case "batch_pack_mismatch": return "That batch does not belong to the selected pack.";
    case "party_conflict": return "The tax registration does not agree with the selected State.";
    case "store_tax_conflict": return "This store's GSTIN does not agree with the selected State.";
    case "purchase_not_found": return "This purchase no longer exists.";
    case "purchase_not_draft": return "A posted purchase cannot be changed. Correct it with a later document.";
    case "duplicate_supplier_invoice": return "This supplier invoice number is already recorded.";
    case "supplier_not_eligible": return "That party is not an active supplier.";
    case "store_tax_profile_incomplete": return "Record this store's place of supply in Store Profile before posting.";
    case "supplier_tax_profile_incomplete": return "Record the supplier's place of supply before posting.";
    case "product_tax_classification_incomplete": return "A product on this document has no Tax Category yet.";
    case "tax_rate_not_found": return "No tax rate is in force on this document's date for a product's Tax Category.";
    case "product_pack_mismatch": return "That pack does not belong to the selected product.";
    case "arithmetic_overflow": return "The amounts on this document are too large to record.";
    case "idempotency_conflict": return "This posting was already attempted with different details. Reload and try again.";
    case "posting_conflict": return "This document was already posted.";
    case "sale_not_found": return "This sale no longer exists.";
    case "sale_not_draft": return "A posted sale cannot be changed. Correct it with a later document.";
    case "customer_not_eligible": return "That party is not an active customer.";
    case "pack_not_sellable": return "This pack is not enabled for sale at this store.";
    case "batch_expired": return "This batch has expired and cannot be sold.";
    case "fractional_sale_not_allowed": return "This product cannot be sold in part units.";
    case "quantity_increment_violation": return "That quantity is not a whole multiple of this pack's smallest sellable quantity.";
    case "selling_rate_above_mrp": return "The amount charged exceeds this batch's printed MRP.";
    case "selling_rate_above_ceiling": return "The amount charged exceeds the notified ceiling price for this medicine.";
    case "price_control_unresolved": return "This medicine is price-controlled but no ceiling price is recorded for the sale date.";
    case "price_control_incomparable": return "This medicine's ceiling price cannot be compared with the price charged. Check the ceiling's unit.";
    case "tender_mismatch": return "The payment must be a single amount equal to the invoice total.";
    case "party_role_conflict": return "This role conflicts with the party's current status.";
    case "party_address_conflict": return "This address conflicts with the party's or the State's current status.";
    case "validation_failed": return "Check the highlighted information and try again.";
    default: return "AUSHADHARTH could not complete that request. Try again.";
  }
}
