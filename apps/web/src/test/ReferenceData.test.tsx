import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReferenceMasterResponse, UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { basisPointsToPercent, percentToBasisPoints } from "../reference/referenceApi";

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function record(kind: ReferenceMasterResponse["kind"], attributes: Record<string, unknown>, status: "active" | "archived" = "active", revision = 1): ReferenceMasterResponse {
  return { id: `01997000-0000-7000-8000-${kind.replaceAll("-", "").padEnd(12, "0").slice(0, 12)}`, kind, revision, status, attributes, createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: status === "archived" ? "2026-01-02T00:00:00Z" : null, archiveReason: status === "archived" ? "Test archive" : null } as ReferenceMasterResponse;
}

interface MockOptions {
  role?: UserRole;
  lists?: Partial<Record<ReferenceMasterResponse["kind"], ReferenceMasterResponse[]>>;
  mutationError?: { code: string; status: number; issues?: Array<{ field: string; message: string }> };
  failLists?: boolean;
}

function mockReferenceService(options: MockOptions = {}) {
  const role = options.role ?? "owner_admin";
  const user = { id: "01900000-0000-7000-8000-000000000001", loginIdentifier: role, displayName: "Reference User", role, revision: 1 };
  const lists = options.lists ?? {};
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname.includes("/dashboard/summary")) return response({ storeDisplayName: "Care Pharmacy", activeProductCount: 1, activePackCount: 2 });
    const match = /^\/api\/v1\/reference\/([^/?]+)(?:\/([^/]+))?(?:\/(archive|restore))?$/.exec(url.pathname);
    if (!match) throw new Error(`Unexpected request: ${url.pathname}`);
    const kind = match[1] as ReferenceMasterResponse["kind"];
    const method = init?.method ?? "GET";
    if (method === "GET") {
      if (options.failLists) return response({ code: "internal_error", message: "raw", issues: [], expectedRevision: null, currentRevision: null }, 500);
      const requestedStatus = url.searchParams.get("status") ?? "active";
      return response((lists[kind] ?? []).filter((item) => requestedStatus === "all" || item.status === requestedStatus));
    }
    if (options.mutationError) return response({ code: options.mutationError.code, message: "raw backend detail", issues: options.mutationError.issues ?? [], expectedRevision: 1, currentRevision: 2 }, options.mutationError.status);
    const body = JSON.parse(String(init?.body ?? "{}"));
    const existing = (lists[kind] ?? [])[0];
    const result = existing ? { ...existing, revision: existing.revision + 1, status: match[3] === "archive" ? "archived" : match[3] === "restore" ? "active" : existing.status, attributes: body.attributes ?? existing.attributes } : record(kind, body.attributes, "active", 1);
    lists[kind] = [result];
    return response(result, method === "POST" && !match[2] ? 201 : 200);
  });
}

function renderApp(path: string, fetchMock = mockReferenceService()) {
  vi.stubGlobal("fetch", fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { fetchMock, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("Reference Data UI", () => {
  it("exposes Masters navigation and the real protected landing route", async () => {
    renderApp("/app/reference");
    expect(await screen.findByRole("heading", { name: "Reference Data" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Reference Data/ })).toHaveClass("nav-item--active");
    for (const name of ["Units of Measure", "Dosage Forms", "Pharmaceutical Companies", "Brands", "HSN Codes", "Tax Categories", "Regulatory References"]) expect(screen.getByRole("link", { name: new RegExp(name) })).toBeInTheDocument();
  });

  it("lists seeded units from the API and provides search, status, and accessible row actions", async () => {
    const tablet = record("units", { canonicalCode: "tablet", displayName: "Tablet", dimension: "count", isDiscrete: true, allowedScale: 0 });
    renderApp("/app/reference/units", mockReferenceService({ lists: { units: [tablet] } }));
    expect(await screen.findByRole("cell", { name: "Tablet" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Dimension" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit" })).toBeInTheDocument();
    expect(screen.getByLabelText("Search")).toBeInTheDocument();
    expect(screen.getByLabelText("Status")).toBeInTheDocument();
  });

  it("creates a unit with field validation and backend-shaped attributes", async () => {
    const fetchMock = mockReferenceService({ lists: { units: [] } });
    renderApp("/app/reference/units", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Add unit" }));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(screen.getByLabelText(/Canonical code/)).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByLabelText(/Canonical code/)).toHaveFocus();
    await screen.findByRole("button", { name: "Save" });
    fireEvent.change(screen.getByLabelText(/Canonical code/), { target: { value: "dose" } });
    fireEvent.change(screen.getByLabelText(/Display name/), { target: { value: "Dose" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST" && String(init.body).includes('"canonicalCode":"dose"'))).toBe(true));
  });

  it("requires an archive reason, restores archived records, and labels lifecycle actions accurately", async () => {
    const archived = record("units", { canonicalCode: "old", displayName: "Old unit", dimension: "count", isDiscrete: true, allowedScale: 0 }, "archived", 2);
    renderApp("/app/reference/units", mockReferenceService({ lists: { units: [archived] } }));
    fireEvent.change(await screen.findByLabelText("Status"), { target: { value: "archived" } });
    fireEvent.click(await screen.findByRole("button", { name: "Restore" }));
    expect(screen.getByRole("dialog", { name: /Restore Old unit/ })).toBeInTheDocument();
    expect(screen.getByText(/retains|duplicate and integrity/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restore record" })).toBeEnabled();
  });

  it("shows a deliberate revision-conflict experience without raw backend text", async () => {
    const unit = record("units", { canonicalCode: "dose", displayName: "Dose", dimension: "count", isDiscrete: true, allowedScale: 0 });
    renderApp("/app/reference/units", mockReferenceService({ lists: { units: [unit] }, mutationError: { code: "revision_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText(/Display name/), { target: { value: "Dose unit" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("changed after you opened it");
    expect(screen.queryByText("raw backend detail")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload latest" })).toBeInTheDocument();
  });

  it("manages company identifiers within company context and surfaces verified conflicts safely", async () => {
    const company = record("companies", { displayName: "Care Pharma", legalName: null, city: "Pune", state: "MH", countryCode: "IN" });
    renderApp("/app/reference/companies", mockReferenceService({ lists: { companies: [company], "company-identifiers": [] }, mutationError: { code: "duplicate_conflict", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Identifiers" }));
    fireEvent.click(screen.getByRole("button", { name: "Add identifier" }));
    fireEvent.change(screen.getByLabelText(/Namespace/), { target: { value: "gstin" } });
    fireEvent.change(screen.getByLabelText(/Identifier value/), { target: { value: "TEST123" } });
    fireEvent.change(screen.getByLabelText(/Verification state/), { target: { value: "verified" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("conflicting active record");
  });

  it("keeps identifier child-dialog keyboard handling isolated from its parent", async () => {
    const company = record("companies", { displayName: "Care Pharma", countryCode: "IN" });
    renderApp("/app/reference/companies", mockReferenceService({ lists: { companies: [company], "company-identifiers": [] } }));
    fireEvent.click(await screen.findByRole("button", { name: "Identifiers" }));
    const add = screen.getByRole("button", { name: "Add identifier" });
    add.focus(); fireEvent.click(add);

    const parentTitle = screen.getByRole("heading", { name: "Company identifiers", hidden: true });
    const parent = parentTitle.closest('[role="dialog"]') as HTMLElement;
    const child = screen.getByRole("dialog", { name: "Add identifier" });
    expect(parent).toBeInTheDocument();
    expect(parent).toHaveAttribute("aria-labelledby", parentTitle.id);
    expect(parent.getAttribute("aria-labelledby")).not.toBe(child.getAttribute("aria-labelledby"));

    const childControls = within(child);
    const close = childControls.getByRole("button", { name: "Close dialog" });
    const save = childControls.getByRole("button", { name: "Save" });
    save.focus(); fireEvent.keyDown(document, { key: "Tab" });
    expect(close).toHaveFocus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(save).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Add identifier" })).not.toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Company identifiers" })).toBeInTheDocument();
    expect(add).toHaveFocus();
  });

  it("closes only the tax-rate child and restores its exact launcher", async () => {
    const tax = record("tax-categories", { jurisdiction: "IN", categoryCode: "gst5", displayName: "GST 5%", taxTreatment: "taxable" });
    renderApp("/app/reference/tax", mockReferenceService({ lists: { "tax-categories": [tax], "tax-rate-versions": [] } }));
    fireEvent.click(await screen.findByRole("button", { name: "Rate history" }));
    const add = screen.getByRole("button", { name: "Add rate version" });
    add.focus(); fireEvent.click(add);
    const parentTitle = screen.getByRole("heading", { name: "Tax rate history", hidden: true });
    const parent = parentTitle.closest('[role="dialog"]') as HTMLElement;
    const child = screen.getByRole("dialog", { name: "Add tax rate version" });
    expect(parent).toHaveAttribute("aria-labelledby", parentTitle.id);
    expect(parent.getAttribute("aria-labelledby")).not.toBe(child.getAttribute("aria-labelledby"));
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Add tax rate version" })).not.toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Tax rate history" })).toBeInTheDocument();
    expect(add).toHaveFocus();
  });

  it("shows company ownership, tax labels, exact percentage conversion, and rate history", async () => {
    const company = record("companies", { displayName: "Owner Pharma", countryCode: "IN" });
    const brand = record("brands", { displayName: "Care Brand", brandOwnerCompanyId: company.id });
    const tax = record("tax-categories", { jurisdiction: "IN", categoryCode: "gst5", displayName: "GST 5%", taxTreatment: "nil_rated" });
    const rate = record("tax-rate-versions", { taxCategoryId: tax.id, effectiveFrom: "2026-01-01", effectiveTo: null, cgstBasisPoints: 250, sgstBasisPoints: 250, igstBasisPoints: 500, cessBasisPoints: 0 });
    const fetchMock = mockReferenceService({ lists: { companies: [company], brands: [brand], "tax-categories": [tax], "tax-rate-versions": [rate] } });
    const first = renderApp("/app/reference/brands", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Add brand" }));
    expect(await screen.findByRole("option", { name: "Owner Pharma" })).toBeInTheDocument();
    first.unmount();
    renderApp("/app/reference/tax", fetchMock);
    expect(await screen.findByRole("cell", { name: "Nil rated" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Rate history" }));
    expect(await screen.findByRole("cell", { name: "5.00%" })).toBeInTheDocument();
    expect(percentToBasisPoints("5.00")).toBe(500);
    expect(percentToBasisPoints("5.001")).toBeNull();
    expect(basisPointsToPercent(500)).toBe("5.00");
  });

  it("labels regulatory data as reference metadata rather than legal advice", async () => {
    renderApp("/app/reference/regulatory");
    fireEvent.click(await screen.findByRole("button", { name: "Add regulatory reference" }));
    expect(screen.getByRole("note")).toHaveTextContent("not legal advice");
    expect(screen.getByLabelText(/Source \/ reference/)).toBeInTheDocument();
  });

  it("provides the complete dosage-form list and form workflow", async () => {
    const form = record("dosage-forms", { canonicalCode: "tablet", displayName: "Tablet", description: "Solid form", routeHint: "oral", releaseHint: null });
    renderApp("/app/reference/dosage-forms", mockReferenceService({ lists: { "dosage-forms": [form] } }));
    expect(await screen.findByRole("cell", { name: /^Tablet$/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Add dosage form" }));
    for (const label of [/Canonical code/, /Display name/, /Description/, /Route hint/, /Release hint/]) expect(screen.getByLabelText(label)).toBeInTheDocument();
  });

  it("edits a company without treating its name as a hard unique identity", async () => {
    const company = record("companies", { displayName: "Care Pharma", legalName: "Care Pharma Private Limited", city: "Pune", state: "MH", countryCode: "IN" });
    const fetchMock = mockReferenceService({ lists: { companies: [company] } });
    renderApp("/app/reference/companies", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText(/Legal name/), { target: { value: "Care Pharma Limited" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "PUT" && String(init.body).includes("Care Pharma Limited"))).toBe(true));
  });

  it("creates a company with its optional identity details", async () => {
    const fetchMock = mockReferenceService({ lists: { companies: [] } });
    renderApp("/app/reference/companies", fetchMock);
    fireEvent.click(await screen.findByRole("button", { name: "Add company" }));
    fireEvent.change(screen.getByRole("textbox", { name: /Display \/ trade name/ }), { target: { value: "North Star Pharma" } });
    fireEvent.change(screen.getByLabelText(/Legal name/), { target: { value: "North Star Pharma Private Limited" } });
    fireEvent.change(screen.getByLabelText(/Country code/), { target: { value: "IN" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST" && String(init.body).includes("North Star Pharma"))).toBe(true));
  });

  it("maps HSN validation and jurisdiction conflicts to safe UI feedback", async () => {
    const hsn = record("hsn-codes", { jurisdiction: "IN", hsnCode: "3004", description: "Medicaments" });
    renderApp("/app/reference/hsn", mockReferenceService({ lists: { "hsn-codes": [hsn] }, mutationError: { code: "duplicate_conflict", status: 409, issues: [{ field: "hsnCode", message: "already exists in this jurisdiction" }] } }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByRole("textbox", { name: /HSN code/ }), { target: { value: "3004" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("conflicting active record");
    expect(screen.getByRole("textbox", { name: /HSN code/ })).toHaveAttribute("aria-invalid", "true");
  });

  it("keeps tax versions immutable and surfaces effective-period overlap", async () => {
    const tax = record("tax-categories", { jurisdiction: "IN", categoryCode: "gst5", displayName: "GST 5%", taxTreatment: "taxable" });
    renderApp("/app/reference/tax", mockReferenceService({ lists: { "tax-categories": [tax], "tax-rate-versions": [] }, mutationError: { code: "effective_date_overlap", status: 409 } }));
    fireEvent.click(await screen.findByRole("button", { name: "Rate history" }));
    fireEvent.click(screen.getByRole("button", { name: "Add rate version" }));
    fireEvent.change(screen.getByLabelText(/Effective from/), { target: { value: "2026-01-01" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("overlaps an existing active rate");
  });

  it("provides loading, empty, error, and retry states", async () => {
    const pending = vi.fn(async (input: RequestInfo | URL) => { const path = String(input); if (path.includes("system/info")) return response(system); if (path.includes("auth/status")) return response({ setupRequired: false, authenticated: true, user: { id: "u", loginIdentifier: "owner", displayName: "Owner", role: "owner_admin", revision: 1 }, storeDisplayName: "Care" }); return new Promise(() => undefined); });
    const first = renderApp("/app/reference/units", pending);
    expect(await screen.findByRole("heading", { name: "Units of Measure" })).toBeInTheDocument();
    expect(screen.getByText("Loading reference data…")).toBeInTheDocument();
    first.unmount();
    const empty = renderApp("/app/reference/units", mockReferenceService());
    expect(await screen.findByRole("heading", { name: "No records yet" })).toBeInTheDocument();
    empty.unmount();
    const failing = mockReferenceService({ failLists: true });
    renderApp("/app/reference/units", failing);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not be loaded");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(failing.mock.calls.filter(([input]) => String(input).includes("/reference/units")).length).toBeGreaterThan(1));
  });

  it("keeps pharmacist and cashier reference access read-only", async () => {
    for (const role of ["pharmacist", "cashier"] as const) {
      const view = renderApp("/app/reference/units", mockReferenceService({ role }));
      expect(await screen.findByText("Read-only access")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /Add unit|Edit|Archive/ })).not.toBeInTheDocument();
      view.unmount();
    }
  });

  it("supports Escape close and restores keyboard focus", async () => {
    renderApp("/app/reference/units");
    const add = await screen.findByRole("button", { name: "Add unit" });
    add.focus(); fireEvent.click(add);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(add).toHaveFocus();
  });
});
