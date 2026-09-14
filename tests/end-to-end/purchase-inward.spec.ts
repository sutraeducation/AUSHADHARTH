import { expect, test, type Page } from "@playwright/test";

const IDs = {
  draft: "01997a00-0000-7000-8000-000000000002",
  posted: "01997a00-0000-7000-8000-000000000001",
  supplier: "01997a00-0000-7000-8000-000000000010",
  keralaSupplier: "01997a00-0000-7000-8000-000000000011",
  product: "01997a00-0000-7000-8000-000000000020",
  otherProduct: "01997a00-0000-7000-8000-000000000021",
  pack: "01997a00-0000-7000-8000-000000000030",
  otherPack: "01997a00-0000-7000-8000-000000000031",
  batch: "01997a00-0000-7000-8000-000000000040",
  unit: "01997a00-0000-7000-8000-000000000060",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080"
};

const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const suppliers = [
  supplier(IDs.supplier, "Sharma Medicals", "27AAACS1234A1Z5"),
  supplier(IDs.keralaSupplier, "Kerala Distributors", "32AAACK9876B1Z2")
];

function supplier(id: string, displayName: string, gstin: string) {
  return {
    id, displayName, legalName: null, normalizedSearchName: displayName.toLowerCase(),
    gstRegistrationStatus: "registered", gstin: null, normalizedGstin: gstin, pan: null, normalizedPan: null,
    placeOfSupplyStateId: null, primaryPhone: null, primaryEmail: null, drugLicenceNumber: null,
    drugLicenceValidUpto: null, revision: 1, status: "active", ...stamp
  };
}

function pack(id: string, productId: string, label: string) {
  return {
    id, productId, containerUnitId: IDs.unit, baseQuantityAtoms: 10, containedPackId: null,
    containedPackCount: null, skuCode: null, skuStoreId: null, displayLabel: label,
    revision: 1, status: "active", ...stamp
  };
}

function product(id: string, displayName: string, packs: Array<ReturnType<typeof pack>>) {
  return {
    id, productKind: "general_pharmacy_item", brandId: null, dosageFormId: null, baseUnitId: IDs.unit,
    quantityScale: 0, displayName, formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [], packs
  };
}

const products = [
  product(IDs.product, "Crocin 500 mg Tablet", [pack(IDs.pack, IDs.product, "Strip of 10")]),
  product(IDs.otherProduct, "Dolo 650 mg Tablet", [pack(IDs.otherPack, IDs.otherProduct, "Strip of 15")])
];

const batches = [
  { id: IDs.batch, productPackId: IDs.pack, batchNumber: "B-2601", normalizedBatchNumber: "b-2601", manufacturedOn: null, expiresOn: "2028-03-31", mrpPaise: 4500, revision: 1, status: "active", ...stamp }
];

type LineInput = Record<string, unknown>;

function emptyDraft() {
  return {
    id: IDs.draft, storeId: IDs.store, supplierPartyId: IDs.supplier, supplierInvoiceNumber: "INV-4471",
    normalizedSupplierInvoiceNumber: "inv-4471", invoiceDate: "2026-09-10", status: "draft", revision: 1,
    supplierDisplayName: null, supplierGstRegistrationStatus: null, supplierNormalizedGstin: null,
    supplierPlaceOfSupplyStateId: null, supplierStateCode: null, storeGstRegistrationStatus: null,
    storeNormalizedGstin: null, storePlaceOfSupplyStateId: null, storeStateCode: null, taxTreatment: null,
    taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0,
    createdByUserId: IDs.user, createdAtUtc: "2026-09-10T05:00:00Z", updatedAtUtc: "2026-09-10T05:00:00Z",
    postedByUserId: null, postedAtUtc: null, lines: [] as Record<string, unknown>[]
  };
}

function buildLine(documentId: string, number: number, input: LineInput) {
  const quantityPacks = Number(input.quantityPacks);
  const ratePerPackPaise = Number(input.ratePerPackPaise);
  return {
    id: `${IDs.draft}-line-${number}`, purchaseDocumentId: documentId, lineNumber: number,
    productId: String(input.productId), productPackId: String(input.productPackId),
    batchId: (input.batchId as string | null) ?? null,
    newBatchNumber: (input.newBatchNumber as string | null) ?? null,
    newBatchExpiresOn: (input.newBatchExpiresOn as string | null) ?? null,
    newBatchMrpPaise: (input.newBatchMrpPaise as number | null) ?? null,
    quantityPacks, ratePerPackPaise, quantityAtoms: quantityPacks * 10,
    taxableValuePaise: quantityPacks * ratePerPackPaise,
    hsnCodeId: null, hsnCode: null, taxCategoryId: null, taxTreatmentKind: null, taxRateVersionId: null,
    cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, cessBasisPoints: 0,
    cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, lineTotalPaise: 0
  };
}

/**
 * A stateful purchase double.
 *
 * It resolves the tax the way the Store Service does — one rounding per component, CGST + SGST when
 * the two State codes agree and IGST when they do not — so a UI that mislabels the treatment or
 * loses a paise cannot pass.
 */
async function mockStoreService(page: Page, options: {
  role?: "owner_admin" | "pharmacist";
  documents?: Array<ReturnType<typeof emptyDraft>>;
  storeStateCode?: string;
} = {}) {
  const role = options.role ?? "owner_admin";
  const storeStateCode = options.storeStateCode ?? "27";
  const state = {
    documents: new Map((options.documents ?? [emptyDraft()]).map((document) => [document.id, structuredClone(document)])),
    writes: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  const halfAwayFromZero = (value: number, basisPoints: number) =>
    Math.sign(value * basisPoints) * Math.round(Math.abs(value * basisPoints) / 10_000);

  const post = (document: ReturnType<typeof emptyDraft>) => {
    const supplierRecord = suppliers.find((item) => item.id === document.supplierPartyId)!;
    const supplierStateCode = supplierRecord.normalizedGstin.slice(0, 2);
    const interState = supplierStateCode !== storeStateCode;
    // 12% GST, split 6% + 6% within a State and charged whole as IGST across one.
    const lines = document.lines.map((each) => {
      const taxable = Number((each as { taxableValuePaise: number }).taxableValuePaise);
      const cgst = interState ? 0 : halfAwayFromZero(taxable, 600);
      const igst = interState ? halfAwayFromZero(taxable, 1_200) : 0;
      return {
        ...each, hsnCode: "30049099", taxTreatmentKind: "taxable",
        cgstBasisPoints: interState ? 0 : 600, sgstBasisPoints: interState ? 0 : 600,
        igstBasisPoints: interState ? 1_200 : 0, cessBasisPoints: 0,
        cgstPaise: cgst, sgstPaise: cgst, igstPaise: igst, cessPaise: 0,
        lineTotalPaise: taxable + cgst * 2 + igst
      };
    });
    const sum = (key: string) => lines.reduce((total, each) => total + Number((each as Record<string, number>)[key]), 0);
    return {
      ...document, status: "posted", revision: document.revision + 1,
      supplierDisplayName: supplierRecord.displayName,
      supplierGstRegistrationStatus: "registered", supplierNormalizedGstin: supplierRecord.normalizedGstin,
      supplierStateCode, storeGstRegistrationStatus: "registered",
      storeNormalizedGstin: `${storeStateCode}AAACX0000A1Z9`, storeStateCode,
      taxTreatment: interState ? "inter_state" : "intra_state",
      taxableValuePaise: sum("taxableValuePaise"), cgstPaise: sum("cgstPaise"), sgstPaise: sum("sgstPaise"),
      igstPaise: sum("igstPaise"), cessPaise: 0, grandTotalPaise: sum("lineTotalPaise"),
      postedByUserId: IDs.user, postedAtUtc: "2026-09-11T06:30:00Z", lines
    };
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Store Owner", role, revision: 1 };
    if (method !== "GET") state.writes.push({ method, path: url.pathname, body });

    const deny = (code: string, status: number, extra: Record<string, unknown> = {}) =>
      route.fulfill({ status, json: { code, message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null, ...extra } });

    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/parties") return route.fulfill({ json: suppliers });
    if (url.pathname === "/api/v1/products") return route.fulfill({ json: products });
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) {
      return route.fulfill({ json: products.find((item) => item.id === url.pathname.split("/").pop()) ?? null });
    }
    if (/^\/api\/v1\/packs\/[^/]+\/batches$/.test(url.pathname)) {
      return route.fulfill({ json: batches.filter((batch) => batch.productPackId === url.pathname.split("/")[4]) });
    }
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });

    if (url.pathname === "/api/v1/purchases" && method === "GET") {
      const status = url.searchParams.get("status");
      const supplierFilter = url.searchParams.get("supplierPartyId");
      return route.fulfill({
        json: [...state.documents.values()]
          .filter((document) => !status || status === "all" || document.status === status)
          .filter((document) => !supplierFilter || document.supplierPartyId === supplierFilter)
          .map(({ lines: _lines, ...header }) => header)
      });
    }
    if (url.pathname === "/api/v1/purchases" && method === "POST") {
      if (role !== "owner_admin") return deny("authorization_denied", 403);
      const created = {
        ...emptyDraft(),
        supplierPartyId: String(body.supplierPartyId),
        supplierInvoiceNumber: String(body.supplierInvoiceNumber),
        invoiceDate: String(body.invoiceDate)
      };
      state.documents.set(created.id, created);
      return route.fulfill({ status: 201, json: created });
    }

    const detail = /^\/api\/v1\/purchases\/([^/]+)$/.exec(url.pathname);
    if (detail) {
      const document = state.documents.get(detail[1]);
      if (!document) return deny("purchase_not_found", 404);
      if (method === "GET") return route.fulfill({ json: document });
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      Object.assign(document, {
        supplierPartyId: String(body.supplierPartyId),
        supplierInvoiceNumber: String(body.supplierInvoiceNumber),
        invoiceDate: String(body.invoiceDate),
        revision: document.revision + 1
      });
      return route.fulfill({ json: document });
    }

    const lines = /^\/api\/v1\/purchases\/([^/]+)\/lines$/.exec(url.pathname);
    if (lines) {
      const document = state.documents.get(lines[1])!;
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      // A batch may be chosen or proposed, never both — the real CHECK constraint refuses it.
      if (body.batchId && body.newBatchNumber) return deny("validation_failed", 422);
      document.lines.push(buildLine(document.id, document.lines.length + 1, body));
      document.revision += 1;
      return route.fulfill({ status: 201, json: document });
    }

    const posting = /^\/api\/v1\/purchases\/([^/]+)\/post$/.exec(url.pathname);
    if (posting) {
      const document = state.documents.get(posting[1])!;
      if (document.status === "posted") return route.fulfill({ status: 409, json: { code: "purchase_not_draft", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      const posted = post(document);
      state.documents.set(document.id, posted);
      return route.fulfill({ json: posted });
    }

    const lineRow = /^\/api\/v1\/purchase-lines\/([^/]+)$/.exec(url.pathname);
    if (lineRow) {
      const document = [...state.documents.values()].find((item) => item.lines.some((each) => (each as { id: string }).id === lineRow[1]))!;
      if (body.expectedRevision !== document.revision) return deny("revision_conflict", 409, { expectedRevision: body.expectedRevision, currentRevision: document.revision });
      if (method === "DELETE") document.lines = document.lines.filter((each) => (each as { id: string }).id !== lineRow[1]);
      else {
        const index = document.lines.findIndex((each) => (each as { id: string }).id === lineRow[1]);
        document.lines[index] = { ...buildLine(document.id, index + 1, body), id: lineRow[1] };
      }
      document.revision += 1;
      return route.fulfill({ json: document });
    }

    return deny("not_found", 404);
  });

  return state;
}

async function addLine(page: Page, values: { product: string; pack: string; quantity: string; rate: string; batch?: string; newBatch?: { number: string; expiresOn?: string; mrp?: string } }) {
  await page.getByRole("button", { name: "Add Line" }).click();
  const dialog = page.getByRole("dialog", { name: "Add Line" });
  await dialog.getByLabel(/^Product/).selectOption({ label: values.product });
  await dialog.getByLabel(/^Pack/).selectOption({ label: values.pack });
  if (values.newBatch) {
    await dialog.getByLabel("A batch not recorded yet").check();
    await dialog.getByLabel(/^Batch number/).fill(values.newBatch.number);
    if (values.newBatch.expiresOn) await dialog.getByLabel("Expires on").fill(values.newBatch.expiresOn);
    if (values.newBatch.mrp) await dialog.getByLabel("MRP per pack").fill(values.newBatch.mrp);
  } else {
    await dialog.getByLabel("Batch *").selectOption({ label: values.batch! });
  }
  await dialog.getByLabel(/^Quantity \(Pack\)/).fill(values.quantity);
  await dialog.getByLabel(/^Rate per Pack/).fill(values.rate);
  await dialog.getByRole("button", { name: "Add Line" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

test("an owner records a supplier invoice and posts it as an intra-State purchase", async ({ page }) => {
  const state = await mockStoreService(page, { documents: [] });
  await page.goto("/app/purchases");
  await expect(page.getByText("No purchase documents yet")).toBeVisible();

  await page.getByRole("link", { name: "New Purchase" }).click();
  // The supplier list is a query: its options exist only once the Store Service has answered.
  const supplierField = page.getByLabel("Supplier *");
  await expect(supplierField.getByRole("option", { name: "Sharma Medicals" })).toHaveCount(1);
  await supplierField.selectOption({ label: "Sharma Medicals" });
  await page.getByLabel(/^Supplier invoice number/).fill("INV-4471");
  await page.getByLabel(/^Invoice date/).fill("2026-09-10");
  await page.getByRole("button", { name: "Create Draft" }).click();

  await expect(page.getByRole("heading", { name: "INV-4471" })).toBeVisible();
  await expect(page.getByText("This purchase is a draft")).toBeVisible();

  await addLine(page, { product: "Crocin 500 mg Tablet", pack: "Strip of 10", batch: "B-2601 · expires 2028-03-31", quantity: "10", rate: "30.00" });
  await expect(page.getByRole("cell", { name: "300.00", exact: true })).toHaveCount(2);

  await page.getByRole("button", { name: "Post Purchase" }).click();
  const dialog = page.getByRole("dialog", { name: "Post Purchase" });
  await expect(dialog.getByText("This cannot be undone")).toBeVisible();
  await dialog.getByRole("button", { name: "Post Purchase" }).click();

  await expect(page.getByText("Posted · read-only")).toBeVisible();
  await expect(page.getByText("CGST + SGST (same State)")).toBeVisible();
  await expect(page.getByText("CGST 6.00%")).toBeVisible();
  await expect(page.getByText("SGST 6.00%")).toBeVisible();
  await expect(page.getByText("IGST", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add Line" })).toHaveCount(0);

  const postings = state.writes.filter((write) => write.path.endsWith("/post"));
  expect(postings).toHaveLength(1);
  expect(String(postings[0].body.idempotencyKey)).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
});

test("a supplier in another State produces IGST alone", async ({ page }) => {
  await mockStoreService(page, { documents: [{ ...emptyDraft(), supplierPartyId: IDs.keralaSupplier }] });
  await page.goto(`/app/purchases/${IDs.draft}`);
  await addLine(page, { product: "Crocin 500 mg Tablet", pack: "Strip of 10", batch: "B-2601 · expires 2028-03-31", quantity: "10", rate: "30.00" });

  await page.getByRole("button", { name: "Post Purchase" }).click();
  await page.getByRole("dialog", { name: "Post Purchase" }).getByRole("button", { name: "Post Purchase" }).click();

  await expect(page.getByText("IGST (different States)")).toBeVisible();
  await expect(page.getByText("IGST 12.00%")).toBeVisible();
  await expect(page.getByText("CGST", { exact: false })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Invoice totals" }).getByText("336.00")).toBeVisible();
});

test("a draft never presents a tax figure it does not yet have", async ({ page }) => {
  await mockStoreService(page);
  await page.goto(`/app/purchases/${IDs.draft}`);
  await addLine(page, { product: "Crocin 500 mg Tablet", pack: "Strip of 10", batch: "B-2601 · expires 2028-03-31", quantity: "4", rate: "25.50" });

  await expect(page.getByText(/GST is determined when this purchase is posted/).first()).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "GST" })).toHaveCount(0);
  await expect(page.getByRole("columnheader", { name: "Line total" })).toHaveCount(0);
  await expect(page.getByRole("cell", { name: "102.00", exact: true })).toHaveCount(2);
});

test("a new batch is proposed on the line instead of being created up front", async ({ page }) => {
  const state = await mockStoreService(page);
  await page.goto(`/app/purchases/${IDs.draft}`);
  await addLine(page, {
    product: "Crocin 500 mg Tablet", pack: "Strip of 10", quantity: "6", rate: "40",
    newBatch: { number: "B-2702", expiresOn: "2029-06-30", mrp: "55.00" }
  });

  await expect(page.getByText("B-2702")).toBeVisible();
  await expect(page.getByText("New batch · expires 2029-06-30")).toBeVisible();
  const added = state.writes.find((write) => write.path.endsWith("/lines"))!;
  expect(added.body).toMatchObject({ batchId: null, newBatchNumber: "B-2702", newBatchMrpPaise: 5_500, quantityPacks: 6, ratePerPackPaise: 4_000 });
});

test("only the chosen Product's packs are offered", async ({ page }) => {
  await mockStoreService(page);
  await page.goto(`/app/purchases/${IDs.draft}`);
  await page.getByRole("button", { name: "Add Line" }).click();
  const dialog = page.getByRole("dialog", { name: "Add Line" });

  await dialog.getByLabel(/^Product/).selectOption({ label: "Dolo 650 mg Tablet" });
  await expect(dialog.getByLabel(/^Pack/).getByRole("option", { name: "Strip of 15" })).toHaveCount(1);
  await expect(dialog.getByLabel(/^Pack/).getByRole("option", { name: "Strip of 10" })).toHaveCount(0);
});

test("a purchase cannot be posted with no lines", async ({ page }) => {
  await mockStoreService(page);
  await page.goto(`/app/purchases/${IDs.draft}`);
  await expect(page.getByText("No lines yet")).toBeVisible();
  await expect(page.getByRole("button", { name: "Post Purchase" })).toBeDisabled();
});

test("a pharmacist can read a purchase but cannot change or post it", async ({ page }) => {
  await mockStoreService(page, { role: "pharmacist" });
  await page.goto(`/app/purchases/${IDs.draft}`);
  await expect(page.getByText("This purchase is still a draft")).toBeVisible();
  await expect(page.getByRole("button", { name: "Add Line" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Post Purchase" })).toHaveCount(0);

  await page.goto("/app/purchases");
  await expect(page.getByText("Read-only access")).toBeVisible();
  await expect(page.getByRole("link", { name: "New Purchase" })).toHaveCount(0);
});

test("the purchase screens stay readable on a narrow viewport", async ({ page }) => {
  await mockStoreService(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/app/purchases/${IDs.draft}`);
  await expect(page.getByRole("heading", { name: "INV-4471" })).toBeVisible();

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);

  await page.getByRole("button", { name: "Add Line" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  const dialogOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(dialogOverflow).toBeLessThanOrEqual(1);
});

test("Purchases is reachable from the sidebar", async ({ page }) => {
  await mockStoreService(page);
  await page.goto("/app/dashboard");
  await page.getByRole("navigation").getByRole("link", { name: "Purchases" }).click();
  await expect(page.getByRole("heading", { name: "Purchases", level: 1 })).toBeVisible();
});
