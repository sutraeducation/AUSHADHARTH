import {
  ReturnDetailSchema,
  ReturnQuoteSchema,
  ReturnSchema,
  ReturnableDocumentSchema,
  StockDispositionSchema,
  SupplierCreditNoteSchema,
  type CreateReturnInput,
  type ReturnDetail,
  type ReturnDocument,
  type ReturnKind,
  type ReturnLineInput,
  type ReturnQuote,
  type ReturnableDocument,
  type StockDisposition,
  type StockDispositionInput,
  type SupplierCreditNote,
  type SupplierCreditNoteInput
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

export type ReturnKindFilter = ReturnKind | "all";

export async function listReturns(returnKind: ReturnKindFilter = "all"): Promise<ReturnDocument[]> {
  const query = new URLSearchParams({ returnKind });
  return ReturnSchema.array().parse(await localServiceRequest(`/api/v1/returns?${query}`));
}

export async function getReturn(id: string): Promise<ReturnDetail> {
  return ReturnDetailSchema.parse(await localServiceRequest(`/api/v1/returns/${id}`));
}

/**
 * What is left to return on a posted document.
 *
 * The browser never works this out. Returnability depends on every *posted* return already made
 * against the original, which only the Store Service can see, and which changes under the operator's
 * feet while they are looking at the screen.
 */
export async function listReturnableSaleLines(saleId: string): Promise<ReturnableDocument> {
  return ReturnableDocumentSchema.parse(
    await localServiceRequest(`/api/v1/sales/${saleId}/returnable-lines`)
  );
}

export async function listReturnablePurchaseLines(purchaseId: string): Promise<ReturnableDocument> {
  return ReturnableDocumentSchema.parse(
    await localServiceRequest(`/api/v1/purchases/${purchaseId}/returnable-lines`)
  );
}

export async function createReturn(input: CreateReturnInput): Promise<ReturnDetail> {
  return ReturnDetailSchema.parse(await localServiceRequest("/api/v1/returns", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

export async function addReturnLine(
  returnId: string,
  line: ReturnLineInput
): Promise<ReturnDetail> {
  return ReturnDetailSchema.parse(await localServiceRequest(`/api/v1/returns/${returnId}/lines`, {
    method: "POST",
    body: JSON.stringify(line)
  }));
}

export async function removeReturnLine(
  lineId: string,
  expectedRevision: number
): Promise<ReturnDetail> {
  return ReturnDetailSchema.parse(await localServiceRequest(`/api/v1/return-lines/${lineId}`, {
    method: "DELETE",
    body: JSON.stringify({ expectedRevision })
  }));
}

/** What this return comes to, resolved by the Store Service and never added up here. */
export async function quoteReturn(id: string): Promise<ReturnQuote> {
  return ReturnQuoteSchema.parse(await localServiceRequest(`/api/v1/returns/${id}/quote`));
}

/**
 * Posting is a named command, never a status assignment.
 *
 * The GST facts travel with it because they are part of what is being decided: a sales return says
 * whether it adjusts tax, and a purchase return says which route documents it. Neither is inferred.
 */
export async function postReturn(
  id: string,
  body: {
    expectedRevision: number;
    idempotencyKey: string;
    taxAdjustmentStatus?: string | null;
    taxAdjustmentReason?: string | null;
    gstRoute?: string | null;
  }
): Promise<ReturnDetail> {
  return ReturnDetailSchema.parse(await localServiceRequest(`/api/v1/returns/${id}/post`, {
    method: "POST",
    body: JSON.stringify(body)
  }));
}

/** Evidence of the credit note the supplier issued. Never our own tax document. */
export async function recordSupplierCreditNote(
  returnId: string,
  input: SupplierCreditNoteInput
): Promise<SupplierCreditNote> {
  return SupplierCreditNoteSchema.parse(
    await localServiceRequest(`/api/v1/returns/${returnId}/supplier-credit-notes`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

/**
 * Moves quantity between stock statuses within one lot.
 *
 * This is how quarantined goods become sellable. It is not an edit: the Store Service writes two
 * append-only movements, so the physical total is unchanged and only its status moves.
 */
export async function createStockDisposition(
  input: StockDispositionInput
): Promise<StockDisposition> {
  return StockDispositionSchema.parse(await localServiceRequest("/api/v1/stock-dispositions", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

/**
 * Exact integer paise to a grouped Indian amount for display only.
 *
 * Split on the decimal digits rather than by dividing, so a large total cannot lose a rupee to
 * floating-point division on its way to the screen.
 */
export function paiseToAmountText(paise: number): string {
  const sign = paise < 0 ? "-" : "";
  const digits = String(Math.abs(paise)).padStart(3, "0");
  return `${sign}${BigInt(digits.slice(0, -2)).toLocaleString("en-IN")}.${digits.slice(-2)}`;
}

/**
 * A quantity typed by an operator, in the basis the original was billed in.
 *
 * Whole numbers only: `parseInt("1.5")` is 1, which would return one strip for a request that said
 * one and a half. There is no fractional pack anywhere in this system.
 */
export function quantityToInteger(value: string): number | null {
  const text = value.trim();
  if (!/^\d+$/.test(text)) return null;
  const quantity = Number.parseInt(text, 10);
  return Number.isSafeInteger(quantity) && quantity >= 1 ? quantity : null;
}

/** What one unit of a returnable line is called, so the screen never asks for "units". */
export function unitLabelFor(line: {
  quantityBasis: string;
  packDisplayLabel: string | null;
  baseUnitLabel: string | null;
}): string {
  return line.quantityBasis === "pack"
    ? (line.packDisplayLabel ?? "pack")
    : (line.baseUnitLabel ?? "unit");
}
