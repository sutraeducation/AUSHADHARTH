import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProductDetail, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { unclassifiedRegulatory } from "./regulatoryDouble";

const IDs = {
  product: "01997b00-0000-7000-8000-000000000001",
  tablet: "01997b00-0000-7000-8000-000000000002",
  strip: "01997b00-0000-7000-8000-000000000003",
  store: "01997b00-0000-7000-8000-000000000004",
  pack: "01997b00-0000-7000-8000-000000000005",
  form: "01997b00-0000-7000-8000-000000000006",
  formulation: "01997b00-0000-7000-8000-000000000010",
  archivedFormulation: "01997b00-0000-7000-8000-000000000011",
  version: "01997b00-0000-7000-8000-000000000020",
  user: "01997b00-0000-7000-8000-000000000030"
};

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const FORMULATIONS = [
  { id: IDs.formulation, kind: "controlled-formulations", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", formulationCode: "PARA-500-TAB", displayName: "Paracetamol 500 mg Tablet", dosageFormId: null, strengthText: "500 mg", verificationState: "verified", sourceNote: null } },
  { id: IDs.archivedFormulation, kind: "controlled-formulations", revision: 2, status: "archived", ...stamp, attributes: { jurisdiction: "IN", formulationCode: "WITHDRAWN", displayName: "Withdrawn formulation", dosageFormId: null, strengthText: null, verificationState: "rejected", sourceNote: null } }
];

const CEILING = {
  priceControlVersionId: IDs.version,
  effectiveFrom: "2026-04-01",
  effectiveTo: null,
  ceilingPricePaise: 109,
  ceilingBasis: "per_base_unit",
  ceilingBasisUnitId: IDs.tablet,
  notificationReference: "S.O. 2222(E)"
};

function product(overrides: Partial<ProductDetail> = {}): ProductDetail {
  return {
    id: IDs.product, productKind: "medicine", brandId: null, dosageFormId: IDs.form,
    baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Paracetamol 500 mg Tablet",
    formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 15, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: IDs.store, displayLabel: "Strip of 15", revision: 1, status: "active", ...stamp }],
    ...overrides
  };
}

function control(overrides: Record<string, unknown> = {}) {
  return {
    productId: IDs.product, revision: 1, priceControlStatus: "unknown",
    controlledFormulationId: null, asOf: "2026-09-14", applicableCeiling: null,
    comparability: null, resolved: false, ...overrides
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null }, status);
}

type Options = {
  role?: UserRole;
  initial?: Record<string, unknown>;
  failControl?: boolean;
  saveError?: { code: string; status: number };
  productKind?: ProductDetail["productKind"];
};

/**
 * A Store Service double that resolves the ceiling the way the real service does: from the assigned
 * formulation and the date, never from anything stored on the Product.
 */
function priceService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = { current: control(options.initial ?? {}), saved: [] as Record<string, unknown>[] };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Price User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/catalog/context") return response({ storeId: IDs.store });
    if (url.pathname === "/api/v1/reference/controlled-formulations") return response(FORMULATIONS);
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (/\/price-control$/.test(url.pathname)) {
      if (method === "GET") {
        return options.failControl ? failure("internal_error", 500) : response(state.current);
      }
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      if (options.saveError) return failure(options.saveError.code, options.saveError.status);
      state.saved.push(body);
      const controlled = body.priceControlStatus === "controlled";
      state.current = control({
        revision: (state.current.revision as number) + 1,
        priceControlStatus: body.priceControlStatus,
        controlledFormulationId: body.controlledFormulationId,
        // The ceiling follows the formulation, exactly as the service resolves it.
        applicableCeiling: controlled && body.controlledFormulationId === IDs.formulation ? CEILING : null,
        comparability: controlled && body.controlledFormulationId === IDs.formulation ? "comparable" : null,
        resolved: controlled && body.controlledFormulationId === IDs.formulation
      });
      return response(state.current);
    }
    if (/\/tax-classification$/.test(url.pathname)) {
      return response({ productId: IDs.product, revision: 1, hsnCodeId: null, taxCategoryId: null, complete: false, asOf: "2026-09-14", applicableRate: null });
    }
    if (url.pathname === "/api/v1/products") return response([product(options.productKind ? { productKind: options.productKind } : {})]);
    // Phase 1M-A: every product page reads its Drugs Rules position.
    if (/^\/api\/v1\/products\/[^/]+\/regulatory$/.test(url.pathname)) return response(unclassifiedRegulatory(IDs.product, options.productKind ?? "medicine"));
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return response(product(options.productKind ? { productKind: options.productKind } : {}));
    if (/\/packs\/[^/]+\/(batches|barcodes)$/.test(url.pathname)) return response([]);
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) return response(null, 204);
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock };
}

function renderApp(path: string, service = priceService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

const panel = () => screen.getByRole("region", { name: "Price Control" });

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Product price control UI", () => {
  it("says a product is unassessed rather than implying it is uncontrolled", async () => {
    renderApp(`/app/products/${IDs.product}`);
    expect(await screen.findByRole("heading", { name: "Price Control" })).toBeInTheDocument();
    expect(await within(panel()).findByText("Not assessed yet")).toBeInTheDocument();
    expect(within(panel()).getByText("Nobody has assessed this product yet")).toBeInTheDocument();
    expect(within(panel()).getByText(/not the same as saying it is uncontrolled/)).toBeInTheDocument();
    // And it never claims a ceiling it has not resolved.
    expect(within(panel()).queryByText(/Ceiling price/)).not.toBeInTheDocument();
  });

  it("assigns a formulation and shows the resolved ceiling with the date it belongs to", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Price Control" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Price Control" });

    fireEvent.change(within(dialog).getByLabelText("Applicability"), { target: { value: "controlled" } });
    fireEvent.change(await within(dialog).findByLabelText(/^Controlled formulation/), { target: { value: IDs.formulation } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Price Control" }));

    await waitFor(() => expect(app.state.saved).toHaveLength(1));
    expect(app.state.saved[0]).toMatchObject({
      expectedRevision: 1,
      priceControlStatus: "controlled",
      controlledFormulationId: IDs.formulation
    });
    // No price is ever submitted from the browser: the ceiling is the server's to resolve.
    expect(app.state.saved[0]).not.toHaveProperty("ceilingPricePaise");
    expect(app.state.saved[0]).not.toHaveProperty("applicableCeiling");

    expect(await within(panel()).findByText("Price-controlled")).toBeInTheDocument();
    expect(within(panel()).getByText("1.09 per base unit")).toBeInTheDocument();
    expect(within(panel()).getByText("S.O. 2222(E)")).toBeInTheDocument();
    expect(within(panel()).getByText(/resolved for 2026-09-14/)).toBeInTheDocument();
  });

  it("keeps the notified ceiling and the printed MRP visibly distinct", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({
      initial: { priceControlStatus: "controlled", controlledFormulationId: IDs.formulation, applicableCeiling: CEILING, comparability: "comparable", resolved: true }
    }));
    await screen.findByRole("heading", { name: "Price Control" });
    expect(await within(panel()).findByText(/A notified ceiling is exclusive of GST/)).toBeInTheDocument();
    expect(within(panel()).getByText(/never compared with each other/)).toBeInTheDocument();
  });

  it("warns when a controlled product has no ceiling in force", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({
      initial: { priceControlStatus: "controlled", controlledFormulationId: IDs.formulation, applicableCeiling: null, resolved: false }
    }));
    await screen.findByRole("heading", { name: "Price Control" });
    const alert = await within(panel()).findByRole("alert");
    expect(alert).toHaveTextContent("No ceiling is in force");
    expect(alert).toHaveTextContent(/must refuse rather than treat the product as unconstrained/);
  });

  it("explains an incomparable ceiling instead of converting it", async () => {
    for (const [comparability, expected] of [
      ["incomparable_pack_basis", /quoted per pack/],
      ["incomparable_unit", /different unit/]
    ] as const) {
      renderApp(`/app/products/${IDs.product}`, priceService({
        initial: { priceControlStatus: "controlled", controlledFormulationId: IDs.formulation, applicableCeiling: { ...CEILING, ceilingBasis: "per_pack" }, comparability, resolved: true }
      }));
      await screen.findByRole("heading", { name: "Price Control" });
      const alert = await within(panel()).findByRole("alert");
      expect(alert).toHaveTextContent("cannot be compared with a selling rate");
      expect(alert).toHaveTextContent(expected);
      cleanup();
    }
  });

  it("refuses to save a controlled product with no formulation, before sending anything", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Price Control" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Applicability"), { target: { value: "controlled" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Price Control" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("Select the notified formulation");
    expect(app.state.saved).toHaveLength(0);
    expect(document.activeElement).toHaveAttribute("id", "price-control-formulation");
  });

  it("separates a failed load from a product without price control", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({ failControl: true }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).toHaveTextContent("not the same as a product without price control");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByRole("button", { name: "Edit Price Control" })).not.toBeInTheDocument();
  });

  it("surfaces a safe message when the service refuses the assertion", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({ saveError: { code: "archived_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Price Control" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Applicability"), { target: { value: "not_applicable" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Price Control" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("archive status");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("is read-only for a pharmacist", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({ role: "pharmacist" }));
    expect(await screen.findByRole("heading", { name: "Price Control" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit Price Control" })).not.toBeInTheDocument();
  });

  it("does not put a price-control panel on a product that is not a medicine", async () => {
    renderApp(`/app/products/${IDs.product}`, priceService({ productKind: "general_pharmacy_item" }));
    expect(await screen.findByRole("heading", { name: "Tax Classification" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Price Control" })).not.toBeInTheDocument();
  });
});
