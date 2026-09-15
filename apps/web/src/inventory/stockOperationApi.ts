import {
  StockOperationDetailSchema,
  StockOperationQuoteSchema,
  StockOperationSchema,
  type CreateStockOperationInput,
  type StockOperationDetail,
  type StockOperationKind,
  type StockOperationLineInput,
  type StockOperationQuote,
  type StockOperation
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/**
 * Phase 1J stock operations.
 *
 * Every figure that matters comes back from the Store Service. This module sends what the operator
 * typed — a counted quantity, a damaged quantity, a reason — and never computes a delta, a balance
 * or a resulting figure of its own. The quote below is a preview the server produced, not a local
 * calculation dressed up as one.
 */

export type StockOperationKindFilter = StockOperationKind | "all";

export async function listStockOperations(
  kind: StockOperationKindFilter = "all"
): Promise<StockOperation[]> {
  const query = kind === "all" ? "" : `?kind=${encodeURIComponent(kind)}`;
  return StockOperationSchema.array().parse(
    await localServiceRequest(`/api/v1/stock-operations${query}`)
  );
}

export async function getStockOperation(id: string): Promise<StockOperationDetail> {
  return StockOperationDetailSchema.parse(await localServiceRequest(`/api/v1/stock-operations/${id}`));
}

export async function createStockOperation(
  input: CreateStockOperationInput
): Promise<StockOperationDetail> {
  return StockOperationDetailSchema.parse(
    await localServiceRequest("/api/v1/stock-operations", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export async function addStockOperationLine(
  id: string,
  input: StockOperationLineInput
): Promise<StockOperationDetail> {
  return StockOperationDetailSchema.parse(
    await localServiceRequest(`/api/v1/stock-operations/${id}/lines`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export async function removeStockOperationLine(
  lineId: string,
  expectedRevision: number
): Promise<StockOperationDetail> {
  return StockOperationDetailSchema.parse(
    await localServiceRequest(`/api/v1/stock-operation-lines/${lineId}`, {
      method: "DELETE",
      body: JSON.stringify({ expectedRevision })
    })
  );
}

/**
 * Throws away a draft nobody posted.
 *
 * A draft has moved no stock and told nobody anything, so backing out of a half-filled screen
 * should leave nothing behind. Earlier phases had no way to say this, and their lists fill up
 * with abandoned drafts that no one can clear.
 */
export async function discardStockOperation(id: string, expectedRevision: number): Promise<void> {
  await localServiceRequest(`/api/v1/stock-operations/${id}`, {
    method: "DELETE",
    body: JSON.stringify({ expectedRevision })
  });
}

/** What posting would do, as the server sees it. Reading this changes nothing. */
export async function quoteStockOperation(id: string): Promise<StockOperationQuote> {
  return StockOperationQuoteSchema.parse(
    await localServiceRequest(`/api/v1/stock-operations/${id}/quote`)
  );
}

export async function postStockOperation(
  id: string,
  expectedRevision: number,
  idempotencyKey: string
): Promise<StockOperationDetail> {
  return StockOperationDetailSchema.parse(
    await localServiceRequest(`/api/v1/stock-operations/${id}/post`, {
      method: "POST",
      body: JSON.stringify({ expectedRevision, idempotencyKey })
    })
  );
}

/**
 * What each kind of operation is called on screen, and what it actually does to the stock.
 *
 * The wording is the safeguard. "Write off" and "dispose of" are different acts — one says these
 * cannot be sold, the other says these are no longer here — and an operator who confuses them
 * produces a stock figure nobody can reconcile.
 */
export const OPERATION_KIND_LABELS: Record<StockOperationKind, string> = {
  physical_count: "Physical Count",
  adjustment: "Adjust Stock",
  damage: "Mark Damaged",
  expiry: "Handle Expired Stock",
  quarantine: "Quarantine",
  removal: "Remove / Dispose"
};

export const OPERATION_KIND_BLURBS: Record<StockOperationKind, string> = {
  physical_count:
    "Count what is on the shelf. AUSHADHARTH works out the difference — you never type one.",
  adjustment:
    "Correct a quantity that was recorded wrongly, or record stock that has gone missing.",
  damage:
    "Goods spoiled in the store. They stay in the building; they just stop being sellable.",
  expiry:
    "Classify a lot that has passed its expiry date. This does not mean it has been disposed of.",
  quarantine: "Hold stock back until a pharmacist decides whether it can be sold.",
  removal:
    "Record that written-off goods have physically left — destroyed, or collected for disposal."
};

export const REASON_LABELS: Record<string, string> = {
  physical_count_gain: "Counted more than recorded",
  physical_count_loss: "Counted less than recorded",
  damage: "Damaged",
  breakage: "Broken",
  expiry: "Past its expiry date",
  theft_or_loss: "Missing, stolen or lost",
  data_correction: "Recorded wrongly",
  quality_hold: "Quality concern",
  disposal: "Destroyed or collected for disposal"
};

/** Exact integer parsing. A quantity is a count of things, never a float. */
export function quantityToInteger(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const parsed = Number(trimmed);
  return Number.isSafeInteger(parsed) ? parsed : null;
}
