import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BackupErrorCodeSchema, MAXIMUM_BACKUP_BYTES } from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * Backup and restore is where a silent gap in the error vocabulary would cost the most.
 *
 * Everywhere else a generic "AUSHADHARTH could not complete that request" is merely unhelpful. Here
 * it is the difference between an owner understanding that their existing data is untouched and an
 * owner believing they have just destroyed their pharmacy. The vocabulary is therefore derived from
 * the Rust source rather than restated, so a code added to `BackupError` without a message here —
 * or without a place in the contract enum — fails immediately.
 */
const BACKUPS_RS = locateRepositoryFile("apps/store-service/src/api/backups.rs");

/** Walks up to the repository root so the test does not depend on the runner's working directory. */
function locateRepositoryFile(relativePath: string): string {
  let directory = resolve(process.cwd());
  for (;;) {
    const candidate = join(directory, relativePath);
    if (existsSync(candidate)) return candidate;
    const parent = dirname(directory);
    if (parent === directory) throw new Error(`${relativePath} was not found above ${process.cwd()}`);
    directory = parent;
  }
}

/** Rust source with newlines normalised, so a CRLF checkout asserts the same thing as an LF one. */
function readSource(path: string): string {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n");
}

const FALLBACK = safeErrorMessage("a-code-the-mapper-has-never-heard-of");

function responseCodes(): string[] {
  const source = readSource(BACKUPS_RS);
  const start = source.indexOf("impl IntoResponse for BackupError {");
  expect(start, `no BackupError response block in ${BACKUPS_RS}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, "unterminated BackupError response block").toBeGreaterThan(start);
  const block = source.slice(start, end);

  const codes = new Set<string>();
  for (const [, code] of block.matchAll(/simple\(\s*\n?\s*"([a-z_]+)"/g)) codes.add(code);
  for (const [, code] of block.matchAll(/\bcode:\s*"([a-z_]+)"/g)) codes.add(code);
  return [...codes].sort();
}

const CODES = responseCodes();

function failure(code: string) {
  return {
    ok: false,
    status: 409,
    json: async () => ({ code, message: "raw backend detail", issues: [] })
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("backup and restore error mapping", () => {
  it("extracts the Store Service backup error vocabulary", () => {
    // Anti-vacuity: a regex that silently stopped matching would otherwise pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(16);
    expect(CODES).toContain("backup_corrupt");
    expect(CODES).toContain("backup_product_mismatch");
    expect(CODES).toContain("backup_checksum_mismatch");
    expect(CODES).toContain("candidate_expired");
    expect(CODES).toContain("service_restoring");
    expect(CODES).toContain("restore_failed");
    expect(CODES).toContain("internal_error");
  });

  it("keeps the published contract enum in step with the service", () => {
    const published = new Set(BackupErrorCodeSchema.options as readonly string[]);
    const missing = CODES.filter((code) => !published.has(code));
    expect(missing, "BackupErrorCodeSchema is missing codes the Store Service can return").toEqual([]);
    // The auth codes come from the shared auth layer rather than from this block, so the enum is
    // legitimately wider than the extraction; it must not be wider by anything else.
    const unexplained = [...published].filter(
      (code) => !CODES.includes(code) && !["authentication_required", "session_expired", "authorization_denied"].includes(code)
    );
    expect(unexplained, "BackupErrorCodeSchema lists codes the Store Service never returns").toEqual([]);
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));

    const error = await localServiceRequest("/api/v1/backups/status").catch((thrown) => thrown);

    expect(error).toBeInstanceOf(LocalServiceError);
    expect((error as LocalServiceError).code).toBe(code);
    // The backend's own wording never reaches the counter.
    expect((error as LocalServiceError).message).not.toContain("raw backend detail");
    if (code === "internal_error") {
      expect((error as LocalServiceError).message).toBe(FALLBACK);
    } else {
      expect(
        (error as LocalServiceError).message,
        `${code} has no message in safeErrorMessage, so an owner would see the generic fallback`
      ).not.toBe(FALLBACK);
    }
  });

  /**
   * Every refusal that happens before the swap must say the existing data is safe, and no refusal
   * may imply data was lost when it was not. An owner reading "the restore could not be completed"
   * with nothing after it will assume the worst.
   */
  it("tells an owner what is still true of their existing data when a restore is refused", () => {
    expect(safeErrorMessage("restore_failed").toLowerCase()).toContain("kept");
    for (const code of CODES) {
      const message = safeErrorMessage(code).toLowerCase();
      expect(message, `${code} implies data was destroyed`).not.toContain("lost");
      expect(message, `${code} implies data was destroyed`).not.toContain("deleted");
    }
  });

  /**
   * The service refuses form encodings precisely because a cross-site form can send them. If the web
   * app ever uploaded a backup as multipart, the refusal would be correct and the feature broken.
   */
  it("uploads a backup as raw bytes rather than as a form", () => {
    const source = readFileSync(locateRepositoryFile("apps/web/src/platform/localService.ts"), "utf8");
    expect(source).toContain("application/octet-stream");
    expect(source).not.toContain("multipart/form-data");
    expect(source).not.toContain("new FormData");
  });

  /** The limit is the service's, and the web app must not invent a different one. */
  it("publishes the same upload ceiling the service enforces", () => {
    const rust = readSource(locateRepositoryFile("apps/store-service/src/domain/backup.rs"));
    expect(rust).toContain(`MAX_PAYLOAD_BYTES: u64 = ${MAXIMUM_BACKUP_BYTES.toLocaleString("en-US").replace(/,/g, "_")}`);
  });

  /**
   * The browser must never be able to say where a backup lives. Every path the service accepts is an
   * id or an opaque token; a request body carrying a filesystem path would be a way around that.
   */
  it("never sends a filesystem path to the service", () => {
    const client = readFileSync(locateRepositoryFile("apps/web/src/settings/backupApi.ts"), "utf8");
    for (const name of ["path", "directory", "folder", "destination"]) {
      expect(client.includes(`${name}:`), `backupApi sends a ${name} to the service`).toBe(false);
    }
  });
});
