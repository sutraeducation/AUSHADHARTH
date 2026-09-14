import {
  SaleDetailSchema,
  SaleQuoteSchema,
  SaleSchema,
  SellableBatchSchema,
  type QuantityBasis,
  type Sale,
  type SaleDetail,
  type SaleDraftInput,
  type SaleLineInput,
  type SaleQuote,
  type SaleStatus,
  type SellableBatch,
  type TenderInput
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

export type SaleStatusFilter = SaleStatus | "all";

export async function listSales(
  status: SaleStatusFilter = "all",
  customerPartyId?: string
): Promise<Sale[]> {
  const query = new URLSearchParams({ status });
  if (customerPartyId) query.set("customerPartyId", customerPartyId);
  return SaleSchema.array().parse(await localServiceRequest(`/api/v1/sales?${query}`));
}

export async function getSale(id: string): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sales/${id}`));
}

export async function createSaleDraft(input: SaleDraftInput): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest("/api/v1/sales", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

export async function updateSaleDraft(
  id: string,
  expectedRevision: number,
  input: SaleDraftInput
): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sales/${id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, ...input })
  }));
}

/**
 * Lines are separate server operations, mirroring the backend rather than pretending the draft is
 * saved as one aggregate. Each write carries the document's expected revision.
 */
export async function addSaleLine(saleId: string, line: SaleLineInput): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sales/${saleId}/lines`, {
    method: "POST",
    body: JSON.stringify(line)
  }));
}

export async function updateSaleLine(lineId: string, line: SaleLineInput): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sale-lines/${lineId}`, {
    method: "PUT",
    body: JSON.stringify(line)
  }));
}

export async function removeSaleLine(
  lineId: string,
  expectedRevision: number
): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sale-lines/${lineId}`, {
    method: "DELETE",
    body: JSON.stringify({ expectedRevision })
  }));
}

/**
 * The lots a counter may choose from, with what the ledger says is left.
 *
 * `asOf` is the sale's own business date, so expiry is judged against the date being billed rather
 * than against whatever day the browser thinks it is.
 */
export async function listSellableBatches(
  packId: string,
  asOf: string
): Promise<SellableBatch[]> {
  const query = new URLSearchParams({ asOf });
  return SellableBatchSchema.array().parse(
    await localServiceRequest(`/api/v1/packs/${packId}/sellable-batches?${query}`)
  );
}

/**
 * What this bill would come to if it were posted now.
 *
 * The browser never computes tax. It asks the Store Service, which resolves the quote with the same
 * code the posting uses, so the amount read out at the counter and the amount on the invoice are the
 * same number by construction. A quote commits nothing: no number, no snapshot, no stock.
 */
export async function quoteSale(id: string): Promise<SaleQuote> {
  return SaleQuoteSchema.parse(await localServiceRequest(`/api/v1/sales/${id}/quote`));
}

/**
 * Posting is a named command, never a status assignment.
 *
 * The idempotency key belongs to one logical posting attempt: a transport retry of that attempt
 * must reuse it, so the Store Service returns the original invoice instead of selling twice.
 */
export async function postSale(
  id: string,
  expectedRevision: number,
  idempotencyKey: string,
  tenders: TenderInput[]
): Promise<SaleDetail> {
  return SaleDetailSchema.parse(await localServiceRequest(`/api/v1/sales/${id}/post`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, idempotencyKey, tenders })
  }));
}

/**
 * A selling rate in rupees to exact integer paise, using string arithmetic so `12.5` never becomes
 * a binary float and a third decimal is rejected rather than silently rounded.
 *
 * Zero is accepted, because a line really can be billed at nil rate and the Store Service accepts a
 * non-negative rate.
 */
export function sellingRateToPaise(value: string): number | null {
  const match = /^(\d+)(?:\.(\d{1,2}))?$/.exec(value.trim());
  if (!match) return null;
  const paise = Number.parseInt(`${match[1]}${(match[2] ?? "").padEnd(2, "0")}`, 10);
  return Number.isSafeInteger(paise) && paise >= 0 ? paise : null;
}

/**
 * A typed quantity, read in the basis it was entered in.
 *
 * Only whole numbers are accepted: `parseInt("2.5")` silently yields 2, which would bill two packs
 * for a request that said two and a half. There is no decimal pack anywhere in this system, and a
 * loose quantity is a count of atoms, which is also whole.
 */
export function quantityToInteger(value: string): number | null {
  const text = value.trim();
  if (!/^\d+$/.test(text)) return null;
  const quantity = Number.parseInt(text, 10);
  return Number.isSafeInteger(quantity) && quantity >= 1 ? quantity : null;
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

/**
 * What one unit of a basis is called, for labels the operator reads.
 *
 * The pack's own label is used when there is one — a pharmacist thinks in strips and bottles, not
 * in "packs" — and the product's base unit names the loose case.
 */
export function basisUnitLabel(
  basis: QuantityBasis,
  packLabel: string | null,
  baseUnitLabel: string | null
): string {
  return basis === "pack" ? (packLabel ?? "pack") : (baseUnitLabel ?? "unit");
}
