import { expect, test, type Page } from "@playwright/test";

const id = (suffix: string) => `01997000-0000-7000-8000-${suffix.padStart(12, "0")}`;
const IDs = { product: id("1"), tablet: id("2"), strip: id("3"), store: id("8"), pack: id("9"), batch: id("20"), movement: id("21"), user: id("100") };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function catalogProduct() {
  return { id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-10", skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }] };
}
function batchRecord() {
  return { id: IDs.batch, productPackId: IDs.pack, batchNumber: "AB-123", normalizedBatchNumber: "AB-123", manufacturedOn: "2026-01-01", expiresOn: "2029-12-31", mrpPaise: 12550, revision: 1, status: "active", ...stamp };
}

/**
 * A stateful ledger double. Balances are summed from movements exactly as the Store Service does,
 * so no test can accidentally assert against a stored quantity.
 */
async function mockInventoryService(page: Page, options: { role?: "owner_admin" | "cashier" } = {}) {
  const role = options.role ?? "owner_admin";
  const state = { movements: [] as Record<string, unknown>[], keys: new Map<string, Record<string, unknown>>() };
  const balances = () => {
    const totals = new Map<string, Record<string, unknown>>();
    for (const movement of state.movements) {
      const key = `${movement.productPackId}|${movement.batchId ?? ""}`;
      const existing = totals.get(key);
      if (existing) (existing.balanceAtoms as number) += movement.quantityDeltaAtoms as number;
      else totals.set(key, { productId: movement.productId, productPackId: movement.productPackId, batchId: movement.batchId, balanceAtoms: movement.quantityDeltaAtoms });
    }
    return [...totals.values()].filter((row) => row.balanceAtoms !== 0);
  };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    const user = { id: IDs.user, loginIdentifier: role, displayName: role === "cashier" ? "Store Cashier" : "Store Owner", role, revision: 1 };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/catalog/context") return route.fulfill({ json: { storeId: IDs.store } });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });
    if (url.pathname === "/api/v1/products") return route.fulfill({ json: [catalogProduct()] });
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return route.fulfill({ json: catalogProduct() });
    if (/\/packs\/[^/]+\/batches$/.test(url.pathname)) return route.fulfill({ json: [batchRecord()] });
    if (url.pathname === "/api/v1/inventory/stock") return route.fulfill({ json: balances() });
    if (url.pathname === "/api/v1/inventory/movements" && method === "GET") return route.fulfill({ json: [...state.movements].reverse() });
    if (url.pathname === "/api/v1/inventory/movements" && method === "POST") {
      if (role !== "owner_admin") return route.fulfill({ status: 403, json: { code: "authorization_denied", message: "denied", issues: [], availableAtoms: null } });
      const body = request.postDataJSON();
      // A replayed key returns the stored movement rather than duplicating stock.
      const replay = state.keys.get(String(body.idempotencyKey));
      if (replay) return route.fulfill({ json: replay });
      const movement = { id: `${IDs.movement}-${state.movements.length}`, storeId: IDs.store, productId: IDs.product, productPackId: body.productPackId, batchId: body.batchId ?? null, movementType: body.movementType, quantityDeltaAtoms: body.quantityDeltaAtoms, occurredOn: body.occurredOn, reason: body.reason ?? null, reversesMovementId: body.reversesMovementId ?? null, purchaseLineId: null, idempotencyKey: body.idempotencyKey, postedByUserId: IDs.user, postedAtUtc: "2026-04-01T00:00:00.000Z" };
      state.movements.push(movement);
      state.keys.set(String(body.idempotencyKey), movement);
      return route.fulfill({ status: 201, json: movement });
    }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], availableAtoms: null } });
  });
  return state;
}

async function postOpeningStock(page: Page, quantity: string, options: { batch?: boolean; base?: boolean } = {}) {
  await page.getByRole("button", { name: "Post Opening Stock" }).click();
  const dialog = page.getByRole("dialog", { name: "Post Opening Stock" });
  await dialog.getByRole("combobox", { name: "Product" }).selectOption(IDs.product);
  await dialog.getByRole("combobox", { name: "Pack" }).selectOption(IDs.pack);
  if (options.batch) await dialog.getByRole("combobox", { name: /Batch/ }).selectOption(IDs.batch);
  if (options.base) await dialog.getByRole("combobox", { name: "Quantity entered in" }).selectOption("base");
  await dialog.getByRole("textbox", { name: /Quantity/ }).fill(quantity);
  return dialog;
}

test("owner posts opening stock and the derived balance and ledger reflect it", async ({ page }) => {
  const state = await mockInventoryService(page);
  await page.goto("/app/inventory/stock");
  await expect(page.getByRole("heading", { name: "Inventory" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "No stock recorded" })).toBeVisible();

  const dialog = await postOpeningStock(page, "5");
  // Five strips of ten tablets is fifty base units, shown before posting.
  await expect(dialog.getByText("50 base units")).toBeVisible();
  await dialog.getByRole("button", { name: "Post Opening Stock" }).click();

  await expect(page.getByRole("cell", { name: "50", exact: true })).toBeVisible();
  expect(state.movements).toHaveLength(1);
  expect(state.movements[0]).toMatchObject({ movementType: "opening_stock", quantityDeltaAtoms: 50, productPackId: IDs.pack });
  // The browser never claims a Store; the service resolves it.
  expect(state.movements[0]).not.toHaveProperty("clientStoreId");

  await page.getByRole("link", { name: "Stock Ledger" }).click();
  await expect(page.getByRole("cell", { name: "+50", exact: true })).toBeVisible();
  await expect(page.getByRole("cell", { name: "Opening stock" })).toBeVisible();
});

test("a batch-aware posting keeps its balance separate", async ({ page }) => {
  const state = await mockInventoryService(page);
  await page.goto("/app/inventory/stock");
  const batched = await postOpeningStock(page, "3", { batch: true });
  await batched.getByRole("button", { name: "Post Opening Stock" }).click();
  await expect(page.getByRole("cell", { name: "AB-123" })).toBeVisible();

  const loose = await postOpeningStock(page, "7", { base: true });
  await loose.getByRole("button", { name: "Post Opening Stock" }).click();
  await expect(page.getByRole("cell", { name: "No batch" })).toBeVisible();

  expect(state.movements).toHaveLength(2);
  expect(state.movements[0]).toMatchObject({ quantityDeltaAtoms: 30, batchId: IDs.batch });
  expect(state.movements[1]).toMatchObject({ quantityDeltaAtoms: 7, batchId: null });
});

test("a read-only role can see stock but cannot post", async ({ page }) => {
  await mockInventoryService(page, { role: "cashier" });
  await page.goto("/app/inventory/stock");
  await expect(page.getByRole("heading", { name: "Inventory" })).toBeVisible();
  await expect(page.getByText("Read-only access")).toBeVisible();
  await expect(page.getByRole("button", { name: "Post Opening Stock" })).toHaveCount(0);
});

test("the inventory pages stay readable on a narrow viewport", async ({ page }) => {
  await mockInventoryService(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/inventory/stock");
  const dialog = await postOpeningStock(page, "5");
  await dialog.getByRole("button", { name: "Post Opening Stock" }).click();
  await expect(page.getByRole("cell", { name: "50", exact: true })).toBeVisible();

  const stockOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(stockOverflow).toBeLessThanOrEqual(1);

  await page.getByRole("link", { name: "Stock Ledger" }).click();
  await expect(page.getByRole("cell", { name: "+50", exact: true })).toBeVisible();
  const ledgerOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(ledgerOverflow).toBeLessThanOrEqual(1);
  const labels = await page.locator("table.data-table tbody td[data-label]").evaluateAll((cells) => cells.map((cell) => cell.getAttribute("data-label")));
  expect(labels).toContain("Quantity");
  expect(labels).toContain("Type");
});
