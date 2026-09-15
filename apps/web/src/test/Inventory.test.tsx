import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  InventoryMovementSchema,
  StockBalanceSchema,
  type Batch,
  type InventoryMovement,
  type ProductDetail,
  type StockBalance,
  type UserRole
} from "@aushadharth/contracts";
import { App } from "../app/App";
import { newIdempotencyKey } from "../inventory/inventoryApi";

const IDs = {
  product: "01997000-0000-7000-8000-000000000001",
  tablet: "01997000-0000-7000-8000-000000000002",
  strip: "01997000-0000-7000-8000-000000000003",
  store: "01997000-0000-7000-8000-000000000008",
  pack: "01997000-0000-7000-8000-000000000009",
  batch: "01997000-0000-7000-8000-000000000020",
  movement: "01997000-0000-7000-8000-000000000021",
  user: "01900000-0000-7000-8000-000000000001",
  disposition: "01997000-0000-7000-8000-000000000031"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

function product(): ProductDetail {
  return { id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-10", skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }]
  };
}
function batchRecord(expiresOn = "2029-12-31"): Batch {
  return { id: IDs.batch, productPackId: IDs.pack, batchNumber: "AB-123", normalizedBatchNumber: "AB-123", manufacturedOn: "2026-01-01", expiresOn, mrpPaise: 12550, revision: 1, status: "active", ...stamp };
}
function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], availableAtoms: null, ...extra }, status);
}

type Options = { role?: UserRole; movements?: InventoryMovement[]; failStock?: number; failMovements?: number; batchExpiresOn?: string; postError?: { code: string; status: number; extra?: Record<string, unknown> } };

/**
 * A stateful Store Service double for the ledger. Balances are derived by summing movements exactly
 * as the real service does — the double never stores a quantity either.
 */
function inventoryService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    movements: (options.movements ?? []).map((movement) => ({ ...movement })),
    remainingStockFailures: options.failStock ?? 0,
    remainingMovementFailures: options.failMovements ?? 0,
    seenKeys: new Map<string, InventoryMovement>()
  };
  const balances = (): StockBalance[] => {
    const totals = new Map<string, StockBalance>();
    for (const movement of state.movements) {
      const key = `${movement.productPackId}|${movement.batchId ?? ""}|${movement.stockStatus}`;
      const existing = totals.get(key);
      if (existing) existing.balanceAtoms += movement.quantityDeltaAtoms;
      else totals.set(key, { productId: movement.productId, productPackId: movement.productPackId, batchId: movement.batchId, stockStatus: movement.stockStatus, balanceAtoms: movement.quantityDeltaAtoms });
    }
    return [...totals.values()].filter((row) => row.balanceAtoms !== 0);
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Inventory User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname.endsWith("/dashboard/summary")) return response({ storeDisplayName: "Care Pharmacy", activeProductCount: 1, activePackCount: 1 });
    if (url.pathname === "/api/v1/catalog/context") return response({ storeId: IDs.store });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (url.pathname === "/api/v1/products") return response([{ ...product(), companyRoles: undefined, packs: undefined, composition: undefined }].map(({ ...rest }) => rest));
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return response(product());
    if (/\/packs\/[^/]+\/batches$/.test(url.pathname)) return response([batchRecord(options.batchExpiresOn)]);
    if (url.pathname === "/api/v1/inventory/stock") {
      if (state.remainingStockFailures > 0) { state.remainingStockFailures -= 1; return failure("internal_error", 500); }
      return response(balances());
    }
    if (url.pathname === "/api/v1/inventory/movements" && method === "GET") {
      if (state.remainingMovementFailures > 0) { state.remainingMovementFailures -= 1; return failure("internal_error", 500); }
      return response([...state.movements].reverse());
    }
    if (url.pathname === "/api/v1/inventory/movements" && method === "POST") {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      // A replayed key returns the stored movement instead of duplicating stock.
      const replay = state.seenKeys.get(String(body.idempotencyKey));
      if (replay) return response(replay);
      if (options.postError) return failure(options.postError.code, options.postError.status, options.postError.extra);
      const movement: InventoryMovement = { id: `${IDs.movement}-${state.movements.length}`, storeId: IDs.store, productId: IDs.product, productPackId: body.productPackId, batchId: body.batchId ?? null, movementType: body.movementType, quantityDeltaAtoms: body.quantityDeltaAtoms, occurredOn: body.occurredOn, reason: body.reason ?? null, reversesMovementId: body.reversesMovementId ?? null, purchaseLineId: null, saleLineId: null, returnLineId: null, stockDispositionId: null, stockStatus: "sellable", idempotencyKey: body.idempotencyKey, postedByUserId: IDs.user, postedAtUtc: "2026-04-01T00:00:00.000Z" };
      state.movements.push(movement);
      state.seenKeys.set(movement.idempotencyKey, movement);
      return response(movement, 201);
    }
    if (url.pathname === "/api/v1/stock-dispositions" && method === "POST") {
      if (role === "cashier") return failure("disposition_denied", 403);
      const held = balances().find((line) => line.productPackId === body.productPackId && line.batchId === body.batchId && line.stockStatus === body.fromStatus);
      const available = held?.balanceAtoms ?? 0;
      if (available < body.quantityAtoms) return failure("insufficient_stock", 409, { availableAtoms: available });
      for (const [status, delta] of [[body.fromStatus, -body.quantityAtoms], [body.toStatus, body.quantityAtoms]] as const) {
        state.movements.push(movement({ id: `${IDs.movement}-d${state.movements.length}`, movementType: "disposition_transfer", productPackId: body.productPackId, batchId: body.batchId, quantityDeltaAtoms: delta as number, stockStatus: status as InventoryMovement["stockStatus"], stockDispositionId: IDs.disposition, reason: body.reason, occurredOn: body.occurredOn, idempotencyKey: `${body.idempotencyKey}-${status}` }));
      }
      return response({ id: IDs.disposition, productId: IDs.product, productPackId: body.productPackId, batchId: body.batchId, quantityAtoms: body.quantityAtoms, fromStatus: body.fromStatus, toStatus: body.toStatus, reason: body.reason, occurredOn: body.occurredOn }, 201);
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock, balances };
}

function renderApp(path: string, service = inventoryService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}
function bodiesFor(fetchMock: ReturnType<typeof inventoryService>["fetchMock"], predicate: (url: string, method: string) => boolean) {
  return fetchMock.mock.calls.filter(([input, init]) => predicate(String(input), init?.method ?? "GET")).map(([, init]) => JSON.parse(String(init?.body ?? "{}")));
}
function movement(overrides: Partial<InventoryMovement> = {}): InventoryMovement {
  return { id: IDs.movement, storeId: IDs.store, productId: IDs.product, productPackId: IDs.pack, batchId: null, movementType: "opening_stock", quantityDeltaAtoms: 50, occurredOn: "2026-04-01", reason: null, reversesMovementId: null, purchaseLineId: null, saleLineId: null, returnLineId: null, stockDispositionId: null, stockStatus: "sellable", idempotencyKey: IDs.movement, postedByUserId: IDs.user, postedAtUtc: "2026-04-01T00:00:00.000Z", ...overrides };
}
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Inventory ledger UI", () => {
  it("adds protected Inventory navigation with an empty stock overview", async () => {
    renderApp("/app/inventory/stock");
    expect(await screen.findByRole("heading", { name: "Inventory" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^Inventory/ })).toHaveClass("nav-item--active");
    expect(await screen.findByRole("heading", { name: "No stock recorded" })).toBeInTheDocument();
    // The page states where quantity authority lives.
    expect(screen.getByText(/derived from the movement ledger/i)).toBeInTheDocument();
  });

  it("shows derived balances and separates a failed query from an empty one", async () => {
    renderApp("/app/inventory/stock", inventoryService({ movements: [movement(), movement({ id: `${IDs.movement}-b`, quantityDeltaAtoms: -3, movementType: "adjustment" })] }));
    expect(await screen.findByRole("cell", { name: "47" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Crocin 500 mg Tablet" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "No batch" })).toBeInTheDocument();
    cleanup();

    const failing = renderApp("/app/inventory/stock", inventoryService({ failStock: 1, movements: [movement()] }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByRole("heading", { name: "No stock recorded" })).not.toBeInTheDocument();
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("cell", { name: "50" })).toBeInTheDocument();
    expect(failing.state.remainingStockFailures).toBe(0);
  });

  /**
   * Quarantine that cannot be cleared is a trap: the goods are in the building, off sale, and no
   * screen offers a way out. This is the whole reason the decision lives on the stock table.
   */
  it("lets a pharmacist release quarantined stock back to the counter", async () => {
    const quarantined = [
      movement({ quantityDeltaAtoms: 50, batchId: IDs.batch }),
      movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })
    ];
    const service = inventoryService({ role: "pharmacist", movements: quarantined });
    renderApp("/app/inventory/stock", service);

    const quarantinedRow = await screen.findByRole("row", { name: /Quarantined/ });
    expect(within(quarantinedRow).getByRole("cell", { name: "20" })).toBeInTheDocument();
    fireEvent.click(within(quarantinedRow).getByRole("button", { name: "Decide…" }));

    // The form opens on the whole quarantined quantity, because clearing it is the usual case.
    expect((screen.getByLabelText("Quantity *") as HTMLInputElement).value).toBe("20");
    fireEvent.change(screen.getByLabelText("Quantity *"), { target: { value: "10" } });
    fireEvent.change(screen.getByLabelText("What was checked *"), { target: { value: "Seal intact, strip unopened" } });
    fireEvent.click(screen.getByRole("button", { name: "Record decision" }));

    await waitFor(() => expect(screen.queryByRole("button", { name: "Record decision" })).not.toBeInTheDocument());
    const [sent] = bodiesFor(service.fetchMock, (url, method) => url.includes("/stock-dispositions") && method === "POST");
    expect(sent).toMatchObject({ productPackId: IDs.pack, batchId: IDs.batch, quantityAtoms: 10, fromStatus: "quarantined", toStatus: "sellable", reason: "Seal intact, strip unopened" });

    // Nothing was created or destroyed: 50 sellable became 60, and 20 quarantined became 10.
    await waitFor(() => expect(within(screen.getByRole("row", { name: /Sellable/ })).getByRole("cell", { name: "60" })).toBeInTheDocument());
    expect(within(screen.getByRole("row", { name: /Quarantined/ })).getByRole("cell", { name: "10" })).toBeInTheDocument();
    expect(service.state.movements.filter((entry) => entry.movementType === "disposition_transfer").map((entry) => entry.quantityDeltaAtoms)).toEqual([-10, 10]);
  });

  /**
   * A lot that expired while it sat in quarantine has exactly one outcome left.
   *
   * The Store Service refuses to release it either way, but a refusal the pharmacist can do nothing
   * about is not an answer — the screen has to say what is wrong with the stock, not that the form
   * was filled in wrongly.
   */
  it("offers only a write-off once the quarantined lot has expired", async () => {
    const service = inventoryService({ role: "pharmacist", batchExpiresOn: "2026-05-31", movements: [movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })] });
    renderApp("/app/inventory/stock", service);
    fireEvent.click(within(await screen.findByRole("row", { name: /Quarantined/ })).getByRole("button", { name: "Decide…" }));

    const reason = await screen.findByRole("alert");
    expect(reason).toHaveTextContent("This batch has expired");
    expect(reason).toHaveTextContent("never be released for sale");
    // The option is gone rather than merely refused later.
    const decision = screen.getByLabelText("Decision") as HTMLSelectElement;
    expect([...decision.options].map((option) => option.value)).toEqual(["non_sellable"]);
    expect(decision.value).toBe("non_sellable");
    expect(screen.getByText(/Expired 2026-05-31/)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("What was checked *"), { target: { value: "Expired in quarantine" } });
    fireEvent.click(screen.getByRole("button", { name: "Record decision" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Record decision" })).not.toBeInTheDocument());
    const [sent] = bodiesFor(service.fetchMock, (url, method) => url.includes("/stock-dispositions") && method === "POST");
    expect(sent).toMatchObject({ toStatus: "non_sellable" });
  });

  /**
   * A cashier took the return, so the cashier must not also be the one who decides it may be sold.
   * The row still has to say what it is waiting for, or the stock looks simply lost.
   */
  it("tells a cashier who the quarantine decision is waiting for, and offers no button", async () => {
    renderApp("/app/inventory/stock", inventoryService({ role: "cashier", movements: [movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })] }));
    const quarantinedRow = await screen.findByRole("row", { name: /Quarantined/ });
    expect(within(quarantinedRow).getByText("Waiting on a pharmacist")).toBeInTheDocument();
    expect(within(quarantinedRow).queryByRole("button", { name: "Decide…" })).not.toBeInTheDocument();
  });

  /**
   * Releasing more than is held would create stock out of nothing. The service refuses it, and the
   * counter has to be told what is actually there.
   */
  it("refuses to release more than is in quarantine", async () => {
    const service = inventoryService({ role: "pharmacist", movements: [movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })] });
    renderApp("/app/inventory/stock", service);
    fireEvent.click(within(await screen.findByRole("row", { name: /Quarantined/ })).getByRole("button", { name: "Decide…" }));
    fireEvent.change(screen.getByLabelText("Quantity *"), { target: { value: "30" } });
    fireEvent.change(screen.getByLabelText("What was checked *"), { target: { value: "Looked fine" } });
    fireEvent.click(screen.getByRole("button", { name: "Record decision" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("between one and 20");
    // Refused in the browser, so nothing reached the ledger at all.
    expect(bodiesFor(service.fetchMock, (url, method) => url.includes("/stock-dispositions") && method === "POST")).toHaveLength(0);
  });

  /** A decision with no stated reason is an unexplained stock movement, which is what the ledger exists to prevent. */
  it("will not record a decision that says nothing about what was checked", async () => {
    const service = inventoryService({ role: "pharmacist", movements: [movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })] });
    renderApp("/app/inventory/stock", service);
    fireEvent.click(within(await screen.findByRole("row", { name: /Quarantined/ })).getByRole("button", { name: "Decide…" }));
    // Spaces satisfy the browser's own required check, so this proves the form has a rule of its own.
    fireEvent.change(screen.getByLabelText("What was checked *"), { target: { value: "   " } });
    fireEvent.click(screen.getByRole("button", { name: "Record decision" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Say what was checked");
    expect(bodiesFor(service.fetchMock, (url, method) => url.includes("/stock-dispositions") && method === "POST")).toHaveLength(0);
  });

  /** Writing off is the other half of the decision, and it must reach the ledger as non-sellable. */
  it("records a write-off as stock that can never be sold", async () => {
    const service = inventoryService({ role: "pharmacist", movements: [movement({ id: `${IDs.movement}-q`, movementType: "sales_return", quantityDeltaAtoms: 20, batchId: IDs.batch, stockStatus: "quarantined" })] });
    renderApp("/app/inventory/stock", service);
    fireEvent.click(within(await screen.findByRole("row", { name: /Quarantined/ })).getByRole("button", { name: "Decide…" }));
    fireEvent.change(screen.getByLabelText("Decision"), { target: { value: "non_sellable" } });
    expect(screen.getByText(/Writing off is final/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("What was checked *"), { target: { value: "Strip crushed in transit" } });
    fireEvent.click(screen.getByRole("button", { name: "Record decision" }));

    await waitFor(() => expect(screen.queryByRole("button", { name: "Record decision" })).not.toBeInTheDocument());
    const [sent] = bodiesFor(service.fetchMock, (url, method) => url.includes("/stock-dispositions") && method === "POST");
    expect(sent).toMatchObject({ quantityAtoms: 20, fromStatus: "quarantined", toStatus: "non_sellable" });
    // The goods left quarantine without ever becoming sellable.
    await waitFor(() => expect(screen.getByRole("row", { name: /Not sellable/ })).toBeInTheDocument());
    expect(screen.queryByRole("row", { name: /\bSellable\b/ })).not.toBeInTheDocument();
  });

  it("keeps batch balances separate from batchless ones", async () => {
    renderApp("/app/inventory/stock", inventoryService({ movements: [movement({ quantityDeltaAtoms: 30, batchId: IDs.batch }), movement({ id: `${IDs.movement}-b`, quantityDeltaAtoms: 20 })] }));
    expect(await screen.findByRole("cell", { name: "AB-123" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "30" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "20" })).toBeInTheDocument();
  });

  it("names a purchase inward as a purchase and shows where it came from", async () => {
    // Both halves of this were real defects: the ledger parsed only two movement types, so a page
    // containing a purchase threw, and the surviving rows called every non-opening movement an
    // "Adjustment" — telling an auditor someone corrected the stock by hand.
    renderApp("/app/inventory/ledger", inventoryService({
      movements: [movement({
        movementType: "purchase",
        quantityDeltaAtoms: 100,
        purchaseLineId: "01997a00-0000-7000-8000-000000000050"
      })]
    }));
    expect(await screen.findByRole("cell", { name: "Purchase" })).toBeInTheDocument();
    expect(screen.queryByRole("cell", { name: "Adjustment" })).not.toBeInTheDocument();
    expect(screen.getByText("Purchase inward")).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "+100" })).toBeInTheDocument();
  });

  it("renders the ledger with signed quantities and its own error state", async () => {
    renderApp("/app/inventory/ledger", inventoryService({ movements: [movement(), movement({ id: `${IDs.movement}-b`, quantityDeltaAtoms: -3, movementType: "adjustment", reason: "Damaged in transit" })] }));
    expect(await screen.findByRole("cell", { name: "−3" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "+50" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Opening stock" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Damaged in transit" })).toBeInTheDocument();
    cleanup();

    renderApp("/app/inventory/ledger", inventoryService({ failMovements: 1 }));
    expect(await screen.findByRole("alert")).toHaveTextContent("could not be loaded");
    expect(screen.queryByRole("heading", { name: "No movements posted" })).not.toBeInTheDocument();
  });

  it("converts a pack entry into exact base-unit atoms and shows it before posting", async () => {
    const app = renderApp("/app/inventory/stock");
    fireEvent.click(await screen.findByRole("button", { name: "Post Opening Stock" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Opening Stock" });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Product" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Product" }), { target: { value: IDs.product } });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Pack" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Pack" }), { target: { value: IDs.pack } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Quantity/ }), { target: { value: "5" } });
    // Five strips of ten tablets is fifty base units, shown before anything is posted.
    expect(within(dialog).getByText("50 base units")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Opening Stock" }));
    await waitFor(() => {
      const [sent] = bodiesFor(app.fetchMock, (url, method) => url.endsWith("/inventory/movements") && method === "POST");
      expect(sent).toMatchObject({ movementType: "opening_stock", productPackId: IDs.pack, quantityDeltaAtoms: 50, occurredOn: expect.any(String) });
      // The Store is resolved by the service; the browser never claims it.
      expect(sent).not.toHaveProperty("storeId");
    });
    expect(await screen.findByRole("cell", { name: "50" })).toBeInTheDocument();
  });

  it("posts a loose base-unit quantity and attaches a batch", async () => {
    const app = renderApp("/app/inventory/stock");
    fireEvent.click(await screen.findByRole("button", { name: "Post Opening Stock" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Opening Stock" });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Product" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Product" }), { target: { value: IDs.product } });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Pack" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Pack" }), { target: { value: IDs.pack } });
    await waitFor(() => expect(within(dialog).getByRole("option", { name: /AB-123/ })).toBeInTheDocument());
    fireEvent.change(within(dialog).getByRole("combobox", { name: /Batch/ }), { target: { value: IDs.batch } });
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Quantity entered in" }), { target: { value: "base" } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Quantity/ }), { target: { value: "7" } });
    expect(within(dialog).getByText("7 base units")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Opening Stock" }));
    await waitFor(() => {
      const [sent] = bodiesFor(app.fetchMock, (url, method) => url.endsWith("/inventory/movements") && method === "POST");
      expect(sent).toMatchObject({ quantityDeltaAtoms: 7, batchId: IDs.batch });
    });
  });

  it("refuses an invalid quantity before it reaches the service", async () => {
    const app = renderApp("/app/inventory/stock");
    fireEvent.click(await screen.findByRole("button", { name: "Post Opening Stock" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Opening Stock" });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Product" })).toBeEnabled());
    // A quantity with no Pack selected must be refused before anything is sent.
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Quantity/ }), { target: { value: "5" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Opening Stock" }));
    expect(await within(dialog).findByText(/Select the Product and Pack/)).toBeInTheDocument();

    fireEvent.change(within(dialog).getByRole("combobox", { name: "Product" }), { target: { value: IDs.product } });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Pack" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Pack" }), { target: { value: IDs.pack } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Quantity/ }), { target: { value: "0" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Opening Stock" }));
    expect(await within(dialog).findByText(/positive quantity/)).toBeInTheDocument();
    expect(bodiesFor(app.fetchMock, (url, method) => url.endsWith("/inventory/movements") && method === "POST")).toHaveLength(0);
  });

  it("reports insufficient stock safely without raw backend detail", async () => {
    renderApp("/app/inventory/stock", inventoryService({ postError: { code: "insufficient_stock", status: 409, extra: { availableAtoms: 10 } } }));
    fireEvent.click(await screen.findByRole("button", { name: "Post Opening Stock" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Opening Stock" });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Product" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Product" }), { target: { value: IDs.product } });
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: "Pack" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: "Pack" }), { target: { value: IDs.pack } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Quantity/ }), { target: { value: "1" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Opening Stock" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(/negative stock balance/i);
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("hides posting from pharmacist and cashier", async () => {
    for (const role of ["pharmacist", "cashier"] as const) {
      renderApp("/app/inventory/stock", inventoryService({ role, movements: [movement()] }));
      expect(await screen.findByRole("heading", { name: "Inventory" })).toBeInTheDocument();
      expect(screen.getByText("Read-only access")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Post Opening Stock" })).not.toBeInTheDocument();
      cleanup();
    }
  });

  it("labels every ledger cell so narrow viewports stay readable", async () => {
    renderApp("/app/inventory/ledger", inventoryService({ movements: [movement()] }));
    const table = await screen.findByRole("table");
    const headers = within(table).getAllByRole("columnheader").map((header) => header.textContent?.trim() ?? "");
    for (const cell of within(table).getAllByRole("row").slice(1).flatMap((row) => within(row).getAllByRole("cell"))) {
      const label = cell.getAttribute("data-label");
      expect(label).toBeTruthy();
      expect(headers).toContain(label);
    }
  });

  it("clears inventory caches when the session ends", async () => {
    const app = renderApp("/app/inventory/stock", inventoryService({ movements: [movement()] }));
    expect(await screen.findByRole("cell", { name: "50" })).toBeInTheDocument();
    const before = app.fetchMock.mock.calls.filter(([input]) => String(input).includes("/inventory/stock")).length;
    fireEvent.click(screen.getByRole("button", { name: /Inventory User/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Sign out" }));
    await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" });
    // Nothing from the ended session may be served to the next one.
    expect(app.fetchMock.mock.calls.filter(([input]) => String(input).includes("/inventory/stock")).length).toBe(before);
  });

  it("generates a UUIDv7-shaped idempotency key the Store Service will accept", () => {
    const key = newIdempotencyKey();
    expect(key).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(newIdempotencyKey()).not.toBe(key);
  });

  it("keeps the test double honest against the real contract schemas", async () => {
    const service = inventoryService({ movements: [movement(), movement({ id: `${IDs.movement}-b`, batchId: IDs.batch, quantityDeltaAtoms: -3, movementType: "adjustment" })] });
    // A double that drifts from the shared contract fails here rather than in production.
    const balances = await (await service.fetchMock("/api/v1/inventory/stock")).json();
    expect(() => StockBalanceSchema.array().parse(balances)).not.toThrow();
    const ledger = await (await service.fetchMock("/api/v1/inventory/movements")).json();
    expect(() => InventoryMovementSchema.array().parse(ledger)).not.toThrow();
    expect((ledger as unknown[]).length).toBe(2);
  });
});
