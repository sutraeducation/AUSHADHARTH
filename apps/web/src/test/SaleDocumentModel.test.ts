import { describe, expect, it } from "vitest";
import type { Invoice } from "@aushadharth/contracts";
import { RULE_46S_DECLARATION, buildSaleDocument, defaultCopy, treatmentLabel } from "../print/saleDocument";

/**
 * Phase 1L-B — the semantic document, proved without a browser.
 *
 * Everything a printed page asserts is decided in `buildSaleDocument`, so this is where the
 * statutory claims are pinned: which title, which copy labels, whether tax is shown, whether the
 * Rule 46(s) declaration prints, whether a signature space appears, and — most of all — when the
 * model refuses to produce a document at all.
 *
 * The recurring attack is a document claiming more compliance than the posted facts support.
 */

function invoice(overrides: Partial<Invoice> = {}): Invoice {
  return {
    saleId: "01997a00-0000-7000-8000-000000000002",
    document: {
      documentNumber: "INV/2627/000001",
      businessDate: "2026-09-18",
      financialYear: "2026-27",
      seriesCode: "INV",
      postedAtUtc: "2026-09-18T06:30:00.000Z",
      documentType: "tax_invoice"
    },
    sellerSnapshot: {
      legalName: "Care Pharmacy Private Limited",
      tradeName: "Care Pharmacy",
      addressLine1: "12 Mall Road",
      addressLine2: null,
      city: "Ludhiana",
      postalCode: "141001",
      stateName: "Punjab",
      stateCode: "03",
      phone: null,
      email: null,
      licenceText: "Form 20: PB-20-1234",
      retailMemoLicenceText: "Form 20: PB-20-1234",
      gstRegistrationStatus: "registered",
      gstin: "03AAPFU0939F1Z5"
    },
    recipient: {
      walkIn: true,
      name: null,
      gstRegistrationStatus: null,
      gstin: null,
      stateCode: null,
      snapshotVersion: 1,
      particularsRequested: false,
      address: null,
      delivery: null
    },
    lines: [{
      lineNumber: 1,
      description: "Cotton Roll 100g",
      packLabel: "Strip of 10",
      batchNumber: "COT-1",
      expiresOn: "2028-03-31",
      hsnCode: "30059040",
      quantityText: "1 Strip of 10",
      quantityBasis: "pack",
      quantityPacks: 1,
      quantityAtoms: 10,
      quantityScale: 0,
      unitLabel: "Tablet",
      mrpPaise: null,
      sellingRatePaise: 10_000,
      taxTreatmentKind: "taxable",
      cgstBasisPoints: 600,
      sgstBasisPoints: 600,
      igstBasisPoints: 1_200,
      cessBasisPoints: 0,
      taxableValuePaise: 10_000,
      cgstPaise: 600,
      sgstPaise: 600,
      igstPaise: 0,
      cessPaise: 0,
      lineTotalPaise: 11_200
    }],
    taxSummary: [{
      taxTreatmentKind: "taxable",
      cgstBasisPoints: 600,
      sgstBasisPoints: 600,
      igstBasisPoints: 1_200,
      cessBasisPoints: 0,
      taxableValuePaise: 10_000,
      cgstPaise: 600,
      sgstPaise: 600,
      igstPaise: 0,
      cessPaise: 0
    }],
    totals: {
      taxableValuePaise: 10_000,
      cgstPaise: 600,
      sgstPaise: 600,
      igstPaise: 0,
      cessPaise: 0,
      grandTotalPaise: 11_200
    },
    tender: [{ method: "cash", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
    regulatory: {
      legacyDocument: false,
      sellerSnapshotVersion: 1,
      sellerRegistered: true,
      taxTreatment: "intra_state",
      einvoiceApplicable: false,
      reverseCharge: false,
      complianceSnapshotVersion: 1,
      rule46sDeclaration: "not_applicable",
      einvoiceApplicability: null,
      hsnTurnoverBand: "up_to_5_crore",
      hsnTurnoverFinancialYear: "2026-27",
      hsnRequiredDigits: 0,
      dynamicQrSnapshotVersion: 1,
      dynamicQrApplicability: "not_required"
    },
    ...overrides
  };
}

const typed = (documentType: Invoice["document"]["documentType"], rest: Partial<Invoice> = {}) =>
  invoice({ document: { ...invoice().document, documentType }, ...rest });

describe("Sale document model", () => {
  // --- Titles come from the Store Service, never from the page ---------------------------------

  it("titles each document as the Store Service classified it", () => {
    expect(buildSaleDocument(typed("tax_invoice")).title).toBe("TAX INVOICE");
    expect(buildSaleDocument(typed("bill_of_supply")).title).toBe("BILL OF SUPPLY");
    expect(buildSaleDocument(typed("invoice_cum_bill_of_supply")).title).toBe("INVOICE-CUM-BILL OF SUPPLY");
    const memo = typed("retail_cash_memo", {
      sellerSnapshot: { ...invoice().sellerSnapshot!, gstin: null, gstRegistrationStatus: "unregistered" },
      regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
    });
    expect(buildSaleDocument(memo).title).toBe("RETAIL CASH MEMO");
  });

  /** The page cannot invent a document the service would not stand behind. */
  it("refuses a sale whose statutory class the service could not settle", () => {
    const model = buildSaleDocument(typed("document_classification_unresolved", {
      document: { ...invoice().document, documentType: "document_classification_unresolved", documentTypeReason: "A mixed taxable and exempt supply to a registered recipient is not covered by Rule 46A." }
    }));
    expect(model.printable).toBe(false);
    expect(model.refusal?.code).toBe("document_classification_unresolved");
    expect(model.refusal?.detail).toContain("Rule 46A");
    expect(model.title).toBe("");
    expect(model.copies).toEqual([]);
  });

  // --- Copy semantics --------------------------------------------------------------------------

  it("offers the Rule 48(1) triplicate, in order, only on a tax invoice", () => {
    const model = buildSaleDocument(typed("tax_invoice"));
    expect(model.copies.map((copy) => copy.label)).toEqual([
      "ORIGINAL FOR RECIPIENT",
      "DUPLICATE FOR TRANSPORTER",
      "TRIPLICATE FOR SUPPLIER"
    ]);
    expect(model.copies.every((copy) => copy.statutory)).toBe(true);
    expect(defaultCopy(model)).toBe("original");
  });

  /** A bill of supply has no copy rule in Rule 49, so it inherits none of the invoice's labels. */
  it("never puts tax-invoice copy labels on a bill of supply", () => {
    const model = buildSaleDocument(typed("bill_of_supply"));
    const labels = model.copies.map((copy) => copy.label);
    expect(labels).toEqual(["CUSTOMER COPY", "STORE COPY"]);
    expect(labels.some((label) => label.includes("TRANSPORTER") || label.includes("TRIPLICATE"))).toBe(false);
    expect(model.copies.every((copy) => !copy.statutory)).toBe(true);
    expect(model.copies.every((copy) => (copy.note ?? "").length > 0)).toBe(true);
  });

  /**
   * Whether Rule 48(1) reaches a Rule 46A invoice-cum-bill of supply is unsettled. The labels are
   * offered so the operator can comply if it does, and are marked as this product's decision.
   */
  it("treats the invoice-cum-bill of supply copy set as a product decision, not statute", () => {
    const model = buildSaleDocument(typed("invoice_cum_bill_of_supply"));
    expect(model.copies.map((copy) => copy.label)).toEqual([
      "ORIGINAL FOR RECIPIENT",
      "DUPLICATE FOR TRANSPORTER",
      "TRIPLICATE FOR SUPPLIER"
    ]);
    expect(model.copies.every((copy) => !copy.statutory)).toBe(true);
    expect(model.copies[0].note).toContain("no primary source settles");
  });

  it("names the drug memo's retained copy without borrowing a GST label", () => {
    const memo = typed("retail_cash_memo", {
      regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
    });
    const model = buildSaleDocument(memo);
    expect(model.copies.map((copy) => copy.label)).toEqual(["CUSTOMER COPY", "LICENSEE'S RECORD COPY"]);
    expect(model.copies.every((copy) => !copy.statutory)).toBe(true);
    expect(model.copies[1].note).toContain("Drugs Rules r.65");
    // The database is never claimed to be the retained copy.
    expect(model.copies[1].note).toContain("not claimed to satisfy");
  });

  /** "DUPLICATE" alone means the transporter's copy in Rule 48, so nothing else may carry it. */
  it("uses DUPLICATE only for the statutory transporter copy", () => {
    for (const documentType of ["bill_of_supply", "invoice_cum_bill_of_supply", "retail_cash_memo"] as const) {
      const model = buildSaleDocument(typed(documentType, {
        regulatory: {
          ...invoice().regulatory,
          sellerRegistered: documentType !== "retail_cash_memo",
          rule46sDeclaration: documentType === "retail_cash_memo" ? null : "not_applicable",
          dynamicQrApplicability: documentType === "retail_cash_memo" ? null : "not_required"
        }
      }));
      for (const copy of model.copies) {
        if (copy.label.includes("DUPLICATE")) expect(copy.label).toBe("DUPLICATE FOR TRANSPORTER");
      }
    }
    const invoiceCopies = buildSaleDocument(typed("tax_invoice")).copies;
    expect(invoiceCopies.filter((copy) => copy.label.includes("DUPLICATE"))).toHaveLength(1);
  });

  // --- Tax presentation ------------------------------------------------------------------------

  it("shows tax columns only where tax was charged as a GST document", () => {
    expect(buildSaleDocument(typed("tax_invoice")).showTax).toBe(true);
    expect(buildSaleDocument(typed("invoice_cum_bill_of_supply")).showTax).toBe(true);
    expect(buildSaleDocument(typed("bill_of_supply")).showTax).toBe(false);
    const memo = buildSaleDocument(typed("retail_cash_memo", {
      regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
    }));
    expect(memo.showTax).toBe(false);
  });

  /**
   * Rule 46A's document carries taxable and exempted supplies together. Only the taxable part has a
   * taxable value, and a page that lumped the two under one "Taxable value" would say otherwise.
   */
  it("names each value band of a mixed basket from the posted tax summary", () => {
    const summary = invoice().taxSummary[0];
    const model = buildSaleDocument(invoice({
      document: { ...invoice().document, documentType: "invoice_cum_bill_of_supply" },
      // Exempt first, as the service may well group it: the document still reads taxable first.
      taxSummary: [
        { ...summary, taxTreatmentKind: "exempt", cgstBasisPoints: 0, sgstBasisPoints: 0, igstBasisPoints: 0, taxableValuePaise: 2_700, cgstPaise: 0, sgstPaise: 0 },
        summary
      ]
    }));
    expect(model.valueBands).toEqual([
      { treatment: "taxable", label: "Taxable value", amountPaise: 10_000 },
      { treatment: "exempt", label: "Exempt value", amountPaise: 2_700 }
    ]);
  });

  it("keeps one value band on a single-treatment document, and calls an untaxed one Value", () => {
    expect(buildSaleDocument(typed("tax_invoice")).valueBands)
      .toEqual([{ treatment: "taxable", label: "Taxable value", amountPaise: 10_000 }]);
    const billOfSupply = buildSaleDocument(typed("bill_of_supply", {
      regulatory: { ...invoice().regulatory, dynamicQrApplicability: null }
    }));
    expect(billOfSupply.valueBands).toEqual([{ treatment: null, label: "Value", amountPaise: 10_000 }]);
  });

  /** A word, not a zero: an exempted supply is not a supply taxed at nothing. */
  it("names an untaxed treatment and leaves a taxed one to its figures", () => {
    expect(treatmentLabel("exempt")).toBe("Exempt");
    expect(treatmentLabel("nil_rated")).toBe("Nil-rated");
    expect(treatmentLabel("non_gst")).toBe("Non-GST");
    expect(treatmentLabel("taxable")).toBeNull();
    expect(treatmentLabel(null)).toBeNull();
  });

  it("passes totals and lines through untouched", () => {
    const source = invoice();
    const model = buildSaleDocument(source);
    expect(model.totals).toEqual(source.totals);
    expect(model.lines).toEqual(source.lines);
    expect(model.taxSummary).toEqual(source.taxSummary);
    expect(model.payments).toEqual(source.tender);
    // The quantity is the service's text, not a number the page re-formats from atoms.
    expect(model.lines[0].quantityText).toBe("1 Strip of 10");
  });

  it("reports the frozen reverse-charge fact on a GST document and nothing on a memo", () => {
    expect(buildSaleDocument(typed("tax_invoice")).reverseCharge).toBe(false);
    const memo = buildSaleDocument(typed("retail_cash_memo", {
      regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
    }));
    expect(memo.reverseCharge).toBeNull();
  });

  // --- Rule 46(s) ------------------------------------------------------------------------------

  it("prints the Rule 46(s) declaration verbatim when the posted fact says it applies", () => {
    const model = buildSaleDocument(invoice({
      regulatory: { ...invoice().regulatory, rule46sDeclaration: "applicable" }
    }));
    expect(model.declaration).toBe(RULE_46S_DECLARATION);
    expect(model.declaration).toContain("we are not required to prepare an invoice in terms of the provisions of the said sub-rule.");
  });

  it("omits the declaration when the posted fact says it does not apply", () => {
    expect(buildSaleDocument(invoice()).declaration).toBeNull();
  });

  /** An invoice whose declaration answer was never settled is refused, not printed. */
  it("refuses a GST document with no resolved Rule 46(s) answer", () => {
    const model = buildSaleDocument(invoice({
      regulatory: { ...invoice().regulatory, rule46sDeclaration: null }
    }));
    expect(model.printable).toBe(false);
    expect(model.refusal?.code).toBe("rule46s_declaration_unresolved");
  });

  /**
   * Clause (s) attaches "in all cases where an invoice is issued", and section 31(3)(c) has a bill
   * of supply issued INSTEAD of a tax invoice. Rule 49 carries across only "the provisos to rule
   * 46", not its clauses. So the same seller fact that prints on the invoice does not print here.
   */
  it("keeps the Rule 46(s) declaration off a bill of supply", () => {
    const seller = { ...invoice().regulatory, rule46sDeclaration: "applicable" as const };
    expect(buildSaleDocument(invoice({ regulatory: seller })).declaration).toBe(RULE_46S_DECLARATION);

    const billOfSupply = buildSaleDocument(invoice({
      document: { ...invoice().document, documentType: "bill_of_supply" },
      regulatory: { ...seller, dynamicQrApplicability: null }
    }));
    expect(billOfSupply.printable).toBe(true);
    expect(billOfSupply.declaration).toBeNull();
  });

  /** And an unsettled invoice fact does not block a document that never needed it. */
  it("prints a bill of supply whose Rule 46(s) answer was never settled", () => {
    const model = buildSaleDocument(invoice({
      document: { ...invoice().document, documentType: "bill_of_supply" },
      regulatory: { ...invoice().regulatory, rule46sDeclaration: null, dynamicQrApplicability: null }
    }));
    expect(model.printable).toBe(true);
    expect(model.declaration).toBeNull();
  });

  /**
   * The invoice-cum-bill of supply keeps the declaration: the Rule 46A proviso inserted by
   * Notification No. 26/2022-CT requires that document to contain the Rule 46 particulars.
   */
  it("keeps the declaration on an invoice-cum-bill of supply", () => {
    const model = buildSaleDocument(invoice({
      document: { ...invoice().document, documentType: "invoice_cum_bill_of_supply" },
      regulatory: { ...invoice().regulatory, rule46sDeclaration: "applicable" }
    }));
    expect(model.declaration).toBe(RULE_46S_DECLARATION);
  });

  // --- Notification No. 14/2020-CT -------------------------------------------------------------

  it("refuses a document that must carry a payment cross-reference and cannot", () => {
    const model = buildSaleDocument(invoice({
      tender: [{ method: "upi", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
      regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
    }));
    expect(model.printable).toBe(false);
    expect(model.refusal?.code).toBe("payment_reference_missing");
  });

  it("prints a required cross-reference document when the payment facts are there", () => {
    const cash = buildSaleDocument(invoice({
      regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
    }));
    expect(cash.printable).toBe(true);
    expect(cash.paymentCrossReferenceRequired).toBe(true);

    const upi = buildSaleDocument(invoice({
      tender: [{ method: "upi", amountPaise: 11_200, referenceText: "UPI-4471", recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
      regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
    }));
    expect(upi.printable).toBe(true);
    expect(upi.payments[0].referenceText).toBe("UPI-4471");
  });

  it("treats a blank reference as no reference", () => {
    const model = buildSaleDocument(invoice({
      tender: [{ method: "card", amountPaise: 11_200, referenceText: "   ", recordedAtUtc: "2026-09-18T06:30:00.100Z" }],
      regulatory: { ...invoice().regulatory, dynamicQrApplicability: "required" }
    }));
    expect(model.printable).toBe(false);
    expect(model.refusal?.code).toBe("payment_reference_missing");
  });

  it("asks nothing of the tender where the notification does not apply", () => {
    const model = buildSaleDocument(invoice({
      tender: [{ method: "upi", amountPaise: 11_200, referenceText: null, recordedAtUtc: "2026-09-18T06:30:00.100Z" }]
    }));
    expect(model.printable).toBe(true);
    expect(model.paymentCrossReferenceRequired).toBe(false);
  });

  // --- Signature -------------------------------------------------------------------------------

  it("leaves a signature space on a GST document and none on a retail memo", () => {
    expect(buildSaleDocument(typed("tax_invoice")).signatureRequired).toBe(true);
    expect(buildSaleDocument(typed("bill_of_supply")).signatureRequired).toBe(true);
    expect(buildSaleDocument(typed("invoice_cum_bill_of_supply")).signatureRequired).toBe(true);
    const memo = buildSaleDocument(typed("retail_cash_memo", {
      regulatory: { ...invoice().regulatory, sellerRegistered: false, rule46sDeclaration: null, dynamicQrApplicability: null }
    }));
    expect(memo.signatureRequired).toBe(false);
  });

  // --- Legacy ----------------------------------------------------------------------------------

  const legacyCases: Array<[string, Partial<Invoice>]> = [
    ["no seller snapshot", { sellerSnapshot: null, regulatory: { ...invoice().regulatory, legacyDocument: true, sellerSnapshotVersion: 0 } }],
    ["no recipient snapshot", { recipient: { ...invoice().recipient, snapshotVersion: 0, particularsRequested: null } }],
    ["no compliance snapshot", { regulatory: { ...invoice().regulatory, complianceSnapshotVersion: 0, rule46sDeclaration: null, dynamicQrSnapshotVersion: 0, dynamicQrApplicability: null } }]
  ];

  it.each(legacyCases)("renders a sale with %s as a legacy record, not an invoice", (_case, overrides) => {
    const model = buildSaleDocument(invoice(overrides));
    expect(model.printable).toBe(true);
    expect(model.legacy).toBe(true);
    expect(model.title).toBe("LEGACY TRANSACTION RECORD");
    expect(model.legacyNotice).toBe("LEGACY TRANSACTION RECORD — NOT ORIGINAL INVOICE REPRODUCTION");
    // No statutory claim of any kind travels with it.
    expect(model.copies).toEqual([]);
    expect(model.declaration).toBeNull();
    expect(model.signatureRequired).toBe(false);
    expect(model.unrecorded.length).toBeGreaterThan(0);
  });

  it("says what a legacy record does not know instead of filling it in", () => {
    const model = buildSaleDocument(invoice({
      sellerSnapshot: null,
      recipient: { ...invoice().recipient, snapshotVersion: 0, particularsRequested: null },
      regulatory: { ...invoice().regulatory, legacyDocument: true, sellerSnapshotVersion: 0, complianceSnapshotVersion: 0, rule46sDeclaration: null }
    }));
    expect(model.seller).toBeNull();
    expect(model.unrecorded.join(" ")).toContain("were not recorded");
    expect(model.unrecorded).toHaveLength(3);
  });

  it("shows a legacy record's tax exactly as it was recorded", () => {
    const taxed = buildSaleDocument(invoice({
      sellerSnapshot: null,
      regulatory: { ...invoice().regulatory, legacyDocument: true, sellerSnapshotVersion: 0 }
    }));
    expect(taxed.showTax).toBe(true);
    expect(taxed.totals.cgstPaise).toBe(600);

    const untaxed = buildSaleDocument(invoice({
      sellerSnapshot: null,
      totals: { ...invoice().totals, cgstPaise: 0, sgstPaise: 0, grandTotalPaise: 10_000 },
      regulatory: { ...invoice().regulatory, legacyDocument: true, sellerSnapshotVersion: 0 }
    }));
    expect(untaxed.showTax).toBe(false);
  });

  // --- Structural guarantees -------------------------------------------------------------------

  /**
   * The model takes the canonical invoice and nothing else. There is no parameter through which a
   * caller could pass today's Store profile, Party record or product master, so no page can "fix"
   * an old document with new data.
   */
  it("accepts no source of facts other than the canonical invoice", () => {
    expect(buildSaleDocument).toHaveLength(1);
  });

  it("carries the frozen drug sale licence and never a licence type", () => {
    const model = buildSaleDocument(invoice());
    expect(model.retailMemoLicenceText).toBe("Form 20: PB-20-1234");
    const none = buildSaleDocument(invoice({
      sellerSnapshot: { ...invoice().sellerSnapshot!, retailMemoLicenceText: null }
    }));
    expect(none.retailMemoLicenceText).toBeNull();
  });

  it("keeps the document's own number and dates", () => {
    const model = buildSaleDocument(invoice());
    expect(model.documentNumber).toBe("INV/2627/000001");
    expect(model.businessDate).toBe("2026-09-18");
    expect(model.postedAtUtc).toBe("2026-09-18T06:30:00.000Z");
    expect(model.financialYear).toBe("2026-27");
  });
});
