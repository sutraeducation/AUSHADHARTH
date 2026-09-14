import {
  PurchaseDetailSchema,
  PurchaseSchema,
  type Purchase,
  type PurchaseDetail,
  type PurchaseDraftInput,
  type PurchaseLineInput,
  type PurchaseStatus
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

export type PurchaseStatusFilter = PurchaseStatus | "all";

export async function listPurchases(
  status: PurchaseStatusFilter = "all",
  supplierPartyId?: string
): Promise<Purchase[]> {
  const query = new URLSearchParams({ status });
  if (supplierPartyId) query.set("supplierPartyId", supplierPartyId);
  return PurchaseSchema.array().parse(await localServiceRequest(`/api/v1/purchases?${query}`));
}

export async function getPurchase(id: string): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchases/${id}`));
}

export async function createPurchaseDraft(input: PurchaseDraftInput): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest("/api/v1/purchases", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

export async function updatePurchaseDraft(
  id: string,
  expectedRevision: number,
  input: PurchaseDraftInput
): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchases/${id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, ...input })
  }));
}

/**
 * Lines are separate server operations, mirroring the backend rather than pretending the draft is
 * saved as one aggregate. Each write carries the document's expected revision.
 */
export async function addPurchaseLine(
  purchaseId: string,
  line: PurchaseLineInput
): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchases/${purchaseId}/lines`, {
    method: "POST",
    body: JSON.stringify(line)
  }));
}

export async function updatePurchaseLine(
  lineId: string,
  line: PurchaseLineInput
): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchase-lines/${lineId}`, {
    method: "PUT",
    body: JSON.stringify(line)
  }));
}

export async function removePurchaseLine(
  lineId: string,
  expectedRevision: number
): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchase-lines/${lineId}`, {
    method: "DELETE",
    body: JSON.stringify({ expectedRevision })
  }));
}

/**
 * Posting is a named command, never a status assignment.
 *
 * The idempotency key belongs to one logical posting attempt: a transport retry of that attempt
 * must reuse it, so the Store Service returns the original document instead of posting twice.
 */
export async function postPurchase(
  id: string,
  expectedRevision: number,
  idempotencyKey: string
): Promise<PurchaseDetail> {
  return PurchaseDetailSchema.parse(await localServiceRequest(`/api/v1/purchases/${id}/post`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, idempotencyKey })
  }));
}

/**
 * A purchase rate in rupees to exact integer paise, using string arithmetic so `12.5` never becomes
 * a binary float and a third decimal is rejected rather than silently rounded.
 *
 * This differs from the catalog's `rupeesToPaise` in one deliberate way: zero is accepted. A
 * catalog MRP of zero is meaningless, but a supplier really does invoice free scheme goods at nil
 * rate, and the Store Service accepts a non-negative rate. Widening the catalog helper instead
 * would let a zero MRP through the batch editors, so the two stay separate.
 */
export function ratePerPackToPaise(value: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(value.trim());
  if (!match) return null;
  const paise = Number.parseInt(`${match[1]}${(match[2] ?? "").padEnd(2, "0")}`, 10);
  return Number.isSafeInteger(paise) && paise >= 0 ? paise : null;
}

/**
 * Exact integer paise to a grouped Indian amount for display only.
 *
 * The split is done on the decimal digits rather than by dividing, so a large total cannot lose a
 * rupee to floating-point division on its way to the screen.
 */
export function paiseToAmountText(paise: number): string {
  const sign = paise < 0 ? "-" : "";
  const digits = String(Math.abs(paise)).padStart(3, "0");
  return `${sign}${BigInt(digits.slice(0, -2)).toLocaleString("en-IN")}.${digits.slice(-2)}`;
}
