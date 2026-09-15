import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReturnDetail, ReturnLine as ReturnLineShape, ReturnableDocument, ReturnableLine, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { paiseToAmountText, quantityToInteger } from "../returns/returnApi";

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000001",
  purchase: "01997a00-0000-7000-8000-000000000002",
  ret: "01997a00-0000-7000-8000-000000000003",
  saleLine: "01997a00-0000-7000-8000-000000000010",
  purchaseLine: "01997a00-0000-7000-8000-000000000011",
  returnLine: "01997a00-0000-7000-8000-000000000012",
  product: "01997a00-0000-7000-8000-000000000020",
  pack: "01997a00-0000-7000-8000-000000000030",
  batch: "01997a00-0000-7000-8000-000000000040",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080"
};

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function returnableLine(overrides: Partial<ReturnableLine> = {}): ReturnableLine {
  return {
    originalLineId: IDs.saleLine,
    lineNumber: 1,
    productId: IDs.product,
    productPackId: IDs.pack,
    batchId: IDs.batch,
    productDisplayName: "Crocin 500 mg Tablet",
    packDisplayLabel: "Strip of 10",
    baseUnitLabel: "Tablet",
    batchNumber: "B-2601",
    batchExpiresOn: "2028-03-31",
    quantityBasis: "pack",
    originalQuantity: 2,
    originalQuantityAtoms: 20,
    alreadyReturnedAtoms: 0,
    returnableAtoms: 20,
    returnableQuantity: 2,
    taxableValuePaise: 16_000,
    cgstPaise: 960,
    sgstPaise: 960,
    igstPaise: 0,
    cessPaise: 0,
    lineTotalPaise: 17_920,
    ...overrides
  } as ReturnableLine;
}

function returnableDocument(overrides: Partial<ReturnableDocument> = {}): ReturnableDocument {
  return {
    documentId: IDs.sale,
    documentNumber: "INV/2627/000001",
    documentDate: "2026-09-12",
    counterpartyDisplayName: "Rahul Deshmukh",
    lines: [returnableLine()],
    ...overrides
  } as ReturnableDocument;
}

function returnLine(overrides: Partial<ReturnLineShape> = {}): ReturnLineShape {
  return {
    id: IDs.returnLine,
    returnDocumentId: IDs.ret,
    lineNumber: 1,
    originalSaleLineId: IDs.saleLine,
    originalPurchaseLineId: null,
    productId: IDs.product,
    productPackId: IDs.pack,
    batchId: IDs.batch,
    quantityBasis: "pack",
    quantityPacks: 1,
    quantityAtoms: 10,
    disposition: "quarantined",
    productDisplayName: "Crocin 500 mg Tablet",
    packDisplayLabel: "Strip of 10",
    baseUnitLabel: "Tablet",
    batchNumber: "B-2601",
    batchExpiresOn: "2028-03-31",
    hsnCodeId: null,
    hsnCode: "30049099",
    taxCategoryId: null,
    taxTreatmentKind: "taxable",
    taxRateVersionId: null,
    cgstBasisPoints: 600,
    sgstBasisPoints: 600,
    igstBasisPoints: 1_200,
    cessBasisPoints: 0,
    taxableValuePaise: 8_000,
    cgstPaise: 480,
    sgstPaise: 480,
    igstPaise: 0,
    cessPaise: 0,
    lineTotalPaise: 8_960,
    ...overrides
  } as ReturnLineShape;
}

function returnDocument(overrides: Partial<ReturnDetail> = {}): ReturnDetail {
  return {
    id: IDs.ret,
    storeId: IDs.store,
    returnKind: "sales_return",
    originalSaleDocumentId: IDs.sale,
    originalPurchaseDocumentId: null,
    originalDocumentNumber: "INV/2627/000001",
    originalDocumentDate: "2026-09-12",
    businessDate: "2026-09-12",
    status: "draft",
    revision: 2,
    seriesCode: null,
    financialYear: null,
    sequenceValue: null,
    documentNumber: null,
    storeGstRegistrationStatus: null,
    storeNormalizedGstin: null,
    storePlaceOfSupplyStateId: null,
    storeStateCode: null,
    counterpartyPartyId: null,
    counterpartyDisplayName: "Rahul Deshmukh",
    counterpartyGstRegistrationStatus: null,
    counterpartyNormalizedGstin: null,
    counterpartyStateCode: null,
    taxTreatment: null,
    taxAdjustmentStatus: null,
    taxAdjustmentReason: null,
    gstRoute: null,
    taxableValuePaise: 0,
    cgstPaise: 0,
    sgstPaise: 0,
    igstPaise: 0,
    cessPaise: 0,
    grandTotalPaise: 0,
    createdByUserId: IDs.user,
    createdAtUtc: "2026-09-12T05:00:00Z",
    updatedAtUtc: "2026-09-12T05:00:00Z",
    postedByUserId: null,
    postedAtUtc: null,
    lines: [returnLine()],
    supplierCreditNotes: [],
    ...overrides
  } as ReturnDetail;
}

function postedReturn(overrides: Partial<ReturnDetail> = {}): ReturnDetail {
  return returnDocument({
    status: "posted",
    revision: 3,
    seriesCode: "SR",
    financialYear: "2026-27",
    sequenceValue: 1,
    documentNumber: "SR/2627/000001",
    storeNormalizedGstin: "27AAACX0000A1Z9",
    taxTreatment: "intra_state",
    taxAdjustmentStatus: "commercial_only",
    taxableValuePaise: 8_000,
    cgstPaise: 480,
    sgstPaise: 480,
    grandTotalPaise: 8_960,
    postedByUserId: IDs.user,
    postedAtUtc: "2026-09-12T06:30:00Z",
    ...overrides
  });
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  document?: ReturnDetail;
  returnable?: ReturnableDocument;
  postError?: { code: string; status: number; extra?: Record<string, unknown> };
  lineError?: { code: string; status: number; extra?: Record<string, unknown> };
};

function returnService(options: Options = {}) {
  const role = options.role ?? "pharmacist";
  const state = {
    document: structuredClone(options.document ?? returnDocument()),
    requests: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Return User", role, revision: 1 };
    if (method !== "GET") state.requests.push({ method, path: url.pathname, body });

    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (/returnable-lines$/.test(url.pathname)) return response(options.returnable ?? returnableDocument());
    if (url.pathname === "/api/v1/returns" && method === "GET") {
      const { lines: _l, supplierCreditNotes: _s, ...header } = state.document;
      return response([header]);
    }
    if (url.pathname === "/api/v1/returns" && method === "POST") {
      return response({ ...state.document, lines: [] }, 201);
    }
    if (/^\/api\/v1\/returns\/[^/]+\/lines$/.test(url.pathname)) {
      if (options.lineError) return failure(options.lineError.code, options.lineError.status, options.lineError.extra);
      return response(state.document, 201);
    }
    if (/^\/api\/v1\/returns\/[^/]+\/quote$/.test(url.pathname)) {
      const lines = state.document.lines;
      return response({
        returnDocumentId: state.document.id,
        revision: state.document.revision,
        taxableValuePaise: lines.reduce((t, l) => t + l.taxableValuePaise, 0),
        cgstPaise: lines.reduce((t, l) => t + l.cgstPaise, 0),
        sgstPaise: lines.reduce((t, l) => t + l.sgstPaise, 0),
        igstPaise: 0,
        cessPaise: 0,
        grandTotalPaise: lines.reduce((t, l) => t + l.lineTotalPaise, 0)
      });
    }
    if (/^\/api\/v1\/returns\/[^/]+\/post$/.test(url.pathname)) {
      if (options.postError) return failure(options.postError.code, options.postError.status, options.postError.extra);
      state.document = postedReturn({ returnKind: state.document.returnKind });
      return response(state.document);
    }
    if (/^\/api\/v1\/returns\/[^/]+$/.test(url.pathname)) return response(state.document);
    if (/^\/api\/v1\/return-lines\/[^/]+$/.test(url.pathname)) {
      state.document = { ...state.document, lines: [], revision: state.document.revision + 1 };
      return response(state.document);
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });

  return { state, fetchMock };
}

function renderApp(path: string, service = returnService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

const writes = (service: ReturnType<typeof returnService>) => service.state.requests;

async function waitForWrite(service: ReturnType<typeof returnService>, match: (write: { method: string; path: string }) => boolean) {
  await waitFor(() => expect(writes(service).some(match)).toBe(true));
  return writes(service).find(match)!;
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("return helpers", () => {
  it("refuses a fractional return quantity instead of truncating it", () => {
    expect(quantityToInteger("2")).toBe(2);
    // `parseInt("1.5")` is 1, which would return one strip for a request that said one and a half.
    expect(quantityToInteger("1.5")).toBeNull();
    expect(quantityToInteger("0")).toBeNull();
    expect(quantityToInteger("")).toBeNull();
  });

  it("groups amounts the Indian way without dividing", () => {
    expect(paiseToAmountText(8_960)).toBe("89.60");
    expect(paiseToAmountText(123_456_789)).toBe("12,34,567.89");
  });
});

describe("Raising a return", () => {
  it("shows what was sold, what is already back, and what can still be returned", async () => {
    renderApp(`/app/sales/${IDs.sale}/return`);
    expect(await screen.findByRole("heading", { name: "Sales return", level: 1 })).toBeInTheDocument();
    expect(screen.getByText(/Against INV\/2627\/000001/)).toBeInTheDocument();

    const table = screen.getByRole("table");
    expect(within(table).getByText("Crocin 500 mg Tablet")).toBeInTheDocument();
    expect(within(table).getByText("B-2601")).toBeInTheDocument();
    // Sold and Can-return both read "2 Strip of 10" on an untouched line, which is the point:
    // nothing has come back yet, so the whole line is still returnable.
    expect(within(table).getAllByRole("cell", { name: "2 Strip of 10" })).toHaveLength(2);
    expect(within(table).getByRole("cell", { name: "—" })).toBeInTheDocument();
    // The raw identifier never reaches the operator.
    expect(within(table).queryByText(IDs.product)).not.toBeInTheDocument();
  });

  it("sends the quantity in the basis the sale was billed in, never in atoms", async () => {
    const service = returnService();
    renderApp(`/app/sales/${IDs.sale}/return`, service);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });

    fireEvent.change(screen.getByLabelText(/Quantity of Crocin/), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: "Start return" }));

    const added = await waitForWrite(service, (write) => write.path.endsWith("/lines"));
    expect(added.body).toMatchObject({ originalLineId: IDs.saleLine, quantity: 1, disposition: "quarantined" });
    expect(added.body).not.toHaveProperty("quantityAtoms");
  });

  it("refuses more than is left to return before asking the service", async () => {
    const service = returnService();
    renderApp(`/app/sales/${IDs.sale}/return`, service);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });

    fireEvent.change(screen.getByLabelText(/Quantity of Crocin/), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Start return" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Only 2 Strip of 10");
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  it("refuses a fractional quantity at the counter", async () => {
    const service = returnService();
    renderApp(`/app/sales/${IDs.sale}/return`, service);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    fireEvent.change(screen.getByLabelText(/Quantity of Crocin/), { target: { value: "1.5" } });
    fireEvent.click(screen.getByRole("button", { name: "Start return" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("whole number of Strip of 10");
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  /**
   * The disposition control cannot express `sellable` at all. A cashier who takes a medicine back
   * cannot put it on the shelf, whatever they choose here.
   */
  it("never offers to put returned goods straight back on the shelf", async () => {
    renderApp(`/app/sales/${IDs.sale}/return`);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    const disposition = screen.getByLabelText(/Where the returned Crocin/);
    const options = within(disposition).getAllByRole("option").map((option) => option.textContent);
    expect(options).toHaveLength(2);
    expect(options.join(" ")).toContain("quarantine");
    expect(options.join(" ")).toContain("Write off");
    expect(options.join(" ").toLowerCase()).not.toContain("sellable");
  });

  it("writes off an expired batch rather than offering to quarantine it", async () => {
    renderApp(`/app/sales/${IDs.sale}/return`, returnService({
      returnable: returnableDocument({ lines: [returnableLine({ batchExpiresOn: "2020-01-31" })] })
    }));
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    expect(screen.getByText(/Expired 2020-01-31/)).toBeInTheDocument();
    expect(screen.getByText("Written off — this batch has expired")).toBeInTheDocument();
    expect(screen.queryByLabelText(/Where the returned/)).not.toBeInTheDocument();
  });

  it("says plainly when a document has nothing left to return", async () => {
    renderApp(`/app/sales/${IDs.sale}/return`, returnService({
      returnable: returnableDocument({ lines: [returnableLine({ alreadyReturnedAtoms: 20, returnableAtoms: 0, returnableQuantity: 0 })] })
    }));
    expect(await screen.findByRole("heading", { name: "Nothing is left to return" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start return" })).not.toBeInTheDocument();
  });

  it("offers no purchase-return door to a cashier", async () => {
    renderApp(`/app/purchases/${IDs.purchase}/return`, returnService({ role: "cashier" }));
    // The page still loads for reading, but the posting control is not offered later.
    expect(await screen.findByRole("heading", { name: "Purchase return", level: 1 })).toBeInTheDocument();
  });
});

describe("Posting a return", () => {
  it("shows the amount from the service and posts the GST treatment that was chosen", async () => {
    const service = returnService();
    renderApp(`/app/returns/${IDs.ret}`, service);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    await waitFor(() => expect(screen.getByText("89.60")).toBeInTheDocument());

    fireEvent.change(screen.getByLabelText("GST treatment"), { target: { value: "tax_adjustable" } });
    fireEvent.change(screen.getByLabelText("Why (optional)"), { target: { value: "Customer is registered and has reversed the credit" } });
    fireEvent.click(screen.getByRole("button", { name: "Post return" }));

    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body).toMatchObject({
      taxAdjustmentStatus: "tax_adjustable",
      taxAdjustmentReason: "Customer is registered and has reversed the credit",
      gstRoute: null
    });
    expect(posted.body.idempotencyKey).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("posts a purchase return with its route and never calls it a debit note", async () => {
    const service = returnService({ role: "owner_admin", document: returnDocument({ returnKind: "purchase_return", lines: [returnLine({ disposition: null, originalSaleLineId: null, originalPurchaseLineId: IDs.purchaseLine })] }) });
    renderApp(`/app/returns/${IDs.ret}`, service);
    await screen.findByRole("heading", { name: "Purchase return", level: 1 });

    expect(screen.getByText(/a debit note is issued by the supplier, never by us/)).toBeInTheDocument();
    expect(screen.queryByLabelText("GST treatment")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("How the goods are going back"), { target: { value: "fresh_supply" } });
    fireEvent.click(screen.getByRole("button", { name: "Post return" }));

    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body).toMatchObject({ gstRoute: "fresh_supply", taxAdjustmentStatus: null });
  });

  /**
   * `disabled` only applies on the next render, so three clicks in one task all reach the handler.
   * A duplicate credit note is worse than a duplicate sale: money goes back twice.
   */
  it("sends one posting even when three clicks land before React can re-render", async () => {
    const service = returnService();
    renderApp(`/app/returns/${IDs.ret}`, service);
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    const button = await screen.findByRole("button", { name: "Post return" });

    await act(async () => {
      button.click();
      button.click();
      button.click();
    });

    await waitFor(() => expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(true));
    expect(writes(service).filter((write) => write.path.endsWith("/post"))).toHaveLength(1);
  });

  it("does not offer posting to a cashier", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({ role: "cashier" }));
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    expect(screen.queryByRole("button", { name: "Post return" })).not.toBeInTheDocument();
    expect(screen.getByText(/A pharmacist or the owner posts a sales return/)).toBeInTheDocument();
  });

  it("does not offer posting a purchase return to a pharmacist", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({
      role: "pharmacist",
      document: returnDocument({ returnKind: "purchase_return", lines: [returnLine({ disposition: null })] })
    }));
    await screen.findByRole("heading", { name: "Purchase return", level: 1 });
    expect(screen.queryByRole("button", { name: "Post return" })).not.toBeInTheDocument();
    expect(screen.getByText(/The owner posts a purchase return/)).toBeInTheDocument();
  });

  it("presents a service refusal in the counter's own words", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({
      postError: { code: "over_return", status: 409, extra: { returnableAtoms: 10 } }
    }));
    await screen.findByRole("heading", { name: "Sales return", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Post return" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("more than is left to return");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });
});

describe("Posted return", () => {
  it("shows the issued number, what it reverses, and where the goods went", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({ document: postedReturn() }));
    expect(await screen.findByRole("heading", { name: "SR/2627/000001", level: 1 })).toBeInTheDocument();
    expect(screen.getByText(/against INV\/2627\/000001/)).toBeInTheDocument();
    expect(screen.getByText("Refund only — do not reduce GST already charged")).toBeInTheDocument();
    expect(screen.getByText(/not available to sell until a pharmacist releases them/)).toBeInTheDocument();

    const table = screen.getByRole("table");
    expect(within(table).getByRole("cell", { name: "1 Strip of 10" })).toBeInTheDocument();
    expect(within(table).getByText("30049099")).toBeInTheDocument();
  });

  it("offers no way to edit a posted return", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({ document: postedReturn() }));
    await screen.findByRole("heading", { name: "SR/2627/000001", level: 1 });
    expect(screen.queryByRole("button", { name: "Post return" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("GST treatment")).not.toBeInTheDocument();
  });

  it("shows supplier credit-note evidence beside a purchase return without changing it", async () => {
    renderApp(`/app/returns/${IDs.ret}`, returnService({
      document: postedReturn({
        returnKind: "purchase_return",
        gstRoute: "supplier_credit_note",
        taxAdjustmentStatus: null,
        lines: [returnLine({ disposition: null })],
        supplierCreditNotes: [{
          id: "01997a00-0000-7000-8000-000000000099",
          returnDocumentId: IDs.ret,
          creditNoteNumber: "SUPP-CN-4471",
          creditNoteDate: "2026-09-20",
          creditNoteAmountPaise: 8_960,
          recordedAtUtc: "2026-09-20T05:00:00Z"
        }]
      })
    }));
    await screen.findByRole("heading", { name: "SR/2627/000001", level: 1 });
    expect(screen.getByText(/Supplier credit note SUPP-CN-4471/)).toBeInTheDocument();
    expect(screen.getByText(/delivery challan and await the supplier's credit note/)).toBeInTheDocument();
  });
});
