import { expect, test, type Page } from "@playwright/test";

const id = (suffix: string) => `01997000-0000-7000-8000-${suffix.padStart(12, "0")}`;
const IDs = { product: id("1"), tablet: id("2"), strip: id("3"), store: id("8"), pack: id("9"), user: id("100") };
const TAX = {
  hsn: "01997500-0000-7000-8000-000000000001",
  archivedHsn: "01997500-0000-7000-8000-000000000002",
  category: "01997500-0000-7000-8000-000000000010",
  rate: "01997500-0000-7000-8000-000000000020"
};
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const HSN_CODES = [
  { id: TAX.hsn, kind: "hsn-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", hsnCode: "30049099", description: "Medicaments" } },
  { id: TAX.archivedHsn, kind: "hsn-codes", revision: 2, status: "archived", ...stamp, attributes: { jurisdiction: "IN", hsnCode: "99999999", description: "Withdrawn heading" } }
];
const TAX_CATEGORIES = [
  { id: TAX.category, kind: "tax-categories", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", categoryCode: "gst-12", displayName: "GST 12%", taxTreatment: "taxable" } }
];
const RATE = { taxRateVersionId: TAX.rate, effectiveFrom: "2025-01-01", effectiveTo: null, cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1200, cessBasisPoints: 0 };

function catalogProduct(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null,
    baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet",
    formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-10", skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }],
    ...overrides
  };
}

/**
 * A stateful classification double. The rate always follows the Tax Category, exactly as the Store
 * Service resolves it, so no test can pass against a rate stored on the Product.
 */
async function mockTaxService(page: Page, options: { role?: "owner_admin" | "cashier"; initial?: Record<string, unknown>; failClassification?: boolean } = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    current: { productId: IDs.product, revision: 1, hsnCodeId: null as string | null, taxCategoryId: null as string | null, complete: false, asOf: "2026-09-13", applicableRate: null as unknown, ...options.initial },
    saved: [] as Record<string, unknown>[],
    failNext: Boolean(options.failClassification)
  };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: role === "cashier" ? "Store Cashier" : "Store Owner", role, revision: 1 };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/catalog/context") return route.fulfill({ json: { storeId: IDs.store } });
    if (url.pathname === "/api/v1/reference/hsn-codes") return route.fulfill({ json: HSN_CODES });
    if (url.pathname === "/api/v1/reference/tax-categories") return route.fulfill({ json: TAX_CATEGORIES });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });

    if (/\/tax-classification$/.test(url.pathname)) {
      if (method === "GET") {
        if (state.failNext) { state.failNext = false; return route.fulfill({ status: 500, json: { code: "internal_error", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } }); }
        return route.fulfill({ json: state.current });
      }
      if (role !== "owner_admin") return route.fulfill({ status: 403, json: { code: "authorization_denied", message: "denied", issues: [], expectedRevision: null, currentRevision: null } });
      state.saved.push(body);
      state.current = {
        ...state.current, revision: state.current.revision + 1,
        hsnCodeId: body.hsnCodeId, taxCategoryId: body.taxCategoryId,
        complete: Boolean(body.hsnCodeId && body.taxCategoryId),
        applicableRate: body.taxCategoryId ? RATE : null
      };
      return route.fulfill({ json: state.current });
    }
    if (url.pathname === "/api/v1/products") return route.fulfill({ json: [catalogProduct()] });
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return route.fulfill({ json: catalogProduct({ hsnCodeId: state.current.hsnCodeId, taxCategoryId: state.current.taxCategoryId }) });
    }
    if (/\/packs\/[^/]+\/(batches|barcodes)$/.test(url.pathname)) return route.fulfill({ json: [] });
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) return route.fulfill({ status: 204, body: "" });
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });
  return state;
}

const panel = (page: Page) => page.getByRole("region", { name: "Tax Classification" });

test("owner classifies a Product and the detail page shows the dated rate", async ({ page }) => {
  const state = await mockTaxService(page);
  await page.goto(`/app/products/${IDs.product}`);
  await expect(panel(page)).toBeVisible();
  await expect(panel(page).getByText("Incomplete")).toBeVisible();

  await page.getByRole("button", { name: "Edit Classification" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit Tax Classification" });
  await dialog.getByLabel("HSN code").selectOption(TAX.hsn);
  await dialog.getByLabel("Tax Category").selectOption(TAX.category);
  await dialog.getByRole("button", { name: "Save Classification" }).click();

  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(panel(page).getByText("30049099 · Medicaments")).toBeVisible();
  await expect(panel(page).getByText("gst-12 · GST 12%")).toBeVisible();
  await expect(panel(page).getByText("Complete")).toBeVisible();
  // The rate is presented with the date it belongs to, never as Product metadata.
  await expect(panel(page).getByText("Rate in force on 2026-09-13")).toBeVisible();
  await expect(panel(page).getByText("CGST 6.00%")).toBeVisible();
  await expect(panel(page).getByText("IGST 12.00%")).toBeVisible();

  expect(state.saved).toHaveLength(1);
  expect(state.saved[0]).toMatchObject({ expectedRevision: 1, hsnCodeId: TAX.hsn, taxCategoryId: TAX.category });
  // The browser never submits a rate.
  expect(state.saved[0]).not.toHaveProperty("cgstBasisPoints");
});

test("an unclassified Product reports incomplete rather than looking broken", async ({ page }) => {
  await mockTaxService(page);
  await page.goto(`/app/products/${IDs.product}`);
  await expect(panel(page).getByText("Incomplete")).toBeVisible();
  await expect(panel(page).getByText(/Assign a Tax Category to resolve/)).toBeVisible();
  await expect(panel(page).getByRole("alert")).toHaveCount(0);
});

test("a failed classification load is distinct from an unclassified Product and retries", async ({ page }) => {
  await mockTaxService(page, { failClassification: true });
  await page.goto(`/app/products/${IDs.product}`);
  const alert = panel(page).getByRole("alert");
  await expect(alert).toContainText("could not be loaded");
  await expect(alert).not.toContainText("raw backend detail");
  await expect(panel(page).getByText("Incomplete")).toHaveCount(0);

  await alert.getByRole("button", { name: "Retry" }).click();
  await expect(panel(page).getByText("Incomplete")).toBeVisible();
});

test("a read-only role sees the classification without any control", async ({ page }) => {
  await mockTaxService(page, {
    role: "cashier",
    initial: { hsnCodeId: TAX.hsn, taxCategoryId: TAX.category, complete: true, applicableRate: RATE }
  });
  await page.goto(`/app/products/${IDs.product}`);
  await expect(panel(page).getByText("30049099 · Medicaments")).toBeVisible();
  await expect(panel(page).getByText("CGST 6.00%")).toBeVisible();
  await expect(page.getByRole("button", { name: "Edit Classification" })).toHaveCount(0);
});

test("the tax classification section stays readable on a narrow viewport", async ({ page }) => {
  await mockTaxService(page, {
    initial: { hsnCodeId: TAX.hsn, taxCategoryId: TAX.category, complete: true, applicableRate: RATE }
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/app/products/${IDs.product}`);
  await expect(panel(page).getByText("30049099 · Medicaments")).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);

  // The dialog is usable at this width too.
  await page.getByRole("button", { name: "Edit Classification" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  const dialogOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(dialogOverflow).toBeLessThanOrEqual(1);
});
