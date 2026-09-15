import { expect, test, type Page } from "@playwright/test";

/**
 * Phase 1J stock operations in a real browser.
 *
 * These tests hold the two things a screenshot cannot: that the browser sends a counted quantity
 * and never a delta, and that the words on screen tell an operator the truth about where the goods
 * physically are. "Written off" and "removed" are different facts, and a pharmacy that confuses
 * them produces a stock figure nobody can reconcile against the shelf.
 */

const id = (suffix: string) => `01997000-0000-7000-8000-${suffix.padStart(12, "0")}`;
const IDs = {
  product: id("1"),
  tablet: id("2"),
  strip: id("3"),
  store: id("8"),
  pack: id("9"),
  batch: id("20"),
  expired: id("22"),
  operation: id("30"),
  line: id("31"),
  user: id("100")
};
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function catalogProduct() {
  return { id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-10", skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }] };
}
const batches = [
  { id: IDs.batch, productPackId: IDs.pack, batchNumber: "AB-123", normalizedBatchNumber: "AB-123", manufacturedOn: "2026-01-01", expiresOn: "2029-12-31", mrpPaise: 12550, revision: 1, status: "active", ...stamp },
  { id: IDs.expired, productPackId: IDs.pack, batchNumber: "AB-900", normalizedBatchNumber: "AB-900", manufacturedOn: "2024-01-01", expiresOn: "2025-04-30", mrpPaise: 12550, revision: 1, status: "active", ...stamp }
];

type State = {
  sent: Record<string, unknown>[];
  posted: string[];
  discarded: string[];
  operation: Record<string, unknown> | null;
};

/** A Store Service double that answers the shapes the contract publishes, and records what arrives. */
async function mockStockService(page: Page, options: { role?: "owner_admin" | "pharmacist" | "cashier" } = {}) {
  const role = options.role ?? "owner_admin";
  const current = 200;
  const state: State = { sent: [], posted: [], discarded: [], operation: null };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) : {};
    const json = (data: unknown, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(data) });
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Stock User", role, revision: 1 };

    if (url.pathname.endsWith("/system/info")) return json(system);
    if (url.pathname.endsWith("/auth/status")) return json({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/dashboard/summary")) return json({ storeDisplayName: "Care Pharmacy", activeProductCount: 1, activePackCount: 1 });
    if (url.pathname === "/api/v1/catalog/context") return json({ storeId: IDs.store });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return json([]);
    if (url.pathname === "/api/v1/products") return json([catalogProduct()]);
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return json(catalogProduct());
    if (/\/packs\/[^/]+\/batches$/.test(url.pathname)) return json(batches);
    if (url.pathname === "/api/v1/inventory/stock") return json([]);
    if (url.pathname === "/api/v1/inventory/movements") return json([]);

    if (url.pathname === "/api/v1/stock-operations" && method === "GET") return json(state.operation ? [state.operation] : []);
    if (url.pathname === "/api/v1/stock-operations" && method === "POST") {
      state.operation = { id: IDs.operation, storeId: IDs.store, operationKind: body.operationKind, businessDate: "2026-06-15", status: "draft", revision: 1, note: null, createdByUserId: IDs.user, createdAtUtc: stamp.createdAtUtc, updatedAtUtc: stamp.updatedAtUtc, postedByUserId: null, postedAtUtc: null, lines: [] };
      return json(state.operation, 201);
    }
    if (/\/stock-operations\/[^/]+\/lines$/.test(url.pathname) && method === "POST") {
      state.sent.push(body);
      const lines = (state.operation!.lines as unknown[]) ?? [];
      const line = {
        id: IDs.line, stockOperationId: IDs.operation, lineNumber: lines.length + 1,
        productId: IDs.product, productPackId: body.productPackId, batchId: body.batchId ?? null,
        stockStatus: body.stockStatus, targetStockStatus: body.targetStockStatus ?? null,
        direction: body.countedQuantity != null ? "count" : body.targetStockStatus ? "transfer" : body.direction,
        reasonCode: body.reasonCode, countedAtoms: body.countedQuantity ?? null,
        quantityAtoms: body.quantity ?? null, quantityBasis: body.quantityBasis, quantityPacks: null,
        appliedDeltaAtoms: null, note: body.note ?? null,
        productDisplayName: "Crocin 500 mg Tablet", packDisplayLabel: "Strip of 10", baseUnitLabel: "Tablet",
        batchNumber: body.batchId === IDs.expired ? "AB-900" : "AB-123",
        batchExpiresOn: body.batchId === IDs.expired ? "2025-04-30" : "2029-12-31",
        quantityScale: 0, baseQuantityAtoms: 10
      };
      state.operation = { ...state.operation!, revision: (state.operation!.revision as number) + 1, lines: [...lines, line] };
      return json(state.operation, 201);
    }
    if (/\/stock-operations\/[^/]+\/quote$/.test(url.pathname)) {
      const lines = (state.operation!.lines as Record<string, number | string | null>[]) ?? [];
      return json({
        operationId: IDs.operation, operationKind: state.operation!.operationKind,
        lines: lines.map((line) => ({
          lineId: line.id, lineNumber: line.lineNumber, productDisplayName: line.productDisplayName,
          batchNumber: line.batchNumber, stockStatus: line.stockStatus, targetStockStatus: line.targetStockStatus,
          reasonCode: line.reasonCode, currentAtoms: current,
          requestedAtoms: (line.countedAtoms ?? line.quantityAtoms ?? 0) as number,
          resultingAtoms: (line.countedAtoms ?? current - ((line.quantityAtoms as number) ?? 0)) as number,
          targetCurrentAtoms: null, targetResultingAtoms: null, sufficient: true
        })),
        postable: true
      });
    }
    if (/\/stock-operations\/[^/]+\/post$/.test(url.pathname)) {
      state.posted.push(IDs.operation);
      state.operation = { ...state.operation!, status: "posted" };
      return json(state.operation);
    }
    if (/^\/api\/v1\/stock-operations\/[^/]+$/.test(url.pathname) && method === "DELETE") {
      state.discarded.push(IDs.operation);
      state.operation = null;
      return route.fulfill({ status: 204, body: "" });
    }
    return route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ code: "not_found", message: "no", issues: [] }) });
  });
  return state;
}

async function chooseLot(page: Page, batchId = IDs.batch) {
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("combobox", { name: "Product" }).selectOption(IDs.product);
  await expect(dialog.getByRole("combobox", { name: "Pack" })).toHaveValue(IDs.pack);
  await dialog.getByRole("combobox", { name: "Batch" }).selectOption(batchId);
  return dialog;
}

test("a physical count sends what was counted and shows the server's variance", async ({ page }) => {
  const state = await mockStockService(page);
  await page.goto("/app/inventory/stock");
  await page.getByRole("button", { name: "Physical Count" }).click();

  const dialog = await chooseLot(page);
  await dialog.getByRole("textbox", { name: "Counted quantity" }).fill("185");
  await dialog.getByRole("button", { name: "Add line" }).click();

  await expect(page.getByText("200 → 185")).toBeVisible();
  expect(state.sent).toHaveLength(1);
  expect(state.sent[0]).toMatchObject({ countedQuantity: 185, quantityBasis: "base_unit" });
  // The browser decided nothing about how much stock exists.
  expect(state.sent[0].quantity).toBeNull();
  expect(JSON.stringify(state.sent[0])).not.toContain("delta");
});

test("an expired lot can only be written off, and the screen does not call that a disposal", async ({ page }) => {
  await mockStockService(page);
  await page.goto("/app/inventory/stock");
  await page.getByRole("button", { name: "Handle Expired Stock" }).click();

  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText(/does not mean it has been disposed of/i)).toBeVisible();
  await expect(dialog.getByRole("combobox", { name: "What happens to them" })).toHaveCount(0);

  await chooseLot(page, IDs.expired);
  await expect(dialog.getByText(/Expired 2025-04-30/)).toBeVisible();
});

test("damage offers both honest outcomes and posts the one chosen", async ({ page }) => {
  const state = await mockStockService(page);
  await page.goto("/app/inventory/stock");
  await page.getByRole("button", { name: "Mark Damaged" }).click();

  const dialog = await chooseLot(page);
  const outcome = dialog.getByRole("combobox", { name: "What happens to them" });
  await expect(outcome.getByRole("option")).toHaveText([
    "Write off — it can never be sold",
    "Hold in quarantine — a pharmacist will decide"
  ]);
  await outcome.selectOption("quarantined");
  await dialog.getByRole("textbox", { name: "Damaged quantity" }).fill("20");
  await dialog.getByRole("button", { name: "Add line" }).click();
  await expect(page.getByRole("row", { name: /AB-123/ })).toBeVisible();

  await page.getByRole("button", { name: /^Post mark damaged$/i }).click();
  await expect.poll(() => state.posted).toEqual([IDs.operation]);
  expect(state.sent[0]).toMatchObject({ targetStockStatus: "quarantined", reasonCode: "damage" });
});

test("backing out of a half-filled operation leaves nothing behind", async ({ page }) => {
  const state = await mockStockService(page);
  await page.goto("/app/inventory/stock");
  await page.getByRole("button", { name: "Physical Count" }).click();

  const dialog = await chooseLot(page);
  await dialog.getByRole("textbox", { name: "Counted quantity" }).fill("190");
  await dialog.getByRole("button", { name: "Add line" }).click();
  await expect(page.getByRole("row", { name: /AB-123/ })).toBeVisible();

  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect.poll(() => state.discarded).toEqual([IDs.operation]);
  expect(state.posted).toEqual([]);
});

test("a pharmacist gets floor work and a cashier gets none", async ({ page }) => {
  await mockStockService(page, { role: "pharmacist" });
  await page.goto("/app/inventory/stock");
  for (const label of ["Physical Count", "Mark Damaged", "Handle Expired Stock", "Quarantine"]) {
    await expect(page.getByRole("button", { name: label })).toBeVisible();
  }
  for (const label of ["Adjust Stock", "Remove / Dispose", "Post Opening Stock"]) {
    await expect(page.getByRole("button", { name: label })).toHaveCount(0);
  }

  await page.goto("/app/dashboard");
  await mockStockService(page, { role: "cashier" });
  await page.goto("/app/inventory/stock");
  await expect(page.getByText("Read-only access")).toBeVisible();
  await expect(page.getByRole("button", { name: "Physical Count" })).toHaveCount(0);
});

test("the stock operations workspace stays usable on a narrow viewport", async ({ page }) => {
  await mockStockService(page);
  await page.setViewportSize({ width: 390, height: 780 });
  await page.goto("/app/inventory/stock");

  await page.getByRole("button", { name: "Physical Count" }).click();
  const dialog = await chooseLot(page);
  await dialog.getByRole("textbox", { name: "Counted quantity" }).fill("185");
  await dialog.getByRole("button", { name: "Add line" }).click();
  await expect(page.getByText("200 → 185")).toBeVisible();

  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth
  );
  expect(overflow).toBeLessThanOrEqual(1);
  await expect(dialog.getByRole("button", { name: /^Post physical count$/i })).toBeVisible();
});
