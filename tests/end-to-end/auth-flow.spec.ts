import { expect, test, type Page } from "@playwright/test";

const system = {
  status: "ok",
  apiVersion: "v1",
  applicationVersion: "0.0.0",
  compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 }
};
const user = {
  id: "01900000-0000-7000-8000-000000000001",
  loginIdentifier: "owner",
  displayName: "Store Owner",
  role: "owner_admin",
  revision: 1
};

async function mockLocalService(page: Page, initial: { setupRequired: boolean; authenticated: boolean }) {
  const state = { ...initial };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: state.setupRequired, authenticated: state.authenticated, user: state.authenticated ? user : null, storeDisplayName: state.setupRequired ? null : "Care Pharmacy" } });
    if (url.pathname === "/api/v1/auth/setup") { state.setupRequired = false; state.authenticated = true; return route.fulfill({ status: 201, json: { user, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" } }); }
    if (url.pathname === "/api/v1/auth/login") { state.authenticated = true; return route.fulfill({ json: { user, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" } }); }
    if (url.pathname === "/api/v1/auth/logout") { state.authenticated = false; return route.fulfill({ status: 204, body: "" }); }
    if (url.pathname === "/api/v1/dashboard/summary") return route.fulfill({ json: { storeDisplayName: "Care Pharmacy", activeProductCount: 4, activePackCount: 7 } });
    return route.abort();
  });
  return state;
}

async function completeLogin(page: Page) {
  await page.getByLabel("Login ID").fill("owner");
  await page.getByLabel("Password", { exact: true }).fill("Strong-Password-42");
  await page.getByRole("button", { name: "Sign In" }).click();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
}

test("first-run setup creates the workspace and opens the dashboard", async ({ page }) => {
  await mockLocalService(page, { setupRequired: true, authenticated: false });
  await page.goto("/");
  await expect(page).toHaveURL(/\/setup$/);
  await page.getByLabel("Business / Store name").fill("Care Pharmacy");
  await page.getByLabel("Owner / Admin name").fill("Store Owner");
  await page.getByRole("textbox", { name: /Login ID/ }).fill("owner");
  await page.getByLabel("Password", { exact: true }).fill("Strong-Password-42");
  await page.getByLabel("Confirm password", { exact: true }).fill("Strong-Password-42");
  await page.getByRole("button", { name: "Create Workspace" }).click();
  await expect(page).toHaveURL(/\/app\/dashboard$/);
  await expect(page.getByText("Active products")).toBeVisible();
});

test("logout returns to login and a subsequent login restores the dashboard", async ({ page }) => {
  await mockLocalService(page, { setupRequired: false, authenticated: true });
  await page.goto("/app/dashboard");
  await page.getByRole("button", { name: /Store Owner/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login$/);
  await completeLogin(page);
});

test("protected routes redirect unauthenticated users", async ({ page }) => {
  await mockLocalService(page, { setupRequired: false, authenticated: false });
  await page.goto("/app/dashboard");
  await expect(page).toHaveURL(/\/login$/);
  await expect(page.getByRole("heading", { name: "Sign in to AUSHADHARTH" })).toBeVisible();
});

test("an authenticated browser refresh preserves the server-authoritative session", async ({ page }) => {
  await mockLocalService(page, { setupRequired: false, authenticated: false });
  await page.goto("/login");
  await completeLogin(page);
  await page.reload();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
});

test("internet offline is secondary while the authenticated local workspace remains usable", async ({ context, page }) => {
  await mockLocalService(page, { setupRequired: false, authenticated: true });
  await page.goto("/app/dashboard");
  await expect(page.getByText("Local Service").first()).toContainText("Online");
  await context.setOffline(true);
  try {
    await expect(page.getByText("Internet Offline")).toBeVisible();
    await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
  } finally {
    await context.setOffline(false);
  }
});
