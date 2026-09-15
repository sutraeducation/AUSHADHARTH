import { expect, test, type Page } from "@playwright/test";

/**
 * Phase 1I returns, in a real browser.
 *
 * What these guard is that the operator can see what is actually happening: what was sold, what is
 * already back, what is left, where the goods go, and that quarantined stock is not stock they can
 * sell. A return that looks right and quietly puts a medicine back on the shelf would be worse than
 * one that refuses.
 */

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000001",
  purchase: "01997a00-0000-7000-8000-000000000002",
  ret: "01997a00-0000-7000-8000-000000000003",
  saleLine: "01997a00-0000-7000-8000-000000000010",
  purchaseLine: "01997a00-0000-7000-8000-000000000011",
  returnLine: "01997a00-0000-7000-8000-000000000012",
  product: "01997a00-0000-7000-8000-000000000020",
  pack: "01997a00-0000-7000-8000-000000000030",
  batch: "01997a00-0000-7000-8000-000000000040",
  store: "01997a00-0000-7000-8000-000000000070",
  user: "01997a00-0000-7000-8000-000000000080"
};

const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

function returnableLine(overrides: Record<string, unknown> = {}) {
  return {
    originalLineId: IDs.saleLine, lineNumber: 1, productId: IDs.product, productPackId: IDs.pack,
    batchId: IDs.batch, productDisplayName: "Crocin 500 mg Tablet", packDisplayLabel: "Strip of 10",
    baseUnitLabel: "Tablet", batchNumber: "B-2601", batchExpiresOn: "2028-03-31",
    quantityBasis: "pack", originalQuantity: 2, originalQuantityAtoms: 20,
    alreadyReturnedAtoms: 0, returnableAtoms: 20, returnableQuantity: 2,
    taxableValuePaise: 16_000, cgstPaise: 960, sgstPaise: 960, igstPaise: 0, cessPaise: 0,
    lineTotalPaise: 17_920, ...overrides
  };
}

function returnLine(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.returnLine, returnDocumentId: IDs.ret, lineNumber: 1,
    originalSaleLineId: IDs.saleLine, originalPurchaseLineId: null,
    productId: IDs.product, productPackId: IDs.pack, batchId: IDs.batch,
    quantityBasis: "pack", quantityPacks: 1, quantityAtoms: 10, disposition: "quarantined",
    productDisplayName: "Crocin 500 mg Tablet", packDisplayLabel: "Strip of 10",
    baseUnitLabel: "Tablet", batchNumber: "B-2601", batchExpiresOn: "2028-03-31",
    hsnCodeId: null, hsnCode: "30049099", taxCategoryId: null, taxTreatmentKind: "taxable",
    taxRateVersionId: null, cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200,
    cessBasisPoints: 0, taxableValuePaise: 8_000, cgstPaise: 480, sgstPaise: 480,
    igstPaise: 0, cessPaise: 0, lineTotalPaise: 8_960, ...overrides
  };
}

function draftReturn(overrides: Record<string, unknown> = {}) {
  return {
    id: IDs.ret, storeId: IDs.store, returnKind: "sales_return",
    originalSaleDocumentId: IDs.sale, originalPurchaseDocumentId: null,
    originalDocumentNumber: "INV/2627/000001", originalDocumentDate: "2026-09-12",
    businessDate: "2026-09-12", status: "draft", revision: 2,
    seriesCode: null, financialYear: null, sequenceValue: null, documentNumber: null,
    storeGstRegistrationStatus: null, storeNormalizedGstin: null,
    storePlaceOfSupplyStateId: null, storeStateCode: null,
    counterpartyPartyId: null, counterpartyDisplayName: "Rahul Deshmukh",
    counterpartyGstRegistrationStatus: null, counterpartyNormalizedGstin: null,
    counterpartyStateCode: null, taxTreatment: null, taxAdjustmentStatus: null,
    taxAdjustmentReason: null, gstRoute: null,
    taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0,
    grandTotalPaise: 0, createdByUserId: IDs.user,
    createdAtUtc: "2026-09-12T05:00:00Z", updatedAtUtc: "2026-09-12T05:00:00Z",
    postedByUserId: null, postedAtUtc: null,
    lines: [returnLine()], supplierCreditNotes: [], ...overrides
  };
}

async function mockStoreService(page: Page, options: {
  role?: "owner_admin" | "pharmacist" | "cashier";
  returnable?: Record<string, unknown>;
  document?: Record<string, unknown>;
} = {}) {
  const role = options.role ?? "pharmacist";
  const state = {
    document: structuredClone(options.document ?? draftReturn()),
    writes: [] as Array<{ method: string; path: string; body: Record<string, unknown> }>
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) as Record<string, unknown> : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Return User", role, revision: 1 };
    if (method !== "GET") state.writes.push({ method, path: url.pathname, body });

    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") {
      return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    }
    if (/returnable-lines$/.test(url.pathname)) {
      return route.fulfill({
        json: options.returnable ?? {
          documentId: IDs.sale, documentNumber: "INV/2627/000001", documentDate: "2026-09-12",
          counterpartyDisplayName: "Rahul Deshmukh", lines: [returnableLine()]
        }
      });
    }
    if (url.pathname === "/api/v1/returns" && method === "GET") {
      const { lines: _l, supplierCreditNotes: _s, ...header } = state.document as Record<string, unknown>;
      return route.fulfill({ json: [header] });
    }
    if (url.pathname === "/api/v1/returns" && method === "POST") {
      return route.fulfill({ status: 201, json: { ...state.document, lines: [] } });
    }
    if (/^\/api\/v1\/returns\/[^/]+\/lines$/.test(url.pathname)) {
      return route.fulfill({ status: 201, json: state.document });
    }
    if (/^\/api\/v1\/returns\/[^/]+\/quote$/.test(url.pathname)) {
      const lines = (state.document as { lines: Array<Record<string, number>> }).lines;
      const sum = (key: string) => lines.reduce((total, line) => total + Number(line[key]), 0);
      return route.fulfill({
        json: {
          returnDocumentId: IDs.ret, revision: 2,
          taxableValuePaise: sum("taxableValuePaise"), cgstPaise: sum("cgstPaise"),
          sgstPaise: sum("sgstPaise"), igstPaise: 0, cessPaise: 0,
          grandTotalPaise: sum("lineTotalPaise")
        }
      });
    }
    if (/^\/api\/v1\/returns\/[^/]+\/post$/.test(url.pathname)) {
      state.document = draftReturn({
        returnKind: (state.document as { returnKind: string }).returnKind,
        status: "posted", revision: 3, seriesCode: "SR", financialYear: "2026-27",
        sequenceValue: 1, documentNumber: "SR/2627/000001",
        storeNormalizedGstin: "27AAACX0000A1Z9", taxTreatment: "intra_state",
        taxAdjustmentStatus: (body.taxAdjustmentStatus as string) ?? null,
        gstRoute: (body.gstRoute as string) ?? null,
        taxableValuePaise: 8_000, cgstPaise: 480, sgstPaise: 480, grandTotalPaise: 8_960,
        postedByUserId: IDs.user, postedAtUtc: "2026-09-12T06:30:00Z",
        lines: [returnLine({ disposition: (state.document as { returnKind: string }).returnKind === "sales_return" ? "quarantined" : null })]
      });
      return route.fulfill({ json: state.document });
    }
    if (/^\/api\/v1\/returns\/[^/]+$/.test(url.pathname)) return route.fulfill({ json: state.document });
    if (/^\/api\/v1\/return-lines\/[^/]+$/.test(url.pathname)) {
      state.document = { ...(state.document as object), lines: [], revision: 3 } as typeof state.document;
      return route.fulfill({ json: state.document });
    }
    return route.fulfill({ status: 404, json: { code: "not_found", message: "", issues: [] } });
  });

  return state;
}

test.describe("Phase 1I returns", () => {
  test("shows the original, what is left, and takes a partial return back into quarantine", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}/return`);
    await expect(page.getByRole("heading", { name: "Sales return", level: 1 })).toBeVisible();

    // The operator can read the original without decoding anything.
    await expect(page.getByText("Against INV/2627/000001 of 2026-09-12 · Rahul Deshmukh")).toBeVisible();
    const row = page.getByRole("row").nth(1);
    await expect(row.getByText("Crocin 500 mg Tablet")).toBeVisible();
    await expect(row.getByText("B-2601")).toBeVisible();
    await expect(page.getByText(IDs.product)).toHaveCount(0);

    await page.getByLabel(/Quantity of Crocin/).fill("1");
    await page.getByRole("button", { name: "Start return" }).click();

    await expect(page).toHaveURL(new RegExp(`/app/returns/${IDs.ret}$`));
    const added = state.writes.find((write) => write.path.endsWith("/lines"))!;
    expect(added.body).toMatchObject({ quantity: 1, disposition: "quarantined" });
    expect(added.body).not.toHaveProperty("quantityAtoms");
  });

  test("never offers to put a returned medicine straight back on the shelf", async ({ page }) => {
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}/return`);
    const disposition = page.getByLabel(/Where the returned Crocin/);
    await expect(disposition).toBeVisible();
    const options = await disposition.locator("option").allTextContents();
    expect(options).toHaveLength(2);
    expect(options.join(" ").toLowerCase()).not.toContain("sellable");
  });

  test("writes off an expired batch instead of offering quarantine", async ({ page }) => {
    await mockStoreService(page, {
      returnable: {
        documentId: IDs.sale, documentNumber: "INV/2627/000001", documentDate: "2026-09-12",
        counterpartyDisplayName: "Rahul Deshmukh",
        lines: [returnableLine({ batchExpiresOn: "2020-01-31" })]
      }
    });
    await page.goto(`/app/sales/${IDs.sale}/return`);
    await expect(page.getByText("Expired 2020-01-31")).toBeVisible();
    await expect(page.getByText("Written off — this batch has expired")).toBeVisible();
    await expect(page.getByLabel(/Where the returned/)).toHaveCount(0);
  });

  test("posts the GST treatment the operator chose and shows the issued number", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/returns/${IDs.ret}`);
    await expect(page.getByRole("heading", { name: "Sales return", level: 1 })).toBeVisible();
    await expect(page.getByText("89.60").first()).toBeVisible();

    await page.getByLabel("GST treatment").selectOption("tax_adjustable");
    await page.getByRole("button", { name: "Post return" }).click();

    await expect(page.getByRole("heading", { name: "SR/2627/000001", level: 1 })).toBeVisible();
    const posted = state.writes.find((write) => write.path.endsWith("/post"))!;
    expect(posted.body).toMatchObject({ taxAdjustmentStatus: "tax_adjustable", gstRoute: null });
    await expect(page.getByText(/not available to sell until a pharmacist releases them/)).toBeVisible();
  });

  test("calls a purchase return what it is, and never a debit note", async ({ page }) => {
    const state = await mockStoreService(page, {
      role: "owner_admin",
      document: draftReturn({ returnKind: "purchase_return", lines: [returnLine({ disposition: null })] })
    });
    await page.goto(`/app/returns/${IDs.ret}`);
    await expect(page.getByRole("heading", { name: "Purchase return", level: 1 })).toBeVisible();
    await expect(page.getByText(/a debit note is issued by the supplier, never by us/)).toBeVisible();
    await expect(page.getByLabel("GST treatment")).toHaveCount(0);

    await page.getByLabel("How the goods are going back").selectOption("fresh_supply");
    await page.getByRole("button", { name: "Post return" }).click();
    await expect(page.getByRole("heading", { name: "SR/2627/000001", level: 1 })).toBeVisible();
    const posted = state.writes.find((write) => write.path.endsWith("/post"))!;
    expect(posted.body).toMatchObject({ gstRoute: "fresh_supply", taxAdjustmentStatus: null });
  });

  test("does not offer posting to a cashier", async ({ page }) => {
    await mockStoreService(page, { role: "cashier" });
    await page.goto(`/app/returns/${IDs.ret}`);
    await expect(page.getByRole("heading", { name: "Sales return", level: 1 })).toBeVisible();
    await expect(page.getByRole("button", { name: "Post return" })).toHaveCount(0);
    await expect(page.getByText(/A pharmacist or the owner posts a sales return/)).toBeVisible();
  });

  test("sends one posting even when the button is clicked three times", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/returns/${IDs.ret}`);
    const button = page.getByRole("button", { name: "Post return" });
    await expect(button).toBeVisible();
    await button.click({ clickCount: 3, delay: 0 });
    await expect(page.getByRole("heading", { name: "SR/2627/000001", level: 1 })).toBeVisible();
    expect(state.writes.filter((write) => write.path.endsWith("/post"))).toHaveLength(1);
  });

  test("refuses more than is left before troubling the service", async ({ page }) => {
    const state = await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}/return`);
    await page.getByLabel(/Quantity of Crocin/).fill("5");
    await page.getByRole("button", { name: "Start return" }).click();
    await expect(page.getByRole("alert")).toContainText("Only 2 Strip of 10");
    expect(state.writes.some((write) => write.path.endsWith("/lines"))).toBe(false);
  });

  test("works at 390px with the quantity and disposition still reachable", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await mockStoreService(page);
    await page.goto(`/app/sales/${IDs.sale}/return`);
    await expect(page.getByLabel(/Quantity of Crocin/)).toBeVisible();
    await expect(page.getByLabel(/Where the returned Crocin/)).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  });

  test("offers no way to edit a posted return", async ({ page }) => {
    await mockStoreService(page);
    await page.goto(`/app/returns/${IDs.ret}`);
    await page.getByRole("button", { name: "Post return" }).click();
    await expect(page.getByRole("heading", { name: "SR/2627/000001", level: 1 })).toBeVisible();
    await expect(page.getByRole("button", { name: "Post return" })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Remove" })).toHaveCount(0);
    await expect(page.getByLabel("GST treatment")).toHaveCount(0);
  });
});
