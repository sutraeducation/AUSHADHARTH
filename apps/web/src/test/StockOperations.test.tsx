import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProductDetail, StockOperationDetail, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

/**
 * Phase 1J stock operations, from the counter's side.
 *
 * The rule these tests exist to hold is the one an operator cannot see: the browser sends what was
 * typed and nothing else. A physical count sends a counted quantity, never a delta, and every
 * resulting figure on screen came back from the Store Service.
 */

const IDs = {
  product: "01997000-0000-7000-8000-000000000001",
  tablet: "01997000-0000-7000-8000-000000000002",
  strip: "01997000-0000-7000-8000-000000000003",
  store: "01997000-0000-7000-8000-000000000008",
  pack: "01997000-0000-7000-8000-000000000009",
  batch: "01997000-0000-7000-8000-000000000020",
  expired: "01997000-0000-7000-8000-000000000021",
  operation: "01997000-0000-7000-8000-000000000030",
  line: "01997000-0000-7000-8000-000000000031",
  user: "01900000-0000-7000-8000-000000000001"
};
const stamp = {
  createdAtUtc: "2026-01-01T00:00:00Z",
  updatedAtUtc: "2026-01-01T00:00:00Z",
  archivedAtUtc: null,
  archiveReason: null
};
const system = {
  status: "ok",
  apiVersion: "v1",
  applicationVersion: "0.0.0",
  compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 }
};

function product(): ProductDetail {
  return {
    id: IDs.product,
    productKind: "general_pharmacy_item",
    brandId: null,
    dosageFormId: null,
    baseUnitId: IDs.tablet,
    quantityScale: 0,
    displayName: "Crocin 500 mg Tablet",
    formulationDescriptor: null,
    routeDescriptor: null,
    releaseDescriptor: null,
    revision: 1,
    status: "active",
    ...stamp,
    companyRoles: [],
    composition: [],
    packs: [
      {
        id: IDs.pack,
        productId: IDs.product,
        containerUnitId: IDs.strip,
        baseQuantityAtoms: 10,
        containedPackId: null,
        containedPackCount: null,
        skuCode: "CROCIN-10",
        skuStoreId: IDs.store,
        displayLabel: "Strip of 10",
        revision: 1,
        status: "active",
        ...stamp
      }
    ]
  };
}

const batches = [
  {
    id: IDs.batch,
    productPackId: IDs.pack,
    batchNumber: "B-900",
    normalizedBatchNumber: "B-900",
    manufacturedOn: "2026-01-01",
    expiresOn: "2029-12-31",
    mrpPaise: 12550,
    revision: 1,
    status: "active" as const,
    ...stamp
  },
  {
    id: IDs.expired,
    productPackId: IDs.pack,
    batchNumber: "B-901",
    normalizedBatchNumber: "B-901",
    manufacturedOn: "2024-01-01",
    expiresOn: "2025-05-31",
    mrpPaise: 12550,
    revision: 1,
    status: "active" as const,
    ...stamp
  }
];

function response(body: unknown, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}
function failure(code: string, status: number) {
  return response({ code, message: "raw backend detail", issues: [] }, status);
}

type Options = { role?: UserRole; lineError?: { code: string; status: number }; currentAtoms?: number };

/** A Store Service double that answers with the shapes the contract publishes. */
function stockOperationService(options: Options = {}) {
  const role = options.role ?? "pharmacist";
  const current = options.currentAtoms ?? 200;
  const state = {
    operation: null as StockOperationDetail | null,
    discarded: [] as string[],
    posted: [] as string[]
  };

  const draft = (kind: string): StockOperationDetail => ({
    id: IDs.operation,
    storeId: IDs.store,
    operationKind: kind as StockOperationDetail["operationKind"],
    businessDate: "2026-06-15",
    status: "draft",
    revision: 1,
    note: null,
    createdByUserId: IDs.user,
    createdAtUtc: stamp.createdAtUtc,
    updatedAtUtc: stamp.updatedAtUtc,
    postedByUserId: null,
    postedAtUtc: null,
    lines: []
  });

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Stock User", role, revision: 1 };

    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status"))
      return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname.endsWith("/dashboard/summary"))
      return response({ storeDisplayName: "Care Pharmacy", activeProductCount: 1, activePackCount: 1 });
    if (url.pathname === "/api/v1/catalog/context") return response({ storeId: IDs.store });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (url.pathname === "/api/v1/products")
      return response([{ ...product(), companyRoles: undefined, packs: undefined, composition: undefined }]);
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return response(product());
    if (/\/packs\/[^/]+\/batches$/.test(url.pathname)) return response(batches);
    if (url.pathname === "/api/v1/inventory/stock") return response([]);
    if (url.pathname === "/api/v1/inventory/movements") return response([]);

    if (url.pathname === "/api/v1/stock-operations" && method === "GET")
      return response(state.operation ? [state.operation] : []);
    if (url.pathname === "/api/v1/stock-operations" && method === "POST") {
      state.operation = draft(body.operationKind);
      return response(state.operation, 201);
    }
    if (/\/stock-operations\/[^/]+\/lines$/.test(url.pathname) && method === "POST") {
      if (options.lineError) return failure(options.lineError.code, options.lineError.status);
      const line = {
        id: IDs.line,
        stockOperationId: IDs.operation,
        lineNumber: (state.operation!.lines.length + 1),
        productId: IDs.product,
        productPackId: body.productPackId,
        batchId: body.batchId ?? null,
        stockStatus: body.stockStatus,
        targetStockStatus: body.targetStockStatus ?? null,
        direction: body.countedQuantity != null ? "count" : body.targetStockStatus ? "transfer" : body.direction,
        reasonCode: body.reasonCode,
        countedAtoms: body.countedQuantity ?? null,
        quantityAtoms: body.quantity ?? null,
        quantityBasis: body.quantityBasis,
        quantityPacks: null,
        appliedDeltaAtoms: null,
        note: body.note ?? null,
        productDisplayName: "Crocin 500 mg Tablet",
        packDisplayLabel: "Strip of 10",
        baseUnitLabel: "Tablet",
        batchNumber: body.batchId === IDs.expired ? "B-901" : "B-900",
        batchExpiresOn: body.batchId === IDs.expired ? "2025-05-31" : "2029-12-31",
        quantityScale: 0,
        baseQuantityAtoms: 10
      } as StockOperationDetail["lines"][number];
      state.operation = {
        ...state.operation!,
        revision: state.operation!.revision + 1,
        lines: [...state.operation!.lines, line]
      };
      return response(state.operation, 201);
    }
    if (/\/stock-operations\/[^/]+\/quote$/.test(url.pathname)) {
      return response({
        operationId: IDs.operation,
        operationKind: state.operation!.operationKind,
        // The server's figures, which the screen must show rather than compute.
        lines: state.operation!.lines.map((line) => ({
          lineId: line.id,
          lineNumber: line.lineNumber,
          productDisplayName: line.productDisplayName,
          batchNumber: line.batchNumber,
          stockStatus: line.stockStatus,
          targetStockStatus: line.targetStockStatus,
          reasonCode: line.reasonCode,
          currentAtoms: current,
          requestedAtoms: line.countedAtoms ?? line.quantityAtoms ?? 0,
          resultingAtoms: line.countedAtoms ?? current - (line.quantityAtoms ?? 0),
          targetCurrentAtoms: null,
          targetResultingAtoms: null,
          sufficient: true
        })),
        postable: true
      });
    }
    if (/\/stock-operations\/[^/]+\/post$/.test(url.pathname)) {
      state.posted.push(IDs.operation);
      state.operation = { ...state.operation!, status: "posted", revision: state.operation!.revision + 1 };
      return response(state.operation);
    }
    if (/^\/api\/v1\/stock-operations\/[^/]+$/.test(url.pathname) && method === "DELETE") {
      state.discarded.push(url.pathname.split("/").pop()!);
      state.operation = null;
      return response(null, 204);
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock };
}

function renderApp(service = stockOperationService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } }
  });
  return {
    ...service,
    ...render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={["/app/inventory/stock"]}>
          <App />
        </MemoryRouter>
      </QueryClientProvider>
    )
  };
}

function bodiesFor(fetchMock: ReturnType<typeof stockOperationService>["fetchMock"], match: string) {
  return fetchMock.mock.calls
    .filter(([input]) => String(input).includes(match))
    .map(([, init]) => JSON.parse(String(init?.body ?? "{}")));
}

async function openDialog(name: string) {
  fireEvent.click(await screen.findByRole("button", { name }));
  return screen.findByRole("heading", { name });
}

async function chooseLot(batchId = IDs.batch) {
  fireEvent.change(await screen.findByLabelText("Product"), { target: { value: IDs.product } });
  await waitFor(() => expect((screen.getByLabelText("Pack") as HTMLSelectElement).value).toBe(IDs.pack));
  await waitFor(() =>
    expect(within(screen.getByLabelText("Batch") as HTMLSelectElement).getAllByRole("option").length).toBeGreaterThan(1)
  );
  fireEvent.change(screen.getByLabelText("Batch"), { target: { value: batchId } });
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("Phase 1J stock operations", () => {
  /** The rule the whole phase turns on. */
  it("sends a counted quantity and never a delta", async () => {
    const app = renderApp();
    await openDialog("Physical Count");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Counted quantity *"), { target: { value: "185" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));

    await waitFor(() => expect(bodiesFor(app.fetchMock, "/lines")).toHaveLength(1));
    const [sent] = bodiesFor(app.fetchMock, "/lines");
    expect(sent).toMatchObject({ countedQuantity: 185, stockStatus: "sellable", quantityBasis: "base_unit" });
    expect(sent.quantity).toBeNull();
    // Nothing resembling a delta or a balance left this browser.
    expect(JSON.stringify(sent)).not.toContain("delta");
    expect(JSON.stringify(sent)).not.toContain("200");
  });

  /** The variance on screen is the server's answer, not a subtraction done here. */
  it("shows the variance the Store Service computed", async () => {
    renderApp();
    await openDialog("Physical Count");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Counted quantity *"), { target: { value: "185" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));

    const row = await screen.findByRole("row", { name: /B-900/ });
    await waitFor(() => expect(within(row).getByText("200 → 185")).toBeInTheDocument());
  });

  it("offers a damaged lot both honest outcomes", async () => {
    renderApp();
    await openDialog("Mark Damaged");
    const outcome = screen.getByLabelText("What happens to them") as HTMLSelectElement;
    expect([...outcome.options].map((option) => option.value)).toEqual(["non_sellable", "quarantined"]);
    expect(within(outcome).getByRole("option", { name: /never be sold/ })).toBeInTheDocument();
    expect(within(outcome).getByRole("option", { name: /pharmacist will decide/ })).toBeInTheDocument();
  });

  /** An expired lot has one outcome, and the screen must not imply it has been thrown away. */
  it("lets an expired lot only be written off, and does not call that a disposal", async () => {
    renderApp();
    const heading = await openDialog("Handle Expired Stock");
    expect(heading).toBeInTheDocument();
    expect(screen.getByText(/does not mean it has been disposed of/i)).toBeInTheDocument();
    // With a single destination there is nothing to choose, so nothing is offered.
    expect(screen.queryByLabelText("What happens to them")).not.toBeInTheDocument();

    await chooseLot(IDs.expired);
    await waitFor(() => expect(screen.getByText(/Expired 2025-05-31/)).toBeInTheDocument());
  });

  it("will not hold stock back without saying what was found", async () => {
    const app = renderApp();
    await openDialog("Quarantine");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Quantity to hold *"), { target: { value: "20" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Say what was found");
    expect(bodiesFor(app.fetchMock, "/lines")).toHaveLength(0);
  });

  /** Backing out of a draft leaves nothing behind — the wart earlier phases still carry. */
  it("discards a draft nobody posted", async () => {
    const app = renderApp();
    await openDialog("Physical Count");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Counted quantity *"), { target: { value: "185" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));
    await screen.findByRole("row", { name: /B-900/ });

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(app.state.discarded).toEqual([IDs.operation]));
    expect(app.state.posted).toEqual([]);
  });

  it("presents a refusal in words the counter can act on", async () => {
    const app = renderApp(
      stockOperationService({ lineError: { code: "stock_operation_line_conflict", status: 409 } })
    );
    await openDialog("Mark Damaged");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Damaged quantity *"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("does not belong on this kind of operation");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(app.state.posted).toEqual([]);
  });

  it("posts what the operator built and closes", async () => {
    const app = renderApp();
    await openDialog("Mark Damaged");
    await chooseLot();
    fireEvent.change(screen.getByLabelText("Damaged quantity *"), { target: { value: "2" } });
    fireEvent.change(screen.getByLabelText("Quantity entered in"), { target: { value: "packs" } });
    fireEvent.click(screen.getByRole("button", { name: "Add line" }));
    await screen.findByRole("row", { name: /B-900/ });

    fireEvent.click(screen.getByRole("button", { name: /^Post mark damaged$/i }));
    await waitFor(() => expect(app.state.posted).toEqual([IDs.operation]));
    const [sent] = bodiesFor(app.fetchMock, "/lines");
    // Two packs are sent as two packs. The Store Service holds the pack size and converts once.
    expect(sent).toMatchObject({ quantity: 2, quantityBasis: "pack", targetStockStatus: "non_sellable" });
  });

  it("names the operations a pharmacy performs without making anyone read the ledger", async () => {
    renderApp();
    fireEvent.click(await screen.findByRole("link", { name: "Stock Operations" }));
    expect(await screen.findByRole("heading", { name: "No stock operations yet" })).toBeInTheDocument();
  });
});
