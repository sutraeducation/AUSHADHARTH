import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * Phase 1E shipped `party_conflict` and Phase 1G-0 shipped `store_tax_conflict` with no entry in
 * the web mapper, so both reached pharmacy staff as "AUSHADHARTH could not complete that request".
 * The defect is structural: the Store Service owns the code vocabulary, and nothing forced the web
 * app to keep up with it.
 *
 * This test therefore derives the vocabulary from the Rust source itself. It does not restate the
 * codes, so a code added to `PurchaseError` without a message here fails immediately.
 */
const PURCHASES_RS = locateRepositoryFile("apps/store-service/src/api/purchases.rs");

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

/**
 * Rust source with newlines normalised.
 *
 * The block below is located by searching for a closing brace on its own line. Read straight off
 * the disk that search asks what newline convention this checkout happens to use, which is a
 * question about the operating system rather than about the Store Service. A Windows working tree
 * with CRLF files would fail every assertion in this file while the repository content was
 * identical. The repository now pins LF in .gitattributes; this makes the test correct either way.
 */
function readSource(path: string): string {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n");
}

/**
 * `internal_error` is the one code whose correct presentation IS the generic fallback: an
 * unclassified server fault has nothing specific that can be safely said about it.
 */
const FALLBACK_IS_CORRECT = new Set(["internal_error"]);

const FALLBACK = safeErrorMessage("a-code-the-mapper-has-never-heard-of");

function responseCodes(): string[] {
  const source = readSource(PURCHASES_RS);
  const start = source.indexOf("impl IntoResponse for PurchaseError {");
  expect(start, `no PurchaseError response block in ${PURCHASES_RS}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, "unterminated PurchaseError response block").toBeGreaterThan(start);
  const block = source.slice(start, end);

  const codes = new Set<string>();
  for (const [, code] of block.matchAll(/simple\(\s*"([a-z_]+)"/g)) codes.add(code);
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

describe("purchase error mapping", () => {
  it("extracts the Store Service purchase error vocabulary", () => {
    // Anti-vacuity: a regex that silently stopped matching would otherwise pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(15);
    expect(CODES).toContain("purchase_not_found");
    expect(CODES).toContain("internal_error");
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));

    const error = await localServiceRequest("/api/v1/purchases").catch((thrown) => thrown);

    expect(error).toBeInstanceOf(LocalServiceError);
    expect((error as LocalServiceError).code).toBe(code);
    // The backend's own wording never reaches the counter.
    expect((error as LocalServiceError).message).not.toContain("raw backend detail");
    if (FALLBACK_IS_CORRECT.has(code)) {
      expect((error as LocalServiceError).message).toBe(FALLBACK);
    } else {
      expect(
        (error as LocalServiceError).message,
        `${code} has no message in safeErrorMessage, so staff would see the generic fallback`
      ).not.toBe(FALLBACK);
    }
  });
});
