import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StoreProfileErrorCodeSchema } from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest, safeErrorMessage } from "../platform/localService";

/**
 * The Store Profile and invoice error vocabulary, derived from the Rust source rather than restated.
 *
 * This matters more here than elsewhere. `store_legal_profile_incomplete` is the refusal a cashier
 * meets mid-sale with a customer waiting; if it arrived as "AUSHADHARTH could not complete that
 * request" the counter would have no idea what to do, and the pharmacy would conclude the software
 * is broken rather than that its own licence has not been recorded.
 */
const STORE_RS = locateRepositoryFile("apps/store-service/src/api/store_profile.rs");
const INVOICES_RS = locateRepositoryFile("apps/store-service/src/api/invoices.rs");
const SALES_RS = locateRepositoryFile("apps/store-service/src/api/sales.rs");

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

function blockCodes(path: string, marker: string): string[] {
  const source = readSource(path);
  const start = source.indexOf(marker);
  expect(start, `no ${marker} in ${path}`).toBeGreaterThan(-1);
  const end = source.indexOf("\n}\n", start);
  expect(end, `unterminated ${marker}`).toBeGreaterThan(start);
  const block = source.slice(start, end);
  const codes = new Set<string>();
  for (const [, code] of block.matchAll(/simple\(\s*\n?\s*"([a-z_]+)"/g)) codes.add(code);
  for (const [, code] of block.matchAll(/\bcode:\s*"([a-z_]+)"/g)) codes.add(code);
  return [...codes];
}

const CODES = [
  ...blockCodes(STORE_RS, "impl IntoResponse for StoreProfileError {"),
  ...blockCodes(INVOICES_RS, "impl IntoResponse for InvoiceError {"),
  // Only the one Phase 1L-A adds to the sales vocabulary; the rest is already covered elsewhere.
  ...["store_legal_profile_incomplete"]
].filter((code, index, all) => all.indexOf(code) === index).sort();

const FALLBACK = safeErrorMessage("a-code-the-mapper-has-never-heard-of");

function failure(code: string) {
  return {
    ok: false,
    status: 409,
    json: async () => ({ code, message: "raw backend detail", issues: [] })
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("store profile and invoice error mapping", () => {
  it("extracts the Store Service vocabulary for this phase", () => {
    // Anti-vacuity: a regex that silently stopped matching would pass every case below.
    expect(CODES.length).toBeGreaterThanOrEqual(10);
    for (const expected of [
      "store_licence_conflict",
      "store_licence_archived",
      "store_legal_profile_incomplete",
      "invoice_not_found",
      "invoice_not_posted",
      "invoice_invariant_failed",
      "revision_conflict"
    ]) {
      expect(CODES).toContain(expected);
    }
  });

  it("keeps the published contract enum in step with the service", () => {
    const published = new Set(StoreProfileErrorCodeSchema.options as readonly string[]);
    const missing = CODES.filter((code) => !published.has(code));
    expect(missing, "StoreProfileErrorCodeSchema is missing codes the service can return").toEqual([]);
    const unexplained = [...published].filter(
      (code) =>
        !CODES.includes(code) &&
        !["authentication_required", "session_expired", "authorization_denied"].includes(code)
    );
    expect(unexplained, "StoreProfileErrorCodeSchema lists codes the service never returns").toEqual([]);
  });

  it.each(CODES)("presents a safe message for %s", async (code) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(failure(code)));
    const error = await localServiceRequest("/api/v1/store/profile").catch((thrown) => thrown);

    expect(error).toBeInstanceOf(LocalServiceError);
    expect((error as LocalServiceError).message).not.toContain("raw backend detail");
    if (code === "internal_error") {
      expect((error as LocalServiceError).message).toBe(FALLBACK);
    } else {
      expect(
        (error as LocalServiceError).message,
        `${code} has no message, so an operator would see the generic fallback`
      ).not.toBe(FALLBACK);
    }
  });

  /**
   * The refusal a cashier meets must send them somewhere. "Store Profile" is the name of the screen
   * on the sidebar, so naming it is the difference between an instruction and a complaint.
   */
  it("sends the operator to the screen that fixes a blocked sale", () => {
    expect(safeErrorMessage("store_legal_profile_incomplete")).toContain("Store Profile");
  });

  /**
   * Phase 1L-B prints, but only from the document surface. Store Profile is a settings screen and
   * the shared stylesheet is not a print target: print rules live beside the document they lay out,
   * so a change to either of these cannot quietly alter what reaches paper.
   */
  it("keeps printing out of the Store Profile screen and the shared stylesheet", () => {
    for (const relative of [
      "apps/web/src/settings/StoreProfile.tsx",
      "apps/web/src/settings/storeProfileApi.ts",
      "apps/web/src/app/styles.css"
    ]) {
      const source = readFileSync(locateRepositoryFile(relative), "utf8");
      for (const forbidden of ["@media print", "@page", "window.print"]) {
        expect(source.includes(forbidden), `${relative} contains ${forbidden}`).toBe(false);
      }
    }
  });

  /**
   * Printing is the browser's, start to finish: no document generator, no code that renders a
   * barcode or a QR, and no dependency that would put either in the bundle.
   */
  it("adds no PDF, QR or driver-level printing anywhere in the web application", () => {
    const manifest = readFileSync(locateRepositoryFile("apps/web/package.json"), "utf8");
    for (const forbidden of ["jspdf", "pdfmake", "html2canvas", "qrcode", "react-to-print", "escpos", "printer"]) {
      expect(manifest.includes(forbidden), `apps/web/package.json depends on ${forbidden}`).toBe(false);
    }
    for (const relative of [
      "apps/web/src/print/SalePrintPage.tsx",
      "apps/web/src/print/saleDocument.ts",
      "apps/web/src/print/print.css"
    ]) {
      const source = readFileSync(locateRepositoryFile(relative), "utf8");
      for (const forbidden of ["jspdf", "pdfmake", "html2canvas", "qrcode", "toDataURL", "escpos", "navigator.usb"]) {
        expect(source.includes(forbidden), `${relative} contains ${forbidden}`).toBe(false);
      }
    }
  });
});
