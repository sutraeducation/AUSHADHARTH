import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProductDetail, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { unclassifiedRegulatory } from "./regulatoryDouble";
import { basisPointsToPercentText } from "../products/productApi";

const IDs = {
  product: "01997000-0000-7000-8000-000000000001",
  tablet: "01997000-0000-7000-8000-000000000002",
  strip: "01997000-0000-7000-8000-000000000003",
  store: "01997000-0000-7000-8000-000000000008",
  pack: "01997000-0000-7000-8000-000000000009",
  hsn: "01997500-0000-7000-8000-000000000001",
  archivedHsn: "01997500-0000-7000-8000-000000000002",
  category: "01997500-0000-7000-8000-000000000010",
  rate: "01997500-0000-7000-8000-000000000020",
  user: "01900000-0000-7000-8000-000000000001"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const HSN_CODES = [
  { id: IDs.hsn, kind: "hsn-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", hsnCode: "30049099", description: "Medicaments" } },
  { id: IDs.archivedHsn, kind: "hsn-codes", revision: 2, status: "archived", ...stamp, attributes: { jurisdiction: "IN", hsnCode: "99999999", description: "Withdrawn heading" } }
];
const TAX_CATEGORIES = [
  { id: IDs.category, kind: "tax-categories", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", categoryCode: "gst-12", displayName: "GST 12%", taxTreatment: "taxable" } }
];

function product(overrides: Partial<ProductDetail> = {}): ProductDetail {
  return {
    id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null,
    baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Crocin 500 mg Tablet",
    formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: "CROCIN-10", skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }],
    ...overrides
  };
}

function classification(overrides: Record<string, unknown> = {}) {
  return {
    productId: IDs.product, revision: 1, hsnCodeId: null, taxCategoryId: null,
    complete: false, asOf: "2026-09-13", applicableRate: null, ...overrides
  };
}

const RATE = {
  taxRateVersionId: IDs.rate, effectiveFrom: "2025-01-01", effectiveTo: null,
  cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1200, cessBasisPoints: 0
};

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  initial?: Record<string, unknown>;
  failClassification?: number;
  failReferences?: boolean;
  saveError?: { code: string; status: number; extra?: Record<string, unknown> };
};

/**
 * A Store Service double that keeps classification state and resolves the rate the way the real
 * service does — from the category, never from a value stored on the Product.
 */
function taxService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    current: classification(options.initial ?? {}),
    remainingFailures: options.failClassification ?? 0,
    saved: [] as Record<string, unknown>[]
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Tax User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/catalog/context") return response({ storeId: IDs.store });
    if (url.pathname === "/api/v1/reference/hsn-codes") return options.failReferences ? failure("internal_error", 500) : response(HSN_CODES);
    if (url.pathname === "/api/v1/reference/tax-categories") return options.failReferences ? failure("internal_error", 500) : response(TAX_CATEGORIES);
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (/\/tax-classification$/.test(url.pathname)) {
      if (method === "GET") {
        if (state.remainingFailures > 0) { state.remainingFailures -= 1; return failure("internal_error", 500); }
        return response(state.current);
      }
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      if (options.saveError) return failure(options.saveError.code, options.saveError.status, options.saveError.extra);
      state.saved.push(body);
      state.current = classification({
        revision: (state.current.revision as number) + 1,
        hsnCodeId: body.hsnCodeId,
        taxCategoryId: body.taxCategoryId,
        complete: Boolean(body.hsnCodeId && body.taxCategoryId),
        // The rate follows the category, exactly as the service resolves it.
        applicableRate: body.taxCategoryId ? RATE : null
      });
      return response(state.current);
    }
    if (url.pathname === "/api/v1/products") return response([product()]);
    // Phase 1M-A: every product page reads its Drugs Rules position.
    if (/^\/api\/v1\/products\/[^/]+\/regulatory$/.test(url.pathname)) return response(unclassifiedRegulatory(IDs.product, "general_pharmacy_item"));
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return response(product({ hsnCodeId: state.current.hsnCodeId as string | null, taxCategoryId: state.current.taxCategoryId as string | null }));
    }
    if (/\/packs\/[^/]+\/(batches|barcodes)$/.test(url.pathname)) return response([]);
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) return response(null, 204);
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock };
}

function renderApp(path: string, service = taxService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

const section = () => screen.getByRole("region", { name: "Tax Classification" });

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Product tax classification UI", () => {
  it("shows an honest incomplete state for an unclassified Product", async () => {
    renderApp(`/app/products/${IDs.product}`);
    expect(await screen.findByRole("heading", { name: "Tax Classification" })).toBeInTheDocument();
    expect(await within(section()).findByText("Incomplete")).toBeInTheDocument();
    expect(within(section()).getAllByText("Not classified")).toHaveLength(2);
    expect(within(section()).getByText(/Assign a Tax Category to resolve/)).toBeInTheDocument();
    // The section states where rate authority lives.
    expect(within(section()).getByText(/resolved by date, never stored on the Product/)).toBeInTheDocument();
  });

  it("shows a resolved rate labelled with the date it belongs to", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      initial: { hsnCodeId: IDs.hsn, taxCategoryId: IDs.category, complete: true, applicableRate: RATE }
    }));
    const panel = await screen.findByRole("region", { name: "Tax Classification" });
    expect(await within(panel).findByText("30049099 · Medicaments")).toBeInTheDocument();
    expect(within(panel).getByText("gst-12 · GST 12%")).toBeInTheDocument();
    expect(within(panel).getByText("Complete")).toBeInTheDocument();
    // The rate is always presented with its date, never as Product metadata.
    expect(within(panel).getByText("Rate in force on 2026-09-13")).toBeInTheDocument();
    expect(within(panel).getByText("CGST 6.00%")).toBeInTheDocument();
    expect(within(panel).getByText("IGST 12.00%")).toBeInTheDocument();
    // Zero cess is not displayed as a line.
    expect(within(panel).queryByText(/^Cess/)).not.toBeInTheDocument();
    // And the page never claims the document treatment.
    expect(within(panel).getByText(/decided by the document, from the places of supply/)).toBeInTheDocument();
  });

  it("separates a failed classification load from an unclassified Product", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, taxService({ failClassification: 1 }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).toHaveTextContent("not the same as an unclassified Product");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByText("Incomplete")).not.toBeInTheDocument();
    // An error hides the edit control rather than inviting a write against unknown state.
    expect(screen.queryByRole("button", { name: "Edit Classification" })).not.toBeInTheDocument();

    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await within(section()).findByText("Incomplete")).toBeInTheDocument();
    expect(app.state.saved).toHaveLength(0);
  });

  it("assigns an HSN and Tax Category through the dialog", async () => {
    const app = renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Classification" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Tax Classification" });

    fireEvent.change(within(dialog).getByLabelText("HSN code"), { target: { value: IDs.hsn } });
    fireEvent.change(within(dialog).getByLabelText("Tax Category"), { target: { value: IDs.category } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Classification" }));

    await waitFor(() => expect(app.state.saved).toHaveLength(1));
    expect(app.state.saved[0]).toMatchObject({ expectedRevision: 1, hsnCodeId: IDs.hsn, taxCategoryId: IDs.category });
    // No rate is ever submitted from the browser.
    expect(app.state.saved[0]).not.toHaveProperty("cgstBasisPoints");
    expect(app.state.saved[0]).not.toHaveProperty("applicableRate");
    expect(await within(section()).findByText("Complete")).toBeInTheDocument();
    expect(await within(section()).findByText("CGST 6.00%")).toBeInTheDocument();
  });

  it("clears a classification by selecting nothing", async () => {
    const app = renderApp(`/app/products/${IDs.product}`, taxService({
      initial: { hsnCodeId: IDs.hsn, taxCategoryId: IDs.category, complete: true, applicableRate: RATE }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Classification" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Tax Category"), { target: { value: "" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Classification" }));
    await waitFor(() => expect(app.state.saved).toHaveLength(1));
    expect(app.state.saved[0]).toMatchObject({ hsnCodeId: IDs.hsn, taxCategoryId: null });
  });

  it("offers only active references but keeps an already assigned archived one", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      initial: { hsnCodeId: IDs.archivedHsn, taxCategoryId: null }
    }));
    // The archived assignment resolves to a real label rather than a raw identifier.
    const panel = await screen.findByRole("region", { name: "Tax Classification" });
    expect(await within(panel).findByText("99999999 · Withdrawn heading (archived)")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Edit Classification" }));
    const select = within(await screen.findByRole("dialog")).getByLabelText("HSN code");
    const options = within(select).getAllByRole("option").map((option) => option.textContent);
    expect(options).toContain("30049099 · Medicaments");
    // Retained because it is the current assignment; it would not be offered otherwise.
    expect(options).toContain("99999999 · Withdrawn heading (archived)");
  });

  it("drops an archived reference from the choices when it is not assigned", async () => {
    renderApp(`/app/products/${IDs.product}`);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Classification" }));
    const select = within(await screen.findByRole("dialog")).getByLabelText("HSN code");
    const options = within(select).getAllByRole("option").map((option) => option.textContent);
    expect(options).toContain("30049099 · Medicaments");
    expect(options).not.toContain("99999999 · Withdrawn heading (archived)");
  });

  it("recovers from a revision conflict by reloading the latest classification", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      saveError: { code: "revision_conflict", status: 409, extra: { expectedRevision: 1, currentRevision: 2 } }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Classification" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Tax Category"), { target: { value: IDs.category } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Classification" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("changed after you opened it");
    expect(alert).not.toHaveTextContent("raw backend detail");
    fireEvent.click(within(alert).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("reports an archived reference refusal without leaking backend detail", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      saveError: { code: "archived_conflict", status: 409 }
    }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Classification" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Tax Category"), { target: { value: IDs.category } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Classification" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("archive status");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("surfaces a reference load failure without blocking the classification itself", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      failReferences: true,
      initial: { hsnCodeId: IDs.hsn, taxCategoryId: IDs.category, complete: true, applicableRate: RATE }
    }));
    const panel = await screen.findByRole("region", { name: "Tax Classification" });
    const alert = await within(panel).findByRole("alert");
    expect(alert).toHaveTextContent("names could not be loaded");
    // The rate still renders: it does not depend on the reference labels.
    expect(within(panel).getByText("CGST 6.00%")).toBeInTheDocument();
    expect(within(panel).getAllByText("Name unavailable").length).toBeGreaterThan(0);
  });

  it("gives a read-only role the classification without any control", async () => {
    renderApp(`/app/products/${IDs.product}`, taxService({
      role: "cashier",
      initial: { hsnCodeId: IDs.hsn, taxCategoryId: IDs.category, complete: true, applicableRate: RATE }
    }));
    const panel = await screen.findByRole("region", { name: "Tax Classification" });
    expect(await within(panel).findByText("30049099 · Medicaments")).toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "Edit Classification" })).not.toBeInTheDocument();
  });

  it("converts exact basis points to a percentage without floating point drift", () => {
    expect(basisPointsToPercentText(600)).toBe("6.00");
    expect(basisPointsToPercentText(1250)).toBe("12.50");
    expect(basisPointsToPercentText(0)).toBe("0.00");
    expect(basisPointsToPercentText(5)).toBe("0.05");
    expect(basisPointsToPercentText(10000)).toBe("100.00");
  });
});
