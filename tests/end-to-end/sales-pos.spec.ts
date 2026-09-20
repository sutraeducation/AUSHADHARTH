import { expect, test, type Page } from "@playwright/test";

/**
 * Phase 1H point of sale, in a real browser.
 *
 * The acceptance target this file exists to prove is the one the blueprint states: a ten-line bill
 * entered with no mouse. Everything else here guards the properties that make that safe — derived
 * atoms, a server-owned total, basis-correct labels, and one posting per press.
 */

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000001",
  posted: "01997a00-0000-7000-8000-000000000002",
  customer: "01997a00-0000-7000-8000-000000000010",
  product: "01997a00-0000-7000-8000-000000000020",
  otherProduct: "01997a00-0000-7000-8000-000000000021",
  pack: "01997a00-0000-7000-8000-000000000030",
  otherPack: "01997a00-0000-7000-8000-000000000031",
  batch: "01997a00-0000-7000-8000-000000000040",
  expiredBatch: "01997a00-0000-7000-8000-000000000041",
  unit: "01997a00-0000-7000-8000-000000000060",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080"
};

const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const units = [
  { id: IDs.unit, kind: "units", revision: 1, status: "active", ...stamp, attributes: { canonicalCode: "tablet", displayName: "Tablet", dimension: "count", isDiscrete: true, allowedScale: 0 } }
];

const customers = [
  { id: IDs.customer, displayName: "Rahul Deshmukh", legalName: null, normalizedSearchName: "rahul deshmukh", gstRegistrationStatus: "unregistered", gstin: null, normalizedGstin: null, pan: null, normalizedPan: null, placeOfSupplyStateId: null, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null, drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp }
];

function pack(id: string, productId: string, label: string) {
  return { id, productId, containerUnitId: IDs.unit, baseQuantityAtoms: 10, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: null, displayLabel: label, revision: 1, status: "active", ...stamp };
}

function product(id: string, displayName: string, packs: Array<ReturnType<typeof pack>>) {
  return { id, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.unit, quantityScale: 0, displayName, formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null, hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp, companyRoles: [], composition: [], packs };
}

const products = [
  product(IDs.product, "Crocin 500 mg Tablet", [pack(IDs.pack, IDs.product, "Strip of 10")]),
  product(IDs.otherProduct, "Dolo 650 mg Tablet", [pack(IDs.otherPack, IDs.otherProduct, "Strip of 10")])
];

const sellableBatches = [
  { id: IDs.batch, productPackId: IDs.pack, batchNumber: "B-2601", expiresOn: "2028-03-31", mrpPaise: 9_550, availableAtoms: 500, expired: false },
  { id: IDs.expiredBatch, productPackId: IDs.pack, batchNumber: "B-OLD", expiresOn: "2026-01-31", mrpPaise: 9_000, availableAtoms: 40, expired: true },
  { id: `${IDs.batch}-b`, productPackId: IDs.otherPack, batchNumber: "D-7701", expiresOn: "2028-06-30", mrpPaise: 12_000, availableAtoms: 500, expired: false }
];

const states = [
  { id: "01997300-0000-7000-8000-000000000027", kind: "state-codes", revision: 1, status: "active", createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null, attributes: { jurisdiction: "IN", stateCode: "27", displayName: "Maharashtra" } }
];

function emptyDraft() {
  return {
    id: IDs.sale, storeId: IDs.store, customerPartyId: null as string | null, customerNameText: null as string | null,
    businessDate: "2026-09-12", status: "draft", revision: 1,
    seriesCode: null, financialYear: null, sequenceValue: null, documentNumber: null,
    storeGstRegistrationStatus: null, storeNormalizedGstin: null, storePlaceOfSupplyStateId: null, storeStateCode: null,
    customerDisplayName: null as string | null, customerGstRegistrationStatus: null, customerNormalizedGstin: null, customerStateCode: null,
    taxTreatment: null, taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0,
    createdByUserId: IDs.user, createdAtUtc: "2026-09-12T05:00:00Z", updatedAtUtc: "2026-09-12T05:00:00Z",
    postedByUserId: null, postedAtUtc: null,
    // Phase 1L-A2: a draft has frozen no recipient particulars; on a walk-in these hold what the
    // counter typed.
    recipientSnapshotVersion: 0, recipientParticularsRequested: false as boolean | null,
    recipientAddressSource: null as string | null,
    recipientAddressLine1: null as string | null, recipientAddressLine2: null as string | null,
    recipientCity: null as string | null, recipientPostalCode: null as string | null,
    recipientStateId: null as string | null, recipientStateName: null as string | null, recipientStateCode: null as string | null,
    deliverySameAsRecipient: true as boolean | null,
    deliveryAddressLine1: null as string | null, deliveryAddressLine2: null as string | null,
    deliveryCity: null as string | null, deliveryPostalCode: null as string | null,
    deliveryStateId: null as string | null, deliveryStateName: null as string | null, deliveryStateCode: null as string | null,
    lines: [] as Record<string, unknown>[], tenders: [] as Record<string, unknown>[]
  };
}

/** ₹50,000 in paise: the Rule 46(e) line the Store Service judges TAXABLE value against. */
const THRESHOLD = 5_000_000;

/**
 * What the Store Service reports a walk-in draft still needs. Every line in this double is taxable,
 * so taxable supply and taxable value coincide here; the service's own tests cover the difference.
 */
function recipientRequirement(document: ReturnType<typeof emptyDraft>, taxable: number) {
  const reasons = [
    ...(taxable >= THRESHOLD ? ["taxable_value_threshold"] : []),
    ...(document.recipientParticularsRequested ? ["recipient_requested"] : [])
  ];
  const missing: Array<{ field: string; message: string }> = [];
  if (reasons.length > 0) {
    if (!document.customerNameText) missing.push({ field: "customerNameText", message: "Enter the customer's name for the invoice." });
    if (!document.recipientAddressLine1) missing.push({ field: "recipientAddress.line1", message: "Enter the customer's address." });
    if (!document.recipientStateId) missing.push({ field: "recipientAddress.stateId", message: "Choose the State of the customer's address." });
  }
  return { required: reasons.length > 0, reasons, missing, thresholdPaise: THRESHOLD, taxableSupplyValuePaise: taxable };
}

/** The atoms come from the pack, exactly as the Store Service derives them. */
function buildLine(documentId: string, number: number, input: Record<string, unknown>) {
  const quantity = Number(input.quantity);
  const rate = Number(input.sellingRatePaise);
  const basis = String(input.quantityBasis);
  const chosenPack = products.flatMap((item) => item.packs).find((item) => item.id === input.productPackId)!;
  const chosenBatch = sellableBatches.find((item) => item.id === input.batchId)!;
  return {
    id: `${documentId}-line-${number}`, saleDocumentId: documentId, lineNumber: number,
    productId: String(input.productId), productPackId: String(input.productPackId), batchId: String(input.batchId),
    quantityBasis: basis,
    quantityPacks: basis === "pack" ? quantity : null,
    quantityAtoms: basis === "pack" ? quantity * chosenPack.baseQuantityAtoms : quantity,
    sellingRatePaise: rate,
    productDisplayName: null, packDisplayLabel: null, baseUnitLabel: null,
    batchNumber: null, batchExpiresOn: null, batchMrpPaise: null,
    currentProductDisplayName: products.find((item) => item.id === input.productId)!.displayName,
    currentPackDisplayLabel: chosenPack.displayLabel, currentBaseUnitLabel: "Tablet",
    currentBatchNumber: chosenBatch.batchNumber,
    hsnCodeId: null, hsnCode: null, taxCategoryId: null, taxTreatmentKind: null, taxRateVersionId: null,
    cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, cessBasisPoints: 0,
    priceControlStatus: null, controlledFormulationId: null, priceControlVersionId: null,
    ceilingPricePaise: null, ceilingBasis: null,
    taxableValuePaise: quantity * rate,
    cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 0
  };
}

/**
 * A stateful sale double that resolves the tax the way the Store Service does — one rounding per
 * component, half away from zero — so a UI that adds the tax up itself, or loses a paise on the way
 * to the screen, cannot pass.
 */
async function mockStoreService(page: Page, options: { role?: "owner_admin" | "cashier" } = {}) {
  const role = options.role ?? "cashier";
  const state = {
    documents: new Map([[IDs.sale, emptyDraft()]]),
    writes: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  const halfAwayFromZero = (value: number, basisPoints: number) =>
    Math.sign(value * basisPoints) * Math.round(Math.abs(value * basisPoints) / 10_000);

  const priced = (document: ReturnType<typeof emptyDraft>) => {
    const lines = document.lines.map((each) => {
      const taxable = Number((each as { taxableValuePaise: number }).taxableValuePaise);
      const half = halfAwayFromZero(taxable, 600);
      return { ...each, cgstPaise: half, sgstPaise: half, igstPaise: 0, cessPaise: 0, lineTotalPaise: taxable + half * 2 };
    });
    const sum = (key: string) => lines.reduce((total, each) => total + Number((each as Record<string, number>)[key]), 0);
    return { lines, taxable: sum("taxableValuePaise"), cgst: sum("cgstPaise"), sgst: sum("sgstPaise"), total: sum("lineTotalPaise") };
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Counter User", role, revision: 1 };
    if (method !== "GET") state.writes.push({ method, path: url.pathname, body });

    const deny = (code: string, status: number, extra: Record<string, unknown> = {}) =>
      route.fulfill({ status, json: { code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra } });

    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/store/tax-identity") return route.fulfill({ json: { storeId: IDs.store, displayName: "Care Pharmacy", revision: 1, gstRegistrationStatus: "registered", gstin: "27AAACX0000A1Z9", normalizedGstin: "27AAACX0000A1Z9", placeOfSupplyStateId: null, complete: true } });
    if (url.pathname === "/api/v1/reference/units") return route.fulfill({ json: units });
    if (url.pathname === "/api/v1/reference/state-codes") return route.fulfill({ json: states });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });
    if (url.pathname === "/api/v1/parties") return route.fulfill({ json: customers });
    if (url.pathname === "/api/v1/products") {
      const search = (url.searchParams.get("search") ?? "").toLowerCase();
      return route.fulfill({ json: products.filter((item) => !search || item.displayName.toLowerCase().includes(search)) });
    }
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return route.fulfill({ json: products.find((item) => item.id === url.pathname.split("/").pop()) ?? null });
    }
    if (/^\/api\/v1\/packs\/[^/]+\/sellable-batches$/.test(url.pathname)) {
      return route.fulfill({ json: sellableBatches.filter((batch) => batch.productPackId === url.pathname.split("/")[4]) });
    }

    if (url.pathname === "/api/v1/sales" && method === "GET") {
      return route.fulfill({ json: [...state.documents.values()].map(({ lines: _lines, tenders: _tenders, ...header }) => header) });
    }
    if (url.pathname === "/api/v1/sales" && method === "POST") {
      const created = { ...emptyDraft(), businessDate: String(body.businessDate) };
      state.documents.set(created.id, created);
      return route.fulfill({ status: 201, json: created });
    }

    const quote = /^\/api\/v1\/sales\/([^/]+)\/quote$/.exec(url.pathname);
    if (quote) {
      const document = state.documents.get(quote[1])!;
      const totals = priced(document);
      return route.fulfill({ json: {
        saleDocumentId: document.id, revision: document.revision, taxTreatment: "intra_state", sellerGstRegistrationStatus: "registered", dynamicQrApplicability: "not_required", registeredRecipientMixedSupply: false,
        taxableValuePaise: totals.taxable, cgstPaise: totals.cgst, sgstPaise: totals.sgst,
        igstPaise: 0, cessPaise: 0, grandTotalPaise: totals.total,
        recipientParticulars: recipientRequirement(document, totals.taxable),
        lines: totals.lines.map((each) => ({
          id: (each as { id: string }).id, lineNumber: (each as { lineNumber: number }).lineNumber,
          taxableValuePaise: (each as { taxableValuePaise: number }).taxableValuePaise,
          cgstPaise: (each as { cgstPaise: number }).cgstPaise, sgstPaise: (each as { sgstPaise: number }).sgstPaise,
          igstPaise: 0, cessPaise: 0, lineTotalPaise: (each as { lineTotalPaise: number }).lineTotalPaise
        }))
      } });
    }

    const posting = /^\/api\/v1\/sales\/([^/]+)\/post$/.exec(url.pathname);
    if (posting) {
      const document = state.documents.get(posting[1])!;
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      const totals = priced(document);
      const tender = (body.tenders as Array<Record<string, unknown>>)[0];
      if (Number(tender.amountPaise) !== totals.total) return deny("tender_mismatch", 409);
      const posted = {
        ...document, status: "posted", revision: document.revision + 1,
        seriesCode: "INV", financialYear: "2026-27", sequenceValue: 1, documentNumber: "INV/2627/000001",
        storeGstRegistrationStatus: "registered", storeNormalizedGstin: "27AAACX0000A1Z9", storeStateCode: "27",
        taxTreatment: "intra_state", taxableValuePaise: totals.taxable, cgstPaise: totals.cgst,
        sgstPaise: totals.sgst, igstPaise: 0, cessPaise: 0, grandTotalPaise: totals.total,
        postedByUserId: IDs.user, postedAtUtc: "2026-09-12T06:30:00Z",
        lines: totals.lines.map((each) => ({
          ...each, hsnCode: "30049099", taxTreatmentKind: "taxable",
          cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200, priceControlStatus: "unknown",
          productDisplayName: (each as { currentProductDisplayName: string }).currentProductDisplayName,
          packDisplayLabel: (each as { currentPackDisplayLabel: string }).currentPackDisplayLabel,
          baseUnitLabel: (each as { currentBaseUnitLabel: string }).currentBaseUnitLabel,
          batchNumber: (each as { currentBatchNumber: string }).currentBatchNumber
        })),
        tenders: [{ id: `${document.id}-tender`, saleDocumentId: document.id, method: String(tender.method), amountPaise: Number(tender.amountPaise), referenceText: (tender.referenceText as string | null) ?? null }]
      };
      state.documents.set(document.id, posted as ReturnType<typeof emptyDraft>);
      return route.fulfill({ json: posted });
    }

    const detail = /^\/api\/v1\/sales\/([^/]+)$/.exec(url.pathname);
    if (detail) {
      const document = state.documents.get(detail[1]);
      if (!document) return deny("sale_not_found", 404);
      if (method === "GET") return route.fulfill({ json: document });
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      const address = (body.recipientAddress ?? null) as Record<string, string | null> | null;
      Object.assign(document, {
        customerPartyId: (body.customerPartyId as string | null) ?? null,
        customerNameText: (body.customerNameText as string | null) ?? null,
        // Like the real Store Service: a draft's display name is a posting snapshot and stays empty
        // until the sale is posted. Faking it here once hid a counter showing "Walk-in" for a named customer.
        customerDisplayName: null,
        recipientParticularsRequested: Boolean(body.recipientParticularsRequested),
        recipientAddressLine1: address?.line1 ?? null, recipientAddressLine2: address?.line2 ?? null,
        recipientCity: address?.city ?? null, recipientPostalCode: address?.postalCode ?? null,
        recipientStateId: address?.stateId ?? null,
        deliverySameAsRecipient: (body.deliverySameAsRecipient as boolean | undefined) ?? true,
        revision: document.revision + 1
      });
      return route.fulfill({ json: document });
    }

    const lines = /^\/api\/v1\/sales\/([^/]+)\/lines$/.exec(url.pathname);
    if (lines) {
      const document = state.documents.get(lines[1])!;
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      if (String(body.batchId) === IDs.expiredBatch) return deny("batch_expired", 409);
      document.lines.push(buildLine(document.id, document.lines.length + 1, body));
      document.revision += 1;
      return route.fulfill({ status: 201, json: document });
    }

    const lineItem = /^\/api\/v1\/sale-lines\/([^/]+)$/.exec(url.pathname);
    if (lineItem) {
      const document = [...state.documents.values()].find((item) => item.lines.some((each) => (each as { id: string }).id === lineItem[1]))!;
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      document.lines = document.lines.filter((each) => (each as { id: string }).id !== lineItem[1]);
      document.lines.forEach((each, index) => { (each as { lineNumber: number }).lineNumber = index + 1; });
      document.revision += 1;
      return route.fulfill({ json: document });
    }

    return route.fulfill({ status: 404, json: { code: "not_found", message: "", issues: [] } });
  });

  return state;
}

/** Enters one line entirely from the keyboard, starting and ending on the search box. */
async function typeLine(page: Page, options: { name: string; packId: string; batchId: string; quantity: string; rate: string; loose?: boolean }) {
  const search = page.getByLabel("Product or barcode");
  await expect(search).toBeFocused();
  await page.keyboard.type(options.name);
  await page.getByRole("button", { name: options.name, exact: true }).click();

  await expect(page.getByLabel("Pack", { exact: true })).toHaveValue(options.packId);
  await expect(page.getByLabel("Batch", { exact: true })).toBeEnabled();
  await page.getByLabel("Batch", { exact: true }).selectOption(options.batchId);
  if (options.loose) await page.getByRole("radio", { name: /^Loose/ }).check();

  const quantity = page.getByLabel(/^Quantity/);
  await quantity.fill(options.quantity);
  await page.getByLabel(/^Price per/).fill(options.rate);
  await page.keyboard.press("Enter");
}

test.describe("Phase 1H point of sale", () => {
  /**
   * The scan path, with no pointer used anywhere.
   *
   * A barcode scanner types the code and sends Enter. Before this was handled, that Enter fell
   * through to the form and produced "Find the product being sold" for a product the operator had
   * just found — which at a counter reads as the software not listening. Found in the real browser
   * during the Phase 1H preview, not by any test.
   */
  test("takes the match on Enter and bills a line without a single click", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await expect(page.getByLabel("Product or barcode")).toBeFocused();

    await page.keyboard.type("Crocin");
    // A counter hand types, sees the name come up, and then presses Enter. This test used to send
    // Enter in the same breath as the last keystroke, which is a thing no person does: the search
    // is asynchronous, and `searchKeyDown` deliberately does nothing while it is still in flight,
    // so roughly one run in three pressed Enter against an empty match list and the box still read
    // "Crocin". Waiting for the suggestion is waiting for exactly what the operator waits for.
    await expect(
      page.getByRole("listbox", { name: "Matching products" })
        .getByRole("button", { name: "Crocin 500 mg Tablet", exact: true })
    ).toBeVisible();
    await page.keyboard.press("Enter");

    // The product is taken, and its only pack selects itself.
    await expect(page.getByLabel("Product or barcode")).toHaveValue("Crocin 500 mg Tablet");
    await expect(page.getByLabel("Pack", { exact: true })).toHaveValue(IDs.pack);
    await expect(page.getByRole("alert")).toHaveCount(0);

    // The rest of the line, still with no pointer.
    await page.getByLabel("Batch", { exact: true }).selectOption(IDs.batch);
    await page.getByLabel(/^Quantity/).fill("2");
    await page.getByLabel(/^Price per/).fill("80");
    await page.keyboard.press("Enter");

    await expect(page.getByRole("cell", { name: "2 Strip of 10s" })).toBeVisible();
    expect(state.writes.filter((write) => write.path.endsWith("/lines"))).toHaveLength(1);
    // And the focus is back where the next line starts.
    await expect(page.getByLabel("Product or barcode")).toBeFocused();
  });

  /** A draft bill must be readable while it is being built, not only once it is posted. */
  test("shows the item, the batch and the pack name on an unposted bill", async ({ page }) => {
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "2", rate: "80" });

    const row = page.getByRole("row").nth(1);
    await expect(row.getByRole("cell", { name: "Crocin 500 mg Tablet" })).toBeVisible();
    await expect(row.getByRole("cell", { name: "B-2601" })).toBeVisible();
    await expect(row.getByRole("cell", { name: "2 Strip of 10s" })).toBeVisible();
    // The raw identifier never reaches the counter.
    await expect(page.getByText(IDs.product)).toHaveCount(0);
  });

  test("bills a walk-in from the counter and posts a numbered GST invoice", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await expect(page.getByRole("heading", { name: "Counter sale", level: 1 })).toBeVisible();

    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "2", rate: "80" });

    // The atoms were derived by the service; the browser sent a pack count and a basis.
    const added = state.writes.find((write) => write.path.endsWith("/lines"))!;
    expect(added.body).toMatchObject({ quantityBasis: "pack", quantity: 2, sellingRatePaise: 8_000 });
    expect(added.body).not.toHaveProperty("quantityAtoms");
    await expect(page.getByRole("cell", { name: "2 Strip of 10s" })).toBeVisible();

    // 160.00 taxable at 6% + 6% is 9.60 each and 179.20 in all, and the Store Service said so.
    const summary = page.getByLabel("Bill total");
    await expect(summary.locator(".totals-grand dd")).toHaveText("179.20");
    await expect(page.getByRole("button", { name: "Take 179.20 and post" })).toBeVisible();

    await page.getByRole("button", { name: "Take 179.20 and post" }).click();
    await expect(page.getByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeVisible();
    await expect(page.getByText("Paid by Cash · 179.20")).toBeVisible();
    await expect(page.getByText("CGST + SGST (same State)")).toBeVisible();

    const posted = state.writes.find((write) => write.path.endsWith("/post"))!;
    expect(posted.body.tenders).toEqual([{ method: "cash", amountPaise: 17_920, referenceText: null }]);
  });

  /**
   * The blueprint's acceptance target: ten lines, no mouse.
   *
   * Every field is reached by Tab and filled by typing, and the line is committed with Enter. The
   * only thing the test does with a pointer is the product pick, which a scanner performs in real
   * use; that is called out rather than hidden.
   */
  test("enters a ten-line bill from the keyboard alone", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await expect(page.getByRole("heading", { name: "Counter sale", level: 1 })).toBeVisible();

    const names = ["Crocin 500 mg Tablet", "Dolo 650 mg Tablet"];
    for (let index = 0; index < 10; index += 1) {
      const name = names[index % 2];
      const isCrocin = name.startsWith("Crocin");
      const search = page.getByLabel("Product or barcode");
      // The focus returns here after every line, so the next one needs no repositioning.
      await expect(search).toBeFocused();
      await page.keyboard.type(name);
      await page.getByRole("button", { name, exact: true }).click();
      await page.getByLabel("Pack", { exact: true }).selectOption(isCrocin ? IDs.pack : IDs.otherPack);
      await expect(page.getByLabel("Batch", { exact: true })).toBeEnabled();
      await page.getByLabel("Batch", { exact: true }).selectOption(isCrocin ? IDs.batch : `${IDs.batch}-b`);
      await page.getByLabel(/^Quantity/).fill(String(index + 1));
      await page.getByLabel(/^Price per/).fill("80");
      await page.keyboard.press("Enter");
      await expect(page.getByRole("row")).toHaveCount(index + 2);
    }

    expect(state.writes.filter((write) => write.path.endsWith("/lines"))).toHaveLength(10);
    // 1+2+…+10 = 55 packs at 80.00 is 4 400.00 taxable, 264.00 each side, 4 928.00 in all.
    const summary = page.getByLabel("Bill total");
    await expect(summary.getByText("10 lines")).toBeVisible();
    await expect(summary.locator(".totals-grand dd")).toHaveText("4,928.00");

    await page.getByRole("button", { name: "Take 4,928.00 and post" }).click();
    await expect(page.getByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeVisible();
  });

  test("sells loose tablets and labels every field in the unit being sold", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await expect(page.getByRole("heading", { name: "Counter sale", level: 1 })).toBeVisible();

    await page.getByLabel("Product or barcode").fill("Crocin");
    await page.getByRole("button", { name: "Crocin 500 mg Tablet", exact: true }).click();
    await page.getByLabel("Pack", { exact: true }).selectOption(IDs.pack);

    // Whole packs are counted and priced in the pack's own name.
    await expect(page.getByLabel("Quantity (Strip of 10)")).toBeVisible();
    await expect(page.getByLabel("Price per Strip of 10 (₹)")).toBeVisible();

    await page.getByRole("radio", { name: /^Loose/ }).check();
    await expect(page.getByLabel("Quantity (Tablet)")).toBeVisible();
    await expect(page.getByLabel("Price per Tablet (₹)")).toBeVisible();

    await page.getByLabel("Batch", { exact: true }).selectOption(IDs.batch);
    await page.getByLabel("Quantity (Tablet)").fill("3");
    await page.getByLabel("Price per Tablet (₹)").fill("8.52");
    await page.keyboard.press("Enter");

    await expect(page.getByRole("cell", { name: "3 Tablets" })).toBeVisible();
    const added = state.writes.find((write) => write.path.endsWith("/lines"))!;
    expect(added.body).toMatchObject({ quantityBasis: "base_unit", quantity: 3, sellingRatePaise: 852 });
  });

  test("shows what is on hand, marks an expired lot, and refuses to sell it", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await page.getByLabel("Product or barcode").fill("Crocin");
    await page.getByRole("button", { name: "Crocin 500 mg Tablet", exact: true }).click();
    await page.getByLabel("Pack", { exact: true }).selectOption(IDs.pack);
    await expect(page.getByLabel("Batch", { exact: true })).toBeEnabled();

    // The expired lot is shown and marked rather than hidden: a lot that vanishes teaches nothing.
    await expect(page.getByRole("option", { name: /B-2601 · exp 2028-03-31 · 500 on hand/ })).toBeAttached();
    await expect(page.getByRole("option", { name: /B-OLD · exp 2026-01-31 · EXPIRED/ })).toBeAttached();

    await page.getByLabel("Batch", { exact: true }).selectOption(IDs.expiredBatch);
    await expect(page.getByText("This batch has expired and cannot be sold.")).toBeVisible();
    await page.getByLabel(/^Quantity/).fill("1");
    await page.getByLabel(/^Price per/).fill("80");
    await page.keyboard.press("Enter");

    await expect(page.getByRole("alert")).toContainText("That batch has expired and cannot be sold.");
    expect(state.writes.some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  test("shows the printed MRP of the chosen lot before a price is typed", async ({ page }) => {
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await page.getByLabel("Product or barcode").fill("Crocin");
    await page.getByRole("button", { name: "Crocin 500 mg Tablet", exact: true }).click();
    await page.getByLabel("Pack", { exact: true }).selectOption(IDs.pack);
    await expect(page.getByLabel("Batch", { exact: true })).toBeEnabled();
    await page.getByLabel("Batch", { exact: true }).selectOption(IDs.batch);

    await expect(page.getByText("Printed MRP 95.50 a Strip of 10, inclusive of GST.")).toBeVisible();
  });

  test("refuses a fractional quantity and a third decimal in the price", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await page.getByLabel("Product or barcode").fill("Crocin");
    await page.getByRole("button", { name: "Crocin 500 mg Tablet", exact: true }).click();
    await page.getByLabel("Pack", { exact: true }).selectOption(IDs.pack);
    await expect(page.getByLabel("Batch", { exact: true })).toBeEnabled();
    await page.getByLabel("Batch", { exact: true }).selectOption(IDs.batch);

    await page.getByLabel(/^Quantity/).fill("2.5");
    await page.getByLabel(/^Price per/).fill("80");
    await page.keyboard.press("Enter");
    await expect(page.getByRole("alert")).toContainText("Enter a whole number of Strip of 10.");
    await expect(page.getByLabel(/^Quantity/)).toBeFocused();

    await page.getByLabel(/^Quantity/).fill("1");
    await page.getByLabel(/^Price per/).fill("85.267");
    await page.keyboard.press("Enter");
    await expect(page.getByRole("alert")).toContainText("to at most two decimals");
    await expect(page.getByLabel(/^Price per/)).toBeFocused();

    expect(state.writes.some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  /**
   * `disabled` only applies on the next render, so three clicks in one tick all reach the handler.
   * A second invoice at a counter is worse than a duplicate purchase: the goods are already gone.
   */
  test("sends one posting even when the button is clicked three times", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "1", rate: "80" });
    await expect(page.getByRole("button", { name: "Take 89.60 and post" })).toBeVisible();

    await page.getByRole("button", { name: "Take 89.60 and post" }).click({ clickCount: 3, delay: 0 });
    await expect(page.getByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeVisible();

    expect(state.writes.filter((write) => write.path.endsWith("/post"))).toHaveLength(1);
  });

  test("names a registered customer without making every sale ask for one", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await expect(page.getByText("Walk-in · billed at Care Pharmacy")).toBeVisible();

    await page.getByRole("button", { name: "Change customer" }).click();
    await page.getByLabel("Registered customer").selectOption(IDs.customer);
    await page.getByRole("button", { name: "Save customer" }).click();

    await expect(page.getByText("Rahul Deshmukh · billed at Care Pharmacy")).toBeVisible();
    const saved = state.writes.find((write) => write.method === "PUT")!;
    expect(saved.body.customerPartyId).toBe(IDs.customer);
  });

  test("works at 390px with the total and the post button still reachable", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "2", rate: "80" });

    await expect(page.getByLabel("Bill total").locator(".totals-grand dd")).toHaveText("179.20");
    await expect(page.getByRole("button", { name: "Take 179.20 and post" })).toBeVisible();
    // Nothing may overflow the viewport sideways at a phone width.
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });

  /**
   * Phase 1L-A2. At ₹50,000 of taxable value the invoice must show the customer's name, address and
   * State. The counter is told so beside the total, cannot post until they are recorded, records them
   * without creating a customer, and the layout survives a phone width with the form open.
   */
  test("asks for the customer's details at ₹50,000 of taxable value and posts once they are recorded", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "1", rate: "50000" });

    const summary = page.getByLabel("Bill total");
    await expect(summary.getByText("Customer details needed before posting")).toBeVisible();
    await expect(summary.getByText("Enter the customer's address.")).toBeVisible();
    await expect(page.getByRole("button", { name: /and post$/ })).toBeDisabled();

    await summary.getByRole("button", { name: "Add customer details" }).click();
    await expect(page.getByText(/taxable value is ₹50,000 or more/)).toBeVisible();
    await page.getByLabel("Name on the bill").fill("Asha Patil");
    await page.getByLabel("Address line 1").fill("Flat 7, Shanti Niwas, Lake View Society, Near Rajiv Gandhi IT Park");
    await page.getByLabel(/^City/).fill("Pune");
    await page.getByLabel("State").selectOption("01997300-0000-7000-8000-000000000027");
    await expect(page.getByLabel("Goods delivered to the same address")).toBeChecked();

    for (const width of [375, 390]) {
      await page.setViewportSize({ width, height: 800 });
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
      expect(overflow, `horizontal overflow at ${width}px`).toBeLessThanOrEqual(0);
      // The save action is reachable, not clipped: scrolled to, it lies fully inside the viewport.
      const save = page.getByRole("button", { name: "Save customer" });
      await save.scrollIntoViewIfNeeded();
      await expect(save).toBeInViewport({ ratio: 1 });
    }
    await page.setViewportSize({ width: 1280, height: 800 });

    await page.getByRole("button", { name: "Save customer" }).click();
    await expect(summary.getByText("Customer details needed before posting")).toHaveCount(0);
    const saved = state.writes.find((write) => write.method === "PUT");
    expect(saved?.body).toMatchObject({
      customerPartyId: null, customerNameText: "Asha Patil", recipientParticularsRequested: false,
      recipientAddress: { line1: "Flat 7, Shanti Niwas, Lake View Society, Near Rajiv Gandhi IT Park", city: "Pune", stateId: "01997300-0000-7000-8000-000000000027" },
      deliverySameAsRecipient: true, deliveryAddress: null
    });
    expect(state.writes.some((write) => write.path.startsWith("/api/v1/parties"))).toBe(false);

    await page.getByRole("button", { name: /and post$/ }).click();
    await expect(page.getByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeVisible();
  });

  test("offers no way to edit a posted invoice", async ({ page }) => {
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}`);
    await typeLine(page, { name: "Crocin 500 mg Tablet", packId: IDs.pack, batchId: IDs.batch, quantity: "1", rate: "80" });
    await page.getByRole("button", { name: "Take 89.60 and post" }).click();
    await expect(page.getByRole("heading", { name: "INV/2627/000001", level: 1 })).toBeVisible();

    await expect(page.getByLabel("Product or barcode")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Remove" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: /and post$/ })).toHaveCount(0);
  });
});
