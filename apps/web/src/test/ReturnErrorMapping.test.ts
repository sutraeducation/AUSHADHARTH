import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ReturnErrorCodeSchema, SaleErrorCodeSchema } from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * The Store Service owns the error vocabulary, and nothing structural forces the web app to keep up
 * with it — that is how Phase 1E's `party_conflict` and Phase 1G-0's `store_tax_conflict` both
 * reached pharmacy staff as "AUSHADHARTH could not complete that request".
 *
 * This test therefore derives the vocabulary from the Rust source rather than restating it, so a
 * code added to `ReturnError` without a message here — or without a place in the contract enum —
 * fails immediately.
 */
const RETURNS_RS = locateRepositoryFile("apps/store-service/src/api/returns.rs");

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
 * `internal_error` is the one code whose correct presentation IS the generic fallback: an
 * unclassified server fault has nothing specific that can be safely said about it.
 */
const FALLBACK_IS_CORRECT = new Set(["internal_error"]);

const FALLBACK = safeErrorMessage("a-code-the-mapper-has-never-heard-of");

function responseCodes(): string[] {
  const source = readFileSync(RETURNS_RS, "utf8");
  const start = source.indexOf("impl IntoResponse for ReturnError {");
  expect(start, `no ReturnError response block in ${RETURNS_RS}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, "unterminated ReturnError response block").toBeGreaterThan(start);
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

describe("return error mapping", () => {
  it("extracts the Store Service return error vocabulary", () => {
    // Anti-vacuity: a regex that silently stopped matching would otherwise pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(18);
    expect(CODES).toContain("return_not_found");
    expect(CODES).toContain("over_return");
    expect(CODES).toContain("disposition_denied");
    expect(CODES).toContain("internal_error");
  });

  /**
   * The contract enum is the other half of the same vocabulary. A code the service can emit but the
   * enum does not list would fail a Zod parse somewhere, turning a precise refusal into a crash.
   */
  it("keeps the published contract enum in step with the service", () => {
    const published = new Set(ReturnErrorCodeSchema.options as readonly string[]);
    const missing = CODES.filter((code) => !published.has(code));
    expect(missing, "ReturnErrorCodeSchema is missing codes the Store Service can return").toEqual([]);
    // The auth codes come from the shared auth layer rather than from this block, so the enum is
    // legitimately wider than the extraction; it must not be wider by anything else.
    const unexplained = [...published].filter(
      (code) => !CODES.includes(code) && !["authentication_required", "session_expired", "authorization_denied"].includes(code)
    );
    expect(unexplained, "ReturnErrorCodeSchema lists codes the Store Service never returns").toEqual([]);
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));

    const error = await localServiceRequest("/api/v1/returns").catch((thrown) => thrown);

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
   * The vocabulary must never acquire a debit-note code.
   *
   * Under CGST s.34(3) a debit note is issued by "the registered person, who has supplied" — the
   * supplier — so a pharmacy returning goods to its supplier never issues one. A code, field or
   * label saying otherwise would be a legal claim this software is not entitled to make.
   */
  it("never names a debit note, which a recipient does not issue", () => {
    expect(CODES.filter((code) => code.includes("debit"))).toEqual([]);
    const returnCodes = ReturnErrorCodeSchema.options as readonly string[];
    const saleCodes = SaleErrorCodeSchema.options as readonly string[];
    expect(returnCodes.filter((code) => code.includes("debit"))).toEqual([]);
    expect(saleCodes.filter((code) => code.includes("debit"))).toEqual([]);
    // And the service source itself never calls our document one.
    const source = readFileSync(RETURNS_RS, "utf8").toLowerCase();
    expect(source).not.toContain("debit_note");
  });
});
