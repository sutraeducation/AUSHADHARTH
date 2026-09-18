import { expect, test, type Page } from "@playwright/test";

const IDs = {
  store: "01997000-0000-7000-8000-000000000008",
  maharashtra: "01997300-0000-7000-8000-000000000027",
  karnataka: "01997300-0000-7000-8000-000000000029",
  user: "01997000-0000-7000-8000-000000000100"
};
const GSTIN = "27AAPFU0939F1ZV";
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const states = [
  { id: IDs.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } },
  { id: IDs.karnataka, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "29", displayName: "Karnataka" } }
];

/**
 * The whole-profile projection the screen reads, built from the tax state this double keeps.
 * `sellerComplete` stays false throughout: this spec is about the GST half, and the blocking
 * callout that produces is asserted in the Vitest suite rather than duplicated here.
 */
function asProfile(current: Record<string, unknown>) {
  return {
    storeId: current.storeId, revision: current.revision, displayName: current.displayName,
    legalName: null, primaryPhone: null, primaryEmail: null,
    gstRegistrationStatus: current.gstRegistrationStatus, gstin: current.gstin,
    normalizedGstin: current.normalizedGstin, placeOfSupplyStateId: current.placeOfSupplyStateId,
    taxComplete: current.complete, address: null, licences: [],
    rule46sDeclarationApplicability: "unknown", einvoiceApplicability: "unknown", hsnTurnoverBand: "unknown", hsnTurnoverFinancialYear: null,
    sellerComplete: false,
    missingSellerFacts: [
      { field: "legalName", message: "Record the pharmacy's registered name in Store Profile." },
      { field: "address.line1", message: "Record the pharmacy's address in Store Profile." },
      { field: "licences", message: "Record at least one active drug sale licence in Store Profile." }
    ]
  };
}

/**
 * A stateful store-profile double. It refuses a GSTIN whose first two characters disagree with the
 * selected State, exactly as the real trigger does, so no test can pass against absent behaviour.
 */
async function mockStoreService(page: Page, options: { role?: "owner_admin" | "cashier"; initial?: Record<string, unknown> } = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    current: {
      storeId: IDs.store, displayName: "Care Pharmacy", revision: 1,
      gstRegistrationStatus: "unknown", gstin: null as string | null, normalizedGstin: null as string | null,
      placeOfSupplyStateId: null as string | null, complete: false, ...options.initial
    },
    saved: [] as Record<string, unknown>[]
  };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: role === "cashier" ? "Store Cashier" : "Store Owner", role, revision: 1 };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/reference/state-codes") return route.fulfill({ json: states });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });
    if (url.pathname === "/api/v1/store/profile" && method === "GET") {
      return route.fulfill({ json: asProfile(state.current) });
    }
    if (url.pathname === "/api/v1/store/tax-identity") {
      if (method === "GET") return route.fulfill({ json: state.current });
      if (role !== "owner_admin") return route.fulfill({ status: 403, json: { code: "authorization_denied", message: "denied", issues: [], expectedRevision: null, currentRevision: null } });
      const normalized = body.gstin ? String(body.gstin).replace(/\s+/g, "").toUpperCase() : null;
      const chosen = states.find((item) => item.id === body.placeOfSupplyStateId);
      // The GSTIN's first two characters are its State code.
      if (normalized && chosen && normalized.slice(0, 2) !== chosen.attributes.stateCode) {
        return route.fulfill({ status: 409, json: { code: "store_tax_conflict", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });
      }
      state.saved.push(body);
      state.current = { ...state.current, revision: state.current.revision + 1, gstRegistrationStatus: body.gstRegistrationStatus, gstin: body.gstin ?? null, normalizedGstin: normalized, placeOfSupplyStateId: body.placeOfSupplyStateId ?? null, complete: Boolean(body.placeOfSupplyStateId) };
      return route.fulfill({ json: state.current });
    }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });
  return state;
}

test("owner records the store place of supply and the warning clears", async ({ page }) => {
  const state = await mockStoreService(page);
  await page.goto("/app/settings/store");
  await expect(page.getByRole("heading", { name: "Store Profile" })).toBeVisible();
  await expect(page.getByText("Place of supply not recorded")).toBeVisible();

  await page.getByRole("button", { name: "Edit Tax Identity" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit Store Tax Identity" });
  await dialog.getByLabel("GST registration").selectOption("registered");
  await dialog.getByLabel("GSTIN *").fill(GSTIN);
  await dialog.getByLabel(/Place of supply/).selectOption(IDs.maharashtra);
  await dialog.getByRole("button", { name: "Save Tax Identity" }).click();

  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("27 · Maharashtra")).toBeVisible();
  await expect(page.getByText(GSTIN)).toBeVisible();
  await expect(page.getByText("Place of supply not recorded")).toHaveCount(0);
  expect(state.saved).toHaveLength(1);
  expect(state.saved[0]).toMatchObject({ expectedRevision: 1, gstRegistrationStatus: "registered", placeOfSupplyStateId: IDs.maharashtra });
});

test("a GSTIN that disagrees with the State is refused without backend detail", async ({ page }) => {
  await mockStoreService(page);
  await page.goto("/app/settings/store");
  await page.getByRole("button", { name: "Edit Tax Identity" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("GST registration").selectOption("registered");
  await dialog.getByLabel("GSTIN *").fill(GSTIN);
  // A Maharashtra GSTIN against a Karnataka place of supply.
  await dialog.getByLabel(/Place of supply/).selectOption(IDs.karnataka);
  await dialog.getByRole("button", { name: "Save Tax Identity" }).click();

  const alert = dialog.getByRole("alert");
  await expect(alert).toContainText("does not agree with the selected State");
  await expect(alert).not.toContainText("raw backend detail");
});

test("a read-only role can see the store profile but cannot change it", async ({ page }) => {
  await mockStoreService(page, {
    role: "cashier",
    initial: { gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN, placeOfSupplyStateId: IDs.maharashtra, complete: true }
  });
  await page.goto("/app/settings/store");
  await expect(page.getByText(GSTIN)).toBeVisible();
  await expect(page.getByText("Read-only access")).toBeVisible();
  await expect(page.getByRole("button", { name: "Edit Tax Identity" })).toHaveCount(0);
});

test("the store profile stays readable on a narrow viewport", async ({ page }) => {
  await mockStoreService(page, {
    initial: { gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN, placeOfSupplyStateId: IDs.maharashtra, complete: true }
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/settings/store");
  await expect(page.getByText("27 · Maharashtra")).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);

  await page.getByRole("button", { name: "Edit Tax Identity" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  const dialogOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(dialogOverflow).toBeLessThanOrEqual(1);
});
