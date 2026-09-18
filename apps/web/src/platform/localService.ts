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

/**
 * Sends a file to the Store Service as raw bytes.
 *
 * `application/octet-stream` rather than a multipart form on purpose: the service refuses form
 * encodings precisely because a cross-site HTML form can send them, and octet-stream cannot be
 * produced by a form at all. There is no timeout — a backup can be a gigabyte, and cutting off an
 * upload at five seconds would make restore impossible on exactly the installations that need it
 * most. The caller passes a signal if it wants to offer a cancel.
 */
export async function localServiceUpload(
  path: string,
  file: Blob,
  signal?: AbortSignal
): Promise<unknown> {
  const response = await fetch(`${LOCAL_SERVICE_ORIGIN}${path}`, {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/octet-stream" },
    body: file,
    credentials: "same-origin",
    signal
  });
  const body = (await response.json().catch(() => null)) as {
    code?: string;
    message?: string;
    issues?: Array<{ field: string; message: string }>;
  } | null;
  if (!response.ok) {
    throw new LocalServiceError(
      response.status,
      body?.code ?? "internal_error",
      safeErrorMessage(body?.code),
      undefined,
      body?.issues ?? []
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
    case "return_not_found": return "This return no longer exists.";
    // Phase 1J — stock operations. Each message says what the operator can do next, because a
    // refusal that only says no leaves somebody standing at a shelf with a crushed strip.
    case "stock_operation_not_found": return "That stock operation no longer exists.";
    case "stock_operation_not_draft": return "This has already been posted. Record a later operation to correct it.";
    case "stock_operation_line_conflict": return "That line does not belong on this kind of operation. Start the operation that matches what happened.";
    case "duplicate_count_line": return "This count already has a line for that item, batch and stock status.";
    case "stock_operation_empty": return "Add at least one line before posting.";
    case "batch_not_expired": return "That batch has not reached its expiry date yet.";
    case "return_not_draft": return "A posted return cannot be changed. Correct it with a later document.";
    case "original_document_not_found": return "The document this return corrects no longer exists.";
    case "original_document_not_posted": return "Only a posted document can be returned against.";
    case "original_line_mismatch": return "That line does not belong to the document being returned.";
    case "over_return": return "That is more than is left to return on this line.";
    case "disposition_required": return "Say where the returned goods are being put before posting.";
    case "disposition_not_allowed": return "Goods returned to a supplier leave the store, so they have no disposition.";
    case "disposition_denied": return "Only a pharmacist or the owner may release quarantined stock for sale.";
    case "gst_route_required": return "Choose how this purchase return is being documented for GST.";
    case "gst_route_not_allowed": return "A GST route belongs to a purchase return, not a sales return.";
    case "tax_adjustment_status_required": return "Record whether this credit note adjusts GST or is a refund only.";
    case "tax_adjustment_status_not_allowed": return "A GST treatment belongs to a sales return, not a purchase return.";
    case "supplier_credit_note_route_conflict": return "A supplier credit note can only be recorded against a posted purchase return sent under that route.";
    case "duplicate_supplier_credit_note": return "That supplier credit note is already recorded against this return.";
    case "party_role_conflict": return "This role conflicts with the party's current status.";
    case "party_address_conflict": return "This address conflicts with the party's or the State's current status.";
    // Backup and restore. Every one of these is read by somebody whose data may be at stake, so
    // each says plainly what happened and what is still true of their existing records.
    case "backup_format_unsupported":
    case "backup_too_new": return "This backup was made by a newer version of AUSHADHARTH. Update this installation first.";
    case "backup_corrupt": return "This file is damaged and cannot be used as a backup.";
    case "backup_product_mismatch": return "That is not an AUSHADHARTH backup.";
    case "backup_invalid_database": return "This backup does not contain a usable AUSHADHARTH database.";
    case "backup_partially_migrated": return "This backup was interrupted while being upgraded and cannot be used.";
    case "backup_checksum_mismatch": return "This backup is damaged: its contents do not match its own record.";
    case "backup_too_large": return "That file is too large to be an AUSHADHARTH backup.";
    case "insufficient_disk_space": return "There is not enough free space on this PC to do that safely. Free some space and try again.";
    case "candidate_not_found": return "That restore is no longer ready. Choose the backup file again.";
    case "candidate_expired": return "That restore was prepared too long ago. Choose the backup file again.";
    case "backup_not_found": return "That backup was not found.";
    case "setup_already_complete": return "This installation is already set up. Sign in to restore a backup.";
    case "service_restoring": return "AUSHADHARTH is restoring a backup. Restart the Local Store Service to continue.";
    case "restore_failed": return "The restore could not be completed. Your existing data has been kept.";
    case "invalid_password": return "That password is not correct.";
    case "backup_unavailable": return "Backup is not available in this configuration.";
    // Phase 1L-A — the Store's legal identity, and the document a posted Sale becomes. A pharmacy
    // cannot lawfully hand over a bill without its own name, address and sale licence, so the
    // refusal has to send the operator somewhere rather than just say no.
    case "store_legal_profile_incomplete": return "Complete the pharmacy's details in Store Profile before selling.";
    // Phase 1L-A2: CGST Rule 46 requires the customer's particulars on this invoice — a registered
    // customer, ₹50,000 or more of taxable value, or a customer who asked.
    case "recipient_particulars_incomplete": return "This invoice must show the customer's details. Complete them before posting.";
    case "store_licence_conflict": return "That licence number is already recorded for this pharmacy.";
    case "store_licence_archived": return "This licence is archived. Restore it before editing it.";
    case "store_profile_conflict": return "Those pharmacy details could not be saved as entered.";
    case "invoice_not_found": return "That sale no longer exists.";
    case "invoice_not_posted": return "This sale has not been posted, so it has no invoice yet.";
    case "invoice_invariant_failed": return "This invoice does not add up and cannot be shown. Report it before using it.";
    case "validation_failed": return "Check the highlighted information and try again.";
    default: return "AUSHADHARTH could not complete that request. Try again.";
  }
}
