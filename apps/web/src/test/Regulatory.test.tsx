import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProductDetail, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { unclassifiedRegulatory } from "./regulatoryDouble";

/**
 * Phase 1M-A — the Drugs Rules screens, proved without a browser.
 *
 * The attack is a screen that lets absence pass for a finding: an empty checkbox read as "not
 * scheduled", a cashier who can decide what a drug is, a display licence read as a typed one, or
 * one election quietly standing in for the other.
 */

const IDs = {
  product: "01997c00-0000-7000-8000-000000000001",
  tablet: "01997c00-0000-7000-8000-000000000002",
  strip: "01997c00-0000-7000-8000-000000000003",
  store: "01997c00-0000-7000-8000-000000000004",
  pack: "01997c00-0000-7000-8000-000000000005",
  form: "01997c00-0000-7000-8000-000000000006",
  user: "01997c00-0000-7000-8000-000000000030",
  finding: "01997c00-0000-7000-8000-000000000040"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

function product(): ProductDetail {
  return {
    id: IDs.product, productKind: "medicine", brandId: null, dosageFormId: IDs.form,
    baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Schedule H1 Alprazolam 0.5 mg",
    formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: IDs.store, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp }]
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null }, status);
}

type Options = { role?: UserRole; regulatory?: Record<string, unknown>; compliance?: Record<string, unknown> };

function service(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const writes: Array<{ path: string; body: Record<string, unknown> }> = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/catalog/context") return response({ storeId: IDs.store });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (/\/tax-classification$/.test(url.pathname)) return response({ productId: IDs.product, revision: 1, hsnCodeId: null, taxCategoryId: null, complete: false, asOf: "2026-09-21", applicableRate: null });
    if (/\/price-control$/.test(url.pathname)) return response({ productId: IDs.product, revision: 1, priceControlStatus: "unknown", controlledFormulationId: null, asOf: "2026-09-21", applicableCeiling: null, comparability: null, resolved: false });
    if (/\/regulatory$/.test(url.pathname)) return response({ ...unclassifiedRegulatory(IDs.product, "medicine"), ...options.regulatory });
    if (/\/regulatory\/classifications$/.test(url.pathname) && method === "POST") {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      writes.push({ path: url.pathname, body });
      return response({ id: IDs.finding, revision: 1, status: "active", ...body, effectiveTo: body.effectiveTo ?? null, reason: body.reason ?? null, determinedByUserId: IDs.user, createdAtUtc: stamp.createdAtUtc, updatedAtUtc: stamp.updatedAtUtc }, 201);
    }
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return response(product());
    if (/\/packs\/[^/]+\/(batches|barcodes)$/.test(url.pathname)) return response([]);
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) return response(null, 204);
    if (url.pathname === "/api/v1/store/drug-compliance") {
      return response({ complianceLicences: [], recordElections: [], professionals: [], ...options.compliance });
    }
    if (url.pathname === "/api/v1/store/record-elections" && method === "POST") {
      writes.push({ path: url.pathname, body });
      return response({ id: IDs.finding, revision: 1, status: "active", ...body, effectiveTo: null, evidenceReference: body.evidenceReference ?? null, reason: null }, 201);
    }
    // Phase 1M-B: Drug Compliance also lists the prescribers, for the dispensing roles only.
    if (url.pathname === "/api/v1/prescribers") return role === "cashier" ? failure("authorization_denied", 403) : response([]);
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { fetchMock, writes };
}

function renderApp(path: string, double = service()) {
  vi.stubGlobal("fetch", double.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>);
  return double;
}

const panel = () => screen.getByRole("region", { name: "Drugs Rules Classification" });

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Drugs Rules classification on a product", () => {
  it("shows every scheme as unknown, in words, for a product nobody has classified", async () => {
    renderApp(`/app/products/${IDs.product}`);
    await screen.findByTestId("regulatory-gate");
    const section = within(panel());
    // A name that says "Schedule H1" classified nothing.
    expect(section.getAllByText("Unknown — not recorded")).toHaveLength(6);
    expect(section.queryByText("Does not apply")).not.toBeInTheDocument();
    expect(section.getByText("Classification unresolved")).toBeInTheDocument();
    expect(section.getByText("No finding has been recorded for this product.")).toBeInTheDocument();
  });

  it("will not record a finding until applies or does-not-apply is chosen explicitly", async () => {
    const double = renderApp(`/app/products/${IDs.product}`);
    await screen.findByTestId("regulatory-gate");
    fireEvent.click(within(panel()).getByRole("button", { name: "Record Finding" }));
    const dialog = await screen.findByRole("dialog");
    const submit = within(dialog).getByRole("button", { name: "Record Finding" });

    fireEvent.change(within(dialog).getByLabelText("Scheme"), { target: { value: "schedule_h" } });
    fireEvent.change(within(dialog).getByLabelText("In force from"), { target: { value: "2020-01-01" } });
    fireEvent.change(within(dialog).getByLabelText("Authority"), { target: { value: "Drugs Rules, 1945, Schedule H" } });
    // Neither radio is checked by default, so the form cannot be sent.
    for (const radio of within(dialog).getAllByRole("radio")) expect(radio).not.toBeChecked();
    expect(submit).toBeDisabled();

    fireEvent.click(within(dialog).getByLabelText(/Does not apply/));
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() => expect(double.writes).toHaveLength(1));
    expect(double.writes[0].body).toMatchObject({ scheme: "schedule_h", applies: false, effectiveFrom: "2020-01-01", sourceCitation: "Drugs Rules, 1945, Schedule H" });
  });

  it("refuses to send a finding with no authority behind it", async () => {
    renderApp(`/app/products/${IDs.product}`);
    await screen.findByTestId("regulatory-gate");
    fireEvent.click(within(panel()).getByRole("button", { name: "Record Finding" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Scheme"), { target: { value: "schedule_h1" } });
    fireEvent.click(within(dialog).getByLabelText(/Applies/));
    fireEvent.change(within(dialog).getByLabelText("In force from"), { target: { value: "2020-01-01" } });
    fireEvent.change(within(dialog).getByLabelText("Authority"), { target: { value: "x" } });
    expect(within(dialog).getByRole("button", { name: "Record Finding" })).toBeDisabled();
  });

  it("offers a cashier no way to classify, and shows the same position read-only", async () => {
    renderApp(`/app/products/${IDs.product}`, service({ role: "cashier" }));
    await screen.findByTestId("regulatory-gate");
    expect(within(panel()).queryByRole("button", { name: "Record Finding" })).not.toBeInTheDocument();
    expect(within(panel()).getAllByText("Unknown — not recorded")).toHaveLength(6);
  });

  it("names the pending workflow for a classified Schedule X medicine", async () => {
    renderApp(`/app/products/${IDs.product}`, service({
      regulatory: {
        saleGate: "workflow_unavailable",
        saleGateScheme: "schedule_x",
        resolved: [
          { scheme: "schedule_h", answer: "does_not_apply" }, { scheme: "schedule_h1", answer: "does_not_apply" },
          { scheme: "schedule_x", answer: "applies" }, { scheme: "schedule_c", answer: "does_not_apply" },
          { scheme: "schedule_c1", answer: "does_not_apply" }, { scheme: "ndps_purview", answer: "unknown" }
        ],
        classifications: [{ id: IDs.finding, revision: 1, status: "active", scheme: "schedule_x", applies: true, effectiveFrom: "2020-01-01", effectiveTo: null, sourceCitation: "Drugs Rules, 1945, Schedule X", reason: null, determinedByUserId: IDs.user, createdAtUtc: stamp.createdAtUtc, updatedAtUtc: stamp.updatedAtUtc }]
      }
    }));
    await screen.findByTestId("regulatory-gate");
    const gate = within(panel()).getByTestId("regulatory-gate");
    expect(gate).toHaveTextContent("Regulated sale — workflow not yet available");
    expect(gate).toHaveTextContent("Schedule X applies");
    expect(within(panel()).getByText("Drugs Rules, 1945, Schedule X")).toBeInTheDocument();
    // An active open-ended finding can be ended for a change in law, or archived if entered in error.
    expect(within(panel()).getByRole("button", { name: "End" })).toBeInTheDocument();
    expect(within(panel()).getByRole("button", { name: "Archive" })).toBeInTheDocument();
  });
});

describe("Drug Compliance settings", () => {
  it("keeps the two rule 65 elections as separate, independently unrecorded facts", async () => {
    renderApp("/app/settings/drug-compliance");
    await screen.findByRole("region", { name: "Professionals" });
    const prescription = screen.getByTestId("election-rule_65_3_prescription_supply");
    const scheduleC = screen.getByTestId("election-rule_65_4_non_prescription_schedule_c");
    expect(within(prescription).getByText("Not recorded.")).toBeInTheDocument();
    expect(within(scheduleC).getByText("Not recorded.")).toBeInTheDocument();
  });

  it("offers each election only the alternatives its own sub-rule names", async () => {
    const double = renderApp("/app/settings/drug-compliance");
    await screen.findByRole("region", { name: "Professionals" });
    fireEvent.click(within(screen.getByTestId("election-rule_65_4_non_prescription_schedule_c")).getByRole("button", { name: "Record Election" }));
    const dialog = await screen.findByRole("dialog");
    const choices = within(dialog).getAllByRole("radio").map((radio) => (radio as HTMLInputElement).value);
    expect(choices).toEqual(["register", "cash_or_credit_memo_book"]);
    expect(choices).not.toContain("prescription_register");
    fireEvent.click(within(dialog).getByLabelText("Register"));
    fireEvent.change(within(dialog).getByLabelText("In force from"), { target: { value: "2026-01-01" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Record Election" }));
    await waitFor(() => expect(double.writes).toHaveLength(1));
    expect(double.writes[0].body).toMatchObject({ election: "rule_65_4_non_prescription_schedule_c", method: "register" });
  });

  it("does not read a display licence as a typed licence form", async () => {
    renderApp("/app/settings/drug-compliance");
    await screen.findByRole("region", { name: "Professionals" });
    const forms = screen.getByRole("region", { name: "Licence Forms" });
    expect(within(forms).getByText("No licence form is asserted.")).toBeInTheDocument();
  });

  it("shows a cashier the professionals without their registration details or any control", async () => {
    renderApp("/app/settings/drug-compliance", service({
      role: "cashier",
      compliance: { professionals: [{ id: IDs.finding, revision: 1, status: "active", fullName: "Meera Iyer", capacity: "registered_pharmacist", registrationNumber: null, registeringAuthority: null, validFrom: null, validUpto: null, linkedUserId: null }] }
    }));
    await screen.findByRole("region", { name: "Professionals" });
    const people = screen.getByRole("region", { name: "Professionals" });
    expect(within(people).getByText("Meera Iyer")).toBeInTheDocument();
    expect(within(people).queryByRole("columnheader", { name: "Registration" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add Professional" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Record Election" })).not.toBeInTheDocument();
  });

  it("will not add a registered pharmacist without a registration number", async () => {
    renderApp("/app/settings/drug-compliance");
    await screen.findByRole("region", { name: "Professionals" });
    fireEvent.click(screen.getByRole("button", { name: "Add Professional" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("Full name"), { target: { value: "Meera Iyer" } });
    fireEvent.change(within(dialog).getByLabelText("Capacity"), { target: { value: "registered_pharmacist" } });
    expect(within(dialog).getByRole("button", { name: "Add Professional" })).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText("Capacity"), { target: { value: "competent_person" } });
    expect(within(dialog).getByRole("button", { name: "Add Professional" })).toBeEnabled();
  });
});
