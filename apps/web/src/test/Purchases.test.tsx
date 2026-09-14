import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PurchaseDetail, PurchaseLine, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { paiseToAmountText, ratePerPackToPaise } from "../purchases/purchaseApi";

const IDs = {
  purchase: "01997a00-0000-7000-8000-000000000001",
  draft: "01997a00-0000-7000-8000-000000000002",
  supplier: "01997a00-0000-7000-8000-000000000010",
  otherSupplier: "01997a00-0000-7000-8000-000000000011",
  product: "01997a00-0000-7000-8000-000000000020",
  otherProduct: "01997a00-0000-7000-8000-000000000021",
  pack: "01997a00-0000-7000-8000-000000000030",
  otherPack: "01997a00-0000-7000-8000-000000000031",
  batch: "01997a00-0000-7000-8000-000000000040",
  line: "01997a00-0000-7000-8000-000000000050",
  unit: "01997a00-0000-7000-8000-000000000060",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080"
};

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const SUPPLIERS = [
  { id: IDs.supplier, displayName: "Sharma Medicals", legalName: null, normalizedSearchName: "sharma medicals", gstRegistrationStatus: "registered", gstin: null, normalizedGstin: "27AAACS1234A1Z5", pan: null, normalizedPan: null, placeOfSupplyStateId: null, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null, drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp },
  { id: IDs.otherSupplier, displayName: "Kerala Distributors", legalName: null, normalizedSearchName: "kerala distributors", gstRegistrationStatus: "registered", gstin: null, normalizedGstin: "32AAACK9876B1Z2", pan: null, normalizedPan: null, placeOfSupplyStateId: null, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null, drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp }
];

function pack(id: string, label: string) {
  return { id, productId: id === IDs.pack ? IDs.product : IDs.otherProduct, containerUnitId: IDs.unit, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: null, displayLabel: label, revision: 1, status: "active", ...stamp };
}

function product(id: string, name: string, packs: Array<ReturnType<typeof pack>>) {
  return { id, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.unit, quantityScale: 0, displayName: name, formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [], packs };
}

const PRODUCTS = [
  product(IDs.product, "Crocin 500 mg Tablet", [pack(IDs.pack, "Strip of 10")]),
  product(IDs.otherProduct, "Dolo 650 mg Tablet", [pack(IDs.otherPack, "Strip of 15")])
];

const BATCHES = [
  { id: IDs.batch, productPackId: IDs.pack, batchNumber: "B-2601", normalizedBatchNumber: "b-2601", manufacturedOn: null, expiresOn: "2028-03-31", mrpPaise: 4500, revision: 1, status: "active", ...stamp }
];

function line(overrides: Partial<PurchaseLine> = {}): PurchaseLine {
  return {
    id: IDs.line, purchaseDocumentId: IDs.draft, lineNumber: 1, productId: IDs.product, productPackId: IDs.pack,
    batchId: IDs.batch, newBatchNumber: null, newBatchExpiresOn: null, newBatchMrpPaise: null,
    quantityPacks: 10, ratePerPackPaise: 3_000, quantityAtoms: 100, taxableValuePaise: 30_000,
    hsnCodeId: null, hsnCode: null, taxCategoryId: null, taxTreatmentKind: null, taxRateVersionId: null,
    cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, cessBasisPoints: 0,
    cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 0, ...overrides
  };
}

function purchase(overrides: Partial<PurchaseDetail> = {}): PurchaseDetail {
  return {
    id: IDs.draft, storeId: IDs.store, supplierPartyId: IDs.supplier, supplierInvoiceNumber: "INV-4471",
    normalizedSupplierInvoiceNumber: "inv-4471", invoiceDate: "2026-09-10", status: "draft", revision: 1,
    supplierDisplayName: null, supplierGstRegistrationStatus: null, supplierNormalizedGstin: null,
    supplierPlaceOfSupplyStateId: null, supplierStateCode: null, storeGstRegistrationStatus: null,
    storeNormalizedGstin: null, storePlaceOfSupplyStateId: null, storeStateCode: null, taxTreatment: null,
    taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0,
    createdByUserId: IDs.user, createdAtUtc: "2026-09-10T05:00:00Z", updatedAtUtc: "2026-09-10T05:00:00Z",
    postedByUserId: null, postedAtUtc: null, lines: [line()], ...overrides
  };
}

/** A posted document as the Store Service returns it: every snapshot filled, every tax resolved. */
function postedPurchase(overrides: Partial<PurchaseDetail> = {}): PurchaseDetail {
  return purchase({
    id: IDs.purchase, status: "posted", revision: 2, supplierDisplayName: "Sharma Medicals",
    supplierGstRegistrationStatus: "registered", supplierNormalizedGstin: "27AAACS1234A1Z5", supplierStateCode: "27",
    storeGstRegistrationStatus: "registered", storeNormalizedGstin: "27AAACX0000A1Z9", storeStateCode: "27",
    taxTreatment: "intra_state", taxableValuePaise: 30_000, cgstPaise: 1_800, sgstPaise: 1_800,
    igstPaise: 0, cessPaise: 0, grandTotalPaise: 33_600,
    postedByUserId: IDs.user, postedAtUtc: "2026-09-11T06:30:00Z",
    lines: [line({
      purchaseDocumentId: IDs.purchase, hsnCode: "30049099", taxTreatmentKind: "taxable",
      cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200, cessBasisPoints: 0,
      cgstPaise: 1_800, sgstPaise: 1_800, lineTotalPaise: 33_600
    })],
    ...overrides
  });
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  documents?: PurchaseDetail[];
  writeError?: { code: string; status: number; extra?: Record<string, unknown> };
  failList?: boolean;
};

/**
 * A Store Service double that keeps each document's revision the way the real service does: every
 * line write bumps the parent document, and a stale `expectedRevision` is refused.
 */
function purchaseService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    documents: new Map((options.documents ?? [purchase()]).map((document) => [document.id, structuredClone(document)])),
    requests: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  const bump = (document: PurchaseDetail) => { document.revision += 1; return document; };
  const guard = (document: PurchaseDetail, body: Record<string, unknown>) =>
    body.expectedRevision === document.revision
      ? null
      : failure("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Purchase User", role, revision: 1 };
    if (method !== "GET") state.requests.push({ method, path: url.pathname, body });

    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/parties") return response(SUPPLIERS);
    if (url.pathname === "/api/v1/products") return response(PRODUCTS);
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return response(PRODUCTS.find((item) => item.id === url.pathname.split("/").pop()) ?? null);
    }
    if (/^\/api\/v1\/packs\/[^/]+\/batches$/.test(url.pathname)) {
      const packId = url.pathname.split("/")[4];
      return response(BATCHES.filter((batch) => batch.productPackId === packId));
    }

    if (url.pathname === "/api/v1/purchases" && method === "GET") {
      if (options.failList) return failure("internal_error", 500);
      const status = url.searchParams.get("status");
      const supplier = url.searchParams.get("supplierPartyId");
      return response([...state.documents.values()]
        .filter((document) => !status || status === "all" || document.status === status)
        .filter((document) => !supplier || document.supplierPartyId === supplier)
        .map(({ lines: _lines, ...header }) => header));
    }
    if (url.pathname === "/api/v1/purchases" && method === "POST") {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      const created = purchase({ id: IDs.draft, ...body, lines: [] });
      state.documents.set(created.id, created);
      return response(created, 201);
    }

    const detailMatch = /^\/api\/v1\/purchases\/([^/]+)$/.exec(url.pathname);
    if (detailMatch) {
      const document = state.documents.get(detailMatch[1]);
      if (!document) return failure("purchase_not_found", 404);
      if (method === "GET") return response(document);
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      Object.assign(document, { supplierPartyId: body.supplierPartyId, supplierInvoiceNumber: body.supplierInvoiceNumber, invoiceDate: body.invoiceDate });
      return response(bump(document));
    }

    const linesMatch = /^\/api\/v1\/purchases\/([^/]+)\/lines$/.exec(url.pathname);
    if (linesMatch) {
      const document = state.documents.get(linesMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      document.lines.push(line({
        id: `${IDs.line}-${document.lines.length + 1}`, purchaseDocumentId: document.id, lineNumber: document.lines.length + 1,
        productId: String(body.productId), productPackId: String(body.productPackId),
        batchId: (body.batchId as string | null) ?? null, newBatchNumber: (body.newBatchNumber as string | null) ?? null,
        newBatchExpiresOn: (body.newBatchExpiresOn as string | null) ?? null, newBatchMrpPaise: (body.newBatchMrpPaise as number | null) ?? null,
        quantityPacks: Number(body.quantityPacks), ratePerPackPaise: Number(body.ratePerPackPaise),
        taxableValuePaise: Number(body.quantityPacks) * Number(body.ratePerPackPaise)
      }));
      return response(bump(document), 201);
    }

    const postMatch = /^\/api\/v1\/purchases\/([^/]+)\/post$/.exec(url.pathname);
    if (postMatch) {
      const document = state.documents.get(postMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      const posted = postedPurchase({ id: document.id, supplierInvoiceNumber: document.supplierInvoiceNumber, invoiceDate: document.invoiceDate });
      state.documents.set(document.id, posted);
      return response(posted);
    }

    const lineMatch = /^\/api\/v1\/purchase-lines\/([^/]+)$/.exec(url.pathname);
    if (lineMatch) {
      const document = [...state.documents.values()].find((item) => item.lines.some((each) => each.id === lineMatch[1]))!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      if (method === "DELETE") document.lines = document.lines.filter((each) => each.id !== lineMatch[1]);
      else {
        const target = document.lines.find((each) => each.id === lineMatch[1])!;
        Object.assign(target, {
          productId: body.productId, productPackId: body.productPackId,
          batchId: body.batchId ?? null, newBatchNumber: body.newBatchNumber ?? null,
          newBatchExpiresOn: body.newBatchExpiresOn ?? null, newBatchMrpPaise: body.newBatchMrpPaise ?? null,
          quantityPacks: Number(body.quantityPacks), ratePerPackPaise: Number(body.ratePerPackPaise),
          taxableValuePaise: Number(body.quantityPacks) * Number(body.ratePerPackPaise)
        });
      }
      return response(bump(document));
    }

    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });

  return { state, fetchMock };
}

function renderApp(path: string, service = purchaseService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

const writes = (service: ReturnType<typeof purchaseService>) => service.state.requests;

/** Chooses a Product and then one of its Packs, waiting for each query to supply its options. */
async function chooseProductAndPack(dialog: HTMLElement, productName: string, packName: string) {
  await waitFor(() => expect(within(dialog).getByRole("option", { name: productName })).toBeInTheDocument());
  fireEvent.change(within(dialog).getByLabelText(/^Product/), { target: { value: PRODUCTS.find((item) => item.displayName === productName)!.id } });
  await waitFor(() => expect(within(dialog).getByRole("option", { name: packName })).toBeInTheDocument());
  fireEvent.change(within(dialog).getByLabelText(/^Pack/), { target: { value: PRODUCTS.flatMap((item) => item.packs).find((item) => item.displayLabel === packName)!.id } });
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("purchase money and quantity conversion", () => {
  it("converts a typed rate to exact paise without binary floating point", () => {
    expect(ratePerPackToPaise("12.5")).toBe(1_250);
    expect(ratePerPackToPaise("0.07")).toBe(7);
    expect(ratePerPackToPaise("1234")).toBe(123_400);
    // Free scheme goods are invoiced at nil rate, and the Store Service accepts it.
    expect(ratePerPackToPaise("0")).toBe(0);
  });

  it("refuses a rate that cannot be recorded exactly", () => {
    expect(ratePerPackToPaise("12.505")).toBeNull();
    expect(ratePerPackToPaise("-5")).toBeNull();
    expect(ratePerPackToPaise("1e3")).toBeNull();
    expect(ratePerPackToPaise("")).toBeNull();
    expect(ratePerPackToPaise("twelve")).toBeNull();
  });

  it("renders paise as a grouped Indian amount without losing a rupee", () => {
    expect(paiseToAmountText(0)).toBe("0.00");
    expect(paiseToAmountText(7)).toBe("0.07");
    expect(paiseToAmountText(33_600)).toBe("336.00");
    expect(paiseToAmountText(12_345_678)).toBe("1,23,456.78");
    expect(paiseToAmountText(-5_000)).toBe("-50.00");
  });
});

describe("Purchase list", () => {
  it("lists documents and never shows a total for an unposted draft", async () => {
    renderApp("/app/purchases", purchaseService({ documents: [purchase(), postedPurchase()] }));
    const rows = await screen.findAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(screen.getByText("Not posted")).toBeInTheDocument();
    expect(screen.getByText("336.00")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "New Purchase" })).toBeInTheDocument();
  });

  it("asks the Store Service to filter rather than filtering a full list in the browser", async () => {
    const app = renderApp("/app/purchases", purchaseService({ documents: [purchase(), postedPurchase()] }));
    await screen.findAllByRole("row");
    fireEvent.change(screen.getByLabelText("Status"), { target: { value: "posted" } });
    await waitFor(() => expect(screen.queryByText("Not posted")).not.toBeInTheDocument());
    const requested = app.fetchMock.mock.calls.map(([input]) => String(input));
    expect(requested.some((url) => url.includes("/api/v1/purchases?status=posted"))).toBe(true);
  });

  it("separates a failed list from an empty one and offers no create action to a pharmacist", async () => {
    renderApp("/app/purchases", purchaseService({ failList: true, role: "pharmacist" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByText("No purchase documents yet")).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "New Purchase" })).not.toBeInTheDocument();
    expect(screen.getByText("Read-only access")).toBeInTheDocument();
  });
});

describe("Purchase draft editor", () => {
  it("states plainly that a draft carries no stock and no GST yet", async () => {
    renderApp(`/app/purchases/${IDs.draft}`);
    expect(await screen.findByRole("heading", { name: "INV-4471" })).toBeInTheDocument();
    expect(screen.getByText(/Nothing has entered stock and no GST has been recorded/)).toBeInTheDocument();
    expect(screen.getAllByText(/GST is determined when this purchase is posted/).length).toBeGreaterThan(0);
    // A draft must not present zeroed tax columns, which would read as "no tax applies".
    expect(screen.queryByText("CGST")).not.toBeInTheDocument();
    expect(screen.queryByText("Invoice total")).not.toBeInTheDocument();
  });

  it("offers only the chosen Product's packs, and only that pack's batches", async () => {
    renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Line" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Line" });

    const packField = within(dialog).getByLabelText(/^Pack/);
    // Before a Product is chosen the Pack list is empty, not the whole catalogue's packs.
    expect(within(packField).queryByRole("option", { name: /Strip of/ })).not.toBeInTheDocument();

    await chooseProductAndPack(dialog, "Crocin 500 mg Tablet", "Strip of 10");
    // The other Product's pack is never offered under this Product.
    expect(within(packField).queryByRole("option", { name: "Strip of 15" })).not.toBeInTheDocument();
    const batchField = within(dialog).getByLabelText(/^Batch/);
    await waitFor(() => expect(within(batchField).getByRole("option", { name: /B-2601/ })).toBeInTheDocument());
  });

  it("sends either an existing batch or a new one, never both", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Line" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Line" });

    await chooseProductAndPack(dialog, "Crocin 500 mg Tablet", "Strip of 10");
    fireEvent.click(within(dialog).getByLabelText("A batch not recorded yet"));
    fireEvent.change(within(dialog).getByLabelText(/^Batch number/), { target: { value: "B-2699" } });
    fireEvent.change(within(dialog).getByLabelText("Expires on"), { target: { value: "2029-01-31" } });
    fireEvent.change(within(dialog).getByLabelText("MRP per pack"), { target: { value: "45.50" } });
    fireEvent.change(within(dialog).getByLabelText(/^Quantity \(Pack\)/), { target: { value: "12" } });
    fireEvent.change(within(dialog).getByLabelText(/^Rate per Pack/), { target: { value: "30.25" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Line" }));

    await waitFor(() => expect(writes(app)).toHaveLength(1));
    expect(writes(app)[0].body).toMatchObject({
      expectedRevision: 1, productId: IDs.product, productPackId: IDs.pack,
      batchId: null, newBatchNumber: "B-2699", newBatchExpiresOn: "2029-01-31", newBatchMrpPaise: 4_550,
      quantityPacks: 12, ratePerPackPaise: 3_025
    });
    // No derived or tax figure is ever submitted from the browser.
    for (const field of ["taxableValuePaise", "cgstPaise", "quantityAtoms", "lineTotalPaise", "taxTreatment"]) {
      expect(writes(app)[0].body).not.toHaveProperty(field);
    }
  });

  it("clears the other batch mode when an existing line switches to a recorded batch", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`, purchaseService({
      documents: [purchase({ lines: [line({ batchId: null, newBatchNumber: "B-OLD", newBatchExpiresOn: "2027-01-31" })] })]
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Line 1" });
    await waitFor(() => expect(within(dialog).getByRole("option", { name: "Strip of 10" })).toBeInTheDocument());

    fireEvent.click(within(dialog).getByLabelText("An existing batch"));
    await waitFor(() => expect(within(dialog).getByRole("option", { name: /B-2601/ })).toBeInTheDocument());
    fireEvent.change(within(dialog).getByLabelText(/^Batch/), { target: { value: IDs.batch } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Line" }));

    await waitFor(() => expect(writes(app)).toHaveLength(1));
    // Nulls are sent, not omitted, so the server clears the abandoned proposal.
    expect(writes(app)[0].body).toMatchObject({ batchId: IDs.batch, newBatchNumber: null, newBatchExpiresOn: null, newBatchMrpPaise: null });
  });

  it("keeps the dialog open for the next line and clears only what changes between lines", async () => {
    // A supplier invoice has many lines; reopening the dialog for each cost a click and a full tab
    // traversal. The product and pack repeat down a distributor's invoice, so they are kept.
    const app = renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Line" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Line" });
    await chooseProductAndPack(dialog, "Crocin 500 mg Tablet", "Strip of 10");
    fireEvent.click(within(dialog).getByLabelText("A batch not recorded yet"));
    fireEvent.change(within(dialog).getByLabelText(/^Batch number/), { target: { value: "B-1" } });
    fireEvent.change(within(dialog).getByLabelText(/^Quantity \(Pack\)/), { target: { value: "5" } });
    fireEvent.change(within(dialog).getByLabelText(/^Rate per Pack/), { target: { value: "20" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add and Next Line" }));

    await waitFor(() => expect(writes(app)).toHaveLength(1));
    // Still open, ready for the next line.
    expect(screen.getByRole("dialog", { name: "Add Line" })).toBeInTheDocument();
    expect(within(dialog).getByLabelText(/^Quantity \(Pack\)/)).toHaveValue("");
    expect(within(dialog).getByLabelText(/^Rate per Pack/)).toHaveValue("");
    expect(within(dialog).getByLabelText(/^Batch number/)).toHaveValue("");
    // The product and pack survive, because the next line is usually from the same invoice block.
    expect(within(dialog).getByLabelText(/^Product/)).toHaveValue(IDs.product);
    expect(within(dialog).getByLabelText(/^Pack/)).toHaveValue(IDs.pack);

    // The second line carries the revision the first one produced.
    fireEvent.change(within(dialog).getByLabelText(/^Batch number/), { target: { value: "B-2" } });
    fireEvent.change(within(dialog).getByLabelText(/^Quantity \(Pack\)/), { target: { value: "7" } });
    fireEvent.change(within(dialog).getByLabelText(/^Rate per Pack/), { target: { value: "21" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Line" }));
    await waitFor(() => expect(writes(app)).toHaveLength(2));
    expect(writes(app)[1].body).toMatchObject({ expectedRevision: 2, newBatchNumber: "B-2", quantityPacks: 7 });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("shows the running taxable value beside Add Line so it survives a long invoice", async () => {
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({
      documents: [purchase({ lines: [line(), line({ id: `${IDs.line}-2`, lineNumber: 2, taxableValuePaise: 12_345 })] })]
    }));
    const header = (await screen.findByRole("heading", { name: "Lines" })).parentElement!;
    expect(within(header).getByText("2 lines")).toBeInTheDocument();
    expect(within(header).getByText("423.45")).toBeInTheDocument();
  });

  it("refuses a rate the Store Service could not record exactly, before sending anything", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Line" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Line" });
    await chooseProductAndPack(dialog, "Crocin 500 mg Tablet", "Strip of 10");
    fireEvent.change(within(dialog).getByLabelText("Batch *"), { target: { value: IDs.batch } });
    fireEvent.change(within(dialog).getByLabelText(/^Quantity \(Pack\)/), { target: { value: "5" } });
    fireEvent.change(within(dialog).getByLabelText(/^Rate per Pack/), { target: { value: "30.257" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Line" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("at most two decimal places");
    expect(writes(app)).toHaveLength(0);
  });

  it("rejects a fractional pack quantity without contacting the Store Service", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Line" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Line" });
    await chooseProductAndPack(dialog, "Crocin 500 mg Tablet", "Strip of 10");
    fireEvent.change(within(dialog).getByLabelText("Batch *"), { target: { value: IDs.batch } });
    fireEvent.change(within(dialog).getByLabelText(/^Quantity \(Pack\)/), { target: { value: "2.5" } });
    fireEvent.change(within(dialog).getByLabelText(/^Rate per Pack/), { target: { value: "30" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Line" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("whole number of packs");
    expect(writes(app)).toHaveLength(0);
  });

  it("surfaces a safe message and a reload when the draft changed underneath", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`, purchaseService({
      documents: [purchase()],
      writeError: { code: "revision_conflict", status: 409, extra: { expectedRevision: 1, currentRevision: 4 } }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Header" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Invoice Header" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Header" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("changed after you opened it");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(within(alert).getByRole("button", { name: "Reload latest" })).toBeInTheDocument();
    expect(writes(app)).toHaveLength(1);
  });

  it("shows the mapped message for a duplicate supplier invoice", async () => {
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({
      documents: [purchase()], writeError: { code: "duplicate_supplier_invoice", status: 409 }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Header" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Invoice Header" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Header" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("already recorded");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });
});

describe("Posting a purchase", () => {
  it("cannot be started on a draft with no lines", async () => {
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({ documents: [purchase({ lines: [] })] }));
    expect(await screen.findByRole("button", { name: "Post Purchase" })).toBeDisabled();
    expect(screen.getByText("No lines yet")).toBeInTheDocument();
  });

  it("warns that posting is permanent and sends one stable idempotency key per attempt", async () => {
    const app = renderApp(`/app/purchases/${IDs.draft}`, purchaseService({
      documents: [purchase()], writeError: { code: "service_busy", status: 503 }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Post Purchase" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Purchase" });
    expect(within(dialog).getByText("This cannot be undone")).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "Post Purchase" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("busy");
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Purchase" }));
    await waitFor(() => expect(writes(app)).toHaveLength(2));

    // A retry of the same attempt must reuse the key, or the invoice could post twice.
    const [first, second] = writes(app);
    expect(first.body.idempotencyKey).toBe(second.body.idempotencyKey);
    expect(String(first.body.idempotencyKey)).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });

  it("issues one request however fast the operator clicks Post", async () => {
    // A disabled attribute lands on the next render, so three synchronous clicks really did send
    // three postings. The idempotency key kept the invoice single, but the extra requests were
    // avoidable work racing each other.
    const app = renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Post Purchase" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Purchase" });
    const confirm = within(dialog).getByRole("button", { name: "Post Purchase" });

    fireEvent.click(confirm);
    fireEvent.click(confirm);
    fireEvent.click(confirm);

    await waitFor(() => expect(writes(app).filter((write) => write.path.endsWith("/post"))).toHaveLength(1));
    expect(await screen.findByText("Posted · read-only")).toBeInTheDocument();
  });

  it("replaces the editor with the posted document once posting succeeds", async () => {
    renderApp(`/app/purchases/${IDs.draft}`);
    fireEvent.click(await screen.findByRole("button", { name: "Post Purchase" }));
    const dialog = await screen.findByRole("dialog", { name: "Post Purchase" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Post Purchase" }));

    expect(await screen.findByText("Posted · read-only")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add Line" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit Header" })).not.toBeInTheDocument();
  });
});

describe("Posted purchase detail", () => {
  it("shows the snapshots and the tax as recorded, not today's profile", async () => {
    renderApp(`/app/purchases/${IDs.purchase}`, purchaseService({ documents: [postedPurchase()] }));
    expect(await screen.findByText("CGST + SGST (same State)")).toBeInTheDocument();
    expect(screen.getByText("27AAACS1234A1Z5")).toBeInTheDocument();
    expect(screen.getByText("27AAACX0000A1Z9")).toBeInTheDocument();
    expect(screen.getByText(/Later changes to the supplier or store profile do not alter this document/)).toBeInTheDocument();

    expect(screen.getByText("CGST 6.00%")).toBeInTheDocument();
    expect(screen.getByText("SGST 6.00%")).toBeInTheDocument();
    // An intra-State document never shows IGST, and a zero cess is not shown as a line.
    expect(screen.queryByText(/^IGST/)).not.toBeInTheDocument();
    expect(screen.queryByText(/^Cess/)).not.toBeInTheDocument();
    expect(screen.getByText("30049099")).toBeInTheDocument();
  });

  it("shows IGST alone for an inter-State purchase", async () => {
    renderApp(`/app/purchases/${IDs.purchase}`, purchaseService({
      documents: [postedPurchase({
        supplierStateCode: "32", taxTreatment: "inter_state", cgstPaise: 0, sgstPaise: 0, igstPaise: 3_600,
        lines: [line({ purchaseDocumentId: IDs.purchase, hsnCode: "30049099", taxTreatmentKind: "taxable", igstBasisPoints: 1_200, igstPaise: 3_600, lineTotalPaise: 33_600 })]
      })]
    }));
    expect(await screen.findByText("IGST (different States)")).toBeInTheDocument();
    expect(screen.getByText("IGST 12.00%")).toBeInTheDocument();
    expect(screen.queryByText(/^CGST/)).not.toBeInTheDocument();
  });

  it("names an exempt line instead of showing it as zero-rate tax", async () => {
    renderApp(`/app/purchases/${IDs.purchase}`, purchaseService({
      documents: [postedPurchase({
        cgstPaise: 0, sgstPaise: 0, grandTotalPaise: 30_000,
        lines: [line({ purchaseDocumentId: IDs.purchase, hsnCode: "30049099", taxTreatmentKind: "exempt", lineTotalPaise: 30_000 })]
      })]
    }));
    expect(await screen.findByText("Exempt")).toBeInTheDocument();
    expect(screen.queryByText("CGST 0.00%")).not.toBeInTheDocument();
  });

  it("is read-only for a pharmacist, including a draft", async () => {
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({ role: "pharmacist" }));
    expect(await screen.findByText("This purchase is still a draft")).toBeInTheDocument();
    expect(screen.getByText(/Only an Owner\/Admin can change or post it/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add Line" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Post Purchase" })).not.toBeInTheDocument();
  });

  it("never shows a party id where a supplier name belongs", async () => {
    // A draft has no supplier snapshot, and the fallback printed the raw party id on screen.
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({ role: "pharmacist" }));
    expect(await screen.findByText("This purchase is still a draft")).toBeInTheDocument();
    await waitFor(() => expect(screen.getAllByText(/Sharma Medicals/).length).toBeGreaterThan(0));
    expect(document.body.textContent).not.toContain(IDs.supplier);
    expect(document.body.textContent).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/);
  });

  it("shows a read-only draft no tax figure and no snapshot it does not have", async () => {
    renderApp(`/app/purchases/${IDs.draft}`, purchaseService({ role: "pharmacist" }));
    expect(await screen.findByText("This purchase is still a draft")).toBeInTheDocument();

    // Zeroed tax would read as "no GST applies", and a null snapshot is not "Not registered".
    expect(screen.queryByRole("columnheader", { name: "GST" })).not.toBeInTheDocument();
    expect(screen.queryByRole("columnheader", { name: "Line total" })).not.toBeInTheDocument();
    expect(screen.queryByText("CGST 0.00%")).not.toBeInTheDocument();
    expect(screen.queryByText("Invoice totals")).not.toBeInTheDocument();
    expect(screen.queryByText("Not registered")).not.toBeInTheDocument();
    expect(screen.queryByText("Not determined")).not.toBeInTheDocument();
    // The taxable value it does have is still shown.
    expect(screen.getByText("300.00")).toBeInTheDocument();
  });
});
