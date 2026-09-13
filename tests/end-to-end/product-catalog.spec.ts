import { expect, test, type Page } from "@playwright/test";

const id = (suffix: string) => `01997000-0000-7000-8000-${suffix.padStart(12, "0")}`;
const IDs = { product: id("1"), tablet: id("2"), strip: id("3"), box: id("4"), dosage: id("5"), brand: id("6"), company: id("7"), store: id("8"), pack: id("9"), pack2: id("10"), policy: id("11"), barcode: id("12") };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const references = {
  units: [ref("units", IDs.tablet, { canonicalCode: "tablet", displayName: "Tablet", dimension: "count", isDiscrete: true, allowedScale: 0 }), ref("units", IDs.strip, { canonicalCode: "strip", displayName: "Strip", dimension: "count", isDiscrete: true, allowedScale: 0 }), ref("units", IDs.box, { canonicalCode: "box", displayName: "Box", dimension: "count", isDiscrete: true, allowedScale: 0 })],
  "dosage-forms": [ref("dosage-forms", IDs.dosage, { canonicalCode: "tablet", displayName: "Tablet", description: null, routeHint: "oral", releaseHint: null })],
  brands: [ref("brands", IDs.brand, { displayName: "Crocin", brandOwnerCompanyId: IDs.company })],
  companies: [ref("companies", IDs.company, { displayName: "GSK Pharma", legalName: null, city: null, state: null, countryCode: "IN" })]
};
function ref(kind: string, recordId: string, attributes: object) { return { id: recordId, kind, revision: 1, status: "active", attributes, ...stamp }; }
function catalogProduct() { return { id: IDs.product, productKind: "medicine", brandId: IDs.brand, dosageFormId: IDs.dosage, baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: "500 mg", routeDescriptor: "Oral", releaseDescriptor: null, revision: 1, status: "active", ...stamp, companyRoles: [], packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 15, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-15", skuStoreId: IDs.store, displayLabel: "Strip of 15", revision: 1, status: "active", ...stamp }] }; }

// Phase 1B: sku_store_id is present exactly when sku_code is present. The double rejects the pairing
// the Store Service rejects, so an unscoped SKU can never pass an end-to-end run.
function skuPairingViolation(pack: { skuCode?: unknown; skuStoreId?: unknown }) {
  const hasSku = typeof pack.skuCode === "string" && pack.skuCode.trim().length > 0;
  const hasStore = typeof pack.skuStoreId === "string" && pack.skuStoreId.length > 0;
  return hasSku !== hasStore;
}
const unprocessable = { status: 422, json: { code: "validation_failed", message: "validation failed", issues: [{ field: "skuStoreId", message: "is required exactly when skuCode is present" }] } };

async function mockCatalogService(page: Page, options: { authenticated?: boolean; role?: "owner_admin" | "cashier"; empty?: boolean } = {}) {
  const state = { authenticated: options.authenticated ?? true, role: options.role ?? "owner_admin", products: options.empty ? [] as ReturnType<typeof catalogProduct>[] : [catalogProduct()], policy: null as Record<string, unknown> | null, barcodes: [] as Record<string, unknown>[], referenceCalls: 0 };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    const user = { id: id("100"), loginIdentifier: state.role, displayName: state.role === "cashier" ? "Store Cashier" : "Store Owner", role: state.role, revision: 1 };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: state.authenticated, user: state.authenticated ? user : null, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/auth/logout") { state.authenticated = false; return route.fulfill({ status: 204, body: "" }); }
    if (url.pathname === "/api/v1/auth/login") { state.authenticated = true; return route.fulfill({ json: { user, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" } }); }
    if (!state.authenticated) return route.fulfill({ status: 401, json: { code: "authentication_required", message: "Sign in", issues: [] } });
    if (url.pathname === "/api/v1/catalog/context") return route.fulfill({ json: { storeId: IDs.store } });
    const refMatch = /^\/api\/v1\/reference\/([^/]+)$/.exec(url.pathname); if (refMatch) { state.referenceCalls += 1; return route.fulfill({ json: references[refMatch[1] as keyof typeof references] ?? [] }); }
    if (url.pathname === "/api/v1/products/duplicate-candidates") return route.fulfill({ json: [] });
    if (url.pathname === "/api/v1/products" && method === "GET") return route.fulfill({ json: state.products });
    if (url.pathname === "/api/v1/products" && method === "POST") {
      const body = request.postDataJSON();
      if ((body.packs ?? []).some(skuPairingViolation)) return route.fulfill(unprocessable);
      const created = catalogProduct(); created.displayName = body.product.displayName; created.packs[0] = { ...created.packs[0], ...body.packs[0], id: IDs.pack, productId: IDs.product, containedPackId: null, containedPackCount: null, revision: 1, status: "active", ...stamp }; state.products = [created]; return route.fulfill({ status: 201, json: created });
    }
    const productMatch = /^\/api\/v1\/products\/([^/]+)(?:\/(archive|restore))?$/.exec(url.pathname);
    if (productMatch) { const product = state.products[0]; if (method === "GET") return route.fulfill({ json: product }); product.status = productMatch[2] === "archive" ? "archived" : "active"; product.revision += 1; return route.fulfill({ json: product }); }
    if (/\/products\/[^/]+\/packs$/.test(url.pathname) && method === "POST") { const body = request.postDataJSON(); if (skuPairingViolation(body)) return route.fulfill(unprocessable); const pack = { id: IDs.pack2, productId: IDs.product, ...body, revision: 1, status: "active", ...stamp }; state.products[0].packs.push(pack); return route.fulfill({ status: 201, json: pack }); }
    if (/\/packs\/[^/]+$/.test(url.pathname) && method === "PUT") { const body = request.postDataJSON(); if (skuPairingViolation(body.pack)) return route.fulfill(unprocessable); const pack = state.products[0].packs.find((item) => url.pathname.endsWith(item.id))!; Object.assign(pack, body.pack, { revision: pack.revision + 1 }); return route.fulfill({ json: pack }); }
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) { if (method === "GET") return state.policy ? route.fulfill({ json: state.policy }) : route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [] } }); const body = request.postDataJSON(); state.policy = { id: IDs.policy, productId: IDs.product, packId: IDs.pack2, ...body.policy, revision: 1, status: "active", ...stamp }; return route.fulfill({ json: state.policy }); }
    if (/\/packs\/[^/]+\/barcodes$/.test(url.pathname)) { if (method === "GET") return route.fulfill({ json: state.barcodes }); const body = request.postDataJSON(); const barcode = { id: IDs.barcode, packId: IDs.pack2, namespace: body.namespace, normalizedValue: body.value, symbology: body.symbology, scope: body.scope, storeId: body.storeId, revision: 1, status: "active", ...stamp }; state.barcodes.push(barcode); return route.fulfill({ status: 201, json: barcode }); }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [] } });
  });
  return state;
}
async function login(page: Page) { await page.getByLabel("Login ID").fill("owner"); await page.getByLabel("Password", { exact: true }).fill("Strong-Password-42"); await page.getByRole("button", { name: "Sign In" }).click(); }

test("owner login opens Products and creates an atomic Product with its initial Pack", async ({ page }) => {
  const state = await mockCatalogService(page, { authenticated: false, empty: true }); await page.goto("/login"); await login(page); await page.getByRole("link", { name: "Products" }).click(); await page.getByRole("link", { name: "Add Product" }).click();
  await page.getByLabel("Display name").fill("Crocin 500 mg Tablet"); await page.getByRole("combobox", { name: /^Dosage Form/ }).selectOption(IDs.dosage); await page.getByRole("combobox", { name: /^Base Unit/ }).selectOption(IDs.tablet); await page.getByRole("combobox", { name: /^Pack Unit/ }).selectOption(IDs.strip); await page.getByLabel("Direct base quantity").fill("15"); await page.getByLabel("Pack label").fill("Strip of 15"); await page.getByLabel("SKU (optional)").fill("CROCIN-15"); await page.getByRole("button", { name: "Review & Create Product" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/products/${IDs.product}$`)); await expect(page.getByRole("heading", { name: "Crocin 500 mg Tablet" })).toBeVisible(); expect(state.products[0].packs).toHaveLength(1);
  await page.reload(); await expect(page.getByRole("cell", { name: "CROCIN-15" })).toBeVisible();
});

test("owner adds a contained Pack, configures defaults, and assigns a Barcode", async ({ page }) => {
  const state = await mockCatalogService(page); await page.goto(`/app/products/${IDs.product}`); await page.getByRole("button", { name: "Add Pack" }).click(); await page.getByRole("combobox", { name: /^Pack Unit/ }).selectOption(IDs.box); await page.getByLabel("Direct base quantity").fill("150"); await page.getByLabel("Contained Pack (optional)").selectOption(IDs.pack); await page.getByLabel("Contained Pack count").fill("10"); await page.getByLabel("Pack label").fill("Box of 10 strips"); await page.getByRole("button", { name: "Save" }).click();
  const row = page.getByRole("row").filter({ hasText: "Box of 10 strips" }); await expect(row).toContainText("10 × Strip of 15"); await row.getByRole("button", { name: "Manage" }).click(); await page.getByRole("button", { name: "Add Policy" }).click(); await page.getByLabel("Purchase enabled").check(); await page.getByLabel("Sale enabled").check(); await page.getByLabel("Default purchase Pack").check(); await page.getByLabel("Default sale Pack").check(); await page.getByRole("button", { name: "Save" }).click(); await page.getByRole("button", { name: "Add Barcode" }).click(); await page.getByLabel("Barcode value").fill("8901234567890"); await page.getByRole("button", { name: "Save" }).click(); await expect(page.getByText("8901234567890")).toBeVisible(); expect(state.policy).not.toBeNull(); expect(state.barcodes).toHaveLength(1);
});

test("owner archives and restores a Product without deleting it", async ({ page }) => {
  const state = await mockCatalogService(page); await page.goto(`/app/products/${IDs.product}`); await page.getByRole("button", { name: "Archive Product" }).click(); await page.getByLabel("Reason").fill("Duplicate catalog entry"); await page.getByRole("button", { name: "Archive record" }).click(); await expect(page.getByRole("button", { name: "Restore Product" })).toBeVisible(); await page.getByRole("button", { name: "Restore Product" }).click(); await page.getByLabel("Reason").fill("Confirmed valid"); await page.getByRole("button", { name: "Restore record" }).click(); await expect(page.getByRole("button", { name: "Archive Product" })).toBeVisible(); expect(state.products[0].status).toBe("active");
});

test("cashier has read-only Product access", async ({ page }) => {
  await mockCatalogService(page, { role: "cashier" }); await page.goto("/app/products"); await expect(page.getByRole("cell", { name: "Crocin 500 mg Tablet" })).toBeVisible(); await expect(page.getByText("Read-only access")).toBeVisible(); await expect(page.getByRole("link", { name: "Add Product" })).toHaveCount(0); await page.getByRole("link", { name: "Crocin 500 mg Tablet" }).click(); await expect(page.getByRole("button", { name: /Archive Product|Add Pack|Add Company Role/ })).toHaveCount(0);
});

test("an added Pack carries a Store-scoped SKU end to end", async ({ page }) => {
  const state = await mockCatalogService(page);
  await page.goto(`/app/products/${IDs.product}`);
  await page.getByRole("button", { name: "Add Pack" }).click();
  await page.getByRole("combobox", { name: /^Pack Unit/ }).selectOption(IDs.box);
  await page.getByLabel("Direct base quantity").fill("150");
  await page.getByLabel("Pack label").fill("Box of 10 strips");
  await page.getByLabel("SKU (optional)").fill("BOX-10");
  await page.getByRole("button", { name: "Save" }).click();
  // The Store Service rejects skuCode without skuStoreId, so a saved row proves the pair was sent.
  await expect(page.getByRole("cell", { name: "BOX-10" })).toBeVisible();
  expect(state.products[0].packs[1]).toMatchObject({ skuCode: "BOX-10", skuStoreId: IDs.store });

  await page.getByRole("row").filter({ hasText: "Box of 10 strips" }).getByRole("button", { name: "Edit" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit Pack" });
  await expect(dialog.getByLabel("SKU (optional)")).toHaveValue("BOX-10");
  await dialog.getByLabel("Pack label").fill("Box of ten strips");
  await dialog.getByRole("button", { name: "Save" }).click();
  await expect(dialog).toBeHidden();
  expect(state.products[0].packs[1]).toMatchObject({ skuCode: "BOX-10", skuStoreId: IDs.store, displayLabel: "Box of ten strips" });
});

test("signing out discards the previous session's Catalog reference data", async ({ page }) => {
  const state = await mockCatalogService(page);
  await page.goto("/app/products");
  await expect(page.getByRole("cell", { name: "Crocin", exact: true })).toBeVisible();
  await page.getByRole("button", { name: /Store Owner/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login$/);
  const afterSignOut = state.referenceCalls;

  await login(page);
  await page.getByRole("link", { name: "Products" }).click();
  await expect(page.getByRole("cell", { name: "Crocin", exact: true })).toBeVisible();
  expect(state.referenceCalls).toBeGreaterThan(afterSignOut);
});

test("the Product catalog stays readable on a narrow viewport", async ({ page }) => {
  await mockCatalogService(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/products");
  await expect(page.getByRole("cell", { name: "Crocin 500 mg Tablet" })).toBeVisible();
  // Collapsed columns must not push the page into horizontal scrolling.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);
  // Every visible data cell keeps its own caption once the header row is no longer adjacent.
  const labels = await page.locator("table.data-table tbody td[data-label]").evaluateAll((cells) => cells.map((cell) => cell.getAttribute("data-label")));
  expect(labels).toContain("Brand");
  expect(labels).toContain("Status");
});

test("unauthenticated Product route redirects while authenticated refresh remains protected", async ({ page }) => {
  await mockCatalogService(page, { authenticated: false }); await page.goto(`/app/products/${IDs.product}`); await expect(page).toHaveURL(/\/login$/); await login(page); await page.goto(`/app/products/${IDs.product}`); await expect(page.getByRole("heading", { name: "Crocin 500 mg Tablet" })).toBeVisible(); await page.reload(); await expect(page).toHaveURL(new RegExp(`/app/products/${IDs.product}$`)); await expect(page.getByRole("heading", { name: "Crocin 500 mg Tablet" })).toBeVisible();
});
