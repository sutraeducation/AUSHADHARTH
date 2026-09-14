import { expect, test, type Page } from "@playwright/test";

const IDs = {
  product: "01997b00-0000-7000-8000-000000000001",
  tablet: "01997b00-0000-7000-8000-000000000002",
  strip: "01997b00-0000-7000-8000-000000000003",
  store: "01997b00-0000-7000-8000-000000000004",
  pack: "01997b00-0000-7000-8000-000000000005",
  form: "01997b00-0000-7000-8000-000000000006",
  formulation: "01997b00-0000-7000-8000-000000000010",
  version: "01997b00-0000-7000-8000-000000000020",
  user: "01997b00-0000-7000-8000-000000000030"
};

const stamp = { createdAtUtc: "2026-01-01T00:00:00Z", updatedAtUtc: "2026-01-01T00:00:00Z", archivedAtUtc: null, archiveReason: null };
const system = { status: "ok", apiVersion: "v1", applicationVersion: "0.0.0", compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 } };

const formulation = {
  id: IDs.formulation, kind: "controlled-formulations", revision: 1, status: "active", ...stamp,
  attributes: {
    jurisdiction: "IN", formulationCode: "PARA-500-TAB",
    displayName: "Paracetamol 500 mg Tablet", dosageFormId: null, strengthText: "500 mg",
    verificationState: "verified", sourceNote: "Recorded from the notification as published"
  }
};

const ceilingVersion = {
  id: IDs.version, kind: "price-control-versions", revision: 1, status: "active", ...stamp,
  attributes: {
    controlledFormulationId: IDs.formulation, effectiveFrom: "2026-04-01", effectiveTo: null,
    ceilingPricePaise: 109, ceilingBasis: "per_base_unit", ceilingBasisUnitId: IDs.tablet,
    notificationReference: "S.O. 2222(E)", sourceNote: null
  }
};

function product() {
  return {
    id: IDs.product, productKind: "medicine", brandId: null, dosageFormId: IDs.form,
    baseUnitId: IDs.tablet, quantityScale: 0, displayName: "Paracetamol 500 mg Tablet",
    formulationDescriptor: null, routeDescriptor: null, releaseDescriptor: null,
    hsnCodeId: null, taxCategoryId: null, revision: 1, status: "active", ...stamp,
    companyRoles: [], composition: [],
    packs: [{ id: IDs.pack, productId: IDs.product, containerUnitId: IDs.strip, baseQuantityAtoms: 15, containedPackId: null, containedPackCount: null, skuCode: null, skuStoreId: IDs.store, displayLabel: "Strip of 15", revision: 1, status: "active", ...stamp }]
  };
}

/**
 * A stateful price-control double. The ceiling always follows the assigned formulation and the
 * requested date, exactly as the real resolver does — nothing is ever stored against the Product.
 */
async function mockStoreService(page: Page, options: {
  role?: "owner_admin" | "pharmacist";
  initial?: Record<string, unknown>;
} = {}) {
  const role = options.role ?? "owner_admin";
  const state = {
    current: {
      productId: IDs.product, revision: 1, priceControlStatus: "unknown",
      controlledFormulationId: null as string | null, asOf: "2026-09-14",
      applicableCeiling: null as Record<string, unknown> | null,
      comparability: null as string | null, resolved: false, ...options.initial
    },
    saved: [] as Record<string, unknown>[],
    versions: [ceilingVersion]
  };

  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    const body = request.postData() ? JSON.parse(request.postData()!) : {};
    const user = { id: IDs.user, loginIdentifier: role, displayName: "Price Owner", role, revision: 1 };

    if (url.pathname === "/api/v1/system/info") return route.fulfill({ json: system });
    if (url.pathname === "/api/v1/auth/status") return route.fulfill({ json: { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" } });
    if (url.pathname === "/api/v1/catalog/context") return route.fulfill({ json: { storeId: IDs.store } });
    if (url.pathname === "/api/v1/reference/controlled-formulations") return route.fulfill({ json: [formulation] });
    if (url.pathname === "/api/v1/reference/price-control-versions") return route.fulfill({ json: state.versions });
    if (/^\/api\/v1\/reference\//.test(url.pathname)) return route.fulfill({ json: [] });

    if (/\/price-control$/.test(url.pathname)) {
      if (method === "GET") return route.fulfill({ json: state.current });
      if (role !== "owner_admin") {
        return route.fulfill({ status: 403, json: { code: "authorization_denied", message: "raw backend detail", issues: [], expectedRevision: null, currentRevision: null } });
      }
      state.saved.push(body);
      const controlled = body.priceControlStatus === "controlled";
      state.current = {
        ...state.current,
        revision: state.current.revision + 1,
        priceControlStatus: body.priceControlStatus,
        controlledFormulationId: body.controlledFormulationId ?? null,
        applicableCeiling: controlled ? ceilingVersion.attributes && {
          priceControlVersionId: IDs.version, effectiveFrom: "2026-04-01", effectiveTo: null,
          ceilingPricePaise: 109, ceilingBasis: "per_base_unit", ceilingBasisUnitId: IDs.tablet,
          notificationReference: "S.O. 2222(E)"
        } : null,
        comparability: controlled ? "comparable" : null,
        resolved: controlled
      };
      return route.fulfill({ json: state.current });
    }
    if (/\/tax-classification$/.test(url.pathname)) {
      return route.fulfill({ json: { productId: IDs.product, revision: 1, hsnCodeId: null, taxCategoryId: null, complete: false, asOf: "2026-09-14", applicableRate: null } });
    }
    if (url.pathname === "/api/v1/products") return route.fulfill({ json: [product()] });
    if (/^\/api\/v1\/products\/[^/]+$/.test(url.pathname)) return route.fulfill({ json: product() });
    if (/\/packs\/[^/]+\/(batches|barcodes)$/.test(url.pathname)) return route.fulfill({ json: [] });
    if (/\/packs\/[^/]+\/policy$/.test(url.pathname)) return route.fulfill({ status: 204, body: "" });

    return route.fulfill({ status: 404, json: { code: "not_found", message: "not found", issues: [], expectedRevision: null, currentRevision: null } });
  });

  return state;
}

test("an unassessed medicine says so rather than looking uncontrolled", async ({ page }) => {
  await mockStoreService(page);
  await page.goto(`/app/products/${IDs.product}`);
  const panel = page.getByRole("region", { name: "Price Control" });
  await expect(panel.getByText("Not assessed yet")).toBeVisible();
  await expect(panel.getByText("Nobody has assessed this product yet")).toBeVisible();
  await expect(panel.getByText(/not the same as saying it is uncontrolled/)).toBeVisible();
});

test("an owner assigns a formulation and the resolved ceiling appears with its date", async ({ page }) => {
  const state = await mockStoreService(page);
  await page.goto(`/app/products/${IDs.product}`);
  await page.getByRole("button", { name: "Edit Price Control" }).click();

  const dialog = page.getByRole("dialog", { name: "Edit Price Control" });
  await dialog.getByLabel("Applicability").selectOption("controlled");
  await dialog.getByLabel(/Controlled formulation/).selectOption(IDs.formulation);
  await dialog.getByRole("button", { name: "Save Price Control" }).click();

  await expect(page.getByRole("dialog")).toHaveCount(0);
  const panel = page.getByRole("region", { name: "Price Control" });
  await expect(panel.getByText("Price-controlled")).toBeVisible();
  await expect(panel.getByText("1.09 per base unit")).toBeVisible();
  await expect(panel.getByText("S.O. 2222(E)")).toBeVisible();
  await expect(panel.getByText(/resolved for/)).toBeVisible();

  expect(state.saved).toHaveLength(1);
  expect(state.saved[0]).toMatchObject({
    expectedRevision: 1, priceControlStatus: "controlled", controlledFormulationId: IDs.formulation
  });
  // No price is ever sent from the browser.
  expect(state.saved[0]).not.toHaveProperty("ceilingPricePaise");
});

test("a controlled product with no ceiling in force warns instead of looking unconstrained", async ({ page }) => {
  await mockStoreService(page, {
    initial: { priceControlStatus: "controlled", controlledFormulationId: IDs.formulation, applicableCeiling: null, resolved: false }
  });
  await page.goto(`/app/products/${IDs.product}`);
  const alert = page.getByRole("region", { name: "Price Control" }).getByRole("alert");
  await expect(alert).toContainText("No ceiling is in force");
  await expect(alert).toContainText(/must refuse rather than treat the product as unconstrained/);
});

test("an incomparable ceiling is explained rather than converted", async ({ page }) => {
  await mockStoreService(page, {
    initial: {
      priceControlStatus: "controlled", controlledFormulationId: IDs.formulation,
      applicableCeiling: { priceControlVersionId: IDs.version, effectiveFrom: "2026-04-01", effectiveTo: null, ceilingPricePaise: 1635, ceilingBasis: "per_pack", ceilingBasisUnitId: null, notificationReference: null },
      comparability: "incomparable_pack_basis", resolved: true
    }
  });
  await page.goto(`/app/products/${IDs.product}`);
  const alert = page.getByRole("region", { name: "Price Control" }).getByRole("alert");
  await expect(alert).toContainText("cannot be compared with a selling rate");
  await expect(alert).toContainText(/quoted per pack/);
});

test("the ceiling and the printed MRP are kept visibly distinct", async ({ page }) => {
  await mockStoreService(page, {
    initial: {
      priceControlStatus: "controlled", controlledFormulationId: IDs.formulation,
      applicableCeiling: { priceControlVersionId: IDs.version, effectiveFrom: "2026-04-01", effectiveTo: null, ceilingPricePaise: 109, ceilingBasis: "per_base_unit", ceilingBasisUnitId: IDs.tablet, notificationReference: "S.O. 2222(E)" },
      comparability: "comparable", resolved: true
    }
  });
  await page.goto(`/app/products/${IDs.product}`);
  const panel = page.getByRole("region", { name: "Price Control" });
  await expect(panel.getByText(/A notified ceiling is exclusive of GST/)).toBeVisible();
  await expect(panel.getByText(/never compared with each other/)).toBeVisible();
});

test("a read-only role can see price control but cannot change it", async ({ page }) => {
  await mockStoreService(page, { role: "pharmacist" });
  await page.goto(`/app/products/${IDs.product}`);
  await expect(page.getByRole("heading", { name: "Price Control" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Edit Price Control" })).toHaveCount(0);
});

test("Medicine Price Control is reachable from Reference Data", async ({ page }) => {
  await mockStoreService(page);
  await page.goto("/app/reference");
  await page.getByRole("link", { name: /Medicine Price Control/ }).click();
  await expect(page.getByRole("heading", { name: "Medicine Price Control", level: 1 })).toBeVisible();
  await expect(page.getByRole("cell", { name: "PARA-500-TAB" })).toBeVisible();
  await expect(page.getByRole("cell", { name: "500 mg", exact: true })).toBeVisible();
});

test("the price control panel stays readable on a narrow viewport", async ({ page }) => {
  await mockStoreService(page, {
    initial: {
      priceControlStatus: "controlled", controlledFormulationId: IDs.formulation,
      applicableCeiling: { priceControlVersionId: IDs.version, effectiveFrom: "2026-04-01", effectiveTo: null, ceilingPricePaise: 109, ceilingBasis: "per_base_unit", ceilingBasisUnitId: IDs.tablet, notificationReference: "S.O. 2222(E)" },
      comparability: "comparable", resolved: true
    }
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`/app/products/${IDs.product}`);
  await expect(page.getByRole("heading", { name: "Price Control" })).toBeVisible();

  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);

  await page.getByRole("button", { name: "Edit Price Control" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  const dialogOverflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(dialogOverflow).toBeLessThanOrEqual(1);
});
