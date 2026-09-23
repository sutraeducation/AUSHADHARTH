import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LinePrescriptionSummary, SaleDetail, SaleLine, SaleQuote, SellableBatch, SupplySummary, UserRole } from "@aushadharth/contracts";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { App } from "../app/App";
import { paiseToAmountText, quantityToInteger, sellingRateToPaise } from "../sales/saleApi";

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000001",
  posted: "01997a00-0000-7000-8000-000000000002",
  customer: "01997a00-0000-7000-8000-000000000010",
  product: "01997a00-0000-7000-8000-000000000020",
  pack: "01997a00-0000-7000-8000-000000000030",
  batch: "01997a00-0000-7000-8000-000000000040",
  expiredBatch: "01997a00-0000-7000-8000-000000000041",
  line: "01997a00-0000-7000-8000-000000000050",
  unit: "01997a00-0000-7000-8000-000000000060",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080",
  registered: "01997a00-0000-7000-8000-000000000011",
  maharashtra: "01997300-0000-7000-8000-000000000027"
};

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };
const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };

const UNITS = [
  { id: IDs.unit, kind: "units", revision: 1, status: "active", ...stamp, attributes: { canonicalCode: "tablet", displayName: "Tablet", dimension: "count", isDiscrete: true, allowedScale: 0 } }
];

const CUSTOMERS = [
  { id: IDs.customer, displayName: "Rahul Deshmukh", legalName: null, normalizedSearchName: "rahul deshmukh", gstRegistrationStatus: "unregistered", gstin: null, normalizedGstin: null, pan: null, normalizedPan: null, placeOfSupplyStateId: null, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null, drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp },
  { id: IDs.registered, displayName: "Mehta Medical Stores", legalName: null, normalizedSearchName: "mehta medical stores", gstRegistrationStatus: "registered", gstin: "27AAACM1234K1Z5", normalizedGstin: "27AAACM1234K1Z5", pan: null, normalizedPan: null, placeOfSupplyStateId: IDs.maharashtra, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null, drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp }
];

const STATES = [
  { id: IDs.maharashtra, kind: "state-codes", revision: 1, status: "active", ...stamp, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } }
];

/** ₹50,000 in paise: the Rule 46(e) line the Store Service judges taxable value against. */
const THRESHOLD = 5_000_000;

const PACK = { id: IDs.pack, productId: IDs.product, containerUnitId: IDs.unit, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: null, displayLabel: "Strip of 10", revision: 1, status: "active", ...stamp };

const PRODUCTS = [
  { id: IDs.product, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.unit, quantityScale: 0, displayName: "Crocin 500 mg Tablet", formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [], packs: [PACK] }
];

const BATCHES: SellableBatch[] = [
  { id: IDs.batch, productPackId: IDs.pack, batchNumber: "B-2601", expiresOn: "2028-03-31", mrpPaise: 9_550, availableAtoms: 100, expired: false },
  { id: IDs.expiredBatch, productPackId: IDs.pack, batchNumber: "B-OLD", expiresOn: "2026-01-31", mrpPaise: 9_000, availableAtoms: 40, expired: true }
];

/** Phase 1M-B: one registered pharmacist on record, as a non-owner sees them — no registration number. */
const PROFESSIONALS = [
  { id: "01997a00-0000-7000-8000-0000000000c1", revision: 1, status: "active", fullName: "Meera Iyer", capacity: "registered_pharmacist", registrationNumber: null, registeringAuthority: null, validFrom: "2020-01-01", validUpto: null, linkedUserId: null },
  { id: "01997a00-0000-7000-8000-0000000000c2", revision: 1, status: "active", fullName: "Store Manager", capacity: "competent_person", registrationNumber: null, registeringAuthority: null, validFrom: null, validUpto: null, linkedUserId: null }
];

const PRESCRIPTION_ITEM_ID = "01997a00-0000-7000-8000-0000000000d1";
const PRESCRIPTION = {
  id: "01997a00-0000-7000-8000-0000000000d0", reference: "RX-000001", revision: 1, status: "active",
  prescribedOn: "2026-09-10", prescriberId: null, prescriberName: "Dr. Anjali Rao", prescriberAddress: "Rao Clinic, Pune",
  prescriberRegistrationNumber: null, prescriberRegisteringAuthority: null,
  subjectKind: "human", subjectName: "Sita Kulkarni", subjectAddress: "14 Lakshmi Road, Pune",
  directionsText: null, repeatAuthority: "once", repeatTimes: null, repeatIntervalDays: null, archiveReason: null,
  createdAtUtc: "2026-09-10T05:00:00Z",
  items: [{ id: PRESCRIPTION_ITEM_ID, lineNumber: 1, productId: IDs.product, productDisplayName: "Crocin 500 mg Tablet", writtenDescription: "Tab. Crocin 500", prescribedQuantityAtoms: 30, doseText: "1 twice daily", dispensedAtoms: 0, reinstatedAtoms: 0 }],
  dispensings: [], occasionsUsed: 0, occasionsAuthorised: 1
};
const PRESCRIPTION_SUMMARY = { id: PRESCRIPTION.id, reference: "RX-000001", prescribedOn: "2026-09-10", prescriberName: "Dr. Anjali Rao", status: "active", itemCount: 1 };

function line(overrides: Partial<SaleLine> = {}): SaleLine {
  return {
    id: IDs.line, saleDocumentId: IDs.sale, lineNumber: 1, productId: IDs.product, productPackId: IDs.pack,
    batchId: IDs.batch, quantityBasis: "pack", quantityPacks: 2, quantityAtoms: 20, sellingRatePaise: 8_000,
    // A draft has issued nothing, so it has frozen nothing. The live names beside them are what the
    // counter reads while the bill is still being built.
    productDisplayName: null, packDisplayLabel: null, baseUnitLabel: null,
    batchNumber: null, batchExpiresOn: null, batchMrpPaise: null,
    currentProductDisplayName: "Crocin 500 mg Tablet", currentPackDisplayLabel: "Strip of 10",
    currentBaseUnitLabel: "Tablet", currentBatchNumber: "B-2601",
    hsnCodeId: null, hsnCode: null, taxCategoryId: null, taxTreatmentKind: null, taxRateVersionId: null,
    cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, cessBasisPoints: 0,
    priceControlStatus: null, controlledFormulationId: null, priceControlVersionId: null,
    ceilingPricePaise: null, ceilingBasis: null,
    taxableValuePaise: 16_000, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 0,
    prescriptionItemId: null,
    ...overrides
  };
}

function sale(overrides: Partial<SaleDetail> = {}): SaleDetail {
  return {
    id: IDs.sale, storeId: IDs.store, customerPartyId: null, customerNameText: null,
    businessDate: "2026-09-12", status: "draft", revision: 1,
    seriesCode: null, financialYear: null, sequenceValue: null, documentNumber: null,
    storeGstRegistrationStatus: null, storeNormalizedGstin: null, storePlaceOfSupplyStateId: null, storeStateCode: null,
    customerDisplayName: null, customerGstRegistrationStatus: null, customerNormalizedGstin: null, customerStateCode: null,
    taxTreatment: null, taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0,
    createdByUserId: IDs.user, createdAtUtc: "2026-09-12T05:00:00Z", updatedAtUtc: "2026-09-12T05:00:00Z",
    postedByUserId: null, postedAtUtc: null,
    recipientSnapshotVersion: 0, recipientParticularsRequested: false, recipientAddressSource: null,
    recipientAddressLine1: null, recipientAddressLine2: null, recipientCity: null, recipientPostalCode: null,
    recipientStateId: null, recipientStateName: null, recipientStateCode: null,
    deliverySameAsRecipient: true, deliveryAddressLine1: null, deliveryAddressLine2: null, deliveryCity: null,
    deliveryPostalCode: null, deliveryStateId: null, deliveryStateName: null, deliveryStateCode: null,
    lines: [], tenders: [],
    supply: { supervisingProfessionalId: null, prescriptionEndorsementConfirmed: false, prescriptionOriginalContainerConfirmed: false },
    prescriptionRecords: [],
    h1RegisterEntries: [],
    ...overrides
  };
}

/** A posted invoice as the Store Service returns it: numbered, snapshotted, every tax resolved. */
function postedSale(overrides: Partial<SaleDetail> = {}): SaleDetail {
  return sale({
    id: IDs.posted, status: "posted", revision: 3,
    seriesCode: "INV", financialYear: "2026-27", sequenceValue: 1, documentNumber: "INV/2627/000001",
    storeGstRegistrationStatus: "registered", storeNormalizedGstin: "27AAACX0000A1Z9", storeStateCode: "27",
    taxTreatment: "intra_state", taxableValuePaise: 16_000, cgstPaise: 960, sgstPaise: 960,
    grandTotalPaise: 17_920, postedByUserId: IDs.user, postedAtUtc: "2026-09-12T06:30:00Z",
    lines: [line({
      saleDocumentId: IDs.posted, hsnCode: "30049099", taxTreatmentKind: "taxable",
      productDisplayName: "Crocin 500 mg Tablet", packDisplayLabel: "Strip of 10", baseUnitLabel: "Tablet",
      batchNumber: "B-2601", batchExpiresOn: "2028-03-31", batchMrpPaise: 9_550,
      cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200,
      cgstPaise: 960, sgstPaise: 960, lineTotalPaise: 17_920, priceControlStatus: "unknown"
    })],
    tenders: [{ id: "01997a00-0000-7000-8000-000000000090", saleDocumentId: IDs.posted, method: "cash", amountPaise: 17_920, referenceText: null }],
    ...overrides
  });
}

function response(body: unknown, status = 200) { return { ok: status >= 200 && status < 300, status, json: async () => body }; }
function failure(code: string, status: number, extra: Record<string, unknown> = {}) {
  return response({ code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra }, status);
}

type Options = {
  role?: UserRole;
  documents?: SaleDetail[];
  writeError?: { code: string; status: number; extra?: Record<string, unknown> };
  quoteError?: { code: string; status: number };
  failList?: boolean;
  /** The Store's GST registration, as the quote reports it. Registered unless a test says not. */
  seller?: "registered" | "unregistered";
  /** Notification No. 14/2020-CT as the quote reports it; "not_required" for a registered seller. */
  dynamicQr?: "unknown" | "not_required" | "required" | null;
  /** The quote's flag for a registered customer's mixed taxable/untaxed bill. */
  mixedSupply?: boolean;
  /** Phase 1M-A: what the Drugs Rules gate reports for every line. */
  regulatoryGate?: "clear" | "prescription_required" | "unresolved" | "workflow_unavailable";
  /** Phase 1M-B: what the quote says of a linked prescription item, when a line has one. */
  prescriptionIssue?: LinePrescriptionSummary["issue"];
  remainingAtoms?: number;
  /** Phase 1M-B: the rule 65(3)(2) election in force; the register unless a test says otherwise. */
  recordMethod?: SupplySummary["recordMethod"];
  regulatoryGateScheme?: "schedule_h" | "schedule_h1" | "schedule_x" | "schedule_c" | "schedule_c1" | "ndps_purview" | "punjab_restricted_supply" | null;
};

/**
 * A Store Service double that keeps the same shape of truth the real one does: the server derives
 * the atoms from the pack, the server owns the revision, and the quote is the server's arithmetic
 * rather than the browser's. GST is 6% + 6%, matching the fixtures elsewhere.
 */
function saleService(options: Options = {}) {
  const role = options.role ?? "cashier";
  const state = {
    documents: new Map((options.documents ?? [sale()]).map((document) => [document.id, structuredClone(document)])),
    requests: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  const bump = (document: SaleDetail) => { document.revision += 1; return document; };
  const guard = (document: SaleDetail, body: Record<string, unknown>) =>
    body.expectedRevision === document.revision
      ? null
      : failure("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });

  /**
   * The recipient requirement as the Store Service reports it. Every line in this double is taxable,
   * so taxable supply and taxable value coincide; the service's own tests prove the case where they
   * do not.
   */
  const sellerRegistered = (options.seller ?? "registered") === "registered";
  const requirementFor = (document: SaleDetail, taxable: number): SaleQuote["recipientParticulars"] => {
    const party = CUSTOMERS.find((each) => each.id === document.customerPartyId);
    const registered = party?.gstRegistrationStatus === "registered";
    // As the Store Service does: Rule 46 belongs to a registered seller's tax invoice only.
    const reasons: SaleQuote["recipientParticulars"]["reasons"] = !sellerRegistered ? [] : [
      ...(registered ? ["registered_recipient" as const] : taxable >= THRESHOLD ? ["taxable_value_threshold" as const] : []),
      ...(document.recipientParticularsRequested ? ["recipient_requested" as const] : [])
    ];
    const missing: Array<{ field: string; message: string }> = [];
    if (reasons.length > 0 && !party) {
      if (!document.customerNameText) missing.push({ field: "customerNameText", message: "Enter the customer's name for the invoice." });
      if (!document.recipientAddressLine1) missing.push({ field: "recipientAddress.line1", message: "Enter the customer's address." });
      if (!document.recipientStateId) missing.push({ field: "recipientAddress.stateId", message: "Choose the State of the customer's address." });
    }
    if (reasons.length > 0 && registered) missing.push({ field: "customer.billingAddress", message: "Add a billing address to this customer in Parties." });
    return { required: reasons.length > 0, reasons, missing, thresholdPaise: THRESHOLD, taxableSupplyValuePaise: taxable };
  };

  const quoteFor = (document: SaleDetail): SaleQuote => {
    const taxable = document.lines.reduce((total, each) => total + each.taxableValuePaise, 0);
    const half = sellerRegistered ? Math.round((taxable * 600) / 10_000) : 0;
    return {
      recipientParticulars: requirementFor(document, taxable),
      sellerGstRegistrationStatus: sellerRegistered ? "registered" : "unregistered",
      dynamicQrApplicability: options.dynamicQr !== undefined ? options.dynamicQr : sellerRegistered ? "not_required" : null,
      registeredRecipientMixedSupply: options.mixedSupply ?? false,
      saleDocumentId: document.id, revision: document.revision, taxTreatment: "intra_state",
      taxableValuePaise: taxable, cgstPaise: half, sgstPaise: half, igstPaise: 0, cessPaise: 0,
      grandTotalPaise: taxable + half * 2,
      lines: document.lines.map((each) => ({
        id: each.id, lineNumber: each.lineNumber, taxableValuePaise: each.taxableValuePaise,
        cgstPaise: Math.round((each.taxableValuePaise * 600) / 10_000),
        sgstPaise: Math.round((each.taxableValuePaise * 600) / 10_000),
        igstPaise: 0, cessPaise: 0,
        lineTotalPaise: each.taxableValuePaise + Math.round((each.taxableValuePaise * 600) / 10_000) * 2,
        regulatoryGate: options.regulatoryGate ?? "clear",
        regulatoryGateScheme: options.regulatoryGateScheme ?? (options.regulatoryGate === "prescription_required" ? "schedule_h" : null),
        prescription: prescriptionFor(each)
      })),
      supply: supplyFor(document)
    };
  };

  // Phase 1M-B, judged the way the Store Service judges it: a Schedule H line needs a linked item,
  // and the supply needs a registered pharmacist and the endorsement confirmation.
  const prescriptionRequired = options.regulatoryGate === "prescription_required";
  const prescriptionFor = (each: SaleLine): LinePrescriptionSummary => {
    const linked = each.prescriptionItemId !== null;
    return {
      required: prescriptionRequired,
      prescriptionItemId: each.prescriptionItemId,
      prescriptionReference: linked ? "RX-000001" : null,
      remainingAtoms: linked ? (options.remainingAtoms ?? 30) : null,
      // A link is "not required" only on a line the gate clears; an H1 or X line keeps its link and
      // is refused by the gate instead.
      issue: !prescriptionRequired
        ? (linked && (options.regulatoryGate ?? "clear") === "clear" ? "prescription_link_not_required" : null)
        : !linked ? "prescription_missing" : (options.prescriptionIssue ?? null)
    };
  };
  const recordMethod = options.recordMethod !== undefined ? options.recordMethod : "prescription_register";
  const supplyFor = (document: SaleDetail): SupplySummary => {
    const issues: SupplySummary["issues"] = [];
    const required = prescriptionRequired && document.lines.length > 0;
    if (required) {
      if (!document.supply.supervisingProfessionalId) issues.push("supervising_pharmacist_required");
      else if (!document.supply.prescriptionEndorsementConfirmed) issues.push("endorsement_not_confirmed");
      if (recordMethod === null) issues.push("prescription_record_election_unresolved");
      else if (recordMethod === "cash_or_credit_memo_book" && !document.supply.prescriptionOriginalContainerConfirmed) {
        issues.push("prescription_memo_path_ineligible");
      }
      const lineIssues = document.lines.some((each) => prescriptionFor(each).issue !== null);
      if (issues.length === 0 && !lineIssues) {
        const live = document.prescriptionRecords.filter((record) => record.status === "prepared" || record.status === "confirmed");
        if (live.length === 0) issues.push("prescription_record_not_prepared");
        else if (live.some((record) => record.status === "prepared")) issues.push("prescription_record_not_confirmed");
      }
    }
    return {
      supervisionRequired: required,
      supervisingProfessionalId: document.supply.supervisingProfessionalId,
      endorsementConfirmed: document.supply.prescriptionEndorsementConfirmed,
      recordMethod: required ? recordMethod : null,
      originalContainerConfirmed: document.supply.prescriptionOriginalContainerConfirmed,
      issues
    };
  };

  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://local.test");
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Counter User", role, revision: 1 };
    if (method !== "GET") state.requests.push({ method, path: url.pathname, body });

    if (url.pathname.endsWith("/system/info")) return response(system);
    if (url.pathname.endsWith("/auth/status")) return response({ setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" });
    if (url.pathname.endsWith("/auth/logout")) return response(null, 204);
    if (url.pathname === "/api/v1/store/tax-identity") return response({ storeId: IDs.store, displayName: "Care Pharmacy", revision: 1, gstRegistrationStatus: "registered", gstin: "27AAACX0000A1Z9", normalizedGstin: "27AAACX0000A1Z9", placeOfSupplyStateId: null, complete: true });
    if (url.pathname === "/api/v1/reference/units") return response(UNITS);
    if (url.pathname === "/api/v1/reference/state-codes") return response(STATES);
    if (url.pathname === "/api/v1/parties") return response(CUSTOMERS);
    if (url.pathname === "/api/v1/products") return response(PRODUCTS);
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return response(PRODUCTS.find((item) => item.id === url.pathname.split("/").pop()) ?? null);
    }
    if (/^\/api\/v1\/packs\/[^/]+\/sellable-batches$/.test(url.pathname)) {
      const packId = url.pathname.split("/")[4];
      return response(BATCHES.filter((batch) => batch.productPackId === packId));
    }

    if (url.pathname === "/api/v1/sales" && method === "GET") {
      if (options.failList) return failure("internal_error", 500);
      const status = url.searchParams.get("status");
      return response([...state.documents.values()]
        .filter((document) => !status || status === "all" || document.status === status)
        .map(({ lines: _lines, tenders: _tenders, ...header }) => header));
    }
    if (url.pathname === "/api/v1/sales" && method === "POST") {
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      const created = sale({ id: IDs.sale, businessDate: String(body.businessDate), lines: [] });
      state.documents.set(created.id, created);
      return response(created, 201);
    }

    const quoteMatch = /^\/api\/v1\/sales\/([^/]+)\/quote$/.exec(url.pathname);
    if (quoteMatch) {
      if (options.quoteError) return failure(options.quoteError.code, options.quoteError.status);
      return response(quoteFor(state.documents.get(quoteMatch[1])!));
    }

    const postMatch = /^\/api\/v1\/sales\/([^/]+)\/post$/.exec(url.pathname);
    if (postMatch) {
      const document = state.documents.get(postMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      const posted = postedSale({ id: document.id, businessDate: document.businessDate });
      state.documents.set(document.id, posted);
      return response(posted);
    }

    const detailMatch = /^\/api\/v1\/sales\/([^/]+)$/.exec(url.pathname);
    if (detailMatch) {
      const document = state.documents.get(detailMatch[1]);
      if (!document) return failure("sale_not_found", 404);
      if (method === "GET") return response(document);
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      const address = (body.recipientAddress ?? null) as Record<string, string | null> | null;
      const delivery = (body.deliveryAddress ?? null) as Record<string, string | null> | null;
      Object.assign(document, {
        customerPartyId: (body.customerPartyId as string | null) ?? null,
        customerNameText: (body.customerNameText as string | null) ?? null,
        // Like the real Store Service: a draft's display name is a posting snapshot and stays empty
        // until the sale is posted. Faking it here once hid a counter showing "Walk-in" for a named customer.
        customerDisplayName: null,
        businessDate: body.businessDate,
        recipientParticularsRequested: Boolean(body.recipientParticularsRequested),
        recipientAddressLine1: address?.line1 ?? null, recipientAddressLine2: address?.line2 ?? null,
        recipientCity: address?.city ?? null, recipientPostalCode: address?.postalCode ?? null,
        recipientStateId: address?.stateId ?? null,
        deliverySameAsRecipient: body.deliverySameAsRecipient ?? true,
        deliveryAddressLine1: delivery?.line1 ?? null, deliveryAddressLine2: delivery?.line2 ?? null,
        deliveryCity: delivery?.city ?? null, deliveryPostalCode: delivery?.postalCode ?? null,
        deliveryStateId: delivery?.stateId ?? null
      });
      return response(bump(document));
    }

    const linesMatch = /^\/api\/v1\/sales\/([^/]+)\/lines$/.exec(url.pathname);
    if (linesMatch) {
      const document = state.documents.get(linesMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      // The atoms come from the pack, exactly as the Store Service derives them; the browser never
      // supplies them, so the double must not accept them either.
      const quantity = Number(body.quantity);
      const atoms = body.quantityBasis === "pack" ? quantity * PACK.baseQuantityAtoms : quantity;
      document.lines.push(line({
        id: `${IDs.line}-${document.lines.length + 1}`, saleDocumentId: document.id,
        lineNumber: document.lines.length + 1, productId: String(body.productId),
        productPackId: String(body.productPackId), batchId: String(body.batchId),
        quantityBasis: body.quantityBasis as "pack" | "base_unit",
        quantityPacks: body.quantityBasis === "pack" ? quantity : null,
        quantityAtoms: atoms, sellingRatePaise: Number(body.sellingRatePaise),
        taxableValuePaise: quantity * Number(body.sellingRatePaise),
        currentProductDisplayName: PRODUCTS.find((item) => item.id === body.productId)!.displayName,
        currentBatchNumber: BATCHES.find((item) => item.id === body.batchId)!.batchNumber,
        lineTotalPaise: 0
      }));
      return response(bump(document), 201);
    }

    if (url.pathname === "/api/v1/store/professionals") return response(PROFESSIONALS);
    if (url.pathname === "/api/v1/prescriptions") {
      if (role === "cashier") return failure("authorization_denied", 403);
      const reference = url.searchParams.get("reference");
      return response(reference && reference !== PRESCRIPTION.reference ? [] : [PRESCRIPTION_SUMMARY]);
    }
    if (url.pathname === `/api/v1/prescriptions/${PRESCRIPTION.id}`) {
      if (role === "cashier") return failure("authorization_denied", 403);
      return response(PRESCRIPTION);
    }
    const linkMatch = /^\/api\/v1\/sale-lines\/([^/]+)\/prescription$/.exec(url.pathname);
    if (linkMatch) {
      if (role === "cashier") return failure("authorization_denied", 403);
      const document = [...state.documents.values()].find((item) => item.lines.some((each) => each.id === linkMatch[1]))!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      document.lines.find((each) => each.id === linkMatch[1])!.prescriptionItemId = (body.prescriptionItemId as string | null) ?? null;
      document.supply.prescriptionEndorsementConfirmed = false;
      return response(bump(document));
    }
    const supplyMatch = /^\/api\/v1\/sales\/([^/]+)\/supply$/.exec(url.pathname);
    if (supplyMatch) {
      if (role === "cashier") return failure("authorization_denied", 403);
      const document = state.documents.get(supplyMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      document.supply = {
        supervisingProfessionalId: (body.supervisingProfessionalId as string | null) ?? null,
        prescriptionEndorsementConfirmed: Boolean(body.prescriptionEndorsementConfirmed),
        prescriptionOriginalContainerConfirmed: Boolean(body.prescriptionOriginalContainerConfirmed)
      };
      return response(bump(document));
    }

    const prepareMatch = /^\/api\/v1\/sales\/([^/]+)\/prescription-records$/.exec(url.pathname);
    if (prepareMatch) {
      if (role === "cashier") return failure("authorization_denied", 403);
      const document = state.documents.get(prepareMatch[1])!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      document.prescriptionRecords = [...document.prescriptionRecords, { id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }];
      return response(bump(document));
    }

    const lineMatch = /^\/api\/v1\/sale-lines\/([^/]+)$/.exec(url.pathname);
    if (lineMatch) {
      const document = [...state.documents.values()].find((item) => item.lines.some((each) => each.id === lineMatch[1]))!;
      const conflict = guard(document, body);
      if (conflict) return conflict;
      if (options.writeError) return failure(options.writeError.code, options.writeError.status, options.writeError.extra);
      document.lines = document.lines.filter((each) => each.id !== lineMatch[1]);
      document.lines.forEach((each, index) => { each.lineNumber = index + 1; });
      return response(bump(document));
    }

    throw new Error(`Unexpected request: ${method} ${url.pathname}`);
  });

  return { state, fetchMock };
}

function renderApp(path: string, service = saleService()) {
  vi.stubGlobal("fetch", service.fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return { ...service, ...render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>) };
}

const writes = (service: ReturnType<typeof saleService>) => service.state.requests;

/**
 * Waits for a write and returns it.
 *
 * `waitFor(() => array.find(...))` resolves immediately on `undefined` rather than retrying, so a
 * test written that way asserts nothing and then fails on a property of nothing.
 */
async function waitForWrite(service: ReturnType<typeof saleService>, match: (write: { method: string; path: string }) => boolean) {
  await waitFor(() => expect(writes(service).some(match)).toBe(true));
  return writes(service).find(match)!;
}

/** The fast path a counter actually uses: find, choose the lot, price it, add. */
async function enterLine(options: { quantity: string; rate: string; basis?: "pack" | "base_unit"; batchId?: string }) {
  fireEvent.change(screen.getByLabelText("Product or barcode"), { target: { value: "Crocin" } });
  const match = await screen.findByRole("button", { name: "Crocin 500 mg Tablet" });
  fireEvent.click(match);
  await waitFor(() => expect(screen.getByRole("option", { name: "Strip of 10" })).toBeInTheDocument());
  fireEvent.change(screen.getByLabelText("Pack"), { target: { value: IDs.pack } });
  await waitFor(() => expect(screen.getByLabelText("Batch")).not.toBeDisabled());
  await waitFor(() => expect(screen.getAllByRole("option", { name: /B-2601/ }).length).toBeGreaterThan(0));
  fireEvent.change(screen.getByLabelText("Batch"), { target: { value: options.batchId ?? IDs.batch } });
  if (options.basis === "base_unit") fireEvent.click(screen.getByLabelText(/^Loose/));
  fireEvent.change(screen.getByLabelText(/^Quantity/), { target: { value: options.quantity } });
  fireEvent.change(screen.getByLabelText(/^Price per/), { target: { value: options.rate } });
  fireEvent.click(screen.getByRole("button", { name: "Add line" }));
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("sale money helpers", () => {
  it("converts a rupee price to exact paise and refuses anything else", () => {
    expect(sellingRateToPaise("85.26")).toBe(8_526);
    expect(sellingRateToPaise("12.5")).toBe(1_250);
    expect(sellingRateToPaise("0")).toBe(0);
    // A third decimal is a price nobody can charge, and rounding it silently would change the bill.
    expect(sellingRateToPaise("12.555")).toBeNull();
    expect(sellingRateToPaise("-1")).toBeNull();
    expect(sellingRateToPaise("")).toBeNull();
  });

  it("refuses a fractional quantity instead of silently truncating it", () => {
    expect(quantityToInteger("3")).toBe(3);
    // `parseInt("2.5")` is 2, which would bill two packs for a request that said two and a half.
    expect(quantityToInteger("2.5")).toBeNull();
    expect(quantityToInteger("0")).toBeNull();
    expect(quantityToInteger("abc")).toBeNull();
  });

  it("groups a large amount the Indian way without dividing", () => {
    expect(paiseToAmountText(17_920)).toBe("179.20");
    // 12 34 567.89 — the Indian grouping, split on the digits rather than by dividing.
    expect(paiseToAmountText(123_456_789)).toBe("12,34,567.89");
    expect(paiseToAmountText(5)).toBe("0.05");
  });
});

describe("Sales list", () => {
  it("lists invoices and marks a draft as unposted", async () => {
    renderApp("/app/sales", saleService({ documents: [sale(), postedSale()] }));
    expect(await screen.findByRole("heading", { name: "Sales", level: 1 })).toBeInTheDocument();
    expect(await screen.findByRole("link", { name: "INV/2627/000001" })).toBeInTheDocument();
    const rows = screen.getAllByRole("row");
    expect(rows.some((row) => within(row).queryByText("Not posted"))).toBe(true);
    expect(screen.getByText("179.20")).toBeInTheDocument();
  });

  it("reports a list failure instead of showing an empty shop", async () => {
    renderApp("/app/sales", saleService({ failList: true }));
    expect(await screen.findByRole("heading", { name: "Sales could not be loaded" })).toBeInTheDocument();
  });
});

describe("Point of sale", () => {
  it("opens straight into a walk-in bill without asking for a customer first", async () => {
    const service = saleService({ documents: [] });
    renderApp("/app/sales/new", service);
    await waitFor(() => expect(writes(service).some((write) => write.path === "/api/v1/sales")).toBe(true));
    const created = writes(service).find((write) => write.path === "/api/v1/sales")!;
    // A counter does not fill in a header before it can start billing.
    expect(created.body.customerPartyId).toBeUndefined();
    expect(created.body.businessDate).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  it("derives the atoms on the server and never sends them from the browser", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "2", rate: "80" });

    await waitFor(() => expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(true));
    const added = writes(service).find((write) => write.path.endsWith("/lines"))!;
    expect(added.body).toMatchObject({ quantityBasis: "pack", quantity: 2, sellingRatePaise: 8_000 });
    expect(added.body).not.toHaveProperty("quantityAtoms");
    // The 20 atoms come back from the service, and the bill states the unit.
    expect(await screen.findByText("2 Strip of 10s")).toBeInTheDocument();
  });

  it("sells loose units in the product's own unit, not in packs", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "3", rate: "8.52", basis: "base_unit" });

    const added = await waitForWrite(service, (write) => write.path.endsWith("/lines"));
    expect(added.body).toMatchObject({ quantityBasis: "base_unit", quantity: 3, sellingRatePaise: 852 });
    expect(await screen.findByText("3 Tablets")).toBeInTheDocument();
  });

  it("labels the quantity and the price in the unit actually being sold", async () => {
    renderApp(`/app/sales/${IDs.sale}`);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.change(screen.getByLabelText("Product or barcode"), { target: { value: "Crocin" } });
    fireEvent.click(await screen.findByRole("button", { name: "Crocin 500 mg Tablet" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "Strip of 10" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Pack"), { target: { value: IDs.pack } });

    // Whole packs are priced per pack, using the pack's own name.
    expect(screen.getByLabelText("Quantity (Strip of 10)")).toBeInTheDocument();
    expect(screen.getByLabelText("Price per Strip of 10 (₹)")).toBeInTheDocument();
    // Loose units are priced per tablet, and the label changes with the basis.
    fireEvent.click(screen.getByLabelText(/^Loose/));
    expect(screen.getByLabelText("Quantity (Tablet)")).toBeInTheDocument();
    expect(screen.getByLabelText("Price per Tablet (₹)")).toBeInTheDocument();
  });

  it("shows what is on hand for each lot and warns before an expired one is chosen", async () => {
    renderApp(`/app/sales/${IDs.sale}`);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.change(screen.getByLabelText("Product or barcode"), { target: { value: "Crocin" } });
    fireEvent.click(await screen.findByRole("button", { name: "Crocin 500 mg Tablet" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "Strip of 10" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Pack"), { target: { value: IDs.pack } });
    await waitFor(() => expect(screen.getByRole("option", { name: /B-2601/ })).toBeInTheDocument());

    // The lot the counter can use says what is left; the expired one is shown and marked, not hidden.
    expect(screen.getByRole("option", { name: /B-2601 · exp 2028-03-31 · 100 on hand/ })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /B-OLD · exp 2026-01-31 · EXPIRED/ })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Batch"), { target: { value: IDs.expiredBatch } });
    expect(screen.getByText("This batch has expired and cannot be sold.")).toBeInTheDocument();
  });

  it("refuses an expired lot at the browser as well as at the service", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "1", rate: "80", batchId: IDs.expiredBatch });

    expect(await screen.findByRole("alert")).toHaveTextContent("That batch has expired and cannot be sold.");
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
    expect(screen.getByLabelText("Batch")).toHaveFocus();
  });

  it("shows the printed MRP of the chosen lot so the operator can price within it", async () => {
    renderApp(`/app/sales/${IDs.sale}`);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.change(screen.getByLabelText("Product or barcode"), { target: { value: "Crocin" } });
    fireEvent.click(await screen.findByRole("button", { name: "Crocin 500 mg Tablet" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "Strip of 10" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Pack"), { target: { value: IDs.pack } });
    await waitFor(() => expect(screen.getByRole("option", { name: /B-2601/ })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Batch"), { target: { value: IDs.batch } });

    expect(screen.getByText("Printed MRP 95.50 a Strip of 10, inclusive of GST.")).toBeInTheDocument();
  });

  it("refuses a fractional quantity at the counter rather than truncating it", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "2.5", rate: "80" });

    expect(await screen.findByRole("alert")).toHaveTextContent("Enter a whole number of Strip of 10.");
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
    expect(screen.getByLabelText(/^Quantity/)).toHaveFocus();
  });

  it("refuses a price with a third decimal instead of rounding the bill", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "1", rate: "85.267" });

    expect(await screen.findByRole("alert")).toHaveTextContent("to at most two decimals");
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  it("returns the focus to the search box so the next line needs no mouse", async () => {
    renderApp(`/app/sales/${IDs.sale}`);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "2", rate: "80" });
    await screen.findByText("2 Strip of 10s");
    expect(screen.getByLabelText("Product or barcode")).toHaveFocus();
    // And the entry row is empty, ready for the next item.
    expect(screen.getByLabelText("Product or barcode")).toHaveValue("");
  });

  it("shows the amount payable from the service rather than adding up the tax itself", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });

    const summary = await screen.findByLabelText("Bill total");
    // 160.00 taxable at 6% + 6% is 9.60 each and 179.20 in all — computed by the Store Service.
    await waitFor(() => expect(within(summary).getByText("179.20")).toBeInTheDocument());
    expect(within(summary).getByText("160.00")).toBeInTheDocument();
    expect(within(summary).getAllByText("9.60")).toHaveLength(2);
    expect(within(summary).getByText("1 line")).toBeInTheDocument();
    // The figure is fetched, never derived here.
    expect(service.fetchMock.mock.calls.some(([input]) => String(input).includes("/quote"))).toBe(true);
  });

  it("says the amount on the post button so nobody has to read it off a table", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByRole("button", { name: "Take 179.20 and post" })).toBeInTheDocument();
  });

  it("will not post an empty bill", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(screen.getByRole("button", { name: "Post" })).toBeDisabled();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
  });

  it("refuses to total a bill the service would refuse, and blocks posting until it is fixed", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      documents: [sale({ lines: [line({ sellingRatePaise: 8_527 })] })],
      quoteError: { code: "selling_rate_above_mrp", status: 409 }
    }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const summary = await screen.findByLabelText("Bill total");
    await waitFor(() => expect(within(summary).getByRole("alert")).toHaveTextContent("exceeds this batch's printed MRP"));
    expect(within(summary).getByRole("alert")).toHaveTextContent("cannot be totalled");
    expect(screen.getByRole("button", { name: "Post" })).toBeDisabled();
  });

  it("posts exactly the quoted amount as a single tender", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));

    await waitFor(() => expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(true));
    const posted = writes(service).find((write) => write.path.endsWith("/post"))!;
    expect(posted.body.tenders).toEqual([{ method: "cash", amountPaise: 17_920, referenceText: null }]);
    expect(posted.body.idempotencyKey).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("records a card reference with the tender when one is given", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.change(screen.getByLabelText("Paid by"), { target: { value: "card" } });
    fireEvent.change(screen.getByLabelText("Reference"), { target: { value: "APPROVAL-9921" } });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));

    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body.tenders).toEqual([{ method: "card", amountPaise: 17_920, referenceText: "APPROVAL-9921" }]);
  });

  /**
   * Three clicks arriving in ONE task, before React can re-render.
   *
   * `disabled={post.isPending}` is not enough on its own for this: the flag only becomes true after
   * a render, and every click in the same task sees the button as it was. Separate clicks — even
   * Playwright's `clickCount: 3` — do give React a chance to paint between them, which is why this
   * has to be written as one flush to be a real test of the guard rather than of the disabled
   * attribute. A duplicated event or a stuck button really does deliver clicks this way, and a
   * second invoice at a counter is worse than a duplicate purchase: the goods are already gone.
   */
  /** Phase 1M-A: an ordinary line shows nothing extra — the counter is not slowed for toothpaste. */
  it("adds no regulatory noise to a line the Drugs Rules gate clears", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByRole("button", { name: "Take 179.20 and post" })).toBeEnabled();
    expect(screen.queryByTestId("regulatory-blocked")).not.toBeInTheDocument();
    expect(screen.queryByText(/Classification unresolved/)).not.toBeInTheDocument();
  });

  /** Phase 1M-A: an unclassified medicine is named on its line and posting is withheld. */
  it("withholds posting and names the line when a medicine is unclassified", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })], regulatoryGate: "unresolved" });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByText(/Classification unresolved — not sellable until recorded/)).toBeInTheDocument();
    expect(screen.getByTestId("regulatory-blocked")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
  });

  /** Phase 1M-A: a classified Schedule H1 line waits for the prescription workflow; no fake form. */
  // Phase 1M-C: an H1 line is refused only where its NDPS purview applies or is unrecorded — the
  // unsupported NDPS-intersection workflow. The supported H1 path is proved further below.
  it("withholds posting for an NDPS-intersection Schedule H1 line without offering a prescription form", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      documents: [sale({ lines: [line()] })],
      regulatoryGate: "workflow_unavailable",
      regulatoryGateScheme: "ndps_purview"
    }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect((await screen.findAllByText((_, element) => element?.tagName === "SMALL" && (element.textContent ?? "").includes("an unsupported NDPS-intersection workflow AUSHADHARTH does not support"))).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    expect(screen.queryByLabelText(/prescri/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/patient/i)).not.toBeInTheDocument();
  });

  // --- Phase 1M-B: Schedule H at the counter ------------------------------------------------

  it("shows nothing about prescriptions on an ordinary sale", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByRole("button", { name: "Take 179.20 and post" })).toBeEnabled();
    expect(screen.queryByTestId("prescription-panel")).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/prescri/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/pharmacist/i)).not.toBeInTheDocument();
  });

  it("names the Schedule H line, lists what is missing and withholds posting", async () => {
    const service = saleService({ role: "pharmacist", documents: [sale({ lines: [line()] })], regulatoryGate: "prescription_required" });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByText("Schedule H — sold only on a prescription")).toBeInTheDocument();
    expect(await screen.findByTestId("prescription-panel")).toBeInTheDocument();
    expect(screen.getByTestId("prescription-issue-1")).toHaveTextContent("Needs a prescription linked to it.");
    expect(screen.getByTestId("supply-issues")).toHaveTextContent("Choose the registered pharmacist supervising this supply.");
    expect(screen.getByTestId("prescription-incomplete")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    // No claim of verification anywhere on the counter.
    expect(screen.queryByText(/verified/i)).not.toBeInTheDocument();
  });

  it("links a prescription by its reference and shows what it has left", async () => {
    const service = saleService({ role: "pharmacist", documents: [sale({ lines: [line()] })], regulatoryGate: "prescription_required" });
    renderApp(`/app/sales/${IDs.sale}`, service);
    const reference = await screen.findByLabelText("Prescription reference");
    fireEvent.change(reference, { target: { value: "rx-000001" } });
    fireEvent.click(screen.getByRole("button", { name: "Link" }));
    const link = await waitForWrite(service, (write) => write.path.endsWith("/prescription"));
    expect(link.path).toBe(`/api/v1/sale-lines/${IDs.line}/prescription`);
    expect((link as { body: Record<string, unknown> }).body).toEqual({ expectedRevision: 1, prescriptionItemId: PRESCRIPTION_ITEM_ID });
    expect(await screen.findByTestId("prescription-state-1")).toHaveTextContent("Prescription RX-000001 linked · 30 Tablet left before this sale");
  });

  it("refuses at the counter a prescription that names another product", async () => {
    const service = saleService({
      role: "pharmacist",
      documents: [sale({ lines: [line({ productId: "01997a00-0000-7000-8000-0000000000ee" })] })],
      regulatoryGate: "prescription_required"
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    fireEvent.change(await screen.findByLabelText("Prescription reference"), { target: { value: "RX-000001" } });
    fireEvent.click(screen.getByRole("button", { name: "Link" }));
    expect(await screen.findByText("RX-000001 does not name this product. Another preparation cannot be supplied in its place.")).toBeInTheDocument();
    expect(writes(service).some((write) => write.path.endsWith("/prescription"))).toBe(false);
  });

  it("records the supervising registered pharmacist and the endorsement confirmation, then posts", async () => {
    const service = saleService({
      role: "pharmacist",
      documents: [sale({ lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })] })],
      regulatoryGate: "prescription_required"
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    const select = await screen.findByLabelText("Supervising registered pharmacist");
    // Only registered pharmacists are offered; a competent person is not.
    await waitFor(() => expect(within(select).getByRole("option", { name: "Meera Iyer" })).toBeInTheDocument());
    expect(within(select).queryByRole("option", { name: "Store Manager" })).not.toBeInTheDocument();
    fireEvent.change(select, { target: { value: PROFESSIONALS[0].id } });
    fireEvent.click(screen.getByLabelText(/I have written the pharmacy's name and address/));
    fireEvent.click(screen.getByRole("button", { name: "Record supervision" }));
    const supply = await waitForWrite(service, (write) => write.path.endsWith("/supply"));
    expect((supply as { body: Record<string, unknown> }).body).toEqual({
      expectedRevision: 1, supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true,
      prescriptionOriginalContainerConfirmed: false
    });
    expect(screen.getByTestId("record-method")).toHaveTextContent("Statutory record: Prescription register (rule 65(3)(2) election)");
    expect(screen.queryByLabelText(/original container/)).not.toBeInTheDocument();
    expect(await screen.findByText(/Registered-pharmacist record selected: Meera Iyer/)).toBeInTheDocument();
    // Everything else is in order, so the statutory entry is prepared next — and posting still waits.
    const post = await screen.findByRole("button", { name: "Take 179.20 and post" });
    expect(post).toBeDisabled();
    fireEvent.click(await screen.findByRole("button", { name: "Prepare statutory record" }));
    const prepared = await waitForWrite(service, (write) => write.path.endsWith("/prescription-records"));
    expect((prepared as { body: Record<string, unknown> }).body).toEqual({ expectedRevision: 2 });
    expect(await screen.findByTestId("live-record-serial")).toHaveTextContent("PR-000001");
    expect(screen.getByTestId("live-record-status")).toHaveTextContent("Prepared — awaiting the pharmacist's handwritten signature");
    const step = screen.getByTestId("statutory-record-step");
    expect(step).toHaveTextContent("The registered pharmacist signs it by hand in the signature box. AUSHADHARTH does not sign it.");
    expect(step).toHaveTextContent("Write PR-000001 on the prescription.");
    expect(within(step).getByRole("link", { name: "Open the entry to print and confirm" })).toHaveAttribute("href", "/app/prescription-records/01997a00-0000-7000-8000-0000000000e1");
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
  });

  it("posts only once the prepared entry's signature and serial are confirmed", async () => {
    const service = saleService({
      role: "cashier",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false },
        prescriptionRecords: [{ ...{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }, status: "confirmed" }]
      })],
      regulatoryGate: "prescription_required"
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    expect(await screen.findByTestId("live-record-status")).toHaveTextContent("Signature and serial confirmed — the sale can be posted");
    // A cashier can post the confirmed supply but cannot open the entry that names the patient.
    expect(within(screen.getByTestId("statutory-record-step")).queryByRole("link")).not.toBeInTheDocument();
    const post = await screen.findByRole("button", { name: "Take 179.20 and post" });
    await waitFor(() => expect(post).toBeEnabled());
    fireEvent.click(post);
    await waitForWrite(service, (write) => write.path.endsWith("/post"));
  });

  it("keeps posting shut while the entry awaits its signature, and a cashier cannot confirm it", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      role: "cashier",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false },
        prescriptionRecords: [{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }]
      })],
      regulatoryGate: "prescription_required"
    }));
    expect(await screen.findByTestId("live-record-serial")).toHaveTextContent("PR-000001");
    expect(screen.getByText("A pharmacist or the owner confirms the signature. A cashier cannot.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Prepare statutory record" })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled());
    expect(document.body.textContent).not.toMatch(/signed by AUSHADHARTH|digitally signed|electronically signed/i);
  });

  /** Phase 1M-C (C-11, C-58, C-60, C-67): the separate H1 layer sits beside the statutory record,
   *  per supply, with its own physical steps; only a dispensing role is sent to confirm it. */
  it("shows the separate Schedule H1 layer beside the statutory record, per supply", async () => {
    for (const role of ["pharmacist", "cashier"] as const) {
      renderApp(`/app/sales/${IDs.sale}`, saleService({
        role,
        documents: [sale({
          lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
          supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false },
          prescriptionRecords: [{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "confirmed" as const, prescriptionReference: "RX-000001" }],
          h1RegisterEntries: [{ id: "01997a00-0000-7000-8000-0000000000f1", reference: "AH1-000001", status: "prepared" as const, lineNumber: 1 }]
        })],
        regulatoryGate: "prescription_required",
        regulatoryGateScheme: "schedule_h1"
      }));
      const layer = await screen.findByTestId("live-h1");
      expect(within(layer).getByTestId("live-h1-reference")).toHaveTextContent("AH1-000001");
      expect(layer).toHaveTextContent("AUSHADHARTH Reference");
      expect(layer).toHaveTextContent("Place it in the pharmacy's separate Schedule H1 register.");
      expect(layer).toHaveTextContent("The registered pharmacist authenticates it by hand. AUSHADHARTH does not sign it.");
      if (role === "pharmacist") {
        expect(within(layer).getByRole("link", { name: "Open the H1 hard copy to print and confirm" })).toHaveAttribute("href", `/app/sales/${IDs.sale}/h1-register`);
      } else {
        expect(within(layer).queryByRole("link")).not.toBeInTheDocument();
        expect(layer).toHaveTextContent("A pharmacist or the owner confirms the H1 hard copy. A cashier cannot.");
      }
      expect(screen.getByText("Schedule H1 — sold only on a prescription, with its separate H1 register entry")).toBeInTheDocument();
      expect(document.body.textContent).not.toMatch(/end of day|signed by AUSHADHARTH|digitally signed/i);
      cleanup();
    }
  });

  /** Phase 1M-C (C-64, C-66, C-70): the NDPS and Punjab refusals say what AUSHADHARTH does not
   *  support, never that a sale is prohibited or a drug banned. */
  it("names the NDPS and Punjab boundaries as unsupported workflows, not prohibitions", async () => {
    for (const [scheme, text] of [
      ["punjab_restricted_supply", "This drug is subject to an additional Punjab drug-control workflow that AUSHADHARTH does not yet support."],
      ["ndps_purview", "an unsupported NDPS-intersection workflow AUSHADHARTH does not support"]
    ] as const) {
      renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })], regulatoryGate: "workflow_unavailable", regulatoryGateScheme: scheme }));
      expect((await screen.findAllByText((_, element) => element?.tagName === "SMALL" && (element.textContent ?? "").includes(text))).length).toBeGreaterThan(0);
      expect(document.body.textContent).not.toMatch(/banned|prohibited|illegal/i);
      cleanup();
    }
  });

  it("lets a cashier see the state but not link prescriptions or name the pharmacist", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ role: "cashier", documents: [sale({ lines: [line()] })], regulatoryGate: "prescription_required" }));
    expect(await screen.findByTestId("prescription-panel")).toBeInTheDocument();
    expect(screen.getByText(/A pharmacist or the owner links the prescription/)).toBeInTheDocument();
    expect(screen.queryByLabelText("Prescription reference")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Supervising registered pharmacist")).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Enter new prescription" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Prescriptions" })).not.toBeInTheDocument();
  });

  it("does not let a cashier post a supervised Schedule H supply before its entry is prepared and signed", async () => {
    const service = saleService({
      role: "cashier",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false }
      })],
      regulatoryGate: "prescription_required"
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    expect(await screen.findByText("A pharmacist or the owner prepares the entry.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Prepare statutory record" })).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled());
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
  });

  it("names a repeat the prescription does not authorise", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      role: "pharmacist",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false }
      })],
      regulatoryGate: "prescription_required",
      prescriptionIssue: "prescription_repeat_not_authorised",
      remainingAtoms: 20
    }));
    expect(await screen.findByTestId("prescription-issue-1")).toHaveTextContent("The prescription does not authorise dispensing it again.");
    expect(screen.getByTestId("prescription-state-1")).toHaveTextContent("20 Tablet left before this sale");
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
  });

  it("keeps a Schedule X line refused with its own wording", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      documents: [sale({ lines: [line()] })],
      regulatoryGate: "workflow_unavailable",
      regulatoryGateScheme: "schedule_x"
    }));
    expect(await screen.findAllByText("Schedule X dispensing requires the Schedule X workflow, which is not yet available.")).not.toHaveLength(0);
    expect(screen.getByTestId("regulatory-blocked")).toHaveTextContent("Line 1 cannot be sold yet");
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    expect(screen.queryByTestId("prescription-panel")).not.toBeInTheDocument();
  });

  it("itemises a posting refused for prescription requirements", async () => {
    const service = saleService({
      role: "pharmacist",
      documents: [sale({ lines: [line()] })],
      writeError: {
        code: "prescription_requirements_incomplete",
        status: 409,
        extra: { issues: [{ field: "lines.1.prescription_quantity_exceeded", message: "This is more than the prescription has left." }] }
      }
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));
    expect(await screen.findByText("This prescription sale is missing prescription requirements. This is more than the prescription has left.")).toBeInTheDocument();
  });

  it("says when no rule 65(3)(2) election is recorded and withholds posting", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      role: "pharmacist",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false }
      })],
      regulatoryGate: "prescription_required",
      recordMethod: null
    }));
    expect(await screen.findByTestId("record-method")).toHaveTextContent("no rule 65(3)(2) election is recorded");
    expect(screen.getByTestId("supply-issues")).toHaveTextContent("The owner records it in Drug Compliance.");
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
  });

  it("asks a memo-book pharmacy for the original-container attestation and sends it", async () => {
    const service = saleService({
      role: "pharmacist",
      documents: [sale({
        lines: [line({ prescriptionItemId: PRESCRIPTION_ITEM_ID })],
        supply: { supervisingProfessionalId: PROFESSIONALS[0].id, prescriptionEndorsementConfirmed: true, prescriptionOriginalContainerConfirmed: false }
      })],
      regulatoryGate: "prescription_required",
      recordMethod: "cash_or_credit_memo_book"
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    expect(await screen.findByTestId("record-method")).toHaveTextContent("Cash or credit memo book");
    expect(screen.getByTestId("supply-issues")).toHaveTextContent("original container");
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    fireEvent.click(await screen.findByLabelText(/Supplied from or in the manufacturer's original container/));
    fireEvent.click(screen.getByRole("button", { name: "Record supervision" }));
    const supply = await waitForWrite(service, (write) => write.path.endsWith("/supply"));
    expect((supply as { body: Record<string, unknown> }).body.prescriptionOriginalContainerConfirmed).toBe(true);
    expect(await screen.findByRole("button", { name: "Prepare statutory record" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
  });

  it("shows a cashier the finalized entry's serial and state after posting, without a link into it", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({
      role: "cashier",
      documents: [postedSale({ prescriptionRecords: [{ ...{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }, status: "finalized" }] })]
    }));
    const section = await screen.findByTestId("sale-records");
    expect(section).toHaveTextContent("PR-000001 · Prescription register · for RX-000001 · Finalized with the posted sale");
    expect(within(section).queryByRole("link")).not.toBeInTheDocument();
  });

  it("offers a pharmacist the finalized entry, and names a voided serial truthfully", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({
      role: "pharmacist",
      documents: [postedSale({ prescriptionRecords: [
        { ...{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }, status: "void" },
        { ...{ id: "01997a00-0000-7000-8000-0000000000e1", serialNumber: "PR-000001", recordMethod: "prescription_register" as const, status: "prepared" as const, prescriptionReference: "RX-000001" }, id: "01997a00-0000-7000-8000-0000000000e2", serialNumber: "PR-000002", status: "finalized" }
      ] })]
    }));
    const section = await screen.findByTestId("sale-records");
    expect(section).toHaveTextContent("PR-000001 · Prescription register · for RX-000001 · Void — cancelled before the supply; serial not reused");
    expect(within(section).getAllByRole("link", { name: "Open the entry" })[1]).toHaveAttribute("href", "/app/prescription-records/01997a00-0000-7000-8000-0000000000e2");
  });

  it("sends one posting even when three clicks land before React can re-render", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const button = await screen.findByRole("button", { name: "Take 179.20 and post" });

    await act(async () => {
      button.click();
      button.click();
      button.click();
    });

    await waitFor(() => expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(true));
    expect(writes(service).filter((write) => write.path.endsWith("/post"))).toHaveLength(1);
  });

  it("reads the item, the batch and the unit on a draft whose snapshot is still empty", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });

    const table = await screen.findByRole("table");
    expect(within(table).getByText("Crocin 500 mg Tablet")).toBeInTheDocument();
    expect(within(table).getByText("B-2601")).toBeInTheDocument();
    expect(within(table).getByText("2 Strip of 10s")).toBeInTheDocument();
    // The raw identifier never reaches the counter.
    expect(within(table).queryByText(IDs.product)).not.toBeInTheDocument();
  });

  /**
   * A scanner types the code and sends Enter. Before that Enter was handled, it fell through to the
   * form and produced "Find the product being sold" for a product the operator had just found —
   * which at a counter reads as the software not listening. Found in the browser, not by a test.
   */
  it("takes the match on Enter instead of submitting an incomplete line", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });

    const search = screen.getByLabelText("Product or barcode");
    fireEvent.change(search, { target: { value: "Crocin" } });
    await screen.findByRole("button", { name: "Crocin 500 mg Tablet" });
    fireEvent.keyDown(search, { key: "Enter" });

    await waitFor(() => expect(search).toHaveValue("Crocin 500 mg Tablet"));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(writes(service).some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  /** Choosing the product is enough when there is only one thing it could mean. */
  it("selects the only active pack itself and leaves an ambiguous one to the operator", async () => {
    renderApp(`/app/sales/${IDs.sale}`);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.change(screen.getByLabelText("Product or barcode"), { target: { value: "Crocin" } });
    fireEvent.click(await screen.findByRole("button", { name: "Crocin 500 mg Tablet" }));

    await waitFor(() => expect(screen.getByLabelText("Pack")).toHaveValue(IDs.pack));
    // And the labels follow it, so the next two fields already name the right unit.
    expect(screen.getByLabelText("Quantity (Strip of 10)")).toBeInTheDocument();
  });

  it("removes a line and renumbers the bill", async () => {
    const service = saleService({ documents: [sale({ lines: [line(), line({ id: `${IDs.line}-2`, lineNumber: 2 })] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[0]);

    await waitFor(() => expect(screen.getAllByRole("button", { name: "Remove" })).toHaveLength(1));
    expect(writes(service).some((write) => write.method === "DELETE")).toBe(true);
  });

  it("reports a stale document rather than overwriting somebody else's change", async () => {
    const service = saleService({
      documents: [sale({ lines: [line()] })],
      writeError: { code: "revision_conflict", status: 409, extra: { expectedRevision: 1, currentRevision: 4 } }
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));

    expect(await screen.findByText(/changed after you opened it/)).toBeInTheDocument();
  });

  // Phase 1L-A3: a refusal that names what is missing says each thing, not just "incomplete".
  it("itemises every fact the posted document still needs", async () => {
    const issues = [
      { field: "lines[1].hsnCode", message: "Record a 4-digit HSN code for line 1 before posting." },
      { field: "store.licences.includeOnRetailMemo", message: "Mark which licence is printed on retail memos in Store Profile." }
    ];
    const service = saleService({
      documents: [sale({ lines: [line()] })],
      writeError: { code: "sale_compliance_incomplete", status: 409, extra: { issues } }
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));

    const alert = await screen.findByText(/needs details recorded before it can be posted/);
    expect(alert).toHaveTextContent("Record a 4-digit HSN code for line 1 before posting.");
    expect(alert).toHaveTextContent("Mark which licence is printed on retail memos in Store Profile.");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });

  it("refuses to total a bill while the pharmacy's GST registration is unrecorded", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({
      documents: [sale({ lines: [line()] })],
      quoteError: { code: "store_gst_status_unresolved", status: 409 }
    }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const summary = await screen.findByLabelText("Bill total");
    await waitFor(() => expect(within(summary).getByRole("alert")).toHaveTextContent("whether this pharmacy is GST-registered"));
    // Not blamed on a line the cashier could "correct".
    expect(within(summary).getByRole("alert")).not.toHaveTextContent("that line is corrected");
    expect(screen.getByRole("button", { name: "Post" })).toBeDisabled();
  });

  // Phase 1L-A3 corrective — an unregistered pharmacy's counter.
  it("totals an unregistered pharmacy's bill without GST wording", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ seller: "unregistered", documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const summary = await screen.findByLabelText("Bill total");
    await waitFor(() => expect(within(summary).getByText("Not charged")).toBeInTheDocument());
    expect(within(summary).queryByText("CGST")).not.toBeInTheDocument();
    expect(within(summary).queryByText("SGST")).not.toBeInTheDocument();
    expect(within(summary).queryByText("Taxable value")).not.toBeInTheDocument();
    expect(within(summary).getByText(/no GST is charged/)).toBeInTheDocument();
    expect(within(summary).queryByText(/charges the GST/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take 160.00 and post" })).toBeEnabled();
  });

  it("offers an unregistered pharmacy no GST-invoice request and never sends one", async () => {
    const service = saleService({
      seller: "unregistered",
      documents: [sale({ lines: [line()], recipientParticularsRequested: true })]
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await screen.findByRole("button", { name: "Take 160.00 and post" });
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    expect(screen.queryByLabelText(/Customer asked for their details/)).not.toBeInTheDocument();
    expect(screen.getByText(/retail cash memo, not a GST tax invoice/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));
    const saved = await waitForWrite(service, (write) => write.method === "PUT");
    expect(saved.body.recipientParticularsRequested).toBe(false);
  });

  it("keeps the Rule 46(f) request for a registered pharmacy", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await screen.findByRole("button", { name: "Take 179.20 and post" });
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    expect(screen.getByLabelText(/Customer asked for their details/)).toBeInTheDocument();
    expect(screen.queryByText(/retail cash memo, not a GST tax invoice/)).not.toBeInTheDocument();
  });

  // Phase 1L-A4 — Notification No. 14/2020-CT and document issuability at the counter.
  it("requires the transaction reference for a UPI payment when Dynamic QR applies", async () => {
    const service = saleService({ dynamicQr: "required", documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await screen.findByRole("button", { name: "Take 179.20 and post" });
    fireEvent.change(screen.getByLabelText("Paid by"), { target: { value: "upi" } });
    const reference = screen.getByLabelText(/Transaction reference/);
    expect(reference).toHaveAttribute("aria-required", "true");
    expect(screen.getByText(/recorded on the invoice with the amount, mode and time/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Take 179.20 and post" }));
    expect(await screen.findByText("Enter the card or UPI transaction reference before posting.")).toBeInTheDocument();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
    expect(document.activeElement).toBe(reference);

    fireEvent.change(reference, { target: { value: "  UPI-4471 " } });
    fireEvent.click(screen.getByRole("button", { name: "Take 179.20 and post" }));
    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body.tenders).toEqual([{ method: "upi", amountPaise: 17_920, referenceText: "UPI-4471" }]);
  });

  it("asks nothing more of a cash payment when Dynamic QR applies", async () => {
    const service = saleService({ dynamicQr: "required", documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));
    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body.tenders).toEqual([{ method: "cash", amountPaise: 17_920, referenceText: null }]);
  });

  it("keeps the reference optional where Dynamic QR does not apply", async () => {
    const service = saleService({ dynamicQr: "not_required", documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await screen.findByRole("button", { name: "Take 179.20 and post" });
    fireEvent.change(screen.getByLabelText("Paid by"), { target: { value: "card" } });
    expect(screen.getByLabelText("Reference")).not.toHaveAttribute("aria-required", "true");
    fireEvent.click(screen.getByRole("button", { name: "Take 179.20 and post" }));
    const posted = await waitForWrite(service, (write) => write.path.endsWith("/post"));
    expect(posted.body.tenders).toEqual([{ method: "card", amountPaise: 17_920, referenceText: null }]);
  });

  it("stops the counter when the Dynamic QR answer is not recorded", async () => {
    const service = saleService({ dynamicQr: "unknown", documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByText("Dynamic QR requirement not recorded")).toBeInTheDocument();
    expect(screen.getByText(/until the owner records it/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
  });

  it("stops a registered customer's mixed taxable and untaxed bill before posting", async () => {
    const service = saleService({ mixedSupply: true, documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByText("Taxable and untaxed items for a GST-registered customer")).toBeInTheDocument();
    expect(screen.getByText(/Bill the taxable and untaxed items separately/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Take 179.20 and post" })).toBeDisabled();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);
  });

  it("explains the service's payment-reference refusal in the counter's words", async () => {
    const service = saleService({
      documents: [sale({ lines: [line()] })],
      writeError: { code: "payment_reference_required", status: 409, extra: { issues: [{ field: "tenders[0].referenceText", message: "Enter the transaction reference." }] } }
    });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(await screen.findByRole("button", { name: "Take 179.20 and post" }));
    expect(await screen.findByText(/Enter the card or UPI transaction reference before posting/)).toBeInTheDocument();
    expect(screen.queryByText("raw backend detail")).not.toBeInTheDocument();
  });

  it("names a registered customer without making every sale ask for one", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(screen.getByText(/^Walk-in/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "Rahul Deshmukh" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Registered customer"), { target: { value: IDs.customer } });
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));

    await waitFor(() => expect(writes(service).some((write) => write.method === "PUT" && write.path === `/api/v1/sales/${IDs.sale}`)).toBe(true));
    const saved = writes(service).find((write) => write.method === "PUT")!;
    expect(saved.body.customerPartyId).toBe(IDs.customer);
  });

  it("keeps a name typed for a walk-in as invoice text and not as a customer record", async () => {
    const service = saleService();
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    fireEvent.change(screen.getByLabelText("Name on the bill"), { target: { value: "Anita" } });
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));

    const saved = await waitForWrite(service, (write) => write.method === "PUT");
    expect(saved.body).toMatchObject({ customerPartyId: null, customerNameText: "Anita" });
  });

  it("presents a service refusal in the counter's own words", async () => {
    const service = saleService({ writeError: { code: "insufficient_stock", status: 409, extra: { availableAtoms: 4 } } });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    await enterLine({ quantity: "50", rate: "80" });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("This would leave a negative stock balance.");
    expect(alert).not.toHaveTextContent("raw backend detail");
  });
});

describe("Recipient particulars", () => {
  /** A draft whose one line alone is worth `taxable` paise. */
  const worth = (taxable: number, overrides: Partial<SaleDetail> = {}) =>
    sale({ lines: [line({ quantityPacks: 1, quantityAtoms: 10, sellingRatePaise: taxable, taxableValuePaise: taxable })], ...overrides });

  it("keeps an ordinary walk-in bill free of any address form", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(await screen.findByRole("button", { name: "Take 179.20 and post" })).toBeEnabled();
    expect(screen.queryByText("Customer details needed before posting")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Address line 1")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Change customer" })).toBeInTheDocument();

    // Opening the customer panel still shows no address until something asks for one.
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    expect(await screen.findByLabelText(/Customer asked for their details/)).not.toBeChecked();
    expect(screen.queryByLabelText("Address line 1")).not.toBeInTheDocument();
  });

  it("asks for the customer's details once the taxable value reaches ₹50,000, and will not post without them", async () => {
    const service = saleService({ documents: [worth(THRESHOLD)] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const summary = await screen.findByLabelText("Bill total");
    await within(summary).findByText("Customer details needed before posting");
    expect(within(summary).getByText("Enter the customer's address.")).toBeInTheDocument();
    const post = within(summary).getByRole("button", { name: /and post$/ });
    expect(post).toBeDisabled();
    expect(writes(service).some((write) => write.path.endsWith("/post"))).toBe(false);

    fireEvent.click(within(summary).getByRole("button", { name: "Add customer details" }));
    expect(await screen.findByLabelText("Address line 1")).toBeInTheDocument();
    expect(screen.getByText(/taxable value is ₹50,000 or more/)).toBeInTheDocument();
    expect(screen.getByLabelText("Goods delivered to the same address")).toBeChecked();
  });

  it("does not ask at ₹49,999.99 of taxable value even though GST takes the bill past ₹50,000", async () => {
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [worth(THRESHOLD - 1)] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    const post = await screen.findByRole("button", { name: /and post$/ });
    expect(post).toBeEnabled();
    expect(screen.queryByText("Customer details needed before posting")).not.toBeInTheDocument();
  });

  it("records a customer's request below the threshold and sends the address typed for it", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    const requested = await screen.findByLabelText(/Customer asked for their details/);
    fireEvent.click(requested);
    expect(await screen.findByLabelText("Address line 1")).toBeInTheDocument();

    // Unticking takes the form away again: nothing about a name typed makes it a request.
    fireEvent.click(requested);
    await waitFor(() => expect(screen.queryByLabelText("Address line 1")).not.toBeInTheDocument());
    fireEvent.click(requested);

    fireEvent.change(screen.getByLabelText("Name on the bill"), { target: { value: "Asha Patil" } });
    fireEvent.change(await screen.findByLabelText("Address line 1"), { target: { value: "4 Lake View Society" } });
    fireEvent.change(screen.getByLabelText(/^City/), { target: { value: "Pune" } });
    await waitFor(() => expect(screen.getByRole("option", { name: "Maharashtra (27)" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("State"), { target: { value: IDs.maharashtra } });
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));

    const saved = await waitForWrite(service, (write) => write.method === "PUT");
    expect(saved.body).toMatchObject({
      customerPartyId: null,
      customerNameText: "Asha Patil",
      recipientParticularsRequested: true,
      recipientAddress: { line1: "4 Lake View Society", line2: null, city: "Pune", postalCode: null, stateId: IDs.maharashtra },
      deliverySameAsRecipient: true,
      deliveryAddress: null
    });
    // A statutory recipient is invoice text, never a customer record.
    expect(writes(service).some((write) => write.path.startsWith("/api/v1/parties"))).toBe(false);
  });

  it("captures a separate delivery address only when delivery is elsewhere", async () => {
    const service = saleService({ documents: [worth(THRESHOLD)] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    // Offered twice — beside the total and on the customer panel — and either opens the same form.
    fireEvent.click((await screen.findAllByRole("button", { name: "Add customer details" }))[0]);
    fireEvent.click(await screen.findByLabelText("Goods delivered to the same address"));
    const deliveryLine = await screen.findByLabelText("Address line 1", { selector: "#pos-delivery-line1" });
    fireEvent.change(deliveryLine, { target: { value: "Site Office, Plot 9" } });
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));

    const saved = await waitForWrite(service, (write) => write.method === "PUT");
    expect(saved.body.deliverySameAsRecipient).toBe(false);
    expect(saved.body.deliveryAddress).toMatchObject({ line1: "Site Office, Plot 9" });
  });

  it("takes a registered customer's address from their record and never sends one from the counter", async () => {
    const service = saleService({ documents: [sale({ lines: [line()] })] });
    renderApp(`/app/sales/${IDs.sale}`, service);
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Change customer" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "Mehta Medical Stores" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("Registered customer"), { target: { value: IDs.registered } });

    expect(await screen.findByText(/taken from this customer's record in Parties/)).toBeInTheDocument();
    expect(screen.getByText(/GST-registered, so their name, address and GSTIN/)).toBeInTheDocument();
    expect(screen.queryByLabelText("Address line 1")).not.toBeInTheDocument();
    // Rule 46(d) asks for no address of delivery.
    expect(screen.queryByLabelText("Goods delivered to the same address")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save customer" }));

    const saved = await waitForWrite(service, (write) => write.method === "PUT");
    expect(saved.body).toMatchObject({ customerPartyId: IDs.registered, recipientAddress: null, deliverySameAsRecipient: true, deliveryAddress: null });

    // Once saved, the service's own finding about the record is what the counter is shown.
    const summary = await screen.findByLabelText("Bill total");
    await within(summary).findByText("Add a billing address to this customer in Parties.");
  });

  it("shows the frozen customer address and delivery on a posted invoice", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [postedSale({
      customerNameText: "Asha Patil", recipientSnapshotVersion: 1, recipientParticularsRequested: true,
      recipientAddressSource: "counter", recipientAddressLine1: "4 Lake View Society", recipientCity: "Pune",
      recipientStateId: IDs.maharashtra, recipientStateName: "Maharashtra", recipientStateCode: "27",
      deliverySameAsRecipient: false, deliveryAddressLine1: "Site Office, Plot 9", deliveryStateId: IDs.maharashtra,
      deliveryStateName: "Maharashtra", deliveryStateCode: "27"
    })] }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.getByText("4 Lake View Society, Pune, Maharashtra")).toBeInTheDocument();
    expect(screen.getByText("Site Office, Plot 9, Maharashtra")).toBeInTheDocument();
  });

  it("describes the requirement factually and never claims compliance", () => {
    const source = readFileSync(resolve(process.cwd(), "src/sales/Sales.tsx"), "utf8").toLowerCase();
    for (const claim of ["gst compliant", "compliance guaranteed", "rule 65 compliant", "fully compliant", "prescriptionverified"]) {
      expect(source).not.toContain(claim);
    }
  });
});

describe("Posted invoice", () => {
  it("shows the issued number, the resolved tax and the tender", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [postedSale()] }));
    expect(await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeInTheDocument();
    expect(screen.getByText("2026-27")).toBeInTheDocument();
    expect(screen.getByText("CGST + SGST (same State)")).toBeInTheDocument();
    expect(screen.getByText("Paid by Cash · 179.20")).toBeInTheDocument();

    const table = screen.getByRole("table");
    expect(within(table).getByText("30049099")).toBeInTheDocument();
    expect(within(table).getByText("2 Strip of 10s")).toBeInTheDocument();
  });

  // Phase 1L-A3 corrective: an unregistered seller's zero-GST Sale is not a GST document.
  const unregisteredMemo = () => postedSale({
    storeGstRegistrationStatus: "unregistered", storeNormalizedGstin: null,
    cgstPaise: 0, sgstPaise: 0, grandTotalPaise: 16_000,
    lines: [line({
      saleDocumentId: IDs.posted, hsnCode: "30049099",
      // The product's own GST classification, kept as catalogue fact. Not what was charged.
      taxTreatmentKind: "taxable",
      productDisplayName: "Crocin 500 mg Tablet", packDisplayLabel: "Strip of 10", baseUnitLabel: "Tablet",
      batchNumber: "B-2601", batchExpiresOn: "2028-03-31", batchMrpPaise: 9_550,
      lineTotalPaise: 16_000, priceControlStatus: "unknown"
    })],
    tenders: [{ id: "01997a00-0000-7000-8000-000000000090", saleDocumentId: IDs.posted, method: "cash", amountPaise: 16_000, referenceText: null }]
  });

  it("presents an unregistered seller's zero-GST sale as a retail cash memo", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [unregisteredMemo()] }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.getByRole("heading", { name: "Retail cash memo" })).toBeInTheDocument();
    expect(screen.queryByText("CGST + SGST (same State)")).not.toBeInTheDocument();
    expect(screen.getByText(/Not charged: this pharmacy was not GST-registered/)).toBeInTheDocument();
    expect(screen.queryByText("CGST")).not.toBeInTheDocument();
    expect(screen.queryByText("SGST")).not.toBeInTheDocument();
    const table = screen.getByRole("table");
    expect(within(table).queryByText(/Taxable/)).not.toBeInTheDocument();
    expect(within(table).queryByRole("columnheader", { name: "GST" })).not.toBeInTheDocument();
    expect(within(table).getByText("30049099")).toBeInTheDocument();
    // The stored amounts are shown as stored.
    expect(screen.getAllByText("160.00").length).toBeGreaterThan(0);
  });

  it("still shows GST a pre-1L-A3 unregistered sale did record, exactly as recorded", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [postedSale({ storeGstRegistrationStatus: "unregistered", storeNormalizedGstin: null })] }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.queryByRole("heading", { name: "Retail cash memo" })).not.toBeInTheDocument();
    expect(screen.getByText("CGST")).toBeInTheDocument();
    expect(screen.getAllByText("9.60").length).toBeGreaterThan(0);
  });

  /** Phase 1L-B: the posted document is where printing starts, and a draft has no such door. */
  it("offers printing from a posted invoice and from nowhere else", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [postedSale()] }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.getByRole("link", { name: "Print" })).toHaveAttribute("href", `/app/sales/${IDs.posted}/print`);

    cleanup();
    renderApp(`/app/sales/${IDs.sale}`, saleService({ documents: [sale({ lines: [line()] })] }));
    await screen.findByRole("heading", { name: "Counter sale", level: 1 });
    expect(screen.queryByRole("link", { name: "Print" })).not.toBeInTheDocument();
  });

  it("offers no way to edit a posted invoice", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({ documents: [postedSale()] }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.queryByRole("button", { name: /post/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Product or barcode")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Remove" })).not.toBeInTheDocument();
  });

  it("says a price-controlled line was checked against its notified ceiling", async () => {
    renderApp(`/app/sales/${IDs.posted}`, saleService({
      documents: [postedSale({ lines: [line({ saleDocumentId: IDs.posted, priceControlStatus: "controlled", ceilingPricePaise: 900, ceilingBasis: "per_base_unit", lineTotalPaise: 17_920 })] })]
    }));
    await screen.findByRole("heading", { name: "INV/2627/000001", level: 1 });
    expect(screen.getByText(/notified ceiling in force on the sale date was checked/)).toBeInTheDocument();
  });
});
