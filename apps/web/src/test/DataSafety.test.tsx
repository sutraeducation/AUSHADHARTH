import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

/**
 * Data Safety, from the operator's side.
 *
 * Restoring is the only action in the product that destroys data on purpose, so most of what is
 * asserted here is about what an owner is told before they commit to it: what the file turns out to
 * be, what will be lost, and — when something goes wrong — that their existing records are still
 * there. A screen that got any of that wrong would be worse than no screen at all.
 */

const IDs = { user: "01900000-0000-7000-8000-000000000001", backup: "01997000-0000-7000-8000-00000000000b" };
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
  compatibility: "ready" as const,
  migrationRequired: false
};

function response(body: unknown, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}
function failure(code: string, status = 409) {
  return response({ code, message: "raw backend detail", issues: [] }, status);
}

interface Options {
  role?: UserRole;
  backups?: Array<Record<string, unknown>>;
  lastBackupAtUtc?: string | null;
  createError?: string;
  prepareError?: string;
  commitError?: string;
  restoring?: boolean;
}

function storeService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    backups: options.backups ?? [],
    last: options.lastBackupAtUtc ?? null,
    committed: [] as unknown[],
    uploads: [] as Array<{ path: string; contentType: string }>
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };

    if (options.restoring && !url.pathname.endsWith("/auth/logout")) return failure("service_restoring", 503);
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) {
      return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    }
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);

    if (url.pathname === "/api/v1/backups/status") {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      return response({
        lastSuccessfulBackupAtUtc: state.last,
        reminderThresholdDays: 7,
        backupOverdue: state.last === null,
        backups: state.backups
      });
    }
    if (url.pathname === "/api/v1/backups/create" && method === "POST") {
      if (options.createError) return failure(options.createError);
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
      return response(
        { jobId: "job", stage: "ready", backupId: IDs.backup, filename: state.backups[0].filename, bytes: 1_048_576, errorCode: null },
        201
      );
    }
    if (url.pathname.endsWith("/download")) {
      return response({ backupId: IDs.backup, filename: "x.aushbackup", bytes: 10, url: "/api/v1/backup-files/x.aushbackup" });
    }
    if (url.pathname.endsWith("/restore/prepare")) {
      state.uploads.push({ path: url.pathname, contentType: String(new Headers(init?.headers).get("content-type")) });
      if (options.prepareError) return failure(options.prepareError);
      return response({ candidateToken: "candidate-token", expiresInSeconds: 1_800, report: REPORT });
    }
    if (url.pathname.endsWith("/restore/commit")) {
      if (options.commitError) return failure(options.commitError, options.commitError === "invalid_password" ? 403 : 409);
      state.committed.push(JSON.parse(String(init?.body)));
      return response({ restoreId: "restore-1", restartRequired: true, safetyBackup: "safety.aushbackup" });
    }
    return response([], 200);
  });
  vi.stubGlobal("fetch", fetchMock);
  return state;
}

function renderApp(path = "/app/settings/data-safety") {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <App />
      </MemoryRouter>
    </QueryClientProvider>
  );
}

/** Puts a file into the hidden picker the way the operating system's dialog would. */
function chooseFile(name = "care-pharmacy.aushbackup") {
  const input = document.querySelector('input[type="file"]') as HTMLInputElement;
  expect(input, "the restore panel has no file picker").not.toBeNull();
  const file = new File([new Uint8Array([1, 2, 3, 4])], name, { type: "application/octet-stream" });
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  fireEvent.change(input);
  return file;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Data Safety", () => {
  it("tells an owner who has never backed up that they are at risk", async () => {
    storeService();
    renderApp();
    expect(await screen.findByRole("heading", { name: "Data Safety" })).toBeTruthy();
    expect(await screen.findByText(/never been backed up/i)).toBeTruthy();
    expect(screen.getByText("Never")).toBeTruthy();
  });

  it("creates a backup and says where it should be kept", async () => {
    storeService();
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Create Backup Now" }));
    // A copy that never leaves the PC does not survive the PC, and the message has to say so.
    expect(await screen.findByText(/somewhere other than this PC/i)).toBeTruthy();
    // Named in the notice and listed in the table: both, because an owner who dismisses the
    // notice must still be able to find the file they were just told to copy away.
    expect(await screen.findAllByText(/AUSHADHARTH-care-pharmacy/)).toHaveLength(2);
  });

  it("reports a failed backup without claiming one was made", async () => {
    storeService({ createError: "insufficient_disk_space" });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Create Backup Now" }));
    const alert = await screen.findByText(/not enough free space/i);
    expect(alert.textContent).not.toContain("raw backend detail");
    expect(screen.queryByText(/Backup created/i)).toBeNull();
  });

  it("refuses the whole screen to anyone but the owner", async () => {
    storeService({ role: "pharmacist" });
    renderApp();
    expect(await screen.findByText("Owner access required")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Create Backup Now" })).toBeNull();
  });

  it("shows what the backup file actually is before anything is replaced", async () => {
    const state = storeService();
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup…" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose backup file…" }));
    chooseFile();

    expect(await screen.findByText("Care Pharmacy")).toBeTruthy();
    expect(screen.getByText(/the file matches its own record/i)).toBeTruthy();
    // The facts come from the file, not from its name.
    expect(screen.getByText("1.0 MB")).toBeTruthy();
    expect(await screen.findByText(/This replaces everything/i)).toBeTruthy();
    // Raw bytes, not a form: the service refuses form encodings as a CSRF defence.
    expect(state.uploads[0].contentType).toBe("application/octet-stream");
    expect(state.uploads[0].path).toBe("/api/v1/backups/restore/prepare");
  });

  it("will not commit a restore until the owner has typed their own password", async () => {
    const state = storeService();
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup…" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose backup file…" }));
    chooseFile();

    const commit = await screen.findByRole("button", { name: /Replace all data with this backup/i });
    expect((commit as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Confirm your password"), { target: { value: "Owner-Password-1" } });
    expect((commit as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(commit);

    await waitFor(() => expect(state.committed.length).toBe(1));
    expect(state.committed[0]).toEqual({ candidateToken: "candidate-token", password: "Owner-Password-1" });
    expect(await screen.findByText(/Restore complete/i)).toBeTruthy();
    expect(screen.getByText(/start the Local Store Service again/i)).toBeTruthy();
    // The safety copy is named, because an owner who has just replaced everything needs to know
    // that what was there is still recoverable.
    expect(screen.getByText("safety.aushbackup")).toBeTruthy();
  });

  it("says plainly that existing data was kept when a restore is refused", async () => {
    storeService({ commitError: "restore_failed" });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup…" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose backup file…" }));
    chooseFile();
    fireEvent.change(await screen.findByLabelText("Confirm your password"), { target: { value: "Owner-Password-1" } });
    fireEvent.click(screen.getByRole("button", { name: /Replace all data with this backup/i }));

    expect(await screen.findByText(/existing data has been kept/i)).toBeTruthy();
    expect(screen.queryByText(/Restore complete/i)).toBeNull();
  });

  it("refuses a file that is not a backup without offering to restore it", async () => {
    storeService({ prepareError: "backup_product_mismatch" });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup…" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose backup file…" }));
    chooseFile("holiday-photos.aushbackup");

    expect(await screen.findByText(/not an AUSHADHARTH backup/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Replace all data with this backup/i })).toBeNull();
    expect(screen.getByRole("button", { name: "Choose backup file…" })).toBeTruthy();
  });

  it("says a backup needs upgrading rather than doing it silently", async () => {
    const state = storeService();
    void state;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = new URL(String(input), "http://local.test");
        if (url.pathname.endsWith("/system/info")) return response(system);
        if (url.pathname.endsWith("/auth/status")) {
          return response({
            setupRequired: false,
            authenticated: true,
            user: { id: IDs.user, loginIdentifier: "owner", displayName: "Owner", role: "owner_admin", revision: 1 },
            storeDisplayName: "Care Pharmacy"
          });
        }
        if (url.pathname === "/api/v1/backups/status") {
          return response({ lastSuccessfulBackupAtUtc: null, reminderThresholdDays: 7, backupOverdue: true, backups: [] });
        }
        if (url.pathname.endsWith("/restore/prepare")) {
          return response({
            candidateToken: "candidate-token",
            expiresInSeconds: 1_800,
            report: { ...REPORT, schemaVersion: 12, migrationRequired: true }
          });
        }
        void init;
        return response([], 200);
      })
    );
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup…" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose backup file…" }));
    chooseFile();
    expect(await screen.findByText(/brought forward from version 12 to 16/i)).toBeTruthy();
  });

  it("offers a restore on first run, where there is nothing to lose", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), "http://local.test");
        if (url.pathname.endsWith("/system/info")) return response(system);
        if (url.pathname.endsWith("/auth/status")) {
          return response({ setupRequired: true, authenticated: false, user: null, storeDisplayName: null });
        }
        return response([], 200);
      })
    );
    renderApp("/setup");
    fireEvent.click(await screen.findByRole("button", { name: "Restore from a backup instead" }));
    expect(await screen.findByRole("heading", { name: "Restore from a backup" })).toBeTruthy();
    expect(screen.getByText(/no data yet, so nothing can be lost/i)).toBeTruthy();
    // No password is asked for, because no account exists yet to have one.
    expect(screen.queryByLabelText("Confirm your password")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Create a new workspace instead" }));
    expect(await screen.findByRole("heading", { name: "Create your workspace" })).toBeTruthy();
  });

  it("tells everyone to restart once a restore has replaced the database", async () => {
    storeService({ restoring: true });
    renderApp("/app/dashboard");
    expect(await screen.findByRole("heading", { name: /Restart Required/i })).toBeTruthy();
    expect(screen.getByText(/start the Local Store Service again/i)).toBeTruthy();
    // Not presented as a failure: nothing is wrong and nothing is lost.
    expect(screen.queryByText(/Local Store Service Unavailable/i)).toBeNull();
  });

  it("downloads a backup by the name and address the service chose", async () => {
    storeService({
      lastBackupAtUtc: "2026-09-15T06:00:00.000Z",
      backups: [
        {
          backupId: IDs.backup,
          filename: "AUSHADHARTH-care-pharmacy-20260915-060000-7000aaaa.aushbackup",
          createdAtUtc: "2026-09-15T06:00:00.000Z",
          bytes: 2_097_152,
          backupKind: "manual"
        }
      ]
    });
    const clicks: Array<{ href: string; download: string }> = [];
    const originalClick = HTMLAnchorElement.prototype.click;
    HTMLAnchorElement.prototype.click = function click(this: HTMLAnchorElement) {
      clicks.push({ href: this.getAttribute("href") ?? "", download: this.download });
    };
    try {
      renderApp();
      const table = await screen.findByRole("table");
      fireEvent.click(within(table).getByRole("button", { name: "Download" }));
      await waitFor(() => expect(clicks.length).toBe(1));
      expect(clicks[0].href).toBe("/api/v1/backup-files/x.aushbackup");
      expect(clicks[0].download).toBe("x.aushbackup");
    } finally {
      HTMLAnchorElement.prototype.click = originalClick;
    }
  });
});
