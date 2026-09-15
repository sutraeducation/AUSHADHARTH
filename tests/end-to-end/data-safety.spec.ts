import { expect, test, type Page } from "@playwright/test";

/**
 * Backup and restore in a real browser.
 *
 * Restoring is the only thing in AUSHADHARTH that deliberately destroys data, and the thing being
 * proved here is not that the buttons work. It is that an owner standing in front of this screen is
 * told what the file actually contains before they commit, that they cannot commit by leaning on the
 * keyboard, and that when something is refused they are told their existing records are still there.
 */

const IDs = {
  user: "01997b00-0000-7000-8000-000000000001",
  backup: "01997b00-0000-7000-8000-000000000002"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const REPORT = {
  product: "AUSHADHARTH",
  backupFormatVersion: 1,
  storeDisplayName: "Care Pharmacy",
  createdAtUtc: "2026-09-10T04:30:00.000Z",
  applicationVersion: "0.0.0",
  schemaVersion: 16,
  currentSchemaVersion: 16,
  bytes: 1_048_576,
  checksumVerified: true,
  compatibility: "ready",
  migrationRequired: false
};

interface Options {
  role?: "owner_admin" | "pharmacist";
  lastBackupAtUtc?: string | null;
  backups?: Array<Record<string, unknown>>;
  prepareError?: string;
  commitError?: string;
  restoring?: boolean;
}

/**
 * A stateful double for the backup routes.
 *
 * It refuses a commit whose password is empty and a prepare whose body is not octet-stream, exactly
 * as the service does, so no test here can pass against absent behaviour.
 */
async function mockStoreService(page: Page, options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    last: options.lastBackupAtUtc ?? null,
    backups: options.backups ?? ([] as Array<Record<string, unknown>>),
    committed: [] as Array<Record<string, unknown>>,
    uploadContentTypes: [] as string[]
  };
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store Owner", role, revision: 1 };
    const fail = (code: string, status = 409) =>
      route.fulfill({ status, json: { code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });

    if (options.restoring && url.pathname !== "/api/v1/auth/logout") return fail("service_restoring", 503);
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") {
      return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    }
    if (url.pathname === "/api/v1/backups/status") {
      if (role !== "owner_admin") return fail("authorization_denied", 403);
      return route.fulfill({
        json: {
          lastSuccessfulBackupAtUtc: state.last,
          reminderThresholdDays: 7,
          backupOverdue: state.last === null,
          backups: state.backups
        }
      });
    }
    if (url.pathname === "/api/v1/backups/create" && method === "POST") {
      state.last = "2026-09-15T06:00:00.000Z";
      state.backups = [
        {
          backupId: IDs.backup,
          filename: "AUSHADHARTH-care-pharmacy-20260915-060000-7000aaaa.aushbackup",
          createdAtUtc: state.last,
          bytes: 1_048_576,
          backupKind: "manual"
        }
      ];
      return route.fulfill({
        status: 201,
        json: { jobId: "job", stage: "ready", backupId: IDs.backup, filename: state.backups[0].filename, bytes: 1_048_576, errorCode: null }
      });
    }
    if (url.pathname.endsWith("/restore/prepare")) {
      const contentType = request.headers()["content-type"] ?? "";
      state.uploadContentTypes.push(contentType);
      // The service refuses form encodings precisely because a cross-site form can send them.
      if (!contentType.startsWith("application/octet-stream")) return fail("validation_failed", 422);
      if (options.prepareError) return fail(options.prepareError);
      return route.fulfill({ json: { candidateToken: "candidate-token", expiresInSeconds: 1800, report: REPORT } });
    }
    if (url.pathname.endsWith("/restore/commit")) {
      const body = request.postData() ? JSON.parse(request.postData()!) : {};
      if (!body.password) return fail("validation_failed", 422);
      if (options.commitError) return fail(options.commitError, options.commitError === "invalid_password" ? 403 : 409);
      state.committed.push(body);
      return route.fulfill({ json: { restoreId: "restore-1", restartRequired: true, safetyBackup: "safety.aushbackup" } });
    }
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });
  return state;
}

/** Hands the page a file the way the operating system's own dialog would. */
async function chooseBackup(page: Page, name = "care-pharmacy.aushbackup") {
  await page.setInputFiles('input[type="file"]', {
    name,
    mimeType: "application/octet-stream",
    buffer: Buffer.from([0x41, 0x55, 0x53, 0x48, 0x42, 0x41, 0x4b, 0x1a])
  });
}

test("an owner takes a backup and is told to keep it off this PC", async ({ page }) => {
  await mockStoreService(page);
  await page.goto("/app/settings/data-safety");

  await expect(page.getByText("This installation has never been backed up. Take a backup now.")).toBeVisible();
  await page.getByRole("button", { name: "Create Backup Now" }).click();

  await expect(page.getByText(/somewhere other than this PC/i)).toBeVisible();
  await expect(page.getByRole("table").getByText(/AUSHADHARTH-care-pharmacy/)).toBeVisible();
  // Unencrypted is a decision, and the screen that produces the file has to say so.
  await expect(page.getByText(/not encrypted/i)).toBeVisible();
});

test("a restore shows what the file is, and needs the owner's password", async ({ page }) => {
  const state = await mockStoreService(page);
  await page.goto("/app/settings/data-safety");

  await page.getByRole("button", { name: "Restore from a backup…" }).click();
  await page.getByRole("button", { name: "Choose backup file…" }).click();
  await chooseBackup(page);

  // Facts read out of the file, not off its name. Scoped to the panel, because the workspace name
  // in the header is what the browser already believed and proves nothing about the file.
  const panel = page.getByRole("region", { name: "Restore from a backup" });
  await expect(panel.getByText("Care Pharmacy")).toBeVisible();
  await expect(page.getByText(/the file matches its own record/i)).toBeVisible();
  await expect(page.getByText(/This replaces everything/i)).toBeVisible();

  const commit = page.getByRole("button", { name: /Replace all data with this backup/i });
  await expect(commit).toBeDisabled();
  await page.getByLabel("Confirm your password").fill("Owner-Password-1");
  await expect(commit).toBeEnabled();
  await commit.click();

  await expect(page.getByText(/Restore complete/i)).toBeVisible();
  await expect(page.getByText(/start the Local Store Service again/i)).toBeVisible();
  await expect(page.getByText("safety.aushbackup")).toBeVisible();
  expect(state.committed).toEqual([{ candidateToken: "candidate-token", password: "Owner-Password-1" }]);
  // Raw bytes, never a form: choosing multipart would have given away the CSRF defence.
  expect(state.uploadContentTypes[0]).toContain("application/octet-stream");
});

test("a file that is not a backup is refused and never offered for restore", async ({ page }) => {
  await mockStoreService(page, { prepareError: "backup_product_mismatch" });
  await page.goto("/app/settings/data-safety");

  await page.getByRole("button", { name: "Restore from a backup…" }).click();
  await page.getByRole("button", { name: "Choose backup file…" }).click();
  await chooseBackup(page, "holiday-photos.aushbackup");

  await expect(page.getByText(/not an AUSHADHARTH backup/i)).toBeVisible();
  await expect(page.getByRole("button", { name: /Replace all data with this backup/i })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Choose backup file…" })).toBeVisible();
});

test("a refused restore says the existing data was kept", async ({ page }) => {
  await mockStoreService(page, { commitError: "restore_failed" });
  await page.goto("/app/settings/data-safety");

  await page.getByRole("button", { name: "Restore from a backup…" }).click();
  await page.getByRole("button", { name: "Choose backup file…" }).click();
  await chooseBackup(page);
  await page.getByLabel("Confirm your password").fill("Owner-Password-1");
  await page.getByRole("button", { name: /Replace all data with this backup/i }).click();

  await expect(page.getByText(/existing data has been kept/i)).toBeVisible();
  await expect(page.getByText(/Restore complete/i)).toHaveCount(0);
  // The backend's own wording never reaches the counter.
  await expect(page.getByText("raw backend detail")).toHaveCount(0);
});

test("a wrong password refuses the restore without replacing anything", async ({ page }) => {
  const state = await mockStoreService(page, { commitError: "invalid_password" });
  await page.goto("/app/settings/data-safety");

  await page.getByRole("button", { name: "Restore from a backup…" }).click();
  await page.getByRole("button", { name: "Choose backup file…" }).click();
  await chooseBackup(page);
  await page.getByLabel("Confirm your password").fill("Not-The-Password-9");
  await page.getByRole("button", { name: /Replace all data with this backup/i }).click();

  await expect(page.getByText(/That password is not correct/i)).toBeVisible();
  expect(state.committed).toEqual([]);
});

test("a role that is not the owner cannot reach backup or restore at all", async ({ page }) => {
  await mockStoreService(page, { role: "pharmacist" });
  await page.goto("/app/settings/data-safety");

  await expect(page.getByText("Owner access required")).toBeVisible();
  await expect(page.getByRole("button", { name: "Create Backup Now" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Restore from a backup…" })).toHaveCount(0);
});

test("a restored service asks to be restarted rather than reporting a failure", async ({ page }) => {
  await mockStoreService(page, { restoring: true });
  await page.goto("/app/dashboard");

  await expect(page.getByRole("heading", { name: /Restart Required/i })).toBeVisible();
  await expect(page.getByText(/start the Local Store Service again/i)).toBeVisible();
  await expect(page.getByText(/Local Store Service Unavailable/i)).toHaveCount(0);
});

test("data safety stays usable on a narrow viewport", async ({ page }) => {
  await mockStoreService(page, {
    lastBackupAtUtc: "2026-09-14T06:00:00.000Z",
    backups: [
      {
        backupId: IDs.backup,
        filename: "AUSHADHARTH-care-pharmacy-20260914-060000-7000aaaa.aushbackup",
        createdAtUtc: "2026-09-14T06:00:00.000Z",
        bytes: 2_097_152,
        backupKind: "manual"
      }
    ]
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/app/settings/data-safety");

  await expect(page.getByRole("heading", { name: "Data Safety" })).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);

  await page.getByRole("button", { name: "Restore from a backup…" }).click();
  await page.getByRole("button", { name: "Choose backup file…" }).click();
  await chooseBackup(page);
  await expect(page.getByText(/This replaces everything/i)).toBeVisible();
  const afterOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(afterOverflow).toBeLessThanOrEqual(1);

  // The one destructive control must still be reachable and fully on screen at this width.
  const commit = page.getByRole("button", { name: /Replace all data with this backup/i });
  await expect(commit).toBeVisible();
  const box = await commit.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(390);
});

test("a first run offers restore as an alternative to creating a workspace", async ({ page }) => {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") {
      return route.fulfill({ json: { setupRequired: true, authenticated: false, user: null, storeDisplayName: null } });
    }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [] } });
  });
  await page.goto("/setup");

  await expect(page.getByRole("heading", { name: "Create your workspace" })).toBeVisible();
  await page.getByRole("button", { name: "Restore from a backup instead" }).click();

  await expect(page.getByRole("heading", { name: "Restore from a backup" })).toBeVisible();
  await expect(page.getByText(/no data yet, so nothing can be lost/i)).toBeVisible();
  // No account exists yet, so there is no password to confirm.
  await expect(page.getByLabel("Confirm your password")).toHaveCount(0);

  await page.getByRole("button", { name: "Create a new workspace instead" }).click();
  await expect(page.getByRole("heading", { name: "Create your workspace" })).toBeVisible();
});
