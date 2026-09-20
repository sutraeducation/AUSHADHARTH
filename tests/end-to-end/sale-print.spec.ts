import { expect, test, type Page } from "@playwright/test";

/**
 * Phase 1L-B printing, in a real browser.
 *
 * A real browser is the only place the layout questions can be answered: whether the roll keeps its
 * content inside 72mm, whether the sheet keeps its content inside the page box, and whether the
 * print stylesheet actually takes effect when the medium is print rather than screen. The native
 * print dialog belongs to the operating system and cannot be inspected from here, so what is proved
 * is that the page asks for it exactly when an operator does — never on load, never twice.
 */

const IDs = {
  sale: "01997a00-0000-7000-8000-000000000002",
  user: "01997a00-0000-7000-8000-000000000080"
};
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const line = (lineNumber: number, description: string) => ({
  lineNumber, description, packLabel: "Strip of 10", batchNumber: `B-${lineNumber}`,
  expiresOn: "2028-03-31", hsnCode: "30049099", quantityText: "1 Strip of 10", quantityBasis: "pack",
  quantityPacks: 1, quantityAtoms: 10, quantityScale: 0, unitLabel: "Tablet", mrpPaise: null,
  sellingRatePaise: 10_000, taxTreatmentKind: "taxable", cgstBasisPoints: 600, sgstBasisPoints: 600,
  igstBasisPoints: 1_200, cessBasisPoints: 0, taxableValuePaise: 10_000, cgstPaise: 600,
  sgstPaise: 600, igstPaise: 0, cessPaise: 0, lineTotalPaise: 11_200
});

function invoice(overrides: Record<string, unknown> = {}, lines = 1) {
  const items = Array.from({ length: lines }, (_, index) =>
    line(index + 1, index === 0 ? "Paracip 500 Tablet" : `Amoxycillin ${index + 1} Capsule 500mg Extended Name`));
  const count = items.length;
  return {
    saleId: IDs.sale,
    document: {
      documentNumber: "INV/2627/000001", businessDate: "2026-09-18", financialYear: "2026-27",
      seriesCode: "INV", postedAtUtc: "2026-09-18T06:30:00.000Z", documentType: "tax_invoice"
    },
    sellerSnapshot: {
      legalName: "Care Pharmacy Private Limited", tradeName: "Care Pharmacy",
      addressLine1: "Shop 14, Guru Nanak Market, Opposite Civil Hospital Gate Number Three",
      addressLine2: "Near Clock Tower Circle", city: "Ludhiana", postalCode: "141001",
      stateName: "Punjab", stateCode: "03", phone: "0161 2345678", email: "counter@care.test",
      licenceText: "Form 20: PB-20-1234, Form 21: PB-21-4321", retailMemoLicenceText: "Form 20: PB-20-1234",
      gstRegistrationStatus: "registered", gstin: "03AAPFU0939F1Z5"
    },
    recipient: {
      walkIn: true, name: null, gstRegistrationStatus: null, gstin: null, stateCode: null,
      snapshotVersion: 1, particularsRequested: false, address: null, delivery: null
    },
    lines: items,
    taxSummary: [{
      taxTreatmentKind: "taxable", cgstBasisPoints: 600, sgstBasisPoints: 600, igstBasisPoints: 1_200,
      cessBasisPoints: 0, taxableValuePaise: 10_000 * count, cgstPaise: 600 * count,
      sgstPaise: 600 * count, igstPaise: 0, cessPaise: 0
    }],
    totals: {
      taxableValuePaise: 10_000 * count, cgstPaise: 600 * count, sgstPaise: 600 * count,
      igstPaise: 0, cessPaise: 0, grandTotalPaise: 11_200 * count
    },
    tender: [{ method: "upi", amountPaise: 11_200 * count, referenceText: "UPI-9988776655443322110099", recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
    regulatory: {
      legacyDocument: false, sellerSnapshotVersion: 1, sellerRegistered: true, taxTreatment: "intra_state",
      einvoiceApplicable: false, reverseCharge: false, complianceSnapshotVersion: 1,
      rule46sDeclaration: "applicable", einvoiceApplicability: null, hsnTurnoverBand: "up_to_5_crore",
      hsnTurnoverFinancialYear: "2026-27", hsnRequiredDigits: 0, dynamicQrSnapshotVersion: 1,
      dynamicQrApplicability: "required"
    },
    ...overrides
  };
}

async function mockStore(page: Page, document: Record<string, unknown> = invoice()) {
  const user = { id: IDs.user, loginIdentifier: "cashier", displayName: "Store Cashier", role: "cashier", revision: 1 };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === `/api/v1/sales/${IDs.sale}/invoice`) return route.fulfill({ json: document });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });
    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });
  // The native dialog is the operating system's. What the page controls — that it asks, once, and
  // only when asked — is recorded here instead.
  await page.addInitScript(() => {
    (window as unknown as { printCalls: number }).printCalls = 0;
    window.print = () => { (window as unknown as { printCalls: number }).printCalls += 1; };
  });
}

const printCalls = (page: Page) => page.evaluate(() => (window as unknown as { printCalls: number }).printCalls);

/** Nothing inside the document may reach past the paper it is laid out for. */
async function overflow(page: Page) {
  return page.evaluate(() => {
    const sheet = document.querySelector(".doc") as HTMLElement;
    const bounds = sheet.getBoundingClientRect();
    const offenders: string[] = [];
    sheet.querySelectorAll<HTMLElement>("*").forEach((node) => {
      const rect = node.getBoundingClientRect();
      if (rect.width > 0 && (rect.right > bounds.right + 1 || rect.left < bounds.left - 1)) {
        offenders.push(`${node.className || node.tagName}: ${Math.round(rect.left)}-${Math.round(rect.right)} vs ${Math.round(bounds.left)}-${Math.round(bounds.right)}`);
      }
    });
    return { offenders, width: Math.round(bounds.width), scrollWidth: document.documentElement.scrollWidth, clientWidth: document.documentElement.clientWidth };
  });
}

test("prints a tax invoice on A4 only when the operator asks", async ({ page }) => {
  await mockStore(page);
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  expect(await printCalls(page)).toBe(0);

  await expect(page.locator(".doc__copy")).toHaveText("ORIGINAL FOR RECIPIENT");
  await expect(page.getByText(/I\/We hereby declare/)).toBeVisible();
  await expect(page.getByText("Authorised Signatory")).toBeVisible();
  await expect(page.getByText(/Ref UPI-9988776655443322110099/)).toBeVisible();

  await page.getByRole("button", { name: "Print" }).click();
  expect(await printCalls(page)).toBe(1);
  // Still one document, still the same number: printing changed nothing.
  await expect(page.locator(".doc__meta")).toContainText("INV/2627/000001");
});

test("keeps an A4 document inside the sheet, across several pages of lines", async ({ page }) => {
  await mockStore(page, invoice({}, 24));
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  const measured = await overflow(page);
  expect(measured.offenders).toEqual([]);
  // A4's printable width at 96dpi is about 718px inside 12mm margins; the sheet stays within it.
  expect(measured.width).toBeLessThanOrEqual(720);
  await expect(page.locator(".doc__lines tbody tr")).toHaveCount(24);
  await expect(page.locator(".doc__grand")).toContainText("2,688.00");
});

test("lays the same document out for an 80mm roll without clipping", async ({ page }) => {
  await mockStore(page, invoice({}, 6));
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await page.getByLabel("Paper").selectOption("thermal80");
  await expect(page.locator(".print-sheet--thermal")).toBeVisible();

  const measured = await overflow(page);
  expect(measured.offenders).toEqual([]);
  // 72mm at 96dpi is about 272px. The roll's content stays inside it.
  expect(measured.width).toBeLessThanOrEqual(275);
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  await expect(page.locator(".doc__copy")).toHaveText("ORIGINAL FOR RECIPIENT");
  await expect(page.getByText("HSN 30049099").first()).toBeVisible();
  await expect(page.getByText(/I\/We hereby declare/)).toBeVisible();
  await expect(page.locator(".doc__grand")).toContainText("672.00");
});

test("applies the print stylesheet only to print media", async ({ page }) => {
  await mockStore(page);
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();

  // On screen the workspace is there; for print it is gone and only the document remains.
  await expect(page.locator(".sidebar")).toBeVisible();
  await page.emulateMedia({ media: "print" });
  await expect(page.locator(".sidebar")).toBeHidden();
  await expect(page.getByRole("button", { name: "Print" })).toBeHidden();
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  await expect(page.locator(".doc__grand")).toContainText("112.00");
  await page.emulateMedia({ media: "screen" });
});

test("shows the print controls without clipping at 390px and 375px", async ({ page }) => {
  await mockStore(page, invoice({}, 3));
  for (const width of [390, 375]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto(`/app/sales/${IDs.sale}/print`);
    await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
    await expect(page.getByLabel("Paper")).toBeVisible();
    await expect(page.getByLabel("Copy")).toBeVisible();
    await expect(page.getByRole("button", { name: "Print" })).toBeVisible();

    const controls = await page.evaluate((viewport) => {
      const offenders: string[] = [];
      document.querySelectorAll<HTMLElement>(".print-hide select, .print-hide button, .master-panel select, .master-panel button").forEach((node) => {
        const rect = node.getBoundingClientRect();
        if (rect.right > viewport + 1 || rect.left < -1) offenders.push(node.id || node.textContent || node.tagName);
      });
      return { offenders, scrollWidth: document.documentElement.scrollWidth };
    }, width);
    expect(controls.offenders, `controls clipped at ${width}px`).toEqual([]);
    expect(controls.scrollWidth, `page scrolls sideways at ${width}px`).toBeLessThanOrEqual(width);

    await page.getByLabel("Paper").selectOption("thermal80");
    const measured = await overflow(page);
    expect(measured.offenders, `roll overflows at ${width}px`).toEqual([]);
  }
});

test("prints a legacy sale as a record, with no statutory copy", async ({ page }) => {
  await mockStore(page, invoice({
    sellerSnapshot: null,
    recipient: { walkIn: true, name: null, gstRegistrationStatus: null, gstin: null, stateCode: null, snapshotVersion: 0, particularsRequested: null, address: null, delivery: null },
    regulatory: {
      legacyDocument: true, sellerSnapshotVersion: 0, sellerRegistered: true, taxTreatment: "intra_state",
      einvoiceApplicable: false, reverseCharge: false, complianceSnapshotVersion: 0,
      rule46sDeclaration: null, einvoiceApplicability: null, hsnTurnoverBand: null,
      hsnTurnoverFinancialYear: null, hsnRequiredDigits: null, dynamicQrSnapshotVersion: 0,
      dynamicQrApplicability: null
    }
  }));
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByRole("heading", { name: "LEGACY TRANSACTION RECORD", level: 1 })).toBeVisible();
  await expect(page.getByText("LEGACY TRANSACTION RECORD — NOT ORIGINAL INVOICE REPRODUCTION")).toBeVisible();
  await expect(page.getByText("ORIGINAL FOR RECIPIENT")).toHaveCount(0);
  await expect(page.getByText("Authorised Signatory")).toHaveCount(0);
  await expect(page.getByLabel("Copy")).toHaveCount(0);
  await expect(page.getByText(/were not recorded on this sale/).first()).toBeVisible();
});

test("refuses to print when the posted facts cannot support a document", async ({ page }) => {
  await mockStore(page, invoice({
    tender: [{ method: "upi", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }]
  }));
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByText("This invoice cannot be printed")).toBeVisible();
  await expect(page.getByRole("button", { name: "Print" })).toHaveCount(0);
  expect(await printCalls(page)).toBe(0);
});

test("survives a refresh and a back-and-forward without printing by itself", async ({ page }) => {
  await mockStore(page);
  await page.goto(`/app/sales/${IDs.sale}/print`);
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  expect(await printCalls(page)).toBe(0);

  await page.goto(`/app/sales/${IDs.sale}`);
  await page.goBack();
  await expect(page.getByRole("heading", { name: "TAX INVOICE", level: 1 })).toBeVisible();
  expect(await printCalls(page)).toBe(0);
});
