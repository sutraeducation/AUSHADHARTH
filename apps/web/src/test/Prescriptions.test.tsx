import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UserRole } from "@aushadharth/contracts";
import { App } from "../app/App";
import { atomsToUnitsText, unitsToAtoms } from "../prescriptions/prescriptionApi";

/**
 * Phase 1M-B — the prescription screens, proved without a browser.
 *
 * The attacks are a cashier browsing patients' records, a patient's name leaking into a list or a
 * URL, a blank repeat field read as permission to repeat, a dispensed prescription quietly edited,
 * and any wording that claims a prescriber or pharmacist was verified.
 */

const IDs = {
  product: "01997d00-0000-7000-8000-000000000001",
  unit: "01997d00-0000-7000-8000-000000000002",
  prescription: "01997d00-0000-7000-8000-000000000010",
  item: "01997d00-0000-7000-8000-000000000011",
  prescriber: "01997d00-0000-7000-8000-000000000020",
  sale: "01997d00-0000-7000-8000-000000000030",
  line: "01997d00-0000-7000-8000-000000000031",
  user: "01997d00-0000-7000-8000-000000000040",
  record: "01997d00-0000-7000-8000-000000000050",
  h1: "01997d00-0000-7000-8000-000000000060"
};

function h1Sheet(overrides: Record<string, unknown> = {}) {
  return {
    store: { legalName: "Care Pharmacy Private Limited", displayName: "Care Pharmacy", addressLine1: "12 Market Road", addressLine2: null, city: "Ludhiana", stateName: "Punjab", postalCode: "141001", licences: ["Form 20 PB-20-1234", "Form 21 PB-21-1234"] },
    entries: [{
      id: IDs.h1, reference: "AH1-000001", status: "prepared", saleDocumentId: IDs.sale, documentNumber: null, lineNumber: 1,
      dateOfSupply: "2026-09-21", prescriberName: "Dr. Anjali Rao", prescriberAddress: "Rao Clinic, Pune", patientName: "Sita Kulkarni",
      drugName: "Cefixime 200 Tablet", quantityAtoms: 10, quantityUnitLabel: "Tablet", supervisingProfessionalName: "Meera Iyer",
      supervisingRegistrationNumber: "MH-PH-44821", supplyRecordSerial: "PR-000003", preparedByDisplayName: "Store User",
      preparedAtUtc: "2026-09-21T09:58:00Z", hardCopyPlacedInRegister: false, pharmacistAuthenticatedHardCopy: false,
      confirmedByDisplayName: null, confirmedAtUtc: null, finalizedAtUtc: null, voidedAtUtc: null, voidReason: null, returnedAtoms: 0,
      ...overrides
    }],
    annotations: []
  };
}

function entry(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.record, serialNumber: "PR-000003", recordMethod: "prescription_register", status: "prepared",
    dateOfSupply: "2026-09-21", prescriberName: "Dr. Anjali Rao", prescriberAddress: "Rao Clinic, Pune",
    subjectKind: "human", subjectName: "Sita Kulkarni", subjectAddress: "14 Lakshmi Road, Pune",
    supervisingProfessionalName: "Meera Iyer", supervisingRegistrationNumber: "MH-PH-44821",
    originalContainerConfirmed: false, manualSignatureConfirmed: false, serialWrittenOnPrescription: false,
    preparedByDisplayName: "Store User", preparedAtUtc: "2026-09-21T09:58:00Z",
    confirmedByDisplayName: null, confirmedAtUtc: null, finalizedAtUtc: null,
    voidedByDisplayName: null, voidedAtUtc: null, voidReason: null, previousSerialNumber: "PR-000001",
    prescriptionId: IDs.prescription, prescriptionReference: "RX-000007", saleDocumentId: IDs.sale,
    documentNumber: null,
    lines: [{ drugName: "Azee 500 Tablet", quantityAtoms: 10, quantityUnitLabel: "Tablet", manufacturerName: "Alkem Laboratories", batchNumber: "AZI-01", batchExpiresOn: "2028-03-31", returnedAtoms: 0 }],
    ...overrides
  };
}
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const PRODUCT = {
  id: IDs.product, productKind: "medicine", brandId: null, dosageFormId: null, baseUnitId: IDs.unit, quantityScale: 0,
  displayName: "Azee 500 Tablet", formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
  hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [], packs: []
};

function detail(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.prescription, reference: "RX-000007", revision: 1, status: "active", prescribedOn: "2026-09-10",
    prescriberId: null, prescriberName: "Dr. Anjali Rao", prescriberAddress: "Rao Clinic, Pune",
    prescriberRegistrationNumber: null, prescriberRegisteringAuthority: null,
    subjectKind: "human", subjectName: "Sita Kulkarni", subjectAddress: "14 Lakshmi Road, Pune",
    directionsText: null, repeatAuthority: "once", repeatTimes: null, repeatIntervalDays: null, archiveReason: null,
    createdAtUtc: "2026-09-10T05:00:00Z",
    items: [{ id: IDs.item, lineNumber: 1, productId: IDs.product, productDisplayName: "Azee 500 Tablet", writtenDescription: "Tab. Azee 500", prescribedQuantityAtoms: 30, doseText: "1 daily", dispensedAtoms: 0, reinstatedAtoms: 0 }],
    dispensings: [], occasionsUsed: 0, occasionsAuthorised: 1,
    ...overrides
  };
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, issues: unknown[] = []) {
  return response({ code, message: "raw backend detail", issues, expectedRevision: null, currentRevision: null }, status);
}

type Options = { role?: UserRole; prescription?: Record<string, unknown>; prescribers?: unknown[]; createError?: { code: string; issues: unknown[] } };

function service(options: Options = {}) {
  const role = options.role ?? "pharmacist";
  const writes: Array<{ method: string; path: string; body: Record<string, unknown> }> = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) : {};
    if (method !== "GET") writes.push({ method, path: url.pathname, body });
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store User", role, revision: 1 };
    const dispenser = role === "owner_admin" || role === "pharmacist";
    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/products") return response([PRODUCT]);
    if (url.pathname === `/api/v1/products/${IDs.product}`) return response(PRODUCT);
    if (url.pathname === "/api/v1/prescribers") {
      if (!dispenser) return failure("authorization_denied", 403);
      if (method === "POST") return response({ id: IDs.prescriber, revision: 1, status: "active", registrationNumber: null, registeringAuthority: null, ...body }, 201);
      return response(options.prescribers ?? []);
    }
    if (url.pathname === "/api/v1/prescriptions") {
      if (!dispenser) return failure("authorization_denied", 403);
      if (method === "POST") {
        if (options.createError) return failure(options.createError.code, 422, options.createError.issues);
        return response(detail({ ...body, id: IDs.prescription, reference: "RX-000007", items: [{ ...detail().items[0], ...(body.items as unknown[])[0] as object, id: IDs.item }] }), 201);
      }
      return response([{ id: IDs.prescription, reference: "RX-000007", prescribedOn: "2026-09-10", prescriberName: "Dr. Anjali Rao", status: "active", itemCount: 1 }]);
    }
    if (url.pathname === `/api/v1/prescriptions/${IDs.prescription}`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(detail(options.prescription));
    }
    if (url.pathname === `/api/v1/sales/${IDs.sale}`) {
      return response({
        id: IDs.sale, storeId: "s", customerPartyId: null, customerNameText: null, businessDate: "2026-09-12", status: "draft", revision: 4,
        seriesCode: null, financialYear: null, sequenceValue: null, documentNumber: null, storeGstRegistrationStatus: null,
        storeNormalizedGstin: null, storePlaceOfSupplyStateId: null, storeStateCode: null, customerDisplayName: null,
        customerGstRegistrationStatus: null, customerNormalizedGstin: null, customerStateCode: null, taxTreatment: null,
        taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0,
        createdByUserId: IDs.user, createdAtUtc: "2026-09-12T05:00:00Z", updatedAtUtc: "2026-09-12T05:00:00Z",
        postedByUserId: null, postedAtUtc: null, recipientSnapshotVersion: 0, recipientParticularsRequested: false,
        recipientAddressSource: null, recipientAddressLine1: null, recipientAddressLine2: null, recipientCity: null,
        recipientPostalCode: null, recipientStateId: null, recipientStateName: null, recipientStateCode: null,
        deliverySameAsRecipient: true, deliveryAddressLine1: null, deliveryAddressLine2: null, deliveryCity: null,
        deliveryPostalCode: null, deliveryStateId: null, deliveryStateName: null, deliveryStateCode: null,
        lines: [{ id: IDs.line, productId: IDs.product }].map((each) => ({
          ...each, saleDocumentId: IDs.sale, lineNumber: 1, productPackId: "p", batchId: "b", quantityBasis: "pack",
          quantityPacks: 1, quantityAtoms: 10, sellingRatePaise: 8000, productDisplayName: null, packDisplayLabel: null,
          baseUnitLabel: null, batchNumber: null, batchExpiresOn: null, batchMrpPaise: null, hsnCodeId: null, hsnCode: null,
          taxCategoryId: null, taxTreatmentKind: null, taxRateVersionId: null, cgstBasisPoints: 0, sgstBasisPoints: 0,
          igstBasisPoints: 0, cessBasisPoints: 0, priceControlStatus: null, controlledFormulationId: null,
          priceControlVersionId: null, ceilingPricePaise: null, ceilingBasis: null, taxableValuePaise: 8000, cgstPaise: 0,
          sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 0, currentProductDisplayName: "Azee 500 Tablet",
          currentPackDisplayLabel: null, currentBaseUnitLabel: null, currentBatchNumber: null, prescriptionItemId: null
        })),
        tenders: [], supply: { supervisingProfessionalId: null, prescriptionEndorsementConfirmed: false }
      });
    }
    if (url.pathname === `/api/v1/sale-lines/${IDs.line}/prescription`) return response(null, 204);
    if (url.pathname === "/api/v1/store/drug-compliance") return response({ complianceLicences: [], recordElections: [], professionals: [] });
    if (url.pathname === `/api/v1/prescription-supply-records/${IDs.record}`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(entry());
    }
    if (url.pathname === `/api/v1/prescription-supply-records/${IDs.record}/confirm`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(entry({ status: "confirmed", manualSignatureConfirmed: true, serialWrittenOnPrescription: true, confirmedAtUtc: "2026-09-21T10:00:00Z", confirmedByDisplayName: "Store User" }));
    }
    if (url.pathname === `/api/v1/prescription-supply-records/${IDs.record}/void`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(entry({ status: "void", voidedByDisplayName: "Store User", voidedAtUtc: "2026-09-21T10:05:00Z", voidReason: String(body.reason) }));
    }
    // Phase 1M-C — the Schedule H1 working record.
    if (url.pathname === `/api/v1/sales/${IDs.sale}/h1-register`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(h1Sheet());
    }
    if (url.pathname === `/api/v1/sales/${IDs.sale}/h1-register/confirm`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(h1Sheet({ status: "confirmed", hardCopyPlacedInRegister: true, pharmacistAuthenticatedHardCopy: true, confirmedByDisplayName: "Store User", confirmedAtUtc: "2026-09-21T10:00:00Z" }));
    }
    if (url.pathname === "/api/v1/h1-register") {
      if (!dispenser) return failure("authorization_denied", 403);
      return response(h1Sheet({ status: "finalized", finalizedAtUtc: "2026-09-21T10:05:00Z", returnedAtoms: 5, documentNumber: "INV/2627/000004" }));
    }
    if (url.pathname === `/api/v1/h1-register/${IDs.h1}/annotations`) {
      if (!dispenser) return failure("authorization_denied", 403);
      return response({ id: "01997d00-0000-7000-8000-000000000071", entryId: IDs.h1, note: String(body.note), createdByDisplayName: "Store User", createdAtUtc: "2026-09-21T11:00:00Z" }, 201);
    }
    // The Sale page the create flow returns to is not under test here.
    if (url.pathname.startsWith("/api/v1/")) return failure("internal_error", 500);
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

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

/** Fills the form with a complete once-only prescription for one product. */
async function fillPrescription() {
  fireEvent.change(await screen.findByLabelText("Prescriber's name"), { target: { value: "Dr. Anjali Rao" } });
  fireEvent.change(screen.getByLabelText("Prescriber's address"), { target: { value: "Rao Clinic, Pune" } });
  fireEvent.change(screen.getByLabelText("Patient's name"), { target: { value: "Sita Kulkarni" } });
  fireEvent.change(screen.getByLabelText("Patient's address"), { target: { value: "14 Lakshmi Road, Pune" } });
  fireEvent.change(screen.getByLabelText("Total quantity (base units)"), { target: { value: "30" } });
  fireEvent.change(screen.getByLabelText("Dose"), { target: { value: "1 daily" } });
}

describe("prescription quantities", () => {
  it("converts base units to exact atoms at the product's scale and refuses anything else", () => {
    expect(unitsToAtoms("30", 0)).toBe(30);
    expect(unitsToAtoms("12.5", 3)).toBe(12_500);
    expect(unitsToAtoms("2.5", 0)).toBeNull();
    expect(unitsToAtoms("0", 0)).toBeNull();
    expect(unitsToAtoms("-3", 0)).toBeNull();
    expect(atomsToUnitsText(12_500, 3)).toBe("12.5");
    expect(atomsToUnitsText(30, 0)).toBe("30");
  });
});

describe("prescriptions and privacy", () => {
  it("refuses the prescription pages to a cashier and hides the navigation", async () => {
    const double = renderApp("/app/prescriptions", service({ role: "cashier" }));
    expect(await screen.findByText("Prescriptions are not available to this role")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Prescriptions" })).not.toBeInTheDocument();
    expect(double.fetchMock.mock.calls.some(([input]) => String(input).includes("/api/v1/prescriptions"))).toBe(false);
  });

  it("lists prescriptions by reference and prescriber, never by patient", async () => {
    renderApp("/app/prescriptions");
    expect(await screen.findByRole("link", { name: "RX-000007" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Prescriptions" })).toBeInTheDocument();
    expect(screen.queryByText("Sita Kulkarni")).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/patient/i)).not.toBeInTheDocument();
  });

  it("searches only by the pharmacy's reference, so no patient's name enters a URL", async () => {
    const double = renderApp("/app/prescriptions");
    fireEvent.change(await screen.findByLabelText("Reference"), { target: { value: "RX-000007" } });
    fireEvent.click(screen.getByRole("button", { name: "Find" }));
    await waitFor(() => expect(double.fetchMock.mock.calls.some(([input]) => String(input).includes("reference=RX-000007"))).toBe(true));
    for (const [input] of double.fetchMock.mock.calls) expect(String(input)).not.toMatch(/Sita|Kulkarni|Lakshmi/);
  });
});

describe("prescription entry", () => {
  it("sends the structured prescription with exact atoms and a once-only default", async () => {
    const double = renderApp(`/app/prescriptions/new?productId=${IDs.product}`);
    await fillPrescription();
    fireEvent.click(screen.getByLabelText(/I have seen this prescription in writing/));
    fireEvent.click(screen.getByRole("button", { name: "Save prescription" }));
    await waitFor(() => expect(double.writes.some((write) => write.path === "/api/v1/prescriptions")).toBe(true));
    const sent = double.writes.find((write) => write.path === "/api/v1/prescriptions")!.body;
    expect(sent).toMatchObject({
      prescriberName: "Dr. Anjali Rao", prescriberAddress: "Rao Clinic, Pune", subjectKind: "human",
      subjectName: "Sita Kulkarni", subjectAddress: "14 Lakshmi Road, Pune", repeatAuthority: "once",
      repeatTimes: null, repeatIntervalDays: null, writtenSignedDatedAttested: true,
      items: [{ productId: IDs.product, writtenDescription: "Azee 500 Tablet", prescribedQuantityAtoms: 30, doseText: "1 daily" }]
    });
    expect(await screen.findByRole("heading", { name: "RX-000007", level: 1 })).toBeInTheDocument();
  });

  it("will not save without the attestation that the paper is written, signed and dated", async () => {
    const double = renderApp(`/app/prescriptions/new?productId=${IDs.product}`);
    await fillPrescription();
    fireEvent.click(screen.getByRole("button", { name: "Save prescription" }));
    expect(await screen.findByText("Confirm the prescription is in writing, signed and dated by the prescriber.")).toBeInTheDocument();
    expect(double.writes).toHaveLength(0);
  });

  it("asks for a number when the prescriber stated a number of times", async () => {
    const double = renderApp(`/app/prescriptions/new?productId=${IDs.product}`);
    await fillPrescription();
    fireEvent.change(screen.getByLabelText("What the prescriber stated"), { target: { value: "stated_times" } });
    fireEvent.click(screen.getByLabelText(/I have seen this prescription in writing/));
    fireEvent.click(screen.getByRole("button", { name: "Save prescription" }));
    expect(await screen.findByText("Enter how many times in all the prescriber allowed, 2 or more.")).toBeInTheDocument();
    expect(double.writes).toHaveLength(0);
  });

  it("records an animal's owner under the owner's words", async () => {
    renderApp(`/app/prescriptions/new?productId=${IDs.product}`);
    fireEvent.click(await screen.findByLabelText(/An animal/));
    expect(screen.getByLabelText("Animal owner's name")).toBeInTheDocument();
    expect(screen.getByLabelText("Animal owner's address")).toBeInTheDocument();
  });

  it("links the new prescription to the sale line it was opened from", async () => {
    const double = renderApp(`/app/prescriptions/new?saleId=${IDs.sale}&lineId=${IDs.line}&productId=${IDs.product}`);
    await fillPrescription();
    fireEvent.click(screen.getByLabelText(/I have seen this prescription in writing/));
    fireEvent.click(screen.getByRole("button", { name: "Save and link to the sale" }));
    await waitFor(() => expect(double.writes.some((write) => write.path.endsWith("/prescription"))).toBe(true));
    expect(double.writes.find((write) => write.path.endsWith("/prescription"))!.body).toEqual({ expectedRevision: 4, prescriptionItemId: IDs.item });
  });

  it("shows the Store Service's refusal and names the field", async () => {
    renderApp(`/app/prescriptions/new?productId=${IDs.product}`, service({
      createError: { code: "validation_failed", issues: [{ field: "prescribedOn", message: "cannot be in the future" }] }
    }));
    await fillPrescription();
    fireEvent.click(screen.getByLabelText(/I have seen this prescription in writing/));
    fireEvent.click(screen.getByRole("button", { name: "Save prescription" }));
    expect(await screen.findByText("cannot be in the future")).toBeInTheDocument();
    await waitFor(() => expect(document.activeElement?.id).toBe("rx-date"));
  });
});

describe("prescription record", () => {
  it("shows the patient to a dispenser and offers correction only before dispensing", async () => {
    renderApp(`/app/prescriptions/${IDs.prescription}`);
    expect(await screen.findByText("Sita Kulkarni")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Correct" })).toBeInTheDocument();
    expect(screen.getByText("Not dispensed yet. It can still be corrected.")).toBeInTheDocument();
  });

  it("keeps a dispensed prescription as history: archive, never correct", async () => {
    renderApp(`/app/prescriptions/${IDs.prescription}`, service({
      prescription: {
        occasionsUsed: 1,
        items: [{ ...detail().items[0], dispensedAtoms: 20, reinstatedAtoms: 10 }],
        dispensings: [{ id: "d1", prescriptionItemId: IDs.item, saleDocumentId: IDs.sale, documentNumber: "INV/2627/000004", quantityAtoms: 20, dispensedOn: "2026-09-12", supervisingProfessionalName: "Meera Iyer", reversedAtoms: 10 }]
      }
    }));
    expect(await screen.findByRole("link", { name: "INV/2627/000004" })).toBeInTheDocument();
    expect(screen.getByText("Meera Iyer")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Correct" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Archive" })).toBeInTheDocument();
    expect(screen.getByText("1 of 1")).toBeInTheDocument();
    // Prescribed 30, dispensed 20, 10 returned: 20 left.
    const row = screen.getByRole("cell", { name: /Azee 500 Tablet/ }).closest("tr")!;
    expect(within(row).getAllByRole("cell").map((cell) => cell.textContent)).toEqual(expect.arrayContaining(["30", "20", "10"]));
    expect(screen.queryByText(/verified/i)).not.toBeInTheDocument();
  });
});

describe("prescribers on Drug Compliance", () => {
  const prescriber = { id: IDs.prescriber, revision: 1, status: "active", fullName: "Dr. Anjali Rao", addressText: "Rao Clinic, Pune", registrationNumber: "MMC-2011-0457", registeringAuthority: null };

  it("lets a pharmacist add a prescriber but not edit or archive one", async () => {
    renderApp("/app/settings/drug-compliance", service({ prescribers: [prescriber] }));
    const panel = await screen.findByRole("region", { name: "Prescribers" });
    expect(await within(panel).findByText("Dr. Anjali Rao")).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "Add Prescriber" })).toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(within(panel).queryByRole("button", { name: "Archive" })).not.toBeInTheDocument();
    expect(within(panel).queryByText(/verified/i)).toHaveTextContent(/not verified/);
  });

  it("lets the owner correct and archive a prescriber", async () => {
    renderApp("/app/settings/drug-compliance", service({ role: "owner_admin", prescribers: [prescriber] }));
    const panel = await screen.findByRole("region", { name: "Prescribers" });
    expect(await within(panel).findByRole("button", { name: "Edit" })).toBeInTheDocument();
    expect(within(panel).getByRole("button", { name: "Archive" })).toBeInTheDocument();
  });

  it("does not show a cashier the prescriber list at all", async () => {
    const double = renderApp("/app/settings/drug-compliance", service({ role: "cashier" }));
    await screen.findByRole("heading", { name: "Drug Compliance", level: 1 });
    expect(screen.queryByRole("region", { name: "Prescribers" })).not.toBeInTheDocument();
    expect(double.fetchMock.mock.calls.some(([input]) => String(input).includes("/api/v1/prescribers"))).toBe(false);
  });
});

describe("the rule 65(3)(1) entry", () => {
  it("shows every particular of the prepared entry with a blank signature box, and claims no signature", async () => {
    renderApp(`/app/prescription-records/${IDs.record}`);
    expect(await screen.findByTestId("record-serial")).toHaveTextContent("PR-000003");
    for (const text of ["2026-09-21", "Dr. Anjali Rao", "Sita Kulkarni", "14 Lakshmi Road, Pune", "Azee 500 Tablet", "Alkem Laboratories", "AZI-01", "2028-03-31", "Meera Iyer · registration MH-PH-44821"]) {
      expect(screen.getAllByText(new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))).length).toBeGreaterThan(0);
    }
    expect(screen.getByText(/previous supply entered as PR-000001/)).toBeInTheDocument();
    expect(screen.getByText("Signature of the registered pharmacist (by hand)")).toBeInTheDocument();
    expect(screen.getByTestId("record-status")).toHaveTextContent("Prepared. Not signed: awaiting the registered pharmacist's handwritten signature");
    expect(screen.getByTestId("record-status")).toHaveTextContent("Nothing has been sold.");
    // The sequence, in order, before anything is posted.
    const steps = screen.getByRole("form", { name: "Confirm the signature" });
    expect(steps).toHaveTextContent("Print this entry.");
    expect(steps).toHaveTextContent("signs it by hand in the signature box");
    expect(steps).toHaveTextContent("Write PR-000003 on the prescription.");
    expect(document.body.textContent).not.toMatch(/signed by AUSHADHARTH|digitally signed|electronically signed|e-signed/i);
  });

  it("confirms only with both manual acts, and then sends the counter back to post", async () => {
    const double = renderApp(`/app/prescription-records/${IDs.record}`);
    const submit = await screen.findByRole("button", { name: "Confirm signature and serial" });
    expect(submit).toBeDisabled();
    fireEvent.click(screen.getByLabelText("The registered pharmacist has signed this printed entry by hand"));
    expect(submit).toBeDisabled();
    fireEvent.click(screen.getByLabelText("The registered pharmacist has signed this printed entry by hand"));
    fireEvent.click(screen.getByLabelText("PR-000003 has been written on the prescription"));
    expect(submit).toBeDisabled();
    fireEvent.click(screen.getByLabelText("The registered pharmacist has signed this printed entry by hand"));
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() => expect(double.writes.some((write) => write.path.endsWith("/confirm"))).toBe(true));
    expect(double.writes.find((write) => write.path.endsWith("/confirm"))!.body).toEqual({ manualSignatureConfirmed: true, serialWrittenOnPrescription: true });
    expect(await screen.findByTestId("record-status")).toHaveTextContent("A pharmacist, Store User, confirmed");
    expect(screen.getByTestId("record-status")).toHaveTextContent("AUSHADHARTH did not sign this entry.");
    expect(screen.queryByRole("button", { name: "Confirm signature and serial" })).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Go to the sale to post it" })).toHaveAttribute("href", `/app/sales/${IDs.sale}`);
  });

  it("voids a prepared entry with its reason and says its serial is not reused", async () => {
    const double = renderApp(`/app/prescription-records/${IDs.record}`);
    fireEvent.click(await screen.findByRole("button", { name: "Void this entry" }));
    const dialog = await screen.findByRole("dialog");
    const action = within(dialog).getByRole("button", { name: "Void the entry" });
    expect(action).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText("Reason"), { target: { value: "Customer left before paying" } });
    fireEvent.click(action);
    await waitFor(() => expect(double.writes.some((write) => write.path.endsWith("/void"))).toBe(true));
    expect(double.writes.find((write) => write.path.endsWith("/void"))!.body).toEqual({ reason: "Customer left before paying" });
    expect(await screen.findByTestId("record-status")).toHaveTextContent("Void — cancelled before the supply");
    expect(screen.getByTestId("record-status")).toHaveTextContent("Customer left before paying");
    expect(screen.getByTestId("record-status")).toHaveTextContent("not given to another entry");
    expect(screen.queryByRole("button", { name: "Void this entry" })).not.toBeInTheDocument();
  });

  it("refuses the entry to a cashier", async () => {
    const double = renderApp(`/app/prescription-records/${IDs.record}`, service({ role: "cashier" }));
    expect(await screen.findByText("Prescriptions are not available to this role")).toBeInTheDocument();
    expect(double.fetchMock.mock.calls.some(([input]) => String(input).includes("/prescription-supply-records/"))).toBe(false);
  });
});

describe("the Schedule H1 working record (Phase 1M-C)", () => {
  /** C-07, C-08, C-09, C-57: the clause (h) particulars, the internal reference labelled as such,
   *  the DCC recommendation described as a recommendation, and no signature claimed. */
  it("prints the clause (h) particulars and says what the copy is, and is not", async () => {
    renderApp(`/app/sales/${IDs.sale}/h1-register`);
    const table = await screen.findByTestId("h1-entries");
    for (const text of ["Dr. Anjali Rao", "Rao Clinic, Pune", "Sita Kulkarni", "Cefixime 200 Tablet", "10 Tablet", "AH1-000001"]) {
      expect(within(table).getByText(new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")))).toBeInTheDocument();
    }
    expect(within(table).getByText("AUSHADHARTH Reference (internal)")).toBeInTheDocument();
    expect(within(table).getByText(/Internal — not a register serial/)).toBeInTheDocument();
    expect(screen.getByText(/Care Pharmacy Private Limited/)).toBeInTheDocument();
    expect(screen.getByText(/Form 20 PB-20-1234/)).toBeInTheDocument();
    const body = document.body.textContent ?? "";
    expect(body).toMatch(/official committee recommendation, not a statutory amendment/);
    expect(body).toMatch(/The rule itself lists no signature among the particulars/);
    expect(body).toMatch(/AUSHADHARTH's record is a working record, not that register/);
    expect(body).not.toMatch(/signed by AUSHADHARTH|digitally signed|electronically signed|e-signed|H1 register serial/i);
    expect(screen.getByTestId("h1-status")).toHaveTextContent("Prepared. Not yet placed in the H1 register or authenticated. Nothing has been sold.");
  });

  /** C-58, C-59: the electronic entry alone does not complete it; both physical acts are needed. */
  it("confirms only with both physical acts, per supply", async () => {
    const double = renderApp(`/app/sales/${IDs.sale}/h1-register`);
    const submit = await screen.findByRole("button", { name: "Confirm placement and authentication" });
    expect(submit).toBeDisabled();
    fireEvent.click(screen.getByLabelText("The printed hard copy has been placed in the separate Schedule H1 register"));
    expect(submit).toBeDisabled();
    fireEvent.click(screen.getByLabelText("The registered pharmacist has authenticated the pasted copy by hand"));
    expect(submit).toBeEnabled();
    fireEvent.click(submit);
    await waitFor(() => expect(double.writes.some((write) => write.path.endsWith("/h1-register/confirm"))).toBe(true));
    const write = double.writes.find((each) => each.path.endsWith("/h1-register/confirm"))!;
    expect(write.path).toBe(`/api/v1/sales/${IDs.sale}/h1-register/confirm`);
    expect(write.body).toEqual({ hardCopyPlacedInRegister: true, pharmacistAuthenticatedHardCopy: true });
    expect(await screen.findByTestId("h1-status")).toHaveTextContent("AUSHADHARTH did not sign this copy.");
    expect(screen.queryByRole("button", { name: "Confirm placement and authentication" })).not.toBeInTheDocument();
    // C-67: nothing on the page confirms a day's entries at once.
    expect(screen.queryByRole("button", { name: /all|day|batch/i })).not.toBeInTheDocument();
  });

  /** C-25, C-60: a cashier cannot open or confirm the H1 record, and no request is made. */
  it("keeps the H1 record from a cashier", async () => {
    for (const path of [`/app/sales/${IDs.sale}/h1-register`, "/app/h1-register"]) {
      const double = renderApp(path, service({ role: "cashier" }));
      expect(await screen.findByText("Prescriptions are not available to this role")).toBeInTheDocument();
      expect(double.fetchMock.mock.calls.some(([input]) => String(input).includes("h1-register"))).toBe(false);
      cleanup();
    }
  });

  /** C-46, C-49, C-22: the register view is by date, shows a return beside the unchanged entry,
   *  and appends a note without editing anything. */
  it("reads the register by date and appends notes beside unchanged entries", async () => {
    const double = renderApp("/app/h1-register");
    const table = await screen.findByTestId("h1-entries");
    expect(within(table).getByText(/5 returned since \(recorded separately; this entry is unchanged\)/)).toBeInTheDocument();
    expect(within(table).getByText("Finalized with the posted sale")).toBeInTheDocument();
    const request = double.fetchMock.mock.calls.map(([input]) => String(input)).find((url) => url.includes("/api/v1/h1-register?"))!;
    expect(new URL(request, "http://local.test").searchParams.has("from")).toBe(true);
    expect(request).not.toMatch(/Sita|patient/i);
    fireEvent.click(screen.getByRole("button", { name: "Add a note to AH1-000001" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("The entry itself never changes.");
    fireEvent.change(within(dialog).getByLabelText("Note"), { target: { value: "Initials clarified on the pasted copy." } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Add note" }));
    await waitFor(() => expect(double.writes.some((write) => write.path.endsWith("/annotations"))).toBe(true));
    expect(double.writes.find((write) => write.path.endsWith("/annotations"))!.body).toEqual({ note: "Initials clarified on the pasted copy." });
  });
});
