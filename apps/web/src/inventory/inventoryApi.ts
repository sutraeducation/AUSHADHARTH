import {
  InventoryMovementSchema,
  StockBalanceSchema,
  type InventoryMovement,
  type PostMovementRequest,
  type StockBalance
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/** Derived balances. The Store Service sums the ledger; nothing reads a stored quantity. */
export async function listStock(productId?: string, packId?: string): Promise<StockBalance[]> {
  const query = new URLSearchParams();
  if (productId) query.set("productId", productId);
  if (packId) query.set("packId", packId);
  const suffix = query.toString() ? `?${query}` : "";
  return StockBalanceSchema.array().parse(await localServiceRequest(`/api/v1/inventory/stock${suffix}`));
}

export async function listMovements(packId?: string, batchId?: string): Promise<InventoryMovement[]> {
  const query = new URLSearchParams();
  if (packId) query.set("packId", packId);
  if (batchId) query.set("batchId", batchId);
  const suffix = query.toString() ? `?${query}` : "";
  return InventoryMovementSchema.array().parse(await localServiceRequest(`/api/v1/inventory/movements${suffix}`));
}

/**
 * Posts one movement. The idempotency key makes a retry safe: the Store Service returns the
 * already-posted movement rather than duplicating stock. The request carries no store identifier —
 * the Store is resolved server-side.
 */
export async function postMovement(request: PostMovementRequest): Promise<InventoryMovement> {
  return InventoryMovementSchema.parse(await localServiceRequest("/api/v1/inventory/movements", {
    method: "POST",
    body: JSON.stringify(request)
  }));
}

/**
 * A fresh UUIDv7 idempotency key per posting attempt; a retry of that attempt reuses it. The Store
 * Service requires the project's durable UUIDv7 shape, so this builds a real one — a 48-bit
 * millisecond prefix with the version and RFC variant bits set — rather than reshaping a v4.
 */
export function newIdempotencyKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const timestamp = BigInt(Date.now());
  for (let index = 0; index < 6; index += 1) {
    bytes[index] = Number((timestamp >> BigInt(8 * (5 - index))) & 0xffn);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
