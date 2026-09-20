import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Invoice, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

/**
 * Phase 1L-B — the print screen, against a Store Service double.
 *
 * What is proved here is what a browser does with the document: that printing happens only when an
 * operator asks, that the page reads and never writes, that the copy an operator chose is the copy
 * on the page, and that a refusal from the model reaches the screen instead of a half-built
 * statutory document. The document's own decisions are proved in SaleDocumentModel.test.ts.
 */

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000002",
  user: "01997a00-0000-7000-8000-000000000080"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function invoice(overrides: Partial<Invoice> = {}): Invoice {
  return {
    saleId: IDs.sale,
    document: {
      documentNumber: "INV/2627/000001",
      businessDate: "2026-09-18",
      financialYear: "2026-27",
      seriesCode: "INV",
      postedAtUtc: "2026-09-18T06:30:00.000Z",
      documentType: "tax_invoice"
    },
    sellerSnapshot: {
      legalName: "Care Pharmacy Private Limited",
      tradeName: "Care Pharmacy",
      addressLine1: "12 Mall Road",
      addressLine2: null,
      city: "Ludhiana",
      postalCode: "141001",
      stateName: "Punjab",
      stateCode: "03",
      phone: null,
      email: null,
      licenceText: "Form 20: PB-20-1234",
      retailMemoLicenceText: "Form 20: PB-20-1234",
      gstRegistrationStatus: "registered",
      gstin: "03AAPFU0939F1Z5"
    },
    recipient: {
      walkIn: true, name: null, gstRegistrationStatus: null, gstin: null, stateCode: null,
      snapshotVersion: 1, particularsRequested: false, address: null, delivery: null
    },
    lines: [{
      lineNumber: 1, description: "Cotton Roll 100g", packLabel: "Strip of 10", batchNumber: "COT-1",
      expiresOn: "2028-03-31", hsnCode: "30059040", quantityText: "1 Strip of 10", quantityBasis: "pack",
      quantityPacks: 1, quantityAtoms: 10, quantityScale: 0, unitLabel: "Tablet", mrpPaise: null,
      sellingRatePaise: 10_000, taxTreatmentKind: "taxable", cgstBasisPoints: 600, sgstBasisPoints: 600,
      igstBasisPoints: 1_200, cessBasisPoints: 0, taxableValuePaise: 10_000, cgstPaise: 600,
      sgstPaise: 600, igstPaise: 0, cessPaise: 0, lineTotalPaise: 11_200
    }],
    taxSummary: [{
      taxTreatmentKind: "taxable", cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200,
      cessBasisPoints: 0, taxableValuePaise: 10_000, cgstPaise: 600, sgstPaise: 600, igstPaise: 0, cessPaise: 0
    }],
    totals: { taxableValuePaise: 10_000, cgstPaise: 600, sgstPaise: 600, igstPaise: 0, cessPaise: 0, grandTotalPaise: 11_200 },
    tender: [{ method: "cash", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
    regulatory: {
      legacyDocument: false, sellerSnapshotVersion: 1, sellerRegistered: true, taxTreatment: "intra_state",
      einvoiceApplicable: false, reverseCharge: false, complianceSnapshotVersion: 1,
      rule46sDeclaration: "not_applicable", einvoiceApplicability: null, hsnTurnoverBand: "up_to_5_crore",
      hsnTurnoverFinancialYear: "2026-27", hsnRequiredDigits: 0, dynamicQrSnapshotVersion: 1,
      dynamicQrApplicability: "not_required"
    },
    ...overrides
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null }, status);
}

type Options = { role?: UserRole; document?: Invoice; invoiceError?: { code: string; status: number } };

function printService(options: Options = {}) {
  const role = options.role ?? "cashier";
  const requests: Array<{ method: string; path: string }> = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    requests.push({ method, path: url.pathname });
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === `/api/v1/sales/${IDs.sale}/invoice`) {
      if (options.invoiceError) return failure(options.invoiceError.code, options.invoiceError.status);
      return response(options.document ?? invoice());
    }
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { requests, fetchMock };
}

function renderPrint(service = printService(), path = `/app/sales/${IDs.sale}/print`) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>);
  return service;
}

const sheet = () => screen.getByTestId("print-sheet");

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Printing a posted sale", () => {
  it("renders the posted document from the canonical invoice", async () => {
    renderPrint();
    expect(await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeInTheDocument();
    const document = within(sheet());
    expect(document.getByText("INV/2627/000001")).toBeInTheDocument();
    expect(document.getByText("2026-09-18")).toBeInTheDocument();
    expect(document.getByText("Care Pharmacy")).toBeInTheDocument();
    expect(document.getByText("GSTIN: 03AAPFU0939F1Z5")).toBeInTheDocument();
    expect(document.getByText("30059040")).toBeInTheDocument();
    expect(document.getByText("1 Strip of 10")).toBeInTheDocument();
    // The amount appears as the line, the total and the payment: the total is the one pinned here.
    expect(sheet().querySelector(".doc__grand")?.textContent).toContain("112.00");
    expect(document.getByText("Authorised Signatory")).toBeInTheDocument();
  });

  /** Nothing prints until a person asks for it. */
  it("never prints on its own", async () => {
    const print = vi.fn();
    vi.stubGlobal("print", print);
    renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(print).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Print" }));
    expect(print).toHaveBeenCalledTimes(1);
    // Asked twice, printed twice: no silent repeat, no refusal either.
    fireEvent.click(screen.getByRole("button", { name: "Print" }));
    expect(print).toHaveBeenCalledTimes(2);
  });

  it("reads the sale and writes nothing", async () => {
    const print = vi.fn();
    vi.stubGlobal("print", print);
    const service = renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Print" }));
    await waitFor(() => expect(print).toHaveBeenCalled());
    expect(service.requests.every((request) => request.method === "GET")).toBe(true);
    expect(service.requests.some((request) => request.path.endsWith("/post"))).toBe(false);
  });

  /** A statutory document is built from the posting, never topped up from today's masters. */
  it("asks for no Store Profile, Party or product data", async () => {
    const service = renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    for (const path of ["/api/v1/store/profile", "/api/v1/parties", "/api/v1/products"]) {
      expect(service.requests.some((request) => request.path.startsWith(path))).toBe(false);
    }
  });

  it("prints the copy the operator chose", async () => {
    renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(within(sheet()).getByText("ORIGINAL FOR RECIPIENT")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Copy"), { target: { value: "duplicate" } });
    expect(within(sheet()).getByText("DUPLICATE FOR TRANSPORTER")).toBeInTheDocument();
    expect(within(sheet()).queryByText("ORIGINAL FOR RECIPIENT")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Copy"), { target: { value: "triplicate" } });
    expect(within(sheet()).getByText("TRIPLICATE FOR SUPPLIER")).toBeInTheDocument();
  });

  it("keeps every fact when the paper changes", async () => {
    renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(sheet().querySelector(".doc__grand")?.textContent).toContain("112.00");

    fireEvent.change(screen.getByLabelText("Paper"), { target: { value: "thermal80" } });
    expect(sheet().className).toContain("print-sheet--thermal");
    const document = within(sheet());
    expect(document.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeInTheDocument();
    expect(document.getByText("INV/2627/000001")).toBeInTheDocument();
    expect(sheet().querySelector(".doc__grand")?.textContent).toContain("112.00");
    expect(document.getByText("ORIGINAL FOR RECIPIENT")).toBeInTheDocument();
    // The roll drops the HSN column and carries the code under the item instead.
    expect(document.getByText("HSN 30059040")).toBeInTheDocument();
  });

  /**
   * The one document that carries both kinds of supply must keep them apart: an exempted line is
   * not a line taxed at zero, and its value is not a taxable value.
   */
  it("keeps taxable and exempted supplies distinct on an invoice-cum-bill of supply", async () => {
    const taxable = invoice().lines[0];
    const exemptLine = {
      ...taxable, lineNumber: 2, description: "Cotton Bandage 10 cm", taxTreatmentKind: "exempt",
      cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, taxableValuePaise: 2_700,
      cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 2_700
    };
    const summary = invoice().taxSummary[0];
    renderPrint(printService({
      document: invoice({
        document: { ...invoice().document, documentType: "invoice_cum_bill_of_supply" },
        lines: [taxable, exemptLine],
        taxSummary: [summary, { ...summary, taxTreatmentKind: "exempt", cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, taxableValuePaise: 2_700, cgstPaise: 0, sgstPaise: 0 }],
        totals: { taxableValuePaise: 12_700, cgstPaise: 600, sgstPaise: 600, igstPaise: 0, cessPaise: 0, grandTotalPaise: 13_900 },
        tender: [{ method: "cash", amountPaise: 13_900, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }]
      })
    }));
    await screen.findByRole("heading", { name: "INVOICE-CUM-BILL OF SUPPLY", level: 1 });
    const document = within(sheet());
    // The exempted line says what it is rather than showing a tax of zero.
    expect(document.getByText("Exempt")).toBeInTheDocument();
    const totals = sheet().querySelector(".doc__totals")?.textContent ?? "";
    expect(totals).toContain("Taxable value");
    expect(totals).toContain("Exempt value");
    expect(totals).toContain("27.00");
    expect(sheet().querySelector(".doc__grand")?.textContent).toContain("139.00");
  });

  it("shows a bill of supply without tax columns or invoice copy labels", async () => {
    renderPrint(printService({
      document: invoice({
        document: { ...invoice().document, documentType: "bill_of_supply" },
        lines: [{ ...invoice().lines[0], taxTreatmentKind: "exempt", cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 1_200, cgstPaise: 0, sgstPaise: 0, lineTotalPaise: 10_000 }],
        totals: { taxableValuePaise: 10_000, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 10_000 },
        tender: [{ method: "cash", amountPaise: 10_000, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
        // The seller's own Rule 46(s) position says "applicable", and it still does not belong on
        // this document: clause (s) attaches where an invoice is issued, and this is issued instead.
        regulatory: { ...invoice().regulatory, rule46sDeclaration: "applicable", dynamicQrApplicability: null }
      })
    }));
    expect(await screen.findByRole("heading", { name: "BILL OF SUPPLY", level: 1 })).toBeInTheDocument();
    const document = within(sheet());
    expect(document.queryByRole("columnheader", { name: "GST" })).not.toBeInTheDocument();
    expect(document.queryByText(/TRANSPORTER/)).not.toBeInTheDocument();
    expect(document.getByText("CUSTOMER COPY")).toBeInTheDocument();
    expect(document.queryByText(/I\/We hereby declare/)).not.toBeInTheDocument();
    expect(document.getByText("Authorised Signatory")).toBeInTheDocument();
  });

  it("shows an unregistered pharmacy's memo with its licence and no GST", async () => {
    renderPrint(printService({
      document: invoice({
        document: { ...invoice().document, documentType: "retail_cash_memo" },
        sellerSnapshot: { ...invoice().sellerSnapshot!, gstin: null, gstRegistrationStatus: "unregistered" },
        lines: [{ ...invoice().lines[0], cgstPaise: 0, sgstPaise: 0, lineTotalPaise: 10_000 }],
        totals: { taxableValuePaise: 10_000, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 10_000 },
        tender: [{ method: "cash", amountPaise: 10_000, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
        regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
      })
    }));
    expect(await screen.findByRole("heading", { name: "RETAIL CASH MEMO", level: 1 })).toBeInTheDocument();
    const document = within(sheet());
    expect(document.getByText("Not GST-registered")).toBeInTheDocument();
    expect(document.getByText("Drug sale licence: Form 20: PB-20-1234")).toBeInTheDocument();
    expect(document.getByText("Not charged")).toBeInTheDocument();
    expect(document.queryByText("Authorised Signatory")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Copy")).toHaveValue("customer");
    expect(document.getByText("CUSTOMER COPY")).toBeInTheDocument();
  });

  it("prints the Rule 46(s) declaration only when the posting says so", async () => {
    renderPrint(printService({
      document: invoice({ regulatory: { ...invoice().regulatory, rule46sDeclaration: "applicable" } })
    }));
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(within(sheet()).getByText(/I\/We hereby declare/)).toBeInTheDocument();

    cleanup();
    renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(within(sheet()).queryByText(/I\/We hereby declare/)).not.toBeInTheDocument();
  });

  it("prints the payment cross-reference the posting froze", async () => {
    renderPrint(printService({
      document: invoice({
        tender: [{ method: "upi", amountPaise: 11_200, referenceText: "UPI-4471-99210", recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
        regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
      })
    }));
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    const document = within(sheet());
    expect(document.getByText(/UPI · 112.00 · Ref UPI-4471-99210/)).toBeInTheDocument();
    expect(document.getByText(/Recorded 2026-09-18T06:30:00.100Z/)).toBeInTheDocument();
    // No QR is drawn, and nothing claims one was.
    expect(sheet().querySelector("canvas, svg, img")).toBeNull();
    expect(document.queryByText(/QR/)).not.toBeInTheDocument();
  });

  it("refuses to print a document whose payment reference is missing", async () => {
    renderPrint(printService({
      document: invoice({
        tender: [{ method: "upi", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
        regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
      })
    }));
    expect(await screen.findByText("This invoice cannot be printed")).toBeInTheDocument();
    expect(screen.queryByTestId("print-sheet")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Print" })).not.toBeInTheDocument();
  });

  it("refuses to print a sale with no settled statutory class", async () => {
    renderPrint(printService({
      document: invoice({
        document: { ...invoice().document, documentType: "document_classification_unresolved", documentTypeReason: "A mixed taxable and exempt supply to a registered recipient is not covered by Rule 46A." }
      })
    }));
    expect(await screen.findByText("This sale has no statutory document")).toBeInTheDocument();
    expect(screen.queryByTestId("print-sheet")).not.toBeInTheDocument();
  });

  it("marks a legacy sale as a record and offers no statutory copy", async () => {
    renderPrint(printService({
      document: invoice({
        sellerSnapshot: null,
        recipient: { ...invoice().recipient, snapshotVersion: 0, particularsRequested: null },
        regulatory: { ...invoice().regulatory, legacyDocument: true, sellerSnapshotVersion: 0, complianceSnapshotVersion: 0, rule46sDeclaration: null, dynamicQrSnapshotVersion: 0, dynamicQrApplicability: null }
      })
    }));
    expect(await screen.findByRole("heading", { name: "LEGACY TRANSACTION RECORD", level: 1 })).toBeInTheDocument();
    const document = within(sheet());
    expect(document.getByText("LEGACY TRANSACTION RECORD — NOT ORIGINAL INVOICE REPRODUCTION")).toBeInTheDocument();
    expect(document.getByText(/The pharmacy's own particulars were not recorded/)).toBeInTheDocument();
    expect(document.queryByText(/ORIGINAL FOR RECIPIENT|TRANSPORTER|TRIPLICATE/)).not.toBeInTheDocument();
    expect(document.queryByText("Authorised Signatory")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Copy")).not.toBeInTheDocument();
    expect(screen.getByText("This sale predates the statutory snapshots")).toBeInTheDocument();
  });

  it("says plainly that a draft has nothing to print", async () => {
    renderPrint(printService({ invoiceError: { code: "invoice_not_posted", status: 409 } }));
    expect(await screen.findByText("This sale has not been posted, so it has no invoice yet.")).toBeInTheDocument();
    expect(screen.getByText(/Nothing has been printed/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Print" })).not.toBeInTheDocument();
  });

  const failures: Array<[string, { code: string; status: number }, string]> = [
    ["a sale that no longer exists", { code: "invoice_not_found", status: 404 }, "That sale no longer exists."],
    ["a role that may not read it", { code: "authorization_denied", status: 403 }, "Your role does not permit this operation."],
    ["a document that does not add up", { code: "invoice_invariant_failed", status: 409 }, "This invoice does not add up and cannot be shown. Report it before using it."],
    ["a service that failed", { code: "internal_error", status: 500 }, "AUSHADHARTH could not complete that request. Try again."]
  ];

  it.each(failures)("refuses to print %s", async (_case, error, message) => {
    renderPrint(printService({ invoiceError: error }));
    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(screen.queryByTestId("print-sheet")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Print" })).not.toBeInTheDocument();
    expect(screen.queryByText("raw backend detail")).not.toBeInTheDocument();
  });

  it("reprints the same document without renumbering or relabelling it", async () => {
    const print = vi.fn();
    vi.stubGlobal("print", print);
    const service = renderPrint();
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Print" }));

    const document = within(sheet());
    expect(document.getByText("INV/2627/000001")).toBeInTheDocument();
    expect(document.getByText("2026-09-18")).toBeInTheDocument();
    // A reprint is annotated as one, and never as the statutory transporter copy.
    expect(document.getByText(/Reprinted /)).toBeInTheDocument();
    expect(document.getByText("ORIGINAL FOR RECIPIENT")).toBeInTheDocument();
    expect(document.queryByText("DUPLICATE FOR TRANSPORTER")).not.toBeInTheDocument();
    expect(service.requests.every((request) => request.method === "GET")).toBe(true);
  });

  it("offers printing from the posted sale itself", async () => {
    renderPrint(printService(), `/app/sales/${IDs.sale}/print`);
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    expect(screen.getByRole("link", { name: "Back to Sale" })).toHaveAttribute("href", `/app/sales/${IDs.sale}`);
  });

  it("carries a long transaction reference and a long address without losing them", async () => {
    const reference = "UPI-9988776655443322110099887766554433221100";
    renderPrint(printService({
      document: invoice({
        sellerSnapshot: { ...invoice().sellerSnapshot!, addressLine1: "Shop 14, Guru Nanak Market, Opposite Civil Hospital Gate Number Three", addressLine2: "Near Clock Tower Circle" },
        tender: [{ method: "card", amountPaise: 11_200, referenceText: reference, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
        regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
      })
    }));
    await screen.findByRole("heading", { name: "TAX INVOICE", level: 1 });
    fireEvent.change(screen.getByLabelText("Paper"), { target: { value: "thermal80" } });
    const document = within(sheet());
    expect(document.getByText(new RegExp(reference))).toBeInTheDocument();
    expect(document.getByText(/Guru Nanak Market/)).toBeInTheDocument();
  });
});
