import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PartyAddress, PartyDetail, PartyRole, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { panInsideGstin, previewNormalizedGstin } from "../parties/partyApi";

const IDs = {
  party: "01997400-0000-7000-8000-000000000001",
  other: "01997400-0000-7000-8000-000000000002",
  role: "01997400-0000-7000-8000-000000000010",
  address: "01997400-0000-7000-8000-000000000020",
  maharashtra: "01997300-0000-7000-8000-000000000027",
  karnataka: "01997300-0000-7000-8000-000000000029",
  user: "01900000-0000-7000-8000-000000000001"
};
const GSTIN = "27AAPFU0939F1ZV";
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const STATES = [
  { id: IDs.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } },
  { id: IDs.karnataka, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "29", displayName: "Karnataka" } }
];

function role(overrides: Partial<PartyRole> = {}): PartyRole {
  return { id: IDs.role, partyId: IDs.party, role: "supplier", revision: 1, status: "active", ...stamp, ...overrides };
}
function address(overrides: Partial<PartyAddress> = {}): PartyAddress {
  return { id: IDs.address, partyId: IDs.party, addressRole: "billing", line1: "12 Market Road", line2: null, city: "Pune", stateId: IDs.maharashtra, postalCode: "411001", countryCode: "IN", isPrimary: true, revision: 1, status: "active", ...stamp, ...overrides };
}
function party(overrides: Partial<PartyDetail> = {}): PartyDetail {
  return {
    id: IDs.party, displayName: "Sharma Medicals", legalName: "Sharma Medical And General Stores Private Limited",
    normalizedSearchName: "sharma medicals", gstRegistrationStatus: "registered", gstin: GSTIN, normalizedGstin: GSTIN,
    pan: null, normalizedPan: null, placeOfSupplyStateId: IDs.maharashtra, primaryPhone: "+919820012345",
    primaryEmail: "sales@sharma-medicals.co.in", drugLicenceNumber: "20B-1234 / 21B-5678", drugLicenceValidUpto: "2028-03-31",
    revision: 1, status: "active", ...stamp, roles: [role()], addresses: [address()], ...overrides
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  parties?: PartyDetail[];
  candidates?: Array<{ candidateId: string; score: number; reasonCodes: string[]; explanation: string }>;
  createError?: { code: string; status: number };
  updateError?: { code: string; status: number; extra?: Record<string, unknown> };
  failList?: number;
};

/** A stateful Store Service double for the party master. */
function partyService(options: Options = {}) {
  const actor = options.role ?? "owner_admin";
  const state = {
    parties: (options.parties ?? [party()]).map((record) => ({ ...record })),
    remainingListFailures: options.failList ?? 0,
    created: [] as unknown[],
    duplicateProbes: 0
  };
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: actor, displayName: "Party User", role: actor, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/reference/state-codes") return response(STATES);
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return response([]);

    if (url.pathname === "/api/v1/parties/duplicate-candidates") {
      state.duplicateProbes += 1;
      return response(options.candidates ?? []);
    }
    if (url.pathname === "/api/v1/parties" && method === "GET") {
      if (state.remainingListFailures > 0) { state.remainingListFailures -= 1; return failure("internal_error", 500); }
      const search = (url.searchParams.get("search") ?? "").toLowerCase();
      const status = url.searchParams.get("status") ?? "active";
      return response(state.parties.filter((record) =>
        (status === "all" || record.status === status)
        && (!search || `${record.displayName} ${record.normalizedGstin ?? ""}`.toLowerCase().includes(search))));
    }
    if (url.pathname === "/api/v1/parties" && method === "POST") {
      if (actor !== "owner_admin") return failure("authorization_denied", 403);
      if (options.createError) return failure(options.createError.code, options.createError.status);
      state.created.push(body);
      const created = party({ id: IDs.other, displayName: body.party.displayName, roles: [role({ partyId: IDs.other })], addresses: [] });
      state.parties.push(created);
      return response(created, 201);
    }
    const detail = /^\/api\/v1\/parties\/([^/]+)$/.exec(url.pathname);
    if (detail && method === "GET") {
      const found = state.parties.find((record) => record.id === detail[1]);
      return found ? response(found) : failure("not_found", 404);
    }
    if (detail && method === "PUT") {
      if (actor !== "owner_admin") return failure("authorization_denied", 403);
      if (options.updateError) return failure(options.updateError.code, options.updateError.status, options.updateError.extra);
      const index = state.parties.findIndex((record) => record.id === detail[1]);
      state.parties[index] = { ...state.parties[index], ...body.party, revision: state.parties[index].revision + 1 };
      return response(state.parties[index]);
    }
    if (/\/parties\/[^/]+\/roles$/.test(url.pathname)) {
      if (actor !== "owner_admin") return failure("authorization_denied", 403);
      return response(role(), 201);
    }
    if (/\/parties\/[^/]+\/addresses$/.test(url.pathname)) {
      if (actor !== "owner_admin") return failure("authorization_denied", 403);
      const index = state.parties.findIndex((record) => url.pathname.includes(record.id));
      const added = address({ id: `${IDs.address}-b`, line1: body.line1, isPrimary: body.isPrimary, stateId: body.stateId ?? null });
      state.parties[index] = { ...state.parties[index], addresses: [...state.parties[index].addresses, added] };
      return response(added, 201);
    }
    if (/\/(parties|party-roles|party-addresses)\/[^/]+\/(archive|restore)$/.test(url.pathname)) {
      if (actor !== "owner_admin") return failure("authorization_denied", 403);
      if (url.pathname.startsWith("/api/v1/parties/")) {
        const id = url.pathname.split("/")[4];
        const index = state.parties.findIndex((record) => record.id === id);
        const archiving = url.pathname.endsWith("/archive");
        // The real service refuses to archive while an active role remains.
        if (archiving && state.parties[index].roles.some((item) => item.status === "active")) {
          return failure("archived_conflict", 409);
        }
        state.parties[index] = { ...state.parties[index], status: archiving ? "archived" : "active", revision: state.parties[index].revision + 1, archiveReason: archiving ? body.reason : null };
        return response(state.parties[index]);
      }
      const archiving = url.pathname.endsWith("/archive");
      const index = state.parties.findIndex((record) => record.roles.some((item) => item.id === url.pathname.split("/")[4]));
      if (index >= 0) {
        state.parties[index] = { ...state.parties[index], roles: state.parties[index].roles.map((item) => ({ ...item, status: archiving ? "archived" : "active", revision: item.revision + 1 })) };
      }
      return response(role({ status: archiving ? "archived" : "active", revision: 2, archiveReason: archiving ? body.reason : null, archivedAtUtc: archiving ? "2026-02-01T00:00:00Z" : null }));
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  return { state, fetchMock };
}

function renderApp(path: string, service = partyService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Party and supplier identity UI", () => {
  it("lists suppliers with their registration and place of supply", async () => {
    renderApp("/app/parties");
    expect(await screen.findByRole("heading", { name: "Suppliers" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^Suppliers/ })).toHaveClass("nav-item--active");
    expect(await screen.findByRole("cell", { name: GSTIN })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Registered" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "27 · Maharashtra" })).toBeInTheDocument();
    // The page states plainly that a supplier carries no money.
    expect(screen.getByText(/no balance, credit limit, or outstanding amount/i)).toBeInTheDocument();
  });

  it("separates a failed list from an empty one and never shows backend detail", async () => {
    renderApp("/app/parties", partyService({ failList: 1 }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be loaded");
    expect(alert).not.toHaveTextContent("raw backend detail");
    expect(screen.queryByRole("heading", { name: "No suppliers yet" })).not.toBeInTheDocument();
    fireEvent.click(within(alert).getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("cell", { name: GSTIN })).toBeInTheDocument();

    cleanup();
    renderApp("/app/parties", partyService({ parties: [] }));
    expect(await screen.findByRole("heading", { name: "No suppliers yet" })).toBeInTheDocument();
  });

  it("shows the GSTIN and its embedded PAN before creating, and only asks for a State when registered", async () => {
    const app = renderApp("/app/parties/new");
    expect(await screen.findByRole("heading", { name: "Add Supplier" })).toBeInTheDocument();
    // An unrecorded registration asks for no GSTIN and no State.
    expect(screen.queryByLabelText(/^GSTIN/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/Place of supply/)).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Supplier name *"), { target: { value: "Sharma Medicals" } });
    fireEvent.change(screen.getByLabelText("GST registration"), { target: { value: "registered" } });
    fireEvent.change(await screen.findByLabelText("GSTIN *"), { target: { value: "27 aapfu 0939 f1zv" } });
    // The preview shows exactly what the service will compare, including the derived PAN.
    expect(await screen.findByText(GSTIN)).toBeInTheDocument();
    expect(screen.getByText(/carries PAN AAPFU0939F/)).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Place of supply (State) *"), { target: { value: IDs.maharashtra } });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Supplier" }));
    await waitFor(() => expect(app.state.created).toHaveLength(1));
    const sent = app.state.created[0] as { party: Record<string, unknown>; roles: Array<{ role: string }> };
    expect(sent.party.gstin).toBe("27 aapfu 0939 f1zv");
    expect(sent.party.placeOfSupplyStateId).toBe(IDs.maharashtra);
    expect(sent.roles).toEqual([{ role: "supplier" }]);
    // A duplicate check ran before the create, and no Store identity was ever sent.
    expect(app.state.duplicateProbes).toBe(1);
    expect(sent.party).not.toHaveProperty("storeId");
  });

  it("requires a State and a well-formed GSTIN before the request is sent", async () => {
    const app = renderApp("/app/parties/new");
    fireEvent.change(await screen.findByLabelText("Supplier name *"), { target: { value: "Sharma Medicals" } });
    fireEvent.change(screen.getByLabelText("GST registration"), { target: { value: "registered" } });
    fireEvent.change(await screen.findByLabelText("GSTIN *"), { target: { value: "27AAPFU" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Supplier" }));
    expect(await screen.findByText("A GSTIN is 15 characters.")).toBeInTheDocument();
    expect(screen.getByText("Select the State this registration belongs to.")).toBeInTheDocument();
    expect(app.state.created).toHaveLength(0);
    expect(app.state.duplicateProbes).toBe(0);
  });

  it("warns about a likely duplicate but still allows a legitimate creation", async () => {
    const app = renderApp("/app/parties/new", partyService({
      candidates: [{ candidateId: IDs.party, score: 125, reasonCodes: ["gstin_match", "name_match"], explanation: "gstin_match, name_match" }]
    }));
    fireEvent.change(await screen.findByLabelText("Supplier name *"), { target: { value: "Sharma Medicals" } });
    fireEvent.click(screen.getByRole("button", { name: "Review & Create Supplier" }));

    const warning = await screen.findByRole("alert");
    expect(warning).toHaveTextContent("A similar supplier may already exist");
    expect(warning).toHaveTextContent("same GSTIN, same name");
    expect(await within(warning).findByRole("link", { name: "Sharma Medicals" })).toHaveAttribute("href", `/app/parties/${IDs.party}`);
    expect(app.state.created).toHaveLength(0);

    fireEvent.click(within(warning).getByRole("button", { name: "Continue with a new supplier" }));
    fireEvent.click(screen.getByRole("button", { name: "Create Supplier Anyway" }));
    await waitFor(() => expect(app.state.created).toHaveLength(1));
  });

  it("shows a supplier's identity, roles, and addresses without any money field", async () => {
    renderApp(`/app/parties/${IDs.party}`);
    expect(await screen.findByRole("heading", { name: "Sharma Medicals" })).toBeInTheDocument();
    expect(screen.getByText(GSTIN)).toBeInTheDocument();
    expect(screen.getByText("20B-1234 / 21B-5678")).toBeInTheDocument();
    expect(screen.getByText("Valid to 2028-03-31")).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "Supplier" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "12 Market Road, Pune, 411001" })).toBeInTheDocument();
    expect(screen.getByText(/carries no balance or credit limit/i)).toBeInTheDocument();
    // Nothing in the page offers an amount to record.
    expect(screen.queryByText(/credit limit:/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/opening balance/i)).not.toBeInTheDocument();
  });

  it("refuses to archive while an active role remains, then archives once it is cleared", async () => {
    renderApp(`/app/parties/${IDs.party}`);
    fireEvent.click(await screen.findByRole("button", { name: "Archive supplier" }));
    const dialog = await screen.findByRole("dialog", { name: /Archive Sharma Medicals/ });
    fireEvent.change(within(dialog).getByLabelText("Reason *"), { target: { value: "Closed down" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("archive status");

    // Archive the role first, exactly as the Store Service requires.
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    const roleRow = screen.getByRole("cell", { name: "Supplier" }).closest("tr")!;
    fireEvent.click(within(roleRow).getByRole("button", { name: "Archive" }));
    const roleDialog = await screen.findByRole("dialog", { name: /Archive supplier role/ });
    fireEvent.change(within(roleDialog).getByLabelText("Reason *"), { target: { value: "No longer supplies us" } });
    fireEvent.click(within(roleDialog).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Archive supplier" }));
    const retry = await screen.findByRole("dialog", { name: /Archive Sharma Medicals/ });
    fireEvent.change(within(retry).getByLabelText("Reason *"), { target: { value: "Closed down" } });
    fireEvent.click(within(retry).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    // The party itself is archived now, so the only lifecycle action left is to restore it.
    expect(await screen.findByRole("button", { name: "Restore supplier" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
  });

  it("requires a reason before any lifecycle change", async () => {
    const app = renderApp(`/app/parties/${IDs.party}`);
    fireEvent.click(await screen.findByRole("button", { name: "Archive supplier" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("reason is required");
    expect(app.fetchMock.mock.calls.filter(([input]) => String(input).includes("/archive"))).toHaveLength(0);
  });

  it("adds an address through the dialog", async () => {
    const app = renderApp(`/app/parties/${IDs.party}`);
    fireEvent.click(await screen.findByRole("button", { name: "Add address" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Address" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Address" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("first line");

    fireEvent.change(within(dialog).getByLabelText("Address line 1 *"), { target: { value: "Warehouse 4" } });
    fireEvent.change(within(dialog).getByLabelText("Purpose"), { target: { value: "shipping" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add Address" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    const sent = JSON.parse(String(app.fetchMock.mock.calls.find(([input]) => String(input).endsWith("/addresses"))?.[1]?.body));
    expect(sent).toMatchObject({ addressRole: "shipping", line1: "Warehouse 4", countryCode: "IN" });
  });

  it("recovers from a revision conflict by reloading the latest record", async () => {
    renderApp(`/app/parties/${IDs.party}/edit`, partyService({
      updateError: { code: "revision_conflict", status: 409, extra: { expectedRevision: 1, currentRevision: 2 } }
    }));
    fireEvent.change(await screen.findByLabelText("Supplier name *"), { target: { value: "Sharma Medicals & Sons" } });
    fireEvent.click(screen.getByRole("button", { name: "Save Supplier" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("changed after you opened it");
    expect(alert).not.toHaveTextContent("raw backend detail");
    fireEvent.click(within(alert).getByRole("button", { name: "Reload latest" }));
    await waitFor(() => expect((screen.getByLabelText("Supplier name *") as HTMLInputElement).value).toBe("Sharma Medicals"));
  });

  it("gives a read-only role every view and no control", async () => {
    renderApp("/app/parties", partyService({ role: "cashier" }));
    expect(await screen.findByRole("cell", { name: GSTIN })).toBeInTheDocument();
    expect(screen.getByText("Read-only access")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Add Supplier" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();

    cleanup();
    renderApp(`/app/parties/${IDs.party}`, partyService({ role: "cashier" }));
    expect(await screen.findByRole("heading", { name: "Sharma Medicals" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Archive supplier" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add address" })).not.toBeInTheDocument();
  });

  it("adds States to Reference Data using the shared editor", async () => {
    renderApp("/app/reference/states", partyService());
    expect(await screen.findByRole("heading", { name: "States" })).toBeInTheDocument();
    expect(await screen.findByRole("cell", { name: "Maharashtra" })).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "27" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add State" })).toBeInTheDocument();
  });

  it("normalises a GSTIN preview exactly as the service does", () => {
    expect(previewNormalizedGstin(" 27 aapfu 0939 f1zv ")).toBe(GSTIN);
    expect(panInsideGstin(GSTIN)).toBe("AAPFU0939F");
    expect(panInsideGstin("27AAPFU")).toBeNull();
  });
});
