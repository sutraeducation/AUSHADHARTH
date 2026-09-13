import { expect, test, type Page } from "@playwright/test";

const id = (suffix: string) => `01997400-0000-7000-8000-${suffix.padStart(12, "0")}`;
const IDs = { party: id("1"), created: id("2"), role: id("10"), address: id("20"), user: id("100") };
const STATE = { maharashtra: "01997300-0000-7000-8000-000000000027", karnataka: "01997300-0000-7000-8000-000000000029" };
const GSTIN = "27AAPFU0939F1ZV";
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const states = [
  { id: STATE.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } },
  { id: STATE.karnataka, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "29", displayName: "Karnataka" } }
];

function supplierRecord() {
  return {
    id: IDs.party, displayName: "Sharma Medicals", legalName: "Sharma Medical And General Stores Private Limited",
    normalizedSearchName: "sharma medicals", gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN,
    pan: null, normalizedPan: null, placeOfSupplyStateId: STATE.maharashtra, primaryPhone: "+919820012345",
    primaryEmail: "sales@sharma-medicals.co.in", drugLicenceNumber: "20B-1234 / 21B-5678", drugLicenceValidUpto: "2028-03-31",
    revision: 1, status: "active", ...stamp,
    roles: [{ id: IDs.role, partyId: IDs.party, role: "supplier", revision: 1, status: "active", ...stamp }],
    addresses: [{ id: IDs.address, partyId: IDs.party, addressRole: "billing", line1: "12 Market Road", line2: null, city: "Pune", stateId: STATE.maharashtra, postalCode: "411001", countryCode: "IN", isPrimary: true, revision: 1, status: "active", ...stamp }]
  };
}

/**
 * A stateful party double. It refuses what the real Store Service refuses — a duplicate GSTIN and
 * archiving while an active role remains — so no test can pass against behaviour the service lacks.
 */
async function mockPartyService(page: Page, options: { role?: "owner_admin" | "cashier"; candidates?: unknown[] } = {}) {
  const role = options.role ?? "owner_admin";
  const state = { parties: [supplierRecord()] as ReturnType<typeof supplierRecord>[], created: [] as Record<string, unknown>[] };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: role === "cashier" ? "Store Cashier" : "Store Owner", role, revision: 1 };
    const deny = { status: 403, json: { code: "authorization_denied", message: "denied", issues: [], expectedRevision: null, currentRevision: null } };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/reference/state-codes") return route.fulfill({ json: states });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });

    if (url.pathname === "/api/v1/parties/duplicate-candidates") return route.fulfill({ json: options.candidates ?? [] });
    if (url.pathname === "/api/v1/parties" && method === "GET") {
      const search = (url.searchParams.get("search") ?? "").toLowerCase();
      const status = url.searchParams.get("status") ?? "active";
      return route.fulfill({ json: state.parties.filter((party) => (status === "all" || party.status === status) && (!search || `${party.displayName} ${party.normalizedGstin ?? ""}`.toLowerCase().includes(search))) });
    }
    if (url.pathname === "/api/v1/parties" && method === "POST") {
      if (role !== "owner_admin") return route.fulfill(deny);
      // One active party per GSTIN, exactly as the partial unique index enforces.
      if (body.party.gstin && state.parties.some((party) => party.status === "active" && party.normalizedGstin === String(body.party.gstin).replace(/\s+/g, "").toUpperCase())) {
        return route.fulfill({ status: 409, json: { code: "duplicate_conflict", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });
      }
      state.created.push(body);
      const created = { ...supplierRecord(), id: IDs.created, displayName: body.party.displayName, gstin: body.party.gstin ?? null, normalizedGstin: body.party.gstin ? String(body.party.gstin).replace(/\s+/g, "").toUpperCase() : null, gstRegistrationStatus: body.party.gstRegistrationStatus, addresses: [] };
      state.parties.push(created);
      return route.fulfill({ status: 201, json: created });
    }
    const detail = /^\/api\/v1\/parties\/([^/]+)$/.exec(url.pathname);
    if (detail && method === "GET") {
      const found = state.parties.find((party) => party.id === detail[1]);
      return found ? route.fulfill({ json: found }) : route.fulfill({ status: 404, json: { code: "not_found", message: "gone", issues: [], expectedRevision: null, currentRevision: null } });
    }
    if (/\/parties\/[^/]+\/addresses$/.test(url.pathname)) {
      if (role !== "owner_admin") return route.fulfill(deny);
      const index = state.parties.findIndex((party) => url.pathname.includes(party.id));
      const added = { id: `${IDs.address}-b`, partyId: state.parties[index].id, addressRole: body.addressRole, line1: body.line1, line2: null, city: null, stateId: body.stateId ?? null, postalCode: null, countryCode: "IN", isPrimary: Boolean(body.isPrimary), revision: 1, status: "active", ...stamp };
      state.parties[index] = { ...state.parties[index], addresses: [...state.parties[index].addresses, added] };
      return route.fulfill({ status: 201, json: added });
    }
    if (/\/party-roles\/[^/]+\/archive$/.test(url.pathname)) {
      if (role !== "owner_admin") return route.fulfill(deny);
      const index = state.parties.findIndex((party) => party.roles.some((item) => item.id === url.pathname.split("/")[4]));
      const archived = state.parties[index].roles.map((item) => ({ ...item, status: "archived", revision: item.revision + 1, archivedAtUtc: "2026-02-01T00:00:00Z", archiveReason: body.reason }));
      state.parties[index] = { ...state.parties[index], roles: archived };
      return route.fulfill({ json: archived[0] });
    }
    if (/\/parties\/[^/]+\/archive$/.test(url.pathname)) {
      if (role !== "owner_admin") return route.fulfill(deny);
      const index = state.parties.findIndex((party) => party.id === url.pathname.split("/")[4]);
      // Archiving is refused while an active child remains, as lifecycle_product established.
      if (state.parties[index].roles.some((item) => item.status === "active")) {
        return route.fulfill({ status: 409, json: { code: "archived_conflict", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });
      }
      state.parties[index] = { ...state.parties[index], status: "archived", revision: state.parties[index].revision + 1, archivedAtUtc: "2026-02-01T00:00:00Z", archiveReason: body.reason };
      return route.fulfill({ json: state.parties[index] });
    }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });
  return state;
}

test("an owner records a registered supplier and sees it listed", async ({ page }) => {
  const state = await mockPartyService(page);
  await page.goto("/app/parties");
  await expect(page.getByRole("heading", { name: "Suppliers" })).toBeVisible();
  await expect(page.getByRole("cell", { name: GSTIN })).toBeVisible();
  await expect(page.getByRole("cell", { name: "27 · Maharashtra" })).toBeVisible();

  await page.getByRole("link", { name: "Add Supplier" }).click();
  await page.getByLabel("Supplier name *").fill("Bharat Distributors");
  await page.getByLabel("GST registration").selectOption("registered");
  await page.getByLabel("GSTIN *").fill("29 aagcb 7383 j1z4");
  // The preview shows exactly what the Store Service will compare.
  await expect(page.getByText("29AAGCB7383J1Z4", { exact: true })).toBeVisible();
  await page.getByLabel("Place of supply (State) *").selectOption(STATE.karnataka);
  await page.getByRole("button", { name: "Review & Create Supplier" }).click();

  await expect(page.getByRole("heading", { name: "Bharat Distributors" })).toBeVisible();
  expect(state.created).toHaveLength(1);
  expect(state.created[0]).toMatchObject({ roles: [{ role: "supplier" }] });
  // The browser never claims a Store; the service resolves it.
  expect(state.created[0].party).not.toHaveProperty("storeId");
});

test("a duplicate GSTIN is refused without exposing backend detail", async ({ page }) => {
  await mockPartyService(page);
  await page.goto("/app/parties/new");
  await page.getByLabel("Supplier name *").fill("Sharma Medicals Branch");
  await page.getByLabel("GST registration").selectOption("registered");
  await page.getByLabel("GSTIN *").fill(GSTIN);
  await page.getByLabel("Place of supply (State) *").selectOption(STATE.maharashtra);
  await page.getByRole("button", { name: "Review & Create Supplier" }).click();

  const alert = page.getByRole("alert");
  await expect(alert).toContainText("conflicting active record");
  await expect(alert).not.toContainText("raw backend detail");
});

test("archiving is refused while an active role remains, then succeeds", async ({ page }) => {
  await mockPartyService(page);
  await page.goto(`/app/parties/${IDs.party}`);
  await expect(page.getByRole("heading", { name: "Sharma Medicals" })).toBeVisible();
  await expect(page.getByText("20B-1234 / 21B-5678")).toBeVisible();

  await page.getByRole("button", { name: "Archive supplier" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Reason *").fill("Closed down");
  await dialog.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText("archive status");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  const roleRow = page.getByRole("row").filter({ has: page.getByRole("cell", { name: "Supplier", exact: true }) });
  await roleRow.getByRole("button", { name: "Archive" }).click();
  const roleDialog = page.getByRole("dialog");
  await roleDialog.getByLabel("Reason *").fill("No longer supplies us");
  await roleDialog.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);

  await page.getByRole("button", { name: "Archive supplier" }).click();
  const retry = page.getByRole("dialog");
  await retry.getByLabel("Reason *").fill("Closed down");
  await retry.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(page.getByRole("button", { name: "Restore supplier" })).toBeVisible();
});

test("a read-only role can see suppliers but cannot change them", async ({ page }) => {
  await mockPartyService(page, { role: "cashier" });
  await page.goto("/app/parties");
  await expect(page.getByRole("cell", { name: GSTIN })).toBeVisible();
  await expect(page.getByText("Read-only access")).toBeVisible();
  await expect(page.getByRole("link", { name: "Add Supplier" })).toHaveCount(0);
});

test("the supplier pages stay readable on a narrow viewport", async ({ page }) => {
  await mockPartyService(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/parties");
  await expect(page.getByRole("cell", { name: GSTIN })).toBeVisible();
  const listOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(listOverflow).toBeLessThanOrEqual(1);

  await page.getByRole("link", { name: "Sharma Medicals" }).click();
  await expect(page.getByRole("heading", { name: "Sharma Medicals" })).toBeVisible();
  const detailOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(detailOverflow).toBeLessThanOrEqual(1);
  const labels = await page.locator("table.data-table tbody td[data-label]").evaluateAll((cells) => cells.map((cell) => cell.getAttribute("data-label")));
  expect(labels).toContain("Purpose");
  expect(labels).toContain("Role");
});
