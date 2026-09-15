import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StockOperationErrorCodeSchema } from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * The Store Service owns the error vocabulary, and nothing structural forces the web app to keep up
 * with it — that is how Phase 1E's `party_conflict` and Phase 1G-0's `store_tax_conflict` both
 * reached pharmacy staff as "AUSHADHARTH could not complete that request".
 *
 * This test therefore derives the vocabulary from the Rust source rather than restating it, so a
 * code added to `StockOperationError` without a message here — or without a place in the contract enum —
 * fails immediately.
 */
const STOCK_OPS_RS = locateRepositoryFile("apps/store-service/src/api/stock_operations.rs");

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
  const source = readSource(STOCK_OPS_RS);
  const start = source.indexOf("impl IntoResponse for StockOperationError {");
  expect(start, `no StockOperationError response block in ${STOCK_OPS_RS}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, "unterminated StockOperationError response block").toBeGreaterThan(start);
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

describe("stock operation error mapping", () => {
  it("extracts the Store Service return error vocabulary", () => {
    // Anti-vacuity: a regex that silently stopped matching would otherwise pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(12);
    expect(CODES).toContain("stock_operation_not_found");
    expect(CODES).toContain("stock_operation_line_conflict");
    expect(CODES).toContain("duplicate_count_line");
    expect(CODES).toContain("batch_not_expired");
    expect(CODES).toContain("internal_error");
  });

  /**
   * The contract enum is the other half of the same vocabulary. A code the service can emit but the
   * enum does not list would fail a Zod parse somewhere, turning a precise refusal into a crash.
   */
  it("keeps the published contract enum in step with the service", () => {
    const published = new Set(StockOperationErrorCodeSchema.options as readonly string[]);
    const missing = CODES.filter((code) => !published.has(code));
    expect(missing, "StockOperationErrorCodeSchema is missing codes the Store Service can return").toEqual([]);
    // The auth codes come from the shared auth layer rather than from this block, so the enum is
    // legitimately wider than the extraction; it must not be wider by anything else.
    const unexplained = [...published].filter(
      (code) => !CODES.includes(code) && !["authentication_required", "session_expired", "authorization_denied"].includes(code)
    );
    expect(unexplained, "StockOperationErrorCodeSchema lists codes the Store Service never returns").toEqual([]);
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));

    const error = await localServiceRequest("/api/v1/stock-operations").catch((thrown) => thrown);

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
   * Written off and removed are different facts, and the messages must not blur them.
   *
   * `non_sellable` stock is still in the building. A refusal that told an operator the goods had
   * been destroyed, or that a write-off was a disposal, would produce a stock figure nobody could
   * reconcile against what is physically on the shelf.
   */
  it("never calls a write-off a disposal", () => {
    const wrong = CODES.map((code) => safeErrorMessage(code).toLowerCase()).filter(
      (message) => message.includes("destroyed") || message.includes("disposed")
    );
    expect(wrong).toEqual([]);
  });

  /**
   * A physical count must never ask the browser for a delta. If a request shape ever grows one,
   * the server would be trusting a number it should be computing.
   */
  it("never accepts a delta from the browser", () => {
    const source = readSource(STOCK_OPS_RS);
    const requestBlock = source.slice(source.indexOf("struct LineRequest {"));
    const fields = requestBlock.slice(0, requestBlock.indexOf("}"));
    expect(fields).not.toContain("delta");
    expect(fields).toContain("counted_quantity");
  });
});
