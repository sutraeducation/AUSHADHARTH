import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { StoreProfile, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

/**
 * Store Profile — the legal seller, from the operator's side.
 *
 * The thing worth testing here is not that a form saves. It is that a pharmacy which cannot lawfully
 * issue a bill is told so, in words a counter operator can act on, before they try to sell something
 * — and that the three particulars the Drugs Rules require are each named individually rather than
 * hidden behind "profile incomplete".
 */

const IDs = {
  store: "01997000-0000-7000-8000-000000000008",
  maharashtra: "01997300-0000-7000-8000-000000000027",
  user: "01997000-0000-7000-8000-000000000100",
  licence: "01997000-0000-7000-8000-00000000ac01"
};
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const STATES = [
  { id: IDs.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } }
];

const MISSING = {
  legalName: { field: "legalName", message: "Record the pharmacy's registered name in Store Profile." },
  address: { field: "address.line1", message: "Record the pharmacy's address in Store Profile." },
  licences: { field: "licences", message: "Record at least one active drug sale licence in Store Profile." }
};

function blank(overrides: Partial<StoreProfile> = {}): StoreProfile {
  return {
    storeId: IDs.store, revision: 1, displayName: "Care Pharmacy",
    legalName: null, primaryPhone: null, primaryEmail: null,
    gstRegistrationStatus: "unknown", gstin: null, normalizedGstin: null,
    placeOfSupplyStateId: null, taxComplete: false,
    address: null, licences: [],
    rule46sDeclarationApplicability: "unknown", einvoiceApplicability: "unknown", dynamicQrApplicability: "unknown", hsnTurnoverBand: "unknown", hsnTurnoverFinancialYear: null,
    sellerComplete: false,
    missingSellerFacts: [MISSING.legalName, MISSING.address, MISSING.licences],
    ...overrides
  };
}

function complete(overrides: Partial<StoreProfile> = {}): StoreProfile {
  return blank({
    legalName: "Care Pharmacy Private Limited",
    primaryPhone: "02012345678",
    primaryEmail: "counter@example.test",
    address: { id: "01997000-0000-7000-8000-00000000ab01", revision: 1, line1: "12 Market Road", line2: null, city: "Pune", stateId: IDs.maharashtra, postalCode: "411001", countryCode: "IN" },
    licences: [{ id: IDs.licence, revision: 1, status: "active", licenceType: "Form 20", licenceNumber: "MH-20-1234", issuingAuthority: "FDA Maharashtra", validFrom: null, validUpto: "2031-12-31", includeOnRetailMemo: true }],
    sellerComplete: true,
    missingSellerFacts: [],
    ...overrides
  });
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

interface Options {
  role?: UserRole;
  initial?: StoreProfile;
  /** Applied to the next mutating request, whatever it is. */
  saveError?: { code: string; status: number };
}

function storeService(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const state = { current: options.initial ?? blank(), saved: [] as Array<{ path: string; body: Record<string, unknown> }> };
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
    if (url.pathname === "/api/v1/store/profile" && method === "GET") return response(state.current);

    if (url.pathname.startsWith("/api/v1/store/")) {
      if (role !== "owner_admin") return failure("authorization_denied", 403);
      if (options.saveError) return failure(options.saveError.code, options.saveError.status, { currentRevision: 7 });
      state.saved.push({ path: url.pathname, body });

      if (url.pathname === "/api/v1/store/profile") {
        state.current = { ...state.current, revision: state.current.revision + 1, displayName: String(body.displayName), legalName: (body.legalName as string | null) ?? null, primaryPhone: (body.primaryPhone as string | null) ?? null, primaryEmail: (body.primaryEmail as string | null) ?? null };
      }
      if (url.pathname === "/api/v1/store/address") {
        state.current = { ...state.current, address: { id: "01997000-0000-7000-8000-00000000ab01", revision: (state.current.address?.revision ?? 0) + 1, line1: String(body.line1), line2: (body.line2 as string | null) ?? null, city: (body.city as string | null) ?? null, stateId: (body.stateId as string | null) ?? null, postalCode: (body.postalCode as string | null) ?? null, countryCode: "IN" } };
      }
      if (url.pathname === "/api/v1/store/licences") {
        state.current = { ...state.current, licences: [...state.current.licences, { id: IDs.licence, revision: 1, status: "active", licenceType: String(body.licenceType), licenceNumber: String(body.licenceNumber), issuingAuthority: (body.issuingAuthority as string | null) ?? null, validFrom: null, validUpto: null, includeOnRetailMemo: Boolean(body.includeOnRetailMemo) }] };
      }
      if (url.pathname === "/api/v1/store/invoice-compliance") {
        state.current = { ...state.current, revision: state.current.revision + 1, rule46sDeclarationApplicability: body.rule46sDeclarationApplicability, einvoiceApplicability: body.einvoiceApplicability, dynamicQrApplicability: body.dynamicQrApplicability, hsnTurnoverBand: body.hsnTurnoverBand, hsnTurnoverFinancialYear: body.hsnTurnoverFinancialYear };
      }
      if (url.pathname.endsWith("/archive")) {
        state.current = { ...state.current, licences: state.current.licences.map((licence) => ({ ...licence, status: "archived" as const, revision: licence.revision + 1 })), sellerComplete: false, missingSellerFacts: [MISSING.licences] };
      }
      if (url.pathname.endsWith("/restore")) {
        state.current = { ...state.current, licences: state.current.licences.map((licence) => ({ ...licence, status: "active" as const, revision: licence.revision + 1 })), sellerComplete: true, missingSellerFacts: [] };
      }
      // Whether the store can now sell is the service's decision, recomputed from what it holds.
      if (!url.pathname.endsWith("/archive") && !url.pathname.endsWith("/restore")) {
        const missing = [
          state.current.legalName ? null : MISSING.legalName,
          state.current.address ? null : MISSING.address,
          state.current.licences.some((licence) => licence.status === "active") ? null : MISSING.licences
        ].filter(Boolean) as StoreProfile["missingSellerFacts"];
        state.current = { ...state.current, sellerComplete: missing.length === 0, missingSellerFacts: missing };
      }
      return response(state.current, method === "POST" && url.pathname === "/api/v1/store/licences" ? 201 : 200);
    }
    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });
  vi.stubGlobal("fetch", fetchMock);
  return state;
}

function renderApp(state = storeService()) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/app/settings/store"]}>
        <App />
      </MemoryRouter>
    </QueryClientProvider>
  );
  return state;
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Store legal profile", () => {
  it("tells an owner that sales are blocked and names each missing particular", async () => {
    renderApp();
    expect(await screen.findByText("Sales cannot be posted yet")).toBeInTheDocument();
    // Each of the three Drugs Rules particulars, individually actionable.
    expect(screen.getByText(/registered name in Store Profile/i)).toBeInTheDocument();
    expect(screen.getByText(/address in Store Profile/i)).toBeInTheDocument();
    expect(screen.getByText(/drug sale licence in Store Profile/i)).toBeInTheDocument();
    // And the reassurance that nothing else has stopped working.
    expect(screen.getAllByText(/Nothing else in the application is affected/i).length).toBeGreaterThan(0);
  });

  it("says nothing alarming once the three particulars are recorded", async () => {
    renderApp(storeService({ initial: complete() }));
    expect(await screen.findByRole("heading", { name: "Pharmacy Licences" })).toBeInTheDocument();
    expect(screen.queryByText("Sales cannot be posted yet")).not.toBeInTheDocument();
    expect(screen.getByText("Care Pharmacy Private Limited")).toBeInTheDocument();
    expect(screen.getByText("12 Market Road")).toBeInTheDocument();
    expect(screen.getByText("MH-20-1234")).toBeInTheDocument();
  });

  it("keeps the trading name and the registered name as separate facts", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Edit Identity" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Business Identity" });
    fireEvent.change(within(dialog).getByLabelText(/Trading name/), { target: { value: "Care Chemists" } });
    fireEvent.change(within(dialog).getByLabelText("Registered name"), { target: { value: "Care Pharmacy Private Limited" } });
    fireEvent.change(within(dialog).getByLabelText("Phone"), { target: { value: "020 1234 5678" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Identity" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({
      expectedRevision: 1,
      displayName: "Care Chemists",
      legalName: "Care Pharmacy Private Limited",
      primaryPhone: "020 1234 5678"
    });
    // Both survive; neither stands in for the other.
    expect(await screen.findByText("Care Chemists")).toBeInTheDocument();
    expect(screen.getByText("Care Pharmacy Private Limited")).toBeInTheDocument();
  });

  it("records an address the first time without a revision and with one afterwards", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Record Address" }));
    let dialog = await screen.findByRole("dialog", { name: "Record Address" });
    fireEvent.change(within(dialog).getByLabelText(/Address line 1/), { target: { value: "12 Market Road" } });
    fireEvent.change(within(dialog).getByLabelText("Town / city"), { target: { value: "Pune" } });
    fireEvent.change(within(dialog).getByLabelText("PIN code"), { target: { value: "411001" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Address" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body.expectedRevision).toBeUndefined();

    fireEvent.click(await screen.findByRole("button", { name: "Edit Address" }));
    dialog = await screen.findByRole("dialog", { name: "Edit Address" });
    fireEvent.change(within(dialog).getByLabelText(/Address line 1/), { target: { value: "99 New Road" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Address" }));
    await waitFor(() => expect(state.saved).toHaveLength(2));
    expect(state.saved[1].body).toMatchObject({ expectedRevision: 1, line1: "99 New Road" });
  });

  it("will not send an address with no first line", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Record Address" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Address" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/first line of the address/i);
    expect(state.saved).toHaveLength(0);
  });

  it("says plainly that the address State is not reconciled with the GSTIN", async () => {
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Record Address" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/never changed to match your GSTIN/i)).toBeInTheDocument();
  });

  it("adds a licence and keeps the number exactly as it was transcribed", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Add Licence" }));
    const dialog = await screen.findByRole("dialog", { name: "Add Licence" });
    fireEvent.change(within(dialog).getByLabelText(/Licence type/), { target: { value: "Form 20B/21B" } });
    fireEvent.change(within(dialog).getByLabelText(/Licence number/), { target: { value: "20B-1234 / 21B-5678" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ licenceType: "Form 20B/21B", licenceNumber: "20B-1234 / 21B-5678" });
    expect(await screen.findByText("20B-1234 / 21B-5678")).toBeInTheDocument();
  });

  it("will not send a licence with no type or no number", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Add Licence" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/licence type/i);

    fireEvent.change(within(dialog).getByLabelText(/Licence type/), { target: { value: "Form 20" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/licence number/i);
    expect(state.saved).toHaveLength(0);
  });

  it("archives a licence and says the counter has closed", async () => {
    const state = renderApp(storeService({ initial: complete() }));
    const table = await screen.findByRole("table");
    fireEvent.click(within(table).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(await screen.findByText("Sales cannot be posted yet")).toBeInTheDocument();
    expect(screen.getByText("Archived")).toBeInTheDocument();
    expect(screen.getByText(/does not appear on a bill/i)).toBeInTheDocument();
  });

  it("restores an archived licence", async () => {
    const archived = complete({
      licences: [{ id: IDs.licence, revision: 2, status: "archived", licenceType: "Form 20", licenceNumber: "MH-20-1234", issuingAuthority: null, validFrom: null, validUpto: null, includeOnRetailMemo: true }],
      sellerComplete: false,
      missingSellerFacts: [MISSING.licences]
    });
    const state = renderApp(storeService({ initial: archived }));
    const table = await screen.findByRole("table");
    fireEvent.click(within(table).getByRole("button", { name: "Restore" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ expectedRevision: 2 });
    await waitFor(() => expect(screen.queryByText("Sales cannot be posted yet")).not.toBeInTheDocument());
  });

  it("surfaces a duplicate licence without leaking backend detail", async () => {
    renderApp(storeService({ initial: complete(), saveError: { code: "store_licence_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Add Licence" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Licence type/), { target: { value: "Form 20" } });
    fireEvent.change(within(dialog).getByLabelText(/Licence number/), { target: { value: "MH-20-1234" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));

    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent(/already recorded for this pharmacy/i);
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("explains a stale revision rather than overwriting somebody else's edit", async () => {
    renderApp(storeService({ saveError: { code: "revision_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Identity" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Trading name/), { target: { value: "Care Chemists" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Identity" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/changed after you opened it/i);
  });

  it("gives a read-only role the whole profile and no control at all", async () => {
    renderApp(storeService({ role: "pharmacist", initial: complete() }));
    expect(await screen.findByText("Read-only access")).toBeInTheDocument();
    expect(await screen.findByText("Care Pharmacy Private Limited")).toBeInTheDocument();
    expect(screen.getByText("MH-20-1234")).toBeInTheDocument();
    for (const name of ["Edit Identity", "Edit Address", "Record Address", "Add Licence", "Archive", "Edit Tax Identity", "Edit Turnover Facts"]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });

  // Phase 1L-A3 — facts only the pharmacy can supply.
  it("shows unrecorded turnover facts as not recorded rather than guessing them", async () => {
    renderApp(storeService({ initial: complete() }));
    const panel = (await screen.findByRole("heading", { name: "Turnover Facts" })).closest("section")!;
    // Four separate facts, each unrecorded — Rule 46(s), Rule 48(4), Dynamic QR and the HSN band.
    expect(within(panel).getAllByText("Not recorded yet")).toHaveLength(4);
    expect(within(panel).getByText(/only you can supply/i)).toBeInTheDocument();
    // Factual wording only: the screen never certifies anything.
    expect(screen.queryByText(/compliant/i)).not.toBeInTheDocument();
  });

  it("records the Rule 46(s) fact and the HSN band with its financial year", async () => {
    const state = renderApp(storeService({ initial: complete() }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Turnover Facts" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Turnover Facts" });
    fireEvent.change(within(dialog).getByLabelText(/Rule 46\(s\) declaration/), { target: { value: "not_applicable" } });
    fireEvent.change(within(dialog).getByLabelText(/E-invoicing for GST-registered customers/), { target: { value: "not_required" } });
    fireEvent.change(within(dialog).getByLabelText(/Aggregate turnover/), { target: { value: "up_to_5_crore" } });
    fireEvent.change(within(dialog).getByLabelText(/Financial year of invoices/), { target: { value: "2026-27" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Turnover Facts" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0]).toMatchObject({
      path: "/api/v1/store/invoice-compliance",
      body: { expectedRevision: 1, rule46sDeclarationApplicability: "not_applicable", einvoiceApplicability: "not_required", dynamicQrApplicability: "unknown", hsnTurnoverBand: "up_to_5_crore", hsnTurnoverFinancialYear: "2026-27" }
    });
    const panel = (await screen.findByRole("heading", { name: "Turnover Facts" })).closest("section")!;
    expect(await within(panel).findByText("Up to ₹5 crore")).toBeInTheDocument();
    expect(within(panel).getByText("2026-27")).toBeInTheDocument();
  });

  it("records the Rule 46(s) declaration and e-invoicing as two separate answers", async () => {
    const state = renderApp(storeService({ initial: complete({ hsnTurnoverBand: "above_5_crore", hsnTurnoverFinancialYear: "2026-27" }) }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Turnover Facts" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Turnover Facts" });
    // The high-turnover pharmacy whose counter bills carry the declaration without being e-invoices.
    fireEvent.change(within(dialog).getByLabelText(/Rule 46\(s\) declaration/), { target: { value: "applicable" } });
    // Choosing one answer changes nothing about the other.
    expect(within(dialog).getByLabelText(/E-invoicing for GST-registered customers/)).toHaveValue("unknown");
    fireEvent.change(within(dialog).getByLabelText(/E-invoicing for GST-registered customers/), { target: { value: "not_required" } });
    expect(within(dialog).getByText(/It does not stop any sale/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Turnover Facts" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ rule46sDeclarationApplicability: "applicable", einvoiceApplicability: "not_required" });
    const panel = (await screen.findByRole("heading", { name: "Turnover Facts" })).closest("section")!;
    expect(await within(panel).findByText("Applies: recorded on every invoice")).toBeInTheDocument();
    expect(within(panel).getByText("Not required")).toBeInTheDocument();
  });

  it("records the Dynamic QR answer as its own fact and says no QR is generated", async () => {
    const state = renderApp(storeService({ initial: complete() }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Turnover Facts" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit Turnover Facts" });
    const dynamicQr = within(dialog).getByLabelText(/Dynamic QR for unregistered customers/);
    expect(within(dialog).getByText(/does not generate a QR code/)).toBeInTheDocument();
    expect(within(dialog).getByText(/₹500 crore/)).toBeInTheDocument();
    fireEvent.change(dynamicQr, { target: { value: "required" } });
    // Choosing it moves neither of the other two answers.
    expect(within(dialog).getByLabelText(/Rule 46\(s\) declaration/)).toHaveValue("unknown");
    expect(within(dialog).getByLabelText(/E-invoicing for GST-registered customers/)).toHaveValue("unknown");
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Turnover Facts" }));

    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ dynamicQrApplicability: "required", rule46sDeclarationApplicability: "unknown", einvoiceApplicability: "unknown" });
    const panel = (await screen.findByRole("heading", { name: "Turnover Facts" })).closest("section")!;
    expect(await within(panel).findByText("Required: payment details recorded on each invoice")).toBeInTheDocument();
  });

  it("will not send a turnover band without the financial year it governs", async () => {
    const state = renderApp(storeService({ initial: complete() }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Turnover Facts" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Aggregate turnover/), { target: { value: "above_5_crore" } });
    fireEvent.change(within(dialog).getByLabelText(/Financial year of invoices/), { target: { value: "2026" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Turnover Facts" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(/2026-27/);
    expect(state.saved).toHaveLength(0);
  });

  it("sends no year when the band is set back to unknown", async () => {
    const state = renderApp(storeService({ initial: complete({ hsnTurnoverBand: "up_to_5_crore", hsnTurnoverFinancialYear: "2026-27" }) }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Turnover Facts" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.change(within(dialog).getByLabelText(/Aggregate turnover/), { target: { value: "unknown" } });
    expect(within(dialog).queryByLabelText(/Financial year of invoices/)).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Turnover Facts" }));
    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ hsnTurnoverBand: "unknown", hsnTurnoverFinancialYear: null });
  });

  it("designates a licence for retail memos only when the owner ticks it", async () => {
    const state = renderApp();
    fireEvent.click(await screen.findByRole("button", { name: "Add Licence" }));
    let dialog = await screen.findByRole("dialog", { name: "Add Licence" });
    fireEvent.change(within(dialog).getByLabelText(/Licence type/), { target: { value: "Form 20" } });
    fireEvent.change(within(dialog).getByLabelText(/Licence number/), { target: { value: "MH-20-1" } });
    // Not ticked: the type "Form 20" alone designates nothing.
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));
    await waitFor(() => expect(state.saved).toHaveLength(1));
    expect(state.saved[0].body).toMatchObject({ includeOnRetailMemo: false });
    expect(await screen.findByText("Not designated")).toBeInTheDocument();
    expect(screen.getByText("No licence designated for retail memos")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Add Licence" }));
    dialog = await screen.findByRole("dialog", { name: "Add Licence" });
    fireEvent.change(within(dialog).getByLabelText(/Licence type/), { target: { value: "Form 21" } });
    fireEvent.change(within(dialog).getByLabelText(/Licence number/), { target: { value: "MH-21-1" } });
    fireEvent.click(within(dialog).getByLabelText(/Print this licence number on retail memos/));
    fireEvent.click(within(dialog).getByRole("button", { name: "Save Licence" }));
    await waitFor(() => expect(state.saved).toHaveLength(2));
    expect(state.saved[1].body).toMatchObject({ includeOnRetailMemo: true });
    expect(await screen.findByText("Printed on retail memos")).toBeInTheDocument();
    expect(screen.queryByText("No licence designated for retail memos")).not.toBeInTheDocument();
  });

  it("explains that an unrecorded GST registration stops sales", async () => {
    renderApp(storeService({ initial: complete() }));
    expect(await screen.findByText("GST registration not recorded")).toBeInTheDocument();
    expect(screen.getByText(/refused rather than guessed/i)).toBeInTheDocument();
  });

  it("says an unregistered pharmacy charges no GST", async () => {
    renderApp(storeService({ initial: complete({ gstRegistrationStatus: "unregistered" }) }));
    expect(await screen.findByText(/sales charge no GST/i)).toBeInTheDocument();
    expect(screen.queryByText("GST registration not recorded")).not.toBeInTheDocument();
  });

  /// Phase 1L-A stops at the document's facts. Nothing here prints.
  it("offers no printing of any kind", async () => {
    renderApp(storeService({ initial: complete() }));
    await screen.findByRole("heading", { name: "Pharmacy Licences" });
    for (const name of [/print/i, /download/i, /pdf/i, /invoice/i]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });
});
