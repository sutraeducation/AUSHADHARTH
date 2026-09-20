import type { Invoice } from "@aushadharth/contracts";

/**
 * Phase 1L-B — the semantic Sale document.
 *
 * One model, two layouts. Everything the printed page asserts is decided here, from the canonical
 * invoice DTO and nothing else: the title, whether tax is shown, which copies exist, whether the
 * Rule 46(s) declaration prints, whether a signature space is needed, and whether the document may
 * be printed at all. `A4Document` and `ThermalDocument` are layout only — they arrange these facts
 * and decide nothing.
 *
 * The rules this file encodes, and where they come from:
 *
 * * **Title** is the Store Service's own classification. The renderer never re-derives it: a
 *   document is a tax invoice because the posted facts made it one, not because a page guessed.
 * * **Copies.** CGST Rule 48(1) requires an invoice for goods in triplicate — ORIGINAL FOR
 *   RECIPIENT, DUPLICATE FOR TRANSPORTER, TRIPLICATE FOR SUPPLIER. Rule 49 prescribes no copy rule
 *   for a bill of supply, and none is invented for one; its copies here are product labels, marked
 *   as such. For a Rule 46A invoice-cum-bill of supply the position is NOT settled by any primary
 *   source, so the three labels are offered conservatively and flagged as a product decision.
 * * **Rule 46(s)** prints verbatim when, and only when, the posted snapshot says it applies AND the
 *   document issued is an invoice. Clause (s) attaches "in all cases where an invoice is issued",
 *   and Rule 49 carries only "the provisos to rule 46" into a bill of supply — not its clauses — so
 *   a bill of supply, issued under section 31(3)(c) instead of a tax invoice, does not carry it.
 *   A Rule 46A invoice-cum-bill of supply does carry it: the proviso inserted by Notification No.
 *   26/2022-CT requires that document to contain "the particulars as specified under rule 46 or
 *   rule 54, as the case may be, and rule 49". That proviso speaks to particulars, not to the
 *   number of copies, so it leaves the Rule 48(1) copy question above exactly as unsettled as it
 *   was.
 * * **Signature.** Rule 46(q) and Rule 49(h) require a signature; their exception covers an
 *   electronic invoice under the IT Act, which a printed page is not. So a GST document carries a
 *   blank space for a human to sign. Nothing is ever signed by the software.
 * * **Legacy.** A Sale without the statutory snapshots is a record of a transaction, not a
 *   reproduction of a document. It carries no copy label, no declaration and no signature space,
 *   and nothing is filled in from today's masters.
 * * **Fail closed.** Where a statutory fact the document needs is missing, or the class is
 *   unresolved, the model refuses to produce a printable document instead of printing a document
 *   that misrepresents compliance.
 *
 * No QR is generated here or anywhere else. Under Notification No. 14/2020-CT the payment
 * cross-reference frozen at posting is what the document carries, and it is printed as the plain
 * fact it is, with no claim about compliance on the page.
 */

/** CGST Rule 46(s), as published by CBIC. Printed verbatim or not at all. */
export const RULE_46S_DECLARATION =
  "I/We hereby declare that though our aggregate turnover in any preceding financial year from " +
  "2017-18 onwards is more than the aggregate turnover notified under sub-rule (4) of rule 48, we " +
  "are not required to prepare an invoice in terms of the provisions of the said sub-rule.";

export type PrintLayout = "a4" | "thermal80";

export interface DocumentCopy {
  id: string;
  /** Printed on the page exactly as written here. */
  label: string;
  /**
   * True only for the CGST Rule 48(1) triplicate on a tax invoice. False marks a label this
   * product chose, which the page therefore states as a copy name and never as a statutory claim.
   */
  statutory: boolean;
  /** Shown on screen beside the choice; never printed. */
  note?: string;
}

export interface DocumentRefusal {
  code: string;
  title: string;
  detail: string;
}

/**
 * One band of the document's value, named by the treatment that earned it.
 *
 * A Rule 46A invoice-cum-bill of supply carries taxable and exempted supplies on one page, and
 * their values are different things: only the taxable part has a "taxable value". The bands come
 * from the posted tax summary's own grouping — nothing here re-derives a treatment.
 */
export interface DocumentValueBand {
  treatment: string | null;
  label: string;
  amountPaise: number;
}

const VALUE_LABELS: Record<string, string> = {
  taxable: "Taxable value",
  exempt: "Exempt value",
  nil_rated: "Nil-rated value",
  non_gst: "Non-GST value"
};

/** What a line that charged no tax is, said in a word rather than shown as a zero. */
export function treatmentLabel(kind: string | null): string | null {
  switch (kind) {
    case "exempt":
      return "Exempt";
    case "nil_rated":
      return "Nil-rated";
    case "non_gst":
      return "Non-GST";
    default:
      return null;
  }
}

function valueBands(invoice: Invoice, showTax: boolean): DocumentValueBand[] {
  if (!showTax || invoice.taxSummary.length === 0) {
    return [{ treatment: null, label: showTax ? "Taxable value" : "Value", amountPaise: invoice.totals.taxableValuePaise }];
  }
  const bands: DocumentValueBand[] = [];
  for (const group of invoice.taxSummary) {
    const kind = group.taxTreatmentKind ?? null;
    const label = (kind && VALUE_LABELS[kind]) ?? "Value";
    const existing = bands.find((band) => band.label === label);
    if (existing) {
      existing.amountPaise += group.taxableValuePaise;
    } else {
      bands.push({ treatment: kind, label, amountPaise: group.taxableValuePaise });
    }
  }
  // The taxed part reads first, whatever order the summary arrived in. Presentation only: no band
  // is added, dropped or re-added up.
  return [...bands.filter((band) => band.treatment === "taxable"), ...bands.filter((band) => band.treatment !== "taxable")];
}

export interface SaleDocumentModel {
  /** When false, `refusal` says why, and no document is rendered. */
  printable: boolean;
  refusal: DocumentRefusal | null;
  title: string;
  documentNumber: string;
  businessDate: string;
  financialYear: string;
  postedAtUtc: string;
  /** A record of a posted transaction rather than a reproduction of a statutory document. */
  legacy: boolean;
  legacyNotice: string | null;
  /** Facts the Sale never recorded, said plainly instead of filled in from today's masters. */
  unrecorded: string[];
  seller: Invoice["sellerSnapshot"];
  recipient: Invoice["recipient"];
  lines: Invoice["lines"];
  taxSummary: Invoice["taxSummary"];
  totals: Invoice["totals"];
  payments: Invoice["tender"];
  /** Tax columns and the tax summary belong to a document that charged tax. */
  showTax: boolean;
  /** The document's value, split by treatment where it carries more than one. */
  valueBands: DocumentValueBand[];
  /** CGST Rule 46(p), as frozen: this product has no reverse-charge workflow. */
  reverseCharge: boolean | null;
  declaration: string | null;
  /** A drug memo's dealer sale licence, frozen at posting. Never inferred from a licence type. */
  retailMemoLicenceText: string | null;
  signatureRequired: boolean;
  copies: DocumentCopy[];
  /**
   * True when the posted snapshot says Notification No. 14/2020-CT applies, so the payment
   * cross-reference is the compliance path this document relies on. It changes no printed claim:
   * the payment facts print either way.
   */
  paymentCrossReferenceRequired: boolean;
}

const TITLES: Record<string, string> = {
  tax_invoice: "TAX INVOICE",
  bill_of_supply: "BILL OF SUPPLY",
  invoice_cum_bill_of_supply: "INVOICE-CUM-BILL OF SUPPLY",
  retail_cash_memo: "RETAIL CASH MEMO"
};

/** CGST Rule 48(1), for an invoice for goods. */
const RULE_48_COPIES: DocumentCopy[] = [
  { id: "original", label: "ORIGINAL FOR RECIPIENT", statutory: true },
  { id: "duplicate", label: "DUPLICATE FOR TRANSPORTER", statutory: true },
  { id: "triplicate", label: "TRIPLICATE FOR SUPPLIER", statutory: true }
];

/**
 * The same three labels for a Rule 46A invoice-cum-bill of supply, offered because no primary
 * source settles whether Rule 48(1) reaches that document. Conservative, and marked as this
 * product's decision rather than a statutory rule.
 */
const ICBS_COPIES: DocumentCopy[] = RULE_48_COPIES.map((copy) => ({
  ...copy,
  statutory: false,
  note: "Applied conservatively: no primary source settles whether the Rule 48(1) copy set governs an invoice-cum-bill of supply."
}));

/** Rule 49 prescribes no copies for a bill of supply, so these are plainly this product's names. */
const PRODUCT_COPIES: DocumentCopy[] = [
  { id: "customer", label: "CUSTOMER COPY", statutory: false, note: "A copy name chosen by this product. No copy rule is prescribed for this document." },
  { id: "store", label: "STORE COPY", statutory: false, note: "A copy name chosen by this product. No copy rule is prescribed for this document." }
];

/**
 * A drug memo's second print exists so the licensee can keep one: Drugs Rules r.65, condition
 * (3)(ii), requires carbon copies of cash or credit memos to be maintained by the licensee. The
 * wording below is this product's; the retention requirement is the rule's, and a database row is
 * not claimed to satisfy it.
 */
const MEMO_COPIES: DocumentCopy[] = [
  { id: "customer", label: "CUSTOMER COPY", statutory: false, note: "A copy name chosen by this product." },
  {
    id: "licensee",
    label: "LICENSEE'S RECORD COPY",
    statutory: false,
    note: "A copy name chosen by this product, for the copy the licensee keeps. Drugs Rules r.65 requires the licensee to maintain copies of cash or credit memos; a database record is not claimed to satisfy that."
  }
];

function refuse(code: string, title: string, detail: string): SaleDocumentModel {
  return {
    printable: false,
    refusal: { code, title, detail },
    title: "",
    documentNumber: "",
    businessDate: "",
    financialYear: "",
    postedAtUtc: "",
    legacy: false,
    legacyNotice: null,
    unrecorded: [],
    seller: null,
    recipient: {} as Invoice["recipient"],
    lines: [],
    taxSummary: [],
    totals: { taxableValuePaise: 0, cgstPaise: 0, sgstPaise: 0, igstPaise: 0, cessPaise: 0, grandTotalPaise: 0 },
    payments: [],
    showTax: false,
    valueBands: [],
    reverseCharge: null,
    declaration: null,
    retailMemoLicenceText: null,
    signatureRequired: false,
    copies: [],
    paymentCrossReferenceRequired: false
  };
}

function electronic(method: string): boolean {
  return method === "card" || method === "upi";
}

/**
 * Builds the semantic document, or refuses.
 *
 * Every value comes from `invoice`. There is deliberately no second argument: a page cannot pass in
 * a Store profile, a Party, a product or a date to fill a gap with.
 */
export function buildSaleDocument(invoice: Invoice): SaleDocumentModel {
  const { document, regulatory, sellerSnapshot, recipient } = invoice;

  if (document.documentType === "document_classification_unresolved") {
    return refuse(
      "document_classification_unresolved",
      "This sale has no statutory document",
      document.documentTypeReason
        ?? "The Store Service could not settle which statutory document this sale is, so none can be printed."
    );
  }

  // A Sale is a legacy record unless every statutory snapshot it would need was taken at posting.
  const legacy =
    regulatory.legacyDocument
    || sellerSnapshot === null
    || regulatory.sellerSnapshotVersion < 1
    || recipient.snapshotVersion < 1
    || regulatory.complianceSnapshotVersion < 1;

  const unrecorded: string[] = [];
  if (legacy) {
    if (sellerSnapshot === null || regulatory.sellerSnapshotVersion < 1) {
      unrecorded.push("The pharmacy's own legal name, address, GSTIN and licence were not recorded on this sale.");
    }
    if (recipient.snapshotVersion < 1) {
      unrecorded.push("The customer's statutory particulars were not recorded on this sale.");
    }
    if (regulatory.complianceSnapshotVersion < 1) {
      unrecorded.push("The Rule 46(s), HSN and licence facts were not recorded on this sale.");
    }
  }

  const base = {
    printable: true,
    refusal: null,
    title: TITLES[document.documentType] ?? "",
    documentNumber: document.documentNumber,
    businessDate: document.businessDate,
    financialYear: document.financialYear,
    postedAtUtc: document.postedAtUtc,
    seller: sellerSnapshot,
    recipient,
    lines: invoice.lines,
    taxSummary: invoice.taxSummary,
    totals: invoice.totals,
    payments: invoice.tender,
    retailMemoLicenceText: sellerSnapshot?.retailMemoLicenceText ?? null
  } satisfies Partial<SaleDocumentModel>;

  if (legacy) {
    // A version-0 Sale states the tax it actually froze, or none at all. Nothing is re-derived.
    const legacyShowsTax =
      invoice.totals.cgstPaise + invoice.totals.sgstPaise + invoice.totals.igstPaise + invoice.totals.cessPaise > 0;
    // No statutory claim of any kind: no copy label, no declaration, no signature space.
    return {
      ...base,
      title: "LEGACY TRANSACTION RECORD",
      legacy: true,
      legacyNotice: "LEGACY TRANSACTION RECORD — NOT ORIGINAL INVOICE REPRODUCTION",
      unrecorded,
      showTax: legacyShowsTax,
      valueBands: valueBands(invoice, legacyShowsTax),
      reverseCharge: null,
      declaration: null,
      signatureRequired: false,
      copies: [],
      paymentCrossReferenceRequired: false
    };
  }

  const gstDocument = document.documentType !== "retail_cash_memo";
  // Clause (s) speaks to an invoice. A bill of supply is issued instead of a tax invoice, and Rule
  // 49 imports only the provisos to Rule 46, so the declaration is neither required nor withheld
  // there — it simply is not that document's particular.
  const invoiceDocument =
    document.documentType === "tax_invoice" || document.documentType === "invoice_cum_bill_of_supply";

  // A registered seller's invoice says whether the Rule 46(s) declaration belongs on it. If that
  // answer is missing on a document that needs one, the page refuses rather than print an invoice
  // that may be missing a statutory declaration.
  if (invoiceDocument && regulatory.rule46sDeclaration === null) {
    return refuse(
      "rule46s_declaration_unresolved",
      "This invoice cannot be printed",
      "Whether the Rule 46(s) declaration belongs on this invoice was not settled when it was posted, so printing it could produce a non-compliant invoice."
    );
  }

  // Where Notification No. 14/2020-CT applies, the document relies on the payment cross-reference.
  // An electronic payment with no reference cannot carry one, so the page refuses.
  const paymentCrossReferenceRequired = regulatory.dynamicQrApplicability === "required";
  if (paymentCrossReferenceRequired) {
    const incomplete = invoice.tender.some(
      (tender) => electronic(tender.method) && (tender.referenceText ?? "").trim() === ""
    );
    if (incomplete) {
      return refuse(
        "payment_reference_missing",
        "This invoice cannot be printed",
        "This pharmacy records the payment details on invoices to unregistered customers, and this sale's card or UPI payment has no transaction reference."
      );
    }
  }

  const showTax =
    document.documentType === "tax_invoice" || document.documentType === "invoice_cum_bill_of_supply";
  const copies =
    document.documentType === "tax_invoice"
      ? RULE_48_COPIES
      : document.documentType === "invoice_cum_bill_of_supply"
        ? ICBS_COPIES
        : document.documentType === "retail_cash_memo"
          ? MEMO_COPIES
          : PRODUCT_COPIES;

  return {
    ...base,
    legacy: false,
    legacyNotice: null,
    unrecorded: [],
    showTax,
    valueBands: valueBands(invoice, showTax),
    reverseCharge: gstDocument ? regulatory.reverseCharge : null,
    declaration:
      invoiceDocument && regulatory.rule46sDeclaration === "applicable" ? RULE_46S_DECLARATION : null,
    signatureRequired: gstDocument,
    copies,
    paymentCrossReferenceRequired
  };
}

/** The copy a page starts on: the recipient's, where copies exist at all. */
export function defaultCopy(model: SaleDocumentModel): string | null {
  return model.copies[0]?.id ?? null;
}
