import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StoreProfile, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

const IDs = {
  store: "01997000-0000-7000-8000-000000000008",
  maharashtra: "01997300-0000-7000-8000-000000000027",
  karnataka: "01997300-0000-7000-8000-000000000029",
  archived: "01997300-0000-7000-8000-000000000025",
  user: "01900000-0000-7000-8000-000000000001"
};
const GSTIN = "27AAPFU0939F1ZV";
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const STATES = [
  { id: IDs.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } },
  { id: IDs.karnataka, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "29", displayName: "Karnataka" } },
  { id: IDs.archived, kind: "state-codes", revision: 2, status: "archived", ...stamp, attributes: { jurisdiction: "IN", stateCode: "25", displayName: "Daman and Diu (superseded)" } }
];

function profile(overrides: Partial<StoreProfile> = {}): StoreProfile {
  return {
    storeId: IDs.store, displayName: "Care Pharmacy", revision: 1,
    legalName: null, primaryPhone: null, primaryEmail: null,
    gstRegistrationStatus: "unknown", gstin: null, normalizedGstin: null,
    placeOfSupplyStateId: null, taxComplete: false,
    address: null, licences: [],
    rule46sDeclarationApplicability: "unknown", einvoiceApplicability: "unknown", dynamicQrApplicability: "unknown", hsnTurnoverBand: "unknown", hsnTurnoverFinancialYear: null,
    sellerComplete: false,
    missingSellerFacts: [
      { field: "legalName", message: "Record the pharmacy's registered name in Store Profile." },
      { field: "address.line1", message: "Record the pharmacy's address in Store Profile." },
      { field: "licences", message: "Record at least one active drug sale licence in Store Profile." }
    ],
    ...overrides
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = { role?: UserRole; initial?: Partial<StoreProfile>; failProfile?: number; saveError?: { code: string; status: number; extra?: Record<string, unknown> } };

function storeService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = { current: profile(options.initial), remainingFailures: options.failProfile ?? 0, saved: [] as Record<string, unknown>[] };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/reference/state-codes") return response(STATES);
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);
    if (url.pathname === "/api/v1/store/profile" && method === "GET") {
      if (state.remainingFailures > 0) { state.remainingFailures -= 1; return failure("internal_error", 500); }
      return response(state.current);
    }
    if (url.pathname === "/api/v1/store/tax-identity") {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      if (options.saveError) return failure(options.saveError.code, options.saveError.status, options.saveError.extra);
      state.saved.push(body);
      state.current = profile({
        ...state.current,
        revision: state.current.revision + 1,
        gstRegistrationStatus: body.gstRegistrationStatus,
        gstin: body.gstin, normalizedGstin: body.gstin ? String(body.gstin).replace(/\s+/g, "").toUpperCase() : null,
        placeOfSupplyStateId: body.placeOfSupplyStateId,
        taxComplete: Boolean(body.placeOfSupplyStateId)
      });
      return response({
        storeId: state.current.storeId, displayName: state.current.displayName,
        revision: state.current.revision, gstRegistrationStatus: state.current.gstRegistrationStatus,
        gstin: state.current.gstin, normalizedGstin: state.current.normalizedGstin,
        placeOfSupplyStateId: state.current.placeOfSupplyStateId, complete: state.current.taxComplete
      });
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock };
}

function renderApp(path: string, service = storeService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Store tax identity UI", () => {
  it("adds Store Profile navigation and explains why the State matters", async () => {
    renderApp("/app/settings/store");
    expect(await screen.findByRole("heading", { name: "Store Profile" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^Store Profile/ })).toHaveClass("nav-item--active");
    expect(await screen.findByText("Place of supply not recorded")).toBeInTheDocument();
    // The consequence is stated before the operator hits it at the counter.
    expect(screen.getByText(/posting one will be refused/i)).toBeInTheDocument();
    // Two sections can say "Not recorded" now — the operating address State and the GST place
    // of supply — so this names the one the test is about.
    const gst = screen.getByRole("region", { name: "GST" });
    expect(within(gst).getByText("Not recorded")).toBeInTheDocument();
  });

  it("shows a completed profile with its resolved State label", async () => {
    renderApp("/app/settings/store", storeService({
      initial: { gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN, placeOfSupplyStateId: IDs.maharashtra, taxComplete: true }
    }));
    expect(await screen.findByText(GSTIN)).toBeInTheDocument();
    expect(screen.getByText("27 · Maharashtra")).toBeInTheDocument();
    expect(screen.getByText("Registered")).toBeInTheDocument();
    expect(screen.getByText(/can determine their tax treatment/i)).toBeInTheDocument();
    expect(screen.queryByText("Place of supply not recorded")).not.toBeInTheDocument();
  });

  it("separates a failed load from an unrecorded profile and retries", async () => {
    renderApp("/app/settings/store", storeService({ failProfile: 1 }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).toHaveTextContent("not the same as an unrecorded profile");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByText("Place of supply not recorded")).not.toBeInTheDocument();
    // No mutation path against unknown state.
    expect(screen.queryByRole("button", { name: "Edit Tax Identity" })).not.toBeInTheDocument();

    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Place of supply not recorded")).toBeInTheDocument();
  });

  it("records an unregistered store that still has a place of supply", async () => {
    const app = renderApp("/app/settings/store");
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Store Tax Identity" });
    fireEvent.change(within(dialog).getByLabelText("GST registration"), { target: { value: "unregistered" } });
    // No GSTIN field appears for an unregistered store.
    expect(within(dialog).queryByLabelText(/^GSTIN/)).not.toBeInTheDocument();
    fireEvent.change(within(dialog).getByLabelText(/Place of supply/), { target: { value: IDs.maharashtra } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));

    await waitFor(() => expect(app.state.saved).toHaveLength(1));
    expect(app.state.saved[0]).toMatchObject({ expectedRevision: 1, gstRegistrationStatus: "unregistered", gstin: null, placeOfSupplyStateId: IDs.maharashtra });
    expect(await screen.findByText(/can determine their tax treatment/i)).toBeInTheDocument();
  });

  it("previews the normalised GSTIN and requires a State before sending", async () => {
    const app = renderApp("/app/settings/store");
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("GST registration"), { target: { value: "registered" } });
    fireEvent.change(await within(dialog).findByLabelText("GSTIN *"), { target: { value: "27 aapfu 0939 f1zv" } });
    expect(within(dialog).getByText(`Will be recorded as ${GSTIN}`)).toBeInTheDocument();

    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Select the State");
    expect(app.state.saved).toHaveLength(0);

    fireEvent.change(within(dialog).getByLabelText(/Place of supply/), { target: { value: IDs.maharashtra } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));
    await waitFor(() => expect(app.state.saved).toHaveLength(1));
  });

  it("rejects a short GSTIN before the request is sent", async () => {
    const app = renderApp("/app/settings/store");
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText("GST registration"), { target: { value: "registered" } });
    fireEvent.change(await within(dialog).findByLabelText("GSTIN *"), { target: { value: "27AAPFU" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("15 characters");
    expect(app.state.saved).toHaveLength(0);
  });

  it("offers only active States but keeps one already assigned and since archived", async () => {
    renderApp("/app/settings/store", storeService({ initial: { placeOfSupplyStateId: IDs.archived, taxComplete: true } }));
    expect(await screen.findByText("25 · Daman and Diu (superseded) (archived)")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Edit Tax Identity" }));
    const select = within(await screen.findByRole("dialog")).getByLabelText(/Place of supply/);
    const options = within(select).getAllByRole("option").map((option) => option.textContent);
    expect(options).toContain("27 · Maharashtra");
    expect(options).toContain("25 · Daman and Diu (superseded) (archived)");
  });

  it("drops an archived State from the choices when it is not assigned", async () => {
    renderApp("/app/settings/store");
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const select = within(await screen.findByRole("dialog")).getByLabelText(/Place of supply/);
    const options = within(select).getAllByRole("option").map((option) => option.textContent);
    expect(options).toContain("27 · Maharashtra");
    expect(options).not.toContain("25 · Daman and Diu (superseded) (archived)");
  });

  it("surfaces a GSTIN and State disagreement without leaking backend detail", async () => {
    renderApp("/app/settings/store", storeService({ saveError: { code: "store_tax_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Place of supply/), { target: { value: IDs.karnataka } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("does not agree with the selected State");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("recovers from a revision conflict by reloading the latest profile", async () => {
    renderApp("/app/settings/store", storeService({ saveError: { code: "revision_conflict", status: 409, extra: { expectedRevision: 1, currentRevision: 2 } } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Tax Identity" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Place of supply/), { target: { value: IDs.maharashtra } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Tax Identity" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("changed after you opened it");
    expect(alert).not.toHaveTextContent("raw backend detail");
    fireEvent.click(within(alert).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("gives a read-only role the profile without any control", async () => {
    renderApp("/app/settings/store", storeService({
      role: "cashier",
      initial: { gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN, placeOfSupplyStateId: IDs.maharashtra, taxComplete: true }
    }));
    expect(await screen.findByText(GSTIN)).toBeInTheDocument();
    expect(screen.getByText("Read-only access")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit Tax Identity" })).not.toBeInTheDocument();
  });
});
