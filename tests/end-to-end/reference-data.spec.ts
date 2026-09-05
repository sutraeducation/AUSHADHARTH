import { expect, test, type Page } from "@playwright/test";

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function unit(id: string, code: string, name: string, status: "active" | "archived" = "active", revision = 1) {
  return { id, kind: "units", revision, status, attributes: { canonicalCode: code, displayName: name, dimension: "count", isDiscrete: true, allowedScale: 0 }, createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: status === "archived" ? "2026-01-02T00:00:00Z" : null, archiveReason: status === "archived" ? "Archived in test" : null };
}

async function mockReferenceService(page: Page, options: { authenticated: boolean; role?: "owner_admin" | "cashier" }) {
  const state = {
    authenticated: options.authenticated,
    role: options.role ?? "owner_admin",
    units: [unit("01997000-0000-7000-8000-000000000101", "tablet", "Tablet")]
  };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request(); const url = new URL(request.url());
    const user = { id: "01900000-0000-7000-8000-000000000001", loginIdentifier: state.role, displayName: state.role === "cashier" ? "Store Cashier" : "Store Owner", role: state.role, revision: 1 };
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: state.authenticated, user: state.authenticated ? user : null, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/auth/login") { state.authenticated = true; return route.fulfill({ json: { user, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" } }); }
    const match = /^\/api\/v1\/reference\/units(?:\/([^/]+))?(?:\/(archive|restore))?$/.exec(url.pathname);
    if (match) {
      if (!state.authenticated) return route.fulfill({ status: 401, json: { code: "authentication_required", message: "Sign in", issues: [], retryAfterSeconds: null } });
      if (request.method() === "GET") {
        const status = url.searchParams.get("status") ?? "active";
        return route.fulfill({ json: state.units.filter((item) => status === "all" || item.status === status) });
      }
      if (state.role !== "owner_admin") return route.fulfill({ status: 403, json: { code: "authorization_denied", message: "Denied", issues: [], retryAfterSeconds: null } });
      const body = request.postDataJSON() as { attributes?: ReturnType<typeof unit>["attributes"] };
      if (!match[1]) {
        const created = unit("01997000-0000-7000-8000-000000000102", body.attributes!.canonicalCode, body.attributes!.displayName);
        state.units.push(created); return route.fulfill({ status: 201, json: created });
      }
      const index = state.units.findIndex((item) => item.id === match[1]); const current = state.units[index];
      const next = { ...current, revision: current.revision + 1, attributes: body.attributes ?? current.attributes, status: match[2] === "archive" ? "archived" as const : match[2] === "restore" ? "active" as const : current.status, archivedAtUtc: match[2] === "archive" ? "2026-01-02T00:00:00Z" : null, archiveReason: match[2] === "archive" ? "Lifecycle test" : null };
      state.units[index] = next; return route.fulfill({ json: next });
    }
    return route.abort();
  });
  return state;
}

async function login(page: Page) {
  await page.getByLabel("Login ID").fill("owner");
  await page.getByLabel("Password", { exact: true }).fill("Strong-Password-42");
  await page.getByRole("button", { name: "Sign In" }).click();
}

test("owner login reaches Reference Data from the real Masters navigation", async ({ page }) => {
  await mockReferenceService(page, { authenticated: false });
  await page.goto("/login"); await login(page);
  await page.getByRole("link", { name: "Reference Data" }).click();
  await expect(page).toHaveURL(/\/app\/reference$/);
  await expect(page.getByRole("heading", { name: "Reference Data" })).toBeVisible();
});

test("owner creates, edits, archives, and restores a unit", async ({ page }) => {
  await mockReferenceService(page, { authenticated: true });
  await page.goto("/app/reference/units");
  await page.getByRole("button", { name: "Add unit" }).click();
  await page.getByLabel("Canonical code").fill("dose"); await page.getByLabel("Display name").fill("Dose"); await page.getByRole("button", { name: "Save" }).click();
  const row = page.getByRole("row").filter({ hasText: "Dose" }); await expect(row).toBeVisible();
  await row.getByRole("button", { name: "Edit" }).click(); await page.getByLabel("Display name").fill("Dose unit"); await page.getByRole("button", { name: "Save" }).click();
  const updated = page.getByRole("row").filter({ hasText: "Dose unit" }); await updated.getByRole("button", { name: "Archive" }).click(); await page.getByLabel("Reason").fill("No longer needed"); await page.getByRole("button", { name: "Archive record" }).click();
  await page.getByLabel("Status", { exact: true }).selectOption("archived"); const archived = page.getByRole("row").filter({ hasText: "Dose unit" }); await expect(archived).toBeVisible(); await archived.getByRole("button", { name: "Restore" }).click(); await page.getByRole("button", { name: "Restore record" }).click();
  await page.getByLabel("Status", { exact: true }).selectOption("active"); await expect(page.getByRole("row").filter({ hasText: "Dose unit" })).toBeVisible();
});

test("unauthenticated Reference Data route redirects to login", async ({ page }) => {
  await mockReferenceService(page, { authenticated: false }); await page.goto("/app/reference/units");
  await expect(page).toHaveURL(/\/login$/); await expect(page.getByRole("heading", { name: "Sign in to AUSHADHARTH" })).toBeVisible();
});

test("cashier can read Reference Data but cannot mutate it", async ({ page }) => {
  await mockReferenceService(page, { authenticated: true, role: "cashier" }); await page.goto("/app/reference/units");
  await expect(page.getByRole("cell", { name: "Tablet", exact: true })).toBeVisible(); await expect(page.getByText("Read-only access")).toBeVisible(); await expect(page.getByRole("button", { name: "Add unit" })).toHaveCount(0);
});

test("browser refresh preserves an authenticated Reference Data route", async ({ page }) => {
  await mockReferenceService(page, { authenticated: true }); await page.goto("/app/reference/units"); await expect(page.getByRole("heading", { name: "Units of Measure", exact: true })).toBeVisible();
  await page.reload(); await expect(page).toHaveURL(/\/app\/reference\/units$/); await expect(page.getByRole("cell", { name: "Tablet", exact: true })).toBeVisible();
});
