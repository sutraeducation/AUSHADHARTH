import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";

/**
 * Phase 1M-D2 — the Schedule X working record, proved without a browser.
 *
 * The attack here is a screen that overstates what it is. Rule 65(21)(a) requires a BOUND, SERIALLY
 * PAGE NUMBERED register, and software cannot be one. So these tests check the wording as carefully
 * as the behaviour: no claim to be the statutory register, no page number by any name, no pre-ticked
 * attestation, and a frozen particular that never borrows today's master value.
 */

const IDs = {
  store: "01997c00-0000-7000-8000-000000000004",
  user: "01997c00-0000-7000-8000-000000000030",
  entry: "01997c00-0000-7000-8000-000000000070",
  product: "01997c00-0000-7000-8000-000000000001",
  purchase: "01997c00-0000-7000-8000-000000000080",
  purchaseLine: "01997c00-0000-7000-8000-000000000081",
  pharmacist: "01997c00-0000-7000-8000-000000000090"
};
const system = {
  status: "ok",
  apiVersion: "v1",
  applicationVersion: "0.0.0",
  compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 }
};

function entry(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.entry,
    entryKind: "receipt",
    reference: "AXR-000001",
    transactionDate: "2026-09-10",
    drugName: "Schedule X Test Medicine A",
    productId: IDs.product,
    batchState: "recorded",
    batchNumber: "BX-7001",
    manufacturerState: "recorded",
    manufacturerName: "Meridian Laboratories",
    quantityAtoms: 50,
    quantityPacks: 5,
    billNumber: "INV-9101",
    billDate: "2026-09-10",
    purchaseDocumentId: IDs.purchase,
    purchaseLineId: IDs.purchaseLine,
    supplierName: "Sunrise Distributors",
    supplierAddressState: "recorded",
    supplierAddress: "14 Ware House Road",
    supplierLicenceState: "recorded",
    supplierLicenceNumber: "20B-MH-9911",
    status: "prepared",
    particularsEnteredInPhysicalRegister: false,
    physicalEntryAuthenticated: false,
    supervisingProfessionalId: null,
    supervisingProfessionalName: null,
    confirmedAtUtc: null,
    finalizedAtUtc: null,
    voidedAtUtc: null,
    voidReason: null,
    createdAtUtc: "2026-09-10T06:00:00Z",
    ...overrides
  };
}

const pharmacist = {
  id: IDs.pharmacist,
  revision: 1,
  status: "active",
  fullName: "Meera Iyer",
  capacity: "registered_pharmacist",
  registrationNumber: "MH-PH-44821",
  registeringAuthority: "Maharashtra State Pharmacy Council",
  validFrom: "2020-01-01",
  validUpto: null,
  linkedUserId: null
};

function response(body: unknown, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}
function failure(code: string, status: number) {
  return response(
    { code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null },
    status
  );
}

type Options = { role?: UserRole; register?: Record<string, unknown> };

function service(options: Options = {}) {
  const role = options.role ?? "owner_admin";
  const writes: Array<{ path: string; body: Record<string, unknown> }> = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) {
      return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    }
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/store/schedule-x/register") {
      if (role !== "owner_admin" && role !== "pharmacist") return failure("authorization_denied", 403);
      return response({ entries: [entry()], legacyReceipts: [], ...options.register });
    }
    if (url.pathname === "/api/v1/store/professionals") return response([pharmacist]);
    if (/\/schedule-x\/register\/[^/]+\/confirm$/.test(url.pathname) && method === "POST") {
      if (role !== "owner_admin" && role !== "pharmacist") return failure("authorization_denied", 403);
      writes.push({ path: url.pathname, body });
      return response(
        entry({
          status: "confirmed",
          particularsEnteredInPhysicalRegister: true,
          physicalEntryAuthenticated: true,
          supervisingProfessionalId: IDs.pharmacist,
          supervisingProfessionalName: "Meera Iyer",
          confirmedAtUtc: "2026-09-10T07:00:00Z"
        })
      );
    }
    if (/\/schedule-x\/register\/[^/]+\/(finalize|void)$/.test(url.pathname) && method === "POST") {
      writes.push({ path: url.pathname, body });
      return response(entry({ status: "finalized", finalizedAtUtc: "2026-09-10T08:00:00Z" }));
    }
    // Any request this screen does not make is a 404, so an accidental new call shows up as a
    // failed assertion rather than a silent success.
    return failure("not_found", 404);
  });
  return { fetchMock, writes };
}

function renderApp(path: string, double = service()) {
  vi.stubGlobal("fetch", double.fetchMock);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } }
  });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <App />
      </MemoryRouter>
    </QueryClientProvider>
  );
  return double;
}

// The working record is ready only after the whole App has booted in this worker. The wait is a
// readiness budget only; every assertion below it stays exact.
// Waits for the LOADED record, not merely the advisory: the advisory renders before the query
// settles, so gating on it would assert against a still-loading table.
const awaitAdvisory = async () => {
  await screen.findByRole("heading", { name: "Working entries" }, { timeout: 3000 });
  return screen.getByTestId("schedule-x-advisory");
};

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("the Schedule X working record", () => {
  it("calls itself a working record and says the bound register remains the legal one", async () => {
    renderApp("/app/settings/schedule-x");
    const advisory = within(await awaitAdvisory());
    expect(advisory.getByText("This is an AUSHADHARTH working record.")).toBeInTheDocument();
    expect(
      advisory.getByText(/bound, serially page-numbered Schedule X register required by rule 65\(21\) remains the legal record/)
    ).toBeInTheDocument();
    expect(advisory.getByText(/AUSHADHARTH does not replace it/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Schedule X Working Record" })).toBeInTheDocument();
  });

  it("never claims to be a statutory or electronic register, and never shows a page number", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    const text = document.body.textContent ?? "";
    for (const forbidden of [
      "electronic statutory register",
      "digital statutory register",
      "electronic register",
      "replaces the register",
      "legally compliant",
      "NDPS compliant",
      "duly licensed",
      "verified licence",
      "page number",
      "Page number",
      "page no",
      "statutory serial"
    ]) {
      expect(text.toLowerCase()).not.toContain(forbidden.toLowerCase());
    }
    // The reference is labelled as AUSHADHARTH's own, not as a register serial.
    expect(screen.getByText("AXR-000001")).toBeInTheDocument();
    expect(screen.getAllByText("AUSHADHARTH reference").length).toBeGreaterThan(0);
  });

  it("shows the frozen particulars, and says Not recorded rather than filling one in", async () => {
    renderApp(
      "/app/settings/schedule-x",
      service({
        register: {
          entries: [
            entry({
              batchState: "not_recorded",
              batchNumber: null,
              manufacturerState: "not_recorded",
              manufacturerName: null,
              supplierLicenceState: "not_recorded",
              supplierLicenceNumber: null
            })
          ]
        }
      })
    );
    await awaitAdvisory();
    const row = within(screen.getByTestId(`schedule-x-entry-${IDs.entry}`));
    // Three absent particulars, each said in words. None is borrowed from today's masters.
    expect(row.getAllByText("Not recorded")).toHaveLength(3);
    expect(row.getByText("Sunrise Distributors")).toBeInTheDocument();
    expect(row.getByText("14 Ware House Road")).toBeInTheDocument();
    expect(row.getByText("Schedule X Test Medicine A")).toBeInTheDocument();
  });

  it("does not pre-check either attestation and will not send one without the other", async () => {
    const double = renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    fireEvent.click(screen.getByRole("button", { name: "Record in register" }));
    const dialog = await screen.findByRole("dialog");
    const boxes = within(dialog).getAllByRole("checkbox");
    expect(boxes).toHaveLength(2);
    // NOT pre-checked. A pre-ticked box is a guess about what somebody did with a pen.
    for (const box of boxes) expect(box).not.toBeChecked();
    const submit = within(dialog).getByRole("button", { name: "Record confirmation" });
    expect(submit).toBeDisabled();

    fireEvent.click(boxes[0]);
    expect(submit).toBeDisabled();
    fireEvent.click(boxes[1]);
    // Still not enough: the supervising pharmacist has to be named.
    expect(submit).toBeDisabled();
    await within(dialog).findByRole("option", { name: "Meera Iyer" });
    fireEvent.change(within(dialog).getByLabelText("Supervising registered pharmacist"), {
      target: { value: IDs.pharmacist }
    });
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() => expect(double.writes).toHaveLength(1));
    expect(double.writes[0].body).toMatchObject({
      supervisingProfessionalId: IDs.pharmacist,
      particularsEnteredInPhysicalRegister: true,
      physicalEntryAuthenticated: true
    });
  });

  it("describes the confirmation as recording a physical act, never as a signature", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    fireEvent.click(screen.getByRole("button", { name: "Record in register" }));
    const dialog = within(await screen.findByRole("dialog"));
    expect(
      dialog.getByText(/AUSHADHARTH records your confirmation; it does not make the register entry or the signature/)
    ).toBeInTheDocument();
    expect(
      dialog.getByText(/I have entered these particulars in the bound, serially page-numbered Schedule X register/)
    ).toBeInTheDocument();
    expect(
      dialog.getByText(/authenticated by the person under whose supervision the transaction took place/)
    ).toBeInTheDocument();
  });

  it("offers no closing action until the physical acts are confirmed", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    expect(screen.queryByRole("button", { name: "Close entry" })).not.toBeInTheDocument();
    cleanup();

    renderApp(
      "/app/settings/schedule-x",
      service({
        register: {
          entries: [
            entry({
              status: "confirmed",
              particularsEnteredInPhysicalRegister: true,
              physicalEntryAuthenticated: true,
              supervisingProfessionalName: "Meera Iyer",
              confirmedAtUtc: "2026-09-10T07:00:00Z"
            })
          ]
        }
      })
    );
    await awaitAdvisory();
    expect(screen.getByRole("button", { name: "Close entry" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Record in register" })).not.toBeInTheDocument();
  });

  it("gives a finalized entry no action at all", async () => {
    renderApp(
      "/app/settings/schedule-x",
      service({
        register: {
          entries: [
            entry({
              status: "finalized",
              particularsEnteredInPhysicalRegister: true,
              physicalEntryAuthenticated: true,
              supervisingProfessionalName: "Meera Iyer",
              finalizedAtUtc: "2026-09-10T08:00:00Z"
            })
          ]
        }
      })
    );
    await awaitAdvisory();
    expect(screen.getByText("Closed")).toBeInTheDocument();
    for (const name of ["Record in register", "Close entry", "Withdraw"]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });

  it("lists earlier receipts as unresolved without reconstructing their particulars", async () => {
    renderApp(
      "/app/settings/schedule-x",
      service({
        register: {
          entries: [],
          legacyReceipts: [
            {
              purchaseDocumentId: IDs.purchase,
              purchaseLineId: IDs.purchaseLine,
              invoiceDate: "2026-04-01",
              supplierInvoiceNumber: "INV-1001",
              frozenDrugName: null,
              productId: IDs.product,
              currentProductName: "Schedule X Test Medicine A"
            }
          ]
        }
      })
    );
    await awaitAdvisory();
    const legacy = within(screen.getByTestId("schedule-x-legacy"));
    expect(legacy.getByText("INV-1001")).toBeInTheDocument();
    // The drug name was never frozen for this one, and the screen says so instead of guessing.
    expect(legacy.getByText("Not recorded")).toBeInTheDocument();
    // The current catalogue name appears only as an identification aid, and is labelled as one.
    expect(legacy.getByText("Current catalogue name, for identification only")).toBeInTheDocument();
    // There is no control to mark a legacy receipt compliant.
    expect(legacy.queryByRole("button")).not.toBeInTheDocument();
  });

  it("keeps the whole working record away from a cashier", async () => {
    renderApp("/app/settings/schedule-x", service({ role: "cashier" }));
    expect(
      await screen.findByRole("heading", { name: "This record is not available to your role" }, { timeout: 3000 })
    ).toBeInTheDocument();
    expect(screen.queryByTestId("schedule-x-advisory")).not.toBeInTheDocument();
    expect(screen.queryByText("AXR-000001")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Record in register" })).not.toBeInTheDocument();
    // And the navigation does not offer it either.
    expect(screen.queryByRole("link", { name: "Schedule X" })).not.toBeInTheDocument();
  });

  it("scrolls the particulars sideways rather than dropping any of them", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    // Every rule 65(21)(b) receipt particular has its own column; none is omitted to fit.
    for (const header of [
      "AUSHADHARTH reference",
      "Date",
      "Drug",
      "Quantity",
      "Batch",
      "Manufacturer",
      "Supplier",
      "Supplier address",
      "Supplier licence",
      "Bill",
      "Physical register"
    ]) {
      expect(screen.getByRole("columnheader", { name: header })).toBeInTheDocument();
    }
    expect(document.querySelector(".table-scroll")).not.toBeNull();
  });
});

/**
 * Phase 1M-D2 U1 — the owner's mobile finding.
 *
 * The desktop compliance table was acceptable; on a phone it showed the reference, the kind and a
 * sliver of the drug name, with the supplier, the licence, the bill and the actions past the edge.
 * The fix turns each row into a labelled card BELOW A CSS BREAKPOINT ONLY — one DOM, one dataset,
 * two presentations — so these tests assert what the markup must carry for that to work at all:
 * every cell still present, every cell labelled, and the reference descriptor on its own line.
 *
 * jsdom applies no media queries, so a test cannot see the card layout itself. What it can prove is
 * that nothing the card needs is missing, which is the part a refactor could silently break.
 */
describe("the Schedule X working record on a narrow viewport", () => {
  it("labels every receipt particular so the mobile card can name it", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    const row = screen.getByTestId(`schedule-x-entry-${IDs.entry}`);
    const labels = [...row.querySelectorAll("td")].map((cell) => cell.getAttribute("data-label"));
    // Every rule 65(21)(b) receipt particular, plus the state and the actions. None dropped, none
    // unlabelled: an unlabelled cell would become an anonymous value in the card.
    expect(labels).toEqual([
      "AUSHADHARTH reference",
      "Kind",
      "Date",
      "Drug",
      "Quantity",
      "Batch",
      "Manufacturer",
      "Supplier",
      "Supplier address",
      "Supplier licence",
      "Bill",
      "Physical register",
      "Actions"
    ]);
    // Each label matches the column header it came from, so the two presentations cannot drift.
    for (const label of labels.filter((name) => name && name !== "Actions")) {
      expect(screen.getByRole("columnheader", { name: label as string })).toBeInTheDocument();
    }
  });

  it("keeps the reference descriptor on its own line, not run into the reference", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    const cell = screen
      .getByTestId(`schedule-x-entry-${IDs.entry}`)
      .querySelector('[data-label="AUSHADHARTH reference"]') as HTMLElement;
    const reference = cell.querySelector(".register-reference") as HTMLElement;
    const descriptor = cell.querySelector("small") as HTMLElement;
    // Two separate elements, so the stylesheet can put the descriptor beneath the reference. The
    // owner saw "AXR-000001AUSHADHARTH reference" when the descriptor was an inline sibling.
    expect(reference).not.toBeNull();
    expect(descriptor).not.toBeNull();
    expect(reference.textContent).toBe("AXR-000001");
    expect(descriptor.textContent).toBe("AUSHADHARTH reference");
    expect(descriptor.parentElement).toBe(cell);
    expect(reference.contains(descriptor)).toBe(false);
  });

  it("carries the physical-register state in its own labelled cell, in words", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    const status = screen
      .getByTestId(`schedule-x-entry-${IDs.entry}`)
      .querySelector('[data-label="Physical register"]') as HTMLElement;
    expect(status).not.toBeNull();
    // Words, not colour alone: the badge carries the text a reader needs.
    expect(status.textContent).toContain("Not yet written in the register");
    expect(status.querySelector(".status-badge")).not.toBeNull();
  });

  it("keeps the row actions inside the row, reachable by name", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    const actions = screen
      .getByTestId(`schedule-x-entry-${IDs.entry}`)
      .querySelector('[data-label="Actions"]') as HTMLElement;
    expect(actions).not.toBeNull();
    expect(within(actions).getByRole("button", { name: "Record in register" })).toBeInTheDocument();
    expect(within(actions).getByRole("button", { name: "Withdraw" })).toBeInTheDocument();
  });

  it("gives the earlier-receipts panel the same labelled treatment, still without a compliance action", async () => {
    renderApp(
      "/app/settings/schedule-x",
      service({
        register: {
          entries: [],
          legacyReceipts: [
            {
              purchaseDocumentId: IDs.purchase,
              purchaseLineId: IDs.purchaseLine,
              invoiceDate: "2026-04-01",
              supplierInvoiceNumber: "INV-1001",
              frozenDrugName: null,
              productId: IDs.product,
              currentProductName: "Schedule X Test Medicine A"
            }
          ]
        }
      })
    );
    await awaitAdvisory();
    const legacy = screen.getByTestId("schedule-x-legacy");
    const labels = [...legacy.querySelectorAll("td")].map((cell) => cell.getAttribute("data-label"));
    expect(labels).toEqual(["Bill", "Date", "Drug name recorded at posting", "Product (name today)"]);
    // Still non-reconstructive, and still no way to declare it compliant.
    expect(within(legacy).getByText("Not recorded")).toBeInTheDocument();
    expect(within(legacy).getByText("Current catalogue name, for identification only")).toBeInTheDocument();
    expect(within(legacy).queryByRole("button")).not.toBeInTheDocument();
  });

  it("keeps both attestations separate, unchecked, and each tied to its own sentence", async () => {
    renderApp("/app/settings/schedule-x");
    await awaitAdvisory();
    fireEvent.click(screen.getByRole("button", { name: "Record in register" }));
    const dialog = await screen.findByRole("dialog");
    const boxes = within(dialog).getAllByRole("checkbox");
    // Two, not one generic confirmation. Neither pre-checked.
    expect(boxes).toHaveLength(2);
    for (const box of boxes) expect(box).not.toBeChecked();
    // Each checkbox sits inside the label carrying its own statement, so the association survives
    // any amount of restyling.
    const first = boxes[0].closest("label") as HTMLElement;
    const second = boxes[1].closest("label") as HTMLElement;
    expect(first).not.toBeNull();
    expect(second).not.toBeNull();
    expect(first.textContent).toContain("bound, serially page-numbered Schedule X register");
    expect(second.textContent).toContain("authenticated by the person under whose supervision");
    expect(first).not.toBe(second);
    // The pharmacist selector keeps its programmatic label, and both dialog actions stay reachable.
    expect(within(dialog).getByLabelText("Supervising registered pharmacist")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Record confirmation" })).toBeInTheDocument();
  });
});
