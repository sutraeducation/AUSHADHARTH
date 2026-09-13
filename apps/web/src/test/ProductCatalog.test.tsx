import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Barcode, ProductDetail, ProductPack, ReferenceKind, ReferenceMasterResponse, StorePackPolicy, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { atomsToQuantity, quantityToAtoms } from "../products/productApi";

const IDs = {
  product: "01997000-0000-7000-8000-000000000001",
  tablet: "01997000-0000-7000-8000-000000000002",
  strip: "01997000-0000-7000-8000-000000000003",
  box: "01997000-0000-7000-8000-000000000004",
  dosage: "01997000-0000-7000-8000-000000000005",
  brand: "01997000-0000-7000-8000-000000000006",
  company: "01997000-0000-7000-8000-000000000007",
  store: "01997000-0000-7000-8000-000000000008",
  pack: "01997000-0000-7000-8000-000000000009",
  role: "01997000-0000-7000-8000-000000000010",
  policy: "01997000-0000-7000-8000-000000000011",
  barcode: "01997000-0000-7000-8000-000000000012"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const refs: Partial<Record<ReferenceKind, ReferenceMasterResponse[]>> = {
  units: [reference("units", IDs.tablet, { canonicalCode: "tablet", displayName: "Tablet", dimension: "count", isDiscrete: true, allowedScale: 0 }), reference("units", IDs.strip, { canonicalCode: "strip", displayName: "Strip", dimension: "count", isDiscrete: true, allowedScale: 0 }), reference("units", IDs.box, { canonicalCode: "box", displayName: "Box", dimension: "count", isDiscrete: true, allowedScale: 0 })],
  "dosage-forms": [reference("dosage-forms", IDs.dosage, { canonicalCode: "tablet", displayName: "Tablet", description: null, routeHint: "oral", releaseHint: null })],
  brands: [reference("brands", IDs.brand, { displayName: "Crocin", brandOwnerCompanyId: IDs.company })],
  companies: [reference("companies", IDs.company, { displayName: "GSK Pharma", legalName: null, city: null, state: null, countryCode: "IN" })]
};

function reference(kind: ReferenceKind, id: string, attributes: Record<string, unknown>): ReferenceMasterResponse {
  return { id, kind, revision: 1, status: "active", attributes, ...stamp } as ReferenceMasterResponse;
}
function product(): ProductDetail {
  return { id: IDs.product, productKind: "medicine", brandId: IDs.brand, dosageFormId: IDs.dosage, baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: "500 mg", routeDescriptor: "Oral", releaseDescriptor: null, revision: 1, status: "active", ...stamp,
    companyRoles: [{ id: IDs.role, productId: IDs.product, companyId: IDs.company, role: "manufacturer", effectiveFrom: null, effectiveTo: null, revision: 1, status: "active", ...stamp }],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 15, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-15", skuStoreId: IDs.store, displayLabel: "Strip of 15", revision: 1, status: "active", ...stamp }]
  };
}
function policyRecord(): StorePackPolicy {
  return { id: IDs.policy, productId: IDs.product, packId: IDs.pack, storeId: IDs.store, purchaseEnabled: true, saleEnabled: true, wholePackOnlyPurchase: false, fractionalSaleAllowed: false, minimumSaleIncrementAtoms: 1, defaultPurchasePack: true, defaultSalePack: true, revision: 1, status: "active", ...stamp };
}
function barcodeRecord(): Barcode {
  return { id: IDs.barcode, packId: IDs.pack, namespace: "gtin", normalizedValue: "8901234567890", symbology: "EAN-13", scope: "global", storeId: null, revision: 1, status: "active", ...stamp };
}
function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, issues: Array<{ field: string; message: string }> = [], extra: Record<string, unknown> = {}) {
  // Mirrors the Store Service error envelope, including the raw detail it never sends.
  return response({ code, message: "raw backend detail", issues, expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  products?: ProductDetail[];
  pendingProducts?: boolean;
  failProducts?: boolean;
  duplicates?: boolean;
  mutationError?: string;
  policy?: boolean;
  barcode?: boolean;
  failPolicy?: number;
  failBarcodes?: number;
  failReferences?: number;
  failContext?: number;
};

/**
 * A stateful Store Service double. It enforces the contracts the real service enforces — Phase 1B
 * SKU/Store pairing and optimistic revisions — so a request the backend would reject fails here too.
 */
function catalogService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    authenticated: true,
    displayName: "Catalog User",
    products: (options.products ?? [product()]).map((item) => JSON.parse(JSON.stringify(item)) as ProductDetail),
    policies: { [IDs.pack]: options.policy ? policyRecord() : null } as Record<string, StorePackPolicy | null>,
    barcodes: { [IDs.pack]: options.barcode ? [barcodeRecord()] : [] } as Record<string, Barcode[]>,
    referenceCalls: 0,
    contextCalls: 0,
    remainingPolicyFailures: options.failPolicy ?? 0,
    remainingBarcodeFailures: options.failBarcodes ?? 0,
    remainingReferenceFailures: options.failReferences ?? 0,
    remainingContextFailures: options.failContext ?? 0
  };

  const findProduct = (id: string) => state.products.find((item) => item.id === id) ?? state.products[0];
  const findPack = (packId: string) => state.products.flatMap((item) => item.packs).find((pack) => pack.id === packId);
  const ownerOf = (packId: string) => state.products.find((item) => item.packs.some((pack) => pack.id === packId));

  // Phase 1B: sku_store_id must be present exactly when sku_code is present.
  const skuPairingError = (pack: { skuCode?: unknown; skuStoreId?: unknown }) => {
    const hasSku = typeof pack.skuCode === "string" && pack.skuCode.trim().length > 0;
    const hasStore = typeof pack.skuStoreId === "string" && pack.skuStoreId.length > 0;
    return hasSku === hasStore ? null : failure("validation_failed", 422, [{ field: "skuStoreId", message: "is required exactly when skuCode is present" }]);
  };
  const revisionError = (current: number, expected: unknown) =>
    current === expected ? null : failure("revision_conflict", 409, [], { expectedRevision: expected ?? null, currentRevision: current });

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: "01900000-0000-7000-8000-000000000001", loginIdentifier: role, displayName: state.displayName, role, revision: 1 };

    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: state.authenticated, user: state.authenticated ? user : null, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) { state.authenticated = false; return response(null, 204); }
    if (url.pathname.endsWith("/auth/login")) { state.authenticated = true; state.displayName = "Second User"; return response({ user: { ...user, displayName: "Second User" }, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" }); }
    if (!state.authenticated) return failure("authentication_required", 401);
    if (url.pathname.endsWith("/dashboard/summary")) return response({ storeDisplayName: "Care Pharmacy", activeProductCount: state.products.length, activePackCount: 1 });

    if (url.pathname === "/api/v1/catalog/context") {
      state.contextCalls += 1;
      if (state.remainingContextFailures > 0) { state.remainingContextFailures -= 1; return failure("internal_error", 500); }
      return response({ storeId: IDs.store });
    }
    const refMatch = /^\/api\/v1\/reference\/([^/]+)$/.exec(url.pathname);
    if (refMatch) {
      state.referenceCalls += 1;
      if (state.remainingReferenceFailures > 0) { state.remainingReferenceFailures -= 1; return failure("internal_error", 500); }
      return response(refs[refMatch[1] as ReferenceKind] ?? []);
    }
    if (url.pathname === "/api/v1/products/duplicate-candidates") return response(options.duplicates ? [{ candidateId: IDs.product, score: 90, reasonCodes: ["display_name_match"], explanation: "similar" }] : []);

    if (url.pathname === "/api/v1/products" && method === "GET") {
      if (options.pendingProducts) return new Promise(() => undefined);
      if (options.failProducts) return failure("internal_error", 500);
      const search = (url.searchParams.get("search") ?? "").toLowerCase();
      return response(state.products.filter((item) => !search || item.displayName.toLowerCase().includes(search)));
    }
    if (url.pathname === "/api/v1/products" && method === "POST") {
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      for (const pack of body.packs ?? []) { const error = skuPairingError(pack); if (error) return error; }
      const created: ProductDetail = { ...product(), ...body.product, id: IDs.product, revision: 1, status: "active", companyRoles: [], packs: [{ ...product().packs[0], ...body.packs[0], id: IDs.pack, productId: IDs.product, revision: 1, status: "active" }] };
      state.products = [created];
      return response(created, 201);
    }

    const productMatch = /^\/api\/v1\/products\/([^/]+)(?:\/(archive|restore|company-roles|packs))?$/.exec(url.pathname);
    if (productMatch) {
      const record = findProduct(productMatch[1]);
      const action = productMatch[2];
      if (method === "GET" && !action) return response(record);
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      if (!action) {
        const conflict = revisionError(record.revision, body.expectedRevision); if (conflict) return conflict;
        Object.assign(record, body.product, { revision: record.revision + 1 });
        return response(record);
      }
      if (action === "archive" || action === "restore") {
        const conflict = revisionError(record.revision, body.expectedRevision); if (conflict) return conflict;
        record.status = action === "archive" ? "archived" : "active"; record.revision += 1;
        return response(record);
      }
      if (action === "company-roles" && method === "GET") return response(record.companyRoles);
      if (action === "company-roles") {
        const role = { id: `${IDs.role}-new`, productId: record.id, ...body, revision: 1, status: "active" as const, ...stamp };
        record.companyRoles.push(role);
        return response(role, 201);
      }
      if (action === "packs" && method === "GET") return response(record.packs);
      if (action === "packs") {
        const error = skuPairingError(body); if (error) return error;
        const pack: ProductPack = { id: `${IDs.pack}-new`, productId: record.id, ...body, revision: 1, status: "active", ...stamp };
        record.packs.push(pack);
        return response(pack, 201);
      }
    }

    const roleMatch = /^\/api\/v1\/company-roles\/([^/]+)(?:\/(archive|restore))?$/.exec(url.pathname);
    if (roleMatch) {
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      const owner = state.products.find((item) => item.companyRoles.some((role) => role.id === roleMatch[1]))!;
      const record = owner.companyRoles.find((role) => role.id === roleMatch[1])!;
      const conflict = revisionError(record.revision, body.expectedRevision); if (conflict) return conflict;
      if (roleMatch[2]) record.status = roleMatch[2] === "archive" ? "archived" : "active";
      else Object.assign(record, body.role);
      record.revision += 1;
      return response(record);
    }

    const packMatch = /^\/api\/v1\/packs\/([^/]+)(?:\/(archive|restore|policy|barcodes))?$/.exec(url.pathname);
    if (packMatch) {
      const packId = packMatch[1]; const action = packMatch[2];
      if (action === "policy") {
        if (method === "GET") {
          if (state.remainingPolicyFailures > 0) { state.remainingPolicyFailures -= 1; return failure("internal_error", 500); }
          return state.policies[packId] ? response(state.policies[packId]) : failure("not_found", 404);
        }
        if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
        const existing = state.policies[packId] ?? null;
        if (existing) { const conflict = revisionError(existing.revision, body.expectedRevision); if (conflict) return conflict; }
        const saved: StorePackPolicy = { ...policyRecord(), ...body.policy, packId, id: existing?.id ?? IDs.policy, revision: (existing?.revision ?? 0) + 1, status: "active" };
        state.policies[packId] = saved;
        return response(saved, existing ? 200 : 201);
      }
      if (action === "barcodes") {
        if (method === "GET") {
          if (state.remainingBarcodeFailures > 0) { state.remainingBarcodeFailures -= 1; return failure("internal_error", 500); }
          return response(state.barcodes[packId] ?? []);
        }
        if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
        const created: Barcode = { ...barcodeRecord(), id: `${IDs.barcode}-new`, packId, namespace: body.namespace, normalizedValue: String(body.value).replace(/\s+/g, ""), symbology: body.symbology ?? null, scope: body.scope, storeId: body.storeId ?? null };
        state.barcodes[packId] = [...(state.barcodes[packId] ?? []), created];
        return response(created, 201);
      }
      const record = findPack(packId)!;
      if (method === "GET") return response(record);
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      const conflict = revisionError(record.revision, body.expectedRevision); if (conflict) return conflict;
      if (action) record.status = action === "archive" ? "archived" : "active";
      else { const error = skuPairingError(body.pack); if (error) return error; Object.assign(record, body.pack); }
      record.revision += 1;
      void ownerOf(packId);
      return response(record);
    }

    const policyLifecycle = /^\/api\/v1\/pack-policies\/([^/]+)\/(archive|restore)$/.exec(url.pathname);
    if (policyLifecycle) {
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      const packId = Object.keys(state.policies).find((key) => state.policies[key]?.id === policyLifecycle[1])!;
      const record = state.policies[packId]!;
      const conflict = revisionError(record.revision, body.expectedRevision); if (conflict) return conflict;
      state.policies[packId] = { ...record, status: policyLifecycle[2] === "archive" ? "archived" : "active", revision: record.revision + 1 };
      return response(state.policies[packId]);
    }

    const barcodeLifecycle = /^\/api\/v1\/barcodes\/([^/]+)\/(archive|restore)$/.exec(url.pathname);
    if (barcodeLifecycle) {
      if (options.mutationError) return failure(options.mutationError, 409, [], { expectedRevision: 1, currentRevision: 2 });
      const packId = Object.keys(state.barcodes).find((key) => state.barcodes[key].some((item) => item.id === barcodeLifecycle[1]))!;
      state.barcodes[packId] = state.barcodes[packId].map((item) => item.id === barcodeLifecycle[1] ? { ...item, status: barcodeLifecycle[2] === "archive" ? "archived" : "active", revision: item.revision + 1 } : item);
      return response(state.barcodes[packId].find((item) => item.id === barcodeLifecycle[1]));
    }

    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });

  return { state, fetchMock };
}

function renderApp(path: string, service = catalogService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}
function bodiesFor(fetchMock: ReturnType<typeof catalogService>["fetchMock"], predicate: (url: string, method: string) => boolean) {
  return fetchMock.mock.calls.filter(([input, init]) => predicate(String(input), init?.method ?? "GET")).map(([, init]) => JSON.parse(String(init?.body ?? "{}")));
}
/** Reads of the authoritative Product record — the query every in-workspace conflict reload must refetch. */
function productFetches(app: { fetchMock: ReturnType<typeof catalogService>["fetchMock"] }) {
  return app.fetchMock.mock.calls.filter(([input, init]) => /\/api\/v1\/products\/[^/?]+$/.test(String(input)) && (init?.method ?? "GET") === "GET").length;
}
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Product Catalog UI", () => {
  it("adds protected Products navigation and renders the searchable catalog", async () => {
    const app = renderApp("/app/products");
    expect(await screen.findByRole("heading", { name: "Products" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^Products/ })).toHaveClass("nav-item--active");
    expect(await screen.findByRole("cell", { name: "Crocin 500 mg Tablet" }, { timeout: 3000 })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Search"), { target: { value: "Crocin" } });
    await waitFor(() => expect(app.fetchMock.mock.calls.some(([input]) => String(input).includes("search=Crocin"))).toBe(true));
  });

  it("shows loading, empty, no-results, error, and retry states", async () => {
    const loading = renderApp("/app/products", catalogService({ pendingProducts: true })); expect(await screen.findByText("Loading products…")).toBeInTheDocument(); loading.unmount();
    const empty = renderApp("/app/products", catalogService({ products: [] })); expect(await screen.findByRole("heading", { name: "No products yet" })).toBeInTheDocument(); empty.unmount();
    const failing = renderApp("/app/products", catalogService({ failProducts: true }));
    expect(await screen.findByRole("alert")).toHaveTextContent("could not be loaded");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(failing.fetchMock.mock.calls.filter(([input]) => String(input).includes("/products?")).length).toBeGreaterThan(1));
  });

  it("validates creation, focuses the first invalid field, and applies medicine/kind rules", async () => {
    renderApp("/app/products/new"); await screen.findByRole("heading", { name: "Add Product" });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Product" }));
    expect(screen.getByRole("textbox", { name: /Display name/ })).toHaveFocus(); expect(screen.getByRole("combobox", { name: /^Dosage Form/ })).toHaveAttribute("aria-invalid", "true");
    fireEvent.change(screen.getByLabelText("Product kind"), { target: { value: "device" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Product" }));
    expect(screen.getByRole("combobox", { name: /^Dosage Form/ })).not.toHaveAttribute("aria-invalid", "true");
  });

  it("uses real searchable reference selectors and fixes discrete precision to whole units", async () => {
    renderApp("/app/products/new"); await screen.findByRole("option", { name: "Crocin" });
    expect(screen.getByRole("option", { name: "GSK Pharma" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/^Base Unit/), { target: { value: IDs.tablet } });
    expect(screen.getByText("Whole units only")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Search Brand"), { target: { value: "Cro" } });
    expect(await screen.findByRole("option", { name: "Crocin" })).toBeInTheDocument();
  });

  it("warns about advisory duplicates and permits an intentional atomic aggregate create", async () => {
    const app = renderApp("/app/products/new", catalogService({ duplicates: true })); await screen.findByRole("option", { name: "Crocin" });
    fireEvent.change(screen.getByLabelText(/Display name/), { target: { value: "Crocin 500 mg Tablet" } });
    fireEvent.change(screen.getByLabelText(/^Dosage Form/), { target: { value: IDs.dosage } }); fireEvent.change(screen.getByLabelText(/^Base Unit/), { target: { value: IDs.tablet } }); fireEvent.change(screen.getByLabelText(/^Pack Unit/), { target: { value: IDs.strip } }); fireEvent.change(screen.getByLabelText(/Direct base quantity/), { target: { value: "15" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Product" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Possible similar product already exists"); fireEvent.click(screen.getByRole("button", { name: "Continue with legitimate creation" })); fireEvent.click(screen.getByRole("button", { name: "Create Product Anyway" }));
    await waitFor(() => { const [created] = bodiesFor(app.fetchMock, (url, method) => url.endsWith("/products") && method === "POST"); expect(created?.packs[0]).toMatchObject({ clientKey: "initial-pack", baseQuantityAtoms: 15 }); });
  });

  it("renders detail, company role, Pack/SKU containment and policy/barcode workspace", async () => {
    renderApp(`/app/products/${IDs.product}`, catalogService({ policy: true, barcode: true }));
    expect(await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" })).toBeInTheDocument(); expect(screen.getByRole("cell", { name: "GSK Pharma" })).toBeInTheDocument(); expect(screen.getByRole("cell", { name: "CROCIN-15" })).toBeInTheDocument(); expect(screen.getByRole("cell", { name: "Direct" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Manage" })); expect(await screen.findByText("8901234567890")).toBeInTheDocument(); expect(screen.getByText(/Purchase/).closest("section")).toHaveTextContent("Default"); expect(screen.getByRole("button", { name: "Archive Policy" })).toBeInTheDocument();
  });

  it("validates Pack containment exactly and scopes an added SKU to the authenticated Store", async () => {
    const detail = product(); detail.packs.push({ ...detail.packs[0], id: IDs.box, containerUnitId: IDs.box, baseQuantityAtoms: 150, displayLabel: "Box of 10 strips", skuCode: null, skuStoreId: null });
    const app = renderApp(`/app/products/${IDs.product}`, catalogService({ products: [detail] })); fireEvent.click(await screen.findByRole("button", { name: "Add Pack" }));
    const packUnit = screen.getByRole("combobox", { name: /^Pack Unit/ }); await waitFor(() => expect(packUnit).toBeEnabled());
    fireEvent.change(packUnit, { target: { value: IDs.box } });
    fireEvent.change(screen.getByRole("combobox", { name: "Contained Pack (optional)" }), { target: { value: IDs.pack } });
    fireEvent.change(await screen.findByRole("spinbutton", { name: "Contained Pack count" }), { target: { value: "10" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Direct base quantity" }), { target: { value: "149" } });
    fireEvent.change(screen.getByRole("textbox", { name: /SKU \(optional\)/ }), { target: { value: "BOX-10" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("must equal 150");
    fireEvent.change(screen.getByRole("textbox", { name: "Direct base quantity" }), { target: { value: "150" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    // The Store Service rejects a Pack carrying skuCode without skuStoreId, so the added Pack must
    // carry the current Store; the create only succeeds because the pair is complete.
    await waitFor(() => { const [sent] = bodiesFor(app.fetchMock, (url, method) => /\/products\/[^/]+\/packs$/.test(url) && method === "POST"); expect(sent).toMatchObject({ skuCode: "BOX-10", skuStoreId: IDs.store }); });
    await waitFor(() => expect(app.state.products[0].packs).toHaveLength(3));
  });

  it("keeps the Store scope when an existing SKU-bearing Pack is edited and clears both fields when the SKU is blanked", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    const rows = await screen.findAllByRole("button", { name: "Edit" });
    fireEvent.click(rows[rows.length - 1]);
    const dialog = await screen.findByRole("dialog", { name: "Edit Pack" });
    expect(within(dialog).getByRole("textbox", { name: /SKU \(optional\)/ })).toHaveValue("CROCIN-15");
    fireEvent.change(within(dialog).getByRole("textbox", { name: "Pack label" }), { target: { value: "Strip of 15 tablets" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => { const [sent] = bodiesFor(app.fetchMock, (url, method) => /\/packs\/[^/]+$/.test(url) && method === "PUT"); expect(sent.pack).toMatchObject({ skuCode: "CROCIN-15", skuStoreId: IDs.store }); });
    await waitFor(() => expect(app.state.products[0].packs[0].displayLabel).toBe("Strip of 15 tablets"));

    fireEvent.click((await screen.findAllByRole("button", { name: "Edit" })).slice(-1)[0]);
    const reopened = await screen.findByRole("dialog", { name: "Edit Pack" });
    fireEvent.change(within(reopened).getByRole("textbox", { name: /SKU \(optional\)/ }), { target: { value: "   " } });
    fireEvent.click(within(reopened).getByRole("button", { name: "Save" }));
    await waitFor(() => { const sent = bodiesFor(app.fetchMock, (url, method) => /\/packs\/[^/]+$/.test(url) && method === "PUT"); expect(sent.at(-1).pack).toMatchObject({ skuCode: null, skuStoreId: null }); });
  });

  it("saves a Product edit and returns to the updated detail page", async () => {
    const app = renderApp(`/app/products/${IDs.product}/edit`);
    const name = await screen.findByRole("textbox", { name: /Display name/ });
    expect(name).toHaveValue("Crocin 500 mg Tablet");
    fireEvent.change(name, { target: { value: "Crocin 650 mg Tablet" } });
    fireEvent.click(screen.getByRole("button", { name: "Save Product" }));
    await waitFor(() => { const [sent] = bodiesFor(app.fetchMock, (url, method) => /\/products\/[^/]+$/.test(url) && method === "PUT"); expect(sent).toMatchObject({ expectedRevision: 1, product: { displayName: "Crocin 650 mg Tablet" } }); });
    expect(await screen.findByRole("heading", { name: "Crocin 650 mg Tablet" })).toBeInTheDocument();
    expect(app.state.products[0].revision).toBe(2);
  });

  it("creates, edits, archives, and restores a company role", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add Company Role" }));
    const create = await screen.findByRole("dialog", { name: "Add Company Role" });
    await waitFor(() => expect(within(create).getByRole("combobox", { name: /^Company/ })).toBeEnabled());
    fireEvent.change(within(create).getByRole("combobox", { name: /^Company/ }), { target: { value: IDs.company } });
    fireEvent.change(within(create).getByRole("combobox", { name: "Role" }), { target: { value: "marketer" } });
    fireEvent.click(within(create).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(app.state.products[0].companyRoles).toHaveLength(2));

    const roleRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Manufacturer"))!;
    fireEvent.click(within(roleRow()).getByRole("button", { name: "Edit" }));
    const edit = await screen.findByRole("dialog", { name: "Edit Company Role" });
    fireEvent.change(within(edit).getByRole("combobox", { name: "Role" }), { target: { value: "importer" } });
    fireEvent.click(within(edit).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(app.state.products[0].companyRoles[0].role).toBe("importer"));

    const importerRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Importer"))!;
    fireEvent.click(within(importerRow()).getByRole("button", { name: "Archive" }));
    fireEvent.change(screen.getByRole("textbox", { name: /Reason/ }), { target: { value: "Superseded" } });
    fireEvent.click(screen.getByRole("button", { name: "Archive record" }));
    await waitFor(() => expect(app.state.products[0].companyRoles[0].status).toBe("archived"));

    fireEvent.click(within(importerRow()).getByRole("button", { name: "Restore" }));
    fireEvent.click(await screen.findByRole("button", { name: "Restore record" }));
    await waitFor(() => expect(app.state.products[0].companyRoles[0].status).toBe("active"));
  });

  it("archives and restores a Pack without deleting it", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" });
    const packRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Strip of 15"))!;
    fireEvent.click(within(packRow()).getByRole("button", { name: "Archive" }));
    fireEvent.change(await screen.findByRole("textbox", { name: /Reason/ }), { target: { value: "Discontinued presentation" } });
    fireEvent.click(screen.getByRole("button", { name: "Archive record" }));
    await waitFor(() => expect(app.state.products[0].packs[0].status).toBe("archived"));
    fireEvent.click(within(packRow()).getByRole("button", { name: "Restore" }));
    fireEvent.click(await screen.findByRole("button", { name: "Restore record" }));
    await waitFor(() => expect(app.state.products[0].packs[0].status).toBe("active"));
    expect(app.state.products[0].packs).toHaveLength(1);
  });

  it("adds, archives, and restores a Store Pack Policy and a Barcode", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, catalogService({ barcode: true }));
    fireEvent.click(await screen.findByRole("button", { name: "Manage" }));
    fireEvent.click(await screen.findByRole("button", { name: "Add Policy" }));
    const policy = await screen.findByRole("dialog", { name: "Add Store Pack Policy" });
    fireEvent.click(within(policy).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(app.state.policies[IDs.pack]).not.toBeNull());
    fireEvent.click(await screen.findByRole("button", { name: "Archive Policy" }));
    await waitFor(() => expect(app.state.policies[IDs.pack]!.status).toBe("archived"));
    fireEvent.click(await screen.findByRole("button", { name: "Restore Policy" }));
    await waitFor(() => expect(app.state.policies[IDs.pack]!.status).toBe("active"));

    const barcodeItem = () => screen.getByText("8901234567890").closest("li")!;
    fireEvent.click(within(barcodeItem()).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(app.state.barcodes[IDs.pack][0].status).toBe("archived"));
    fireEvent.click(within(barcodeItem()).getByRole("button", { name: "Restore" }));
    await waitFor(() => expect(app.state.barcodes[IDs.pack][0].status).toBe("active"));
  });

  it("reloads the latest Product revision instead of overwriting a concurrent edit", async () => {
    const app = renderApp(`/app/products/${IDs.product}/edit`);
    const name = await screen.findByRole("textbox", { name: /Display name/ });
    // Another session saves first.
    app.state.products[0].revision = 4; app.state.products[0].displayName = "Crocin 500 mg Tablet (revised)";
    fireEvent.change(name, { target: { value: "My stale name" } });
    fireEvent.click(screen.getByRole("button", { name: "Save Product" }));
    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent("changed after you opened it");
    expect(notice).not.toHaveTextContent("raw backend detail");
    expect(app.state.products[0].displayName).toBe("Crocin 500 mg Tablet (revised)");

    fireEvent.click(within(notice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(screen.getByRole("textbox", { name: /Display name/ })).toHaveValue("Crocin 500 mg Tablet (revised)"));
    expect(screen.getByRole("link", { name: "Cancel" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save Product" }));
    await waitFor(() => { const sent = bodiesFor(app.fetchMock, (url, method) => /\/products\/[^/]+$/.test(url) && method === "PUT"); expect(sent.at(-1).expectedRevision).toBe(4); });
    await waitFor(() => expect(app.state.products[0].revision).toBe(5));
  });

  it("reloads the latest company role, Pack, and policy revisions from their conflict notices", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, catalogService({ policy: true }));
    await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" });
    const roleRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Manufacturer"))!;
    fireEvent.click(within(roleRow()).getByRole("button", { name: "Edit" }));
    const roleDialog = await screen.findByRole("dialog", { name: "Edit Company Role" });
    app.state.products[0].companyRoles[0].revision = 6;
    fireEvent.click(within(roleDialog).getByRole("button", { name: "Save" }));
    const roleNotice = await within(roleDialog).findByRole("alert");
    expect(roleNotice).toHaveTextContent("changed after you opened it");
    fireEvent.click(within(roleNotice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(within(roleDialog).queryByRole("alert")).not.toBeInTheDocument());
    fireEvent.click(within(roleDialog).getByRole("button", { name: "Save" }));
    await waitFor(() => { const sent = bodiesFor(app.fetchMock, (url, method) => /\/company-roles\/[^/]+$/.test(url) && method === "PUT"); expect(sent.at(-1).expectedRevision).toBe(6); });

    const packRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Strip of 15"))!;
    fireEvent.click(within(packRow()).getByRole("button", { name: "Edit" }));
    const packDialog = await screen.findByRole("dialog", { name: "Edit Pack" });
    await waitFor(() => expect(within(packDialog).getByRole("button", { name: "Save" })).toBeEnabled());
    app.state.products[0].packs[0].revision = 7; app.state.products[0].packs[0].displayLabel = "Strip of 15 (revised)";
    fireEvent.click(within(packDialog).getByRole("button", { name: "Save" }));
    const packNotice = await within(packDialog).findByRole("alert");
    expect(packNotice).toHaveTextContent("changed after you opened it");
    fireEvent.click(within(packNotice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(within(packDialog).getByRole("textbox", { name: "Pack label" })).toHaveValue("Strip of 15 (revised)"));
    fireEvent.click(within(packDialog).getByRole("button", { name: "Save" }));
    await waitFor(() => { const sent = bodiesFor(app.fetchMock, (url, method) => /\/packs\/[^/]+$/.test(url) && method === "PUT"); expect(sent.at(-1).expectedRevision).toBe(7); });

    fireEvent.click(await screen.findByRole("button", { name: "Manage" }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Policy" }));
    const policyDialog = await screen.findByRole("dialog", { name: "Edit Store Pack Policy" });
    app.state.policies[IDs.pack] = { ...app.state.policies[IDs.pack]!, revision: 9, minimumSaleIncrementAtoms: 4 };
    fireEvent.click(within(policyDialog).getByRole("button", { name: "Save" }));
    const policyNotice = await within(policyDialog).findByRole("alert");
    expect(policyNotice).toHaveTextContent("changed after you opened it");
    fireEvent.click(within(policyNotice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(within(policyDialog).getByRole("textbox", { name: /Minimum sale increment/ })).toHaveValue("4"));
    fireEvent.click(within(policyDialog).getByRole("button", { name: "Save" }));
    await waitFor(() => { const sent = bodiesFor(app.fetchMock, (url, method) => /\/packs\/[^/]+\/policy$/.test(url) && method === "PUT"); expect(sent.at(-1).expectedRevision).toBe(9); });
  });

  it("reloads the latest Product revision before archiving instead of acting on a stale one", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Archive Product" }));
    const dialog = await screen.findByRole("dialog", { name: "Archive record" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Reason/ }), { target: { value: "Duplicate catalog entry" } });
    // Another session saves first; the open dialog still holds revision 1.
    app.state.products[0].revision = 4;
    const archives = () => bodiesFor(app.fetchMock, (url, method) => /\/products\/[^/]+\/archive$/.test(url) && method === "POST");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));

    const notice = await within(dialog).findByRole("alert");
    expect(notice).toHaveTextContent("changed after you opened it");
    expect(notice).not.toHaveTextContent("raw backend detail");
    expect(archives()[0].expectedRevision).toBe(1);
    // The stale lifecycle attempt changed nothing.
    expect(app.state.products[0].status).toBe("active");
    expect(app.state.products[0].revision).toBe(4);

    const before = productFetches(app);
    fireEvent.click(within(notice).getByRole("button", { name: "Reload latest" }));
    // Reload latest must refetch the authoritative Product, not merely close the dialog.
    await waitFor(() => expect(productFetches(app)).toBeGreaterThan(before));
    await waitFor(() => expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument());
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));
    await waitFor(() => expect(archives().at(-1).expectedRevision).toBe(4));
    await waitFor(() => expect(app.state.products[0].status).toBe("archived"));
    expect(app.state.products[0].revision).toBe(5);
  });

  it("reloads the latest company role revision before archiving instead of acting on a stale one", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" });
    const roleRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Manufacturer"))!;
    fireEvent.click(within(roleRow()).getByRole("button", { name: "Archive" }));
    const dialog = await screen.findByRole("dialog", { name: "Archive record" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Reason/ }), { target: { value: "Superseded manufacturer" } });
    app.state.products[0].companyRoles[0].revision = 6;
    const archives = () => bodiesFor(app.fetchMock, (url, method) => /\/company-roles\/[^/]+\/archive$/.test(url) && method === "POST");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));

    const notice = await within(dialog).findByRole("alert");
    expect(notice).toHaveTextContent("changed after you opened it");
    expect(notice).not.toHaveTextContent("raw backend detail");
    expect(archives()[0].expectedRevision).toBe(1);
    expect(app.state.products[0].companyRoles[0].status).toBe("active");
    expect(app.state.products[0].companyRoles[0].revision).toBe(6);

    const before = productFetches(app);
    fireEvent.click(within(notice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(productFetches(app)).toBeGreaterThan(before));
    await waitFor(() => expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument());

    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));
    await waitFor(() => expect(archives().at(-1).expectedRevision).toBe(6));
    await waitFor(() => expect(app.state.products[0].companyRoles[0].status).toBe("archived"));
    expect(app.state.products[0].companyRoles[0].revision).toBe(7);
  });

  it("reloads the latest Pack revision before archiving instead of acting on a stale one", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" });
    const packRow = () => screen.getAllByRole("row").find((row) => within(row).queryByText("Strip of 15"))!;
    fireEvent.click(within(packRow()).getByRole("button", { name: "Archive" }));
    const dialog = await screen.findByRole("dialog", { name: "Archive record" });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Reason/ }), { target: { value: "Discontinued presentation" } });
    app.state.products[0].packs[0].revision = 7;
    const archives = () => bodiesFor(app.fetchMock, (url, method) => /\/packs\/[^/]+\/archive$/.test(url) && method === "POST");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));

    const notice = await within(dialog).findByRole("alert");
    expect(notice).toHaveTextContent("changed after you opened it");
    expect(notice).not.toHaveTextContent("raw backend detail");
    expect(archives()[0].expectedRevision).toBe(1);
    expect(app.state.products[0].packs[0].status).toBe("active");
    expect(app.state.products[0].packs[0].revision).toBe(7);

    const before = productFetches(app);
    fireEvent.click(within(notice).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(productFetches(app)).toBeGreaterThan(before));
    await waitFor(() => expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument());

    fireEvent.click(within(dialog).getByRole("button", { name: "Archive record" }));
    await waitFor(() => expect(archives().at(-1).expectedRevision).toBe(7));
    await waitFor(() => expect(app.state.products[0].packs[0].status).toBe("archived"));
    expect(app.state.products[0].packs[0].revision).toBe(8);
    // Archive is never delete.
    expect(app.state.products[0].packs).toHaveLength(1);
  });

  it("separates policy and barcode query failures from empty results and retries them", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, catalogService({ failPolicy: 1, failBarcodes: 1, policy: true, barcode: true }));
    fireEvent.click(await screen.findByRole("button", { name: "Manage" }));
    const alerts = await screen.findAllByRole("alert");
    const messages = alerts.map((alert) => alert.textContent ?? "").join(" | ");
    expect(messages).toContain("Store policy could not be loaded.");
    expect(messages).toContain("Barcodes could not be loaded.");
    // A failed query is never presented as a valid empty business state.
    expect(screen.queryByText("No store policy configured.")).not.toBeInTheDocument();
    expect(screen.queryByText("No barcodes assigned.")).not.toBeInTheDocument();
    expect(messages).not.toContain("raw backend detail");
    for (const alert of alerts) fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("8901234567890")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Archive Policy" })).toBeInTheDocument();
    expect(app.state.remainingPolicyFailures).toBe(0);
  });

  it("reports a reference-label failure instead of a permanent loading label and retries it", async () => {
    renderApp("/app/products", catalogService({ failReferences: 4 }));
    expect(await screen.findByRole("cell", { name: "Crocin 500 mg Tablet" }, { timeout: 3000 })).toBeInTheDocument();
    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("names could not be loaded");
    expect(screen.getAllByRole("cell", { name: "Name unavailable" }).length).toBeGreaterThan(0);
    expect(screen.queryByRole("cell", { name: "Loading…" })).not.toBeInTheDocument();
    fireEvent.click(within(banner).getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("cell", { name: "Crocin" })).toBeInTheDocument();
  });

  it("reports a Catalog context failure instead of silently discarding a Store-scoped submission", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, catalogService({ failContext: 1 }));
    fireEvent.click(await screen.findByRole("button", { name: "Add Pack" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Pack" });
    await waitFor(() => expect(within(dialog).getByText(/Store information could not be loaded/)).toBeInTheDocument());
    await waitFor(() => expect(within(dialog).getByRole("combobox", { name: /^Pack Unit/ })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("combobox", { name: /^Pack Unit/ }), { target: { value: IDs.box } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: "Direct base quantity" }), { target: { value: "30" } });
    fireEvent.change(within(dialog).getByRole("textbox", { name: /SKU \(optional\)/ }), { target: { value: "NEW-SKU" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByText(/Store could not be identified/)).toBeInTheDocument();
    expect(bodiesFor(app.fetchMock, (url, method) => /\/packs$/.test(url) && method === "POST")).toHaveLength(0);

    fireEvent.click(within(dialog).getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(within(dialog).queryByText(/Store information could not be loaded/)).not.toBeInTheDocument());
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => { const [sent] = bodiesFor(app.fetchMock, (url, method) => /\/packs$/.test(url) && method === "POST"); expect(sent).toMatchObject({ skuCode: "NEW-SKU", skuStoreId: IDs.store }); });
  });

  it("never serves the previous session's Catalog reference data to the next session", async () => {
    const app = renderApp("/app/products");
    expect(await screen.findByRole("cell", { name: "Crocin" }, { timeout: 3000 })).toBeInTheDocument();
    const beforeSignOut = app.state.referenceCalls;
    expect(beforeSignOut).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: /Catalog User/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Sign out" }));
    await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" });
    // Cached Catalog references are session-scoped; nothing from the first session may survive.
    const afterSignOut = app.state.referenceCalls;

    fireEvent.change(screen.getByLabelText("Login ID"), { target: { value: "second.user" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "Strong-Password-42" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign In" }));
    await screen.findByRole("heading", { name: "Dashboard" });
    fireEvent.click(screen.getByRole("link", { name: /^Products/ }));
    expect(await screen.findByRole("cell", { name: "Crocin" }, { timeout: 3000 })).toBeInTheDocument();
    expect(app.state.referenceCalls).toBeGreaterThan(afterSignOut);
  });

  it("traps Tab and Shift+Tab inside a Product dialog and restores launcher focus", async () => {
    renderApp(`/app/products/${IDs.product}`);
    const launcher = await screen.findByRole("button", { name: "Add Company Role" });
    launcher.focus(); fireEvent.click(launcher);
    const dialog = await screen.findByRole("dialog", { name: "Add Company Role" });
    const focusable = Array.from(dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])'));
    const first = focusable[0]; const last = focusable[focusable.length - 1];
    expect(first).toHaveFocus();

    last.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(first).toHaveFocus();

    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(last).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Add Company Role" })).not.toBeInTheDocument();
    expect(launcher).toHaveFocus();
  });

  it("labels every Product table cell so narrow viewports stay readable when columns collapse", async () => {
    renderApp("/app/products");
    const table = await screen.findByRole("table");
    const headers = within(table).getAllByRole("columnheader").map((header) => header.textContent?.trim() ?? "");
    const cells = within(table).getAllByRole("row").slice(1).flatMap((row) => within(row).getAllByRole("cell"));
    // The responsive table swaps headers for per-cell data-label captions, so a data cell without
    // one becomes unreadable once the columns stack.
    for (const cell of cells) {
      const label = cell.getAttribute("data-label");
      if (cell.classList.contains("row-actions")) continue;
      expect(label).toBeTruthy();
      expect(headers).toContain(label);
    }
  });

  it("supports policy and barcode editors plus safe revision/conflict feedback", async () => {
    renderApp(`/app/products/${IDs.product}`, catalogService({ mutationError: "barcode_conflict" }));
    fireEvent.click(await screen.findByRole("button", { name: "Manage" }));
    const addBarcode = screen.getByRole("button", { name: "Add Barcode" }); addBarcode.focus(); fireEvent.click(addBarcode);
    const dialog = screen.getByRole("dialog", { name: "Add Pack Barcode" });
    expect(dialog).toHaveTextContent("cannot be reassigned or edited");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Save" })).toBeEnabled());
    fireEvent.change(within(dialog).getByRole("textbox", { name: /Barcode value/ }), { target: { value: "8901234567890" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(/barcode/i);
    expect(alert).not.toHaveTextContent("raw backend detail");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Add Pack Barcode" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add Barcode" })).toHaveFocus();
  });

  it("supports lifecycle and hides all mutations from pharmacist and cashier", async () => {
    const owner = renderApp(`/app/products/${IDs.product}`); fireEvent.click(await screen.findByRole("button", { name: "Archive Product" })); expect(screen.getByRole("dialog", { name: "Archive record" })).toBeInTheDocument(); expect(screen.getByRole("button", { name: "Archive record" })).toBeDisabled(); owner.unmount();
    renderApp(`/app/products/${IDs.product}`, catalogService({ role: "cashier" })); await screen.findByRole("heading", { name: "Crocin 500 mg Tablet" }); expect(screen.queryByRole("button", { name: /Archive Product|Add Pack|Add Company Role/ })).not.toBeInTheDocument(); expect(screen.getByRole("button", { name: "Manage" })).toBeInTheDocument();
  });

  it("uses exact integer quantity conversion without binary floating point", () => {
    expect(quantityToAtoms("12.345", 3)).toBe(12345); expect(atomsToQuantity(123450, 3)).toBe("123.45"); expect(quantityToAtoms("1.0001", 3)).toBeNull(); expect(quantityToAtoms("0", 0)).toBeNull();
  });
});
