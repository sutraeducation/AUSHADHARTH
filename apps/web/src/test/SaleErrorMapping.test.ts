import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SaleErrorCodeSchema } from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * The Store Service owns the error vocabulary, and nothing structural forces the web app to keep up
 * with it — that is how Phase 1E's `party_conflict` and Phase 1G-0's `store_tax_conflict` both
 * reached pharmacy staff as "AUSHADHARTH could not complete that request".
 *
 * This test therefore derives the vocabulary from the Rust source itself rather than restating it,
 * so a code added to `SaleError` without a message here — or without a place in the contract enum —
 * fails immediately. At a counter this matters more than anywhere else: a cashier who is told
 * nothing useful has a customer waiting while they work out what went wrong.
 */
const SALES_RS = locateRepositoryFile("apps/store-service/src/api/sales.rs");

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
  const source = readSource(SALES_RS);
  const start = source.indexOf("impl IntoResponse for SaleError {");
  expect(start, `no SaleError response block in ${SALES_RS}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, "unterminated SaleError response block").toBeGreaterThan(start);
  const block = source.slice(start, end);

  const codes = new Set<string>();
  // Digits included: an H1 code such as schedule_h1_ndps_workflow_not_available is invisible to [a-z_].
  for (const [, code] of block.matchAll(/simple\(\s*"([a-z0-9_]+)"/g)) codes.add(code);
  for (const [, code] of block.matchAll(/\bcode:\s*"([a-z0-9_]+)"/g)) codes.add(code);
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

describe("sale error mapping", () => {
  it("extracts the Store Service sale error vocabulary", () => {
    // Anti-vacuity: a regex that silently stopped matching would otherwise pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(20);
    expect(CODES).toContain("sale_not_found");
    expect(CODES).toContain("selling_rate_above_mrp");
    expect(CODES).toContain("internal_error");
    expect(CODES).toContain("schedule_h1_ndps_workflow_not_available");
  });

  /**
   * The contract enum is the other half of the same vocabulary. A code the service can emit but the
   * enum does not list would fail a Zod parse somewhere, turning a precise refusal into a crash.
   */
  it("keeps the published contract enum in step with the service", () => {
    const published = new Set(SaleErrorCodeSchema.options as readonly string[]);
    const missing = CODES.filter((code) => !published.has(code));
    expect(missing, "SaleErrorCodeSchema is missing codes the Store Service can return").toEqual([]);
    // The auth codes come from the shared auth layer rather than from this block, so the enum is
    // legitimately wider than the extraction; it must not be wider by anything else.
    const unexplained = [...published].filter(
      (code) => !CODES.includes(code) && !["authentication_required", "session_expired", "authorization_denied"].includes(code)
    );
    expect(unexplained, "SaleErrorCodeSchema lists codes the Store Service never returns").toEqual([]);
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));

    const error = await localServiceRequest("/api/v1/sales").catch((thrown) => thrown);

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

  /**
   * Phase 1G worded several shared codes as if only a purchase could raise them. A sale raises the
   * same ones, so a cashier must not be told about "this purchase".
   */
  it.each(["insufficient_stock", "arithmetic_overflow", "posting_conflict", "tax_rate_not_found", "product_tax_classification_incomplete"])(
    "words the shared code %s without naming a purchase",
    (code) => {
      expect(safeErrorMessage(code).toLowerCase()).not.toContain("purchase");
    }
  );
});
