import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router";
import type { Invoice } from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { getInvoice } from "../sales/invoiceApi";
import { paiseToAmountText } from "../sales/saleApi";
import {
  buildSaleDocument,
  defaultCopy,
  treatmentLabel,
  type PrintLayout,
  type SaleDocumentModel
} from "./saleDocument";
import "./print.css";

/**
 * Phase 1L-B — printing a posted Sale.
 *
 * The page fetches the canonical invoice, hands it to `buildSaleDocument`, and renders what comes
 * back. It decides nothing itself: no tax is computed here, no document type is guessed, no gap is
 * filled from a Store profile or a Party record. The operator chooses a layout and, where the
 * document has copies, which copy — and then the browser's own print dialog does the printing.
 *
 * Printing is a read. Opening this page posts nothing, numbers nothing and changes no snapshot.
 */
export function SalePrintPage() {
  const { id = "" } = useParams();
  const auth = useAuth();
  const [layout, setLayout] = useState<PrintLayout>("a4");
  const [copyId, setCopyId] = useState<string | null>(null);
  const [reprintedAt, setReprintedAt] = useState<string | null>(null);

  const invoice = useQuery({
    queryKey: ["sales", id, "invoice"],
    queryFn: () => getInvoice(id),
    retry: false,
    // A document is read once for this page; nothing here should silently re-read and re-render a
    // statutory page while somebody is looking at it.
    staleTime: Infinity,
    refetchOnWindowFocus: false
  });

  useEffect(() => {
    if (invoice.error instanceof LocalServiceError
      && ["authentication_required", "session_expired"].includes(invoice.error.code)) {
      void auth.expireSession();
    }
  }, [invoice.error, auth]);

  const model = invoice.data ? buildSaleDocument(invoice.data) : null;

  useEffect(() => {
    if (model?.printable) setCopyId((current) => current ?? defaultCopy(model));
  }, [model]);

  useEffect(() => {
    const title = model?.printable ? `${model.title} ${model.documentNumber}` : "Print sale";
    const previous = document.title;
    document.title = `${title} | AUSHADHARTH`;
    return () => { document.title = previous; };
  }, [model]);

  // The only thing that prints. Never called on load, never on a timer: an operator asks for it.
  const print = () => {
    setReprintedAt(new Date().toISOString());
    window.print();
  };

  const copy = model?.copies.find((each) => each.id === copyId) ?? null;

  return <div className="print-page">
    {/* The paper the browser lays out for. A4 takes the sheet's usual margins; the roll is
        continuous, so its height is left to the content and its margins are narrow. */}
    <style>{layout === "a4"
      ? "@page { size: A4; margin: 12mm; }"
      : "@page { size: 72mm auto; margin: 2mm; }"}</style>
    <header className="page-header print-hide">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>Print document</h1>
        <p>The sale as it was posted. Choose the paper and the copy, then print.</p>
      </div>
      <div className="page-header__actions">
        <Link className="button button--secondary" to={`/app/sales/${id}`}>Back to Sale</Link>
      </div>
    </header>

    {invoice.isPending && <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading the document…</b></div>}

    {invoice.isError && <div className="empty-state print-hide" role="alert">
      <h3>{problem(invoice.error)}</h3>
      <p>Nothing has been printed. {invoice.error instanceof LocalServiceError && invoice.error.code === "invoice_not_posted"
        ? "A sale can only be printed once it has been posted."
        : "The Local Store Service did not return this document."}</p>
      <Link className="button button--secondary" to={`/app/sales/${id}`}>Back to Sale</Link>
    </div>}

    {model && !model.printable && model.refusal && <div className="empty-state print-hide" role="alert">
      <h3>{model.refusal.title}</h3>
      <p>{model.refusal.detail}</p>
      <Link className="button button--secondary" to={`/app/sales/${id}`}>Back to Sale</Link>
    </div>}

    {model?.printable && <>
      <section className="master-panel print-hide" aria-labelledby="print-controls-title">
        <div className="panel-heading">
          <h2 id="print-controls-title">{model.title}{model.legacy ? "" : ` · ${model.documentNumber}`}</h2>
          <button className="button button--primary" type="button" onClick={print}>Print</button>
        </div>
        <div className="master-form">
          <div className="field">
            <label htmlFor="print-layout">Paper</label>
            <select id="print-layout" value={layout} onChange={(event) => setLayout(event.target.value as PrintLayout)}>
              <option value="a4">A4 sheet</option>
              <option value="thermal80">80mm roll</option>
            </select>
            <small>The printer itself is chosen in your computer's print dialog.</small>
          </div>
          {model.copies.length > 0 && <div className="field">
            <label htmlFor="print-copy">Copy</label>
            <select id="print-copy" value={copyId ?? ""} onChange={(event) => setCopyId(event.target.value)}>
              {model.copies.map((each) => <option key={each.id} value={each.id}>{each.label}</option>)}
            </select>
            <small>{copy?.statutory
              ? "CGST Rule 48(1) requires an invoice for goods in triplicate. Print each copy you need."
              : copy?.note ?? "A copy name chosen by this product."}</small>
          </div>}
        </div>
        {model.legacy && <div className="panel-callout" role="status">
          <strong>This sale predates the statutory snapshots</strong>
          <small>It prints as a record of what was posted, not as a reproduction of an invoice. Nothing is filled in from today's Store Profile, customer records or licences.</small>
        </div>}
        {model.paymentCrossReferenceRequired && <p className="panel-note">This pharmacy records the payment details on invoices to unregistered customers, so the mode, amount, reference and time recorded at posting appear on the document.</p>}
        <p className="panel-note">Printing does not change this sale. The number, date, amounts and recorded facts are exactly as they were posted.</p>
      </section>

      <div className={`print-sheet print-sheet--${layout === "a4" ? "a4" : "thermal"}`} data-testid="print-sheet">
        <SaleDocumentView model={model} copyLabel={copy?.label ?? null} reprintedAt={reprintedAt} layout={layout} />
      </div>
    </>}
  </div>;
}

function problem(error: unknown): string {
  return error instanceof LocalServiceError ? error.message : "This document could not be loaded.";
}

/**
 * The document itself.
 *
 * One view for both papers. The layout differences live in CSS, so a fact can never appear on one
 * paper and quietly vanish from the other.
 */
function SaleDocumentView({ model, copyLabel, reprintedAt, layout }: {
  model: SaleDocumentModel;
  copyLabel: string | null;
  reprintedAt: string | null;
  layout: PrintLayout;
}) {
  const seller = model.seller;
  const recipient = model.recipient;
  const showBatch = layout === "a4";

  return <article className="doc" aria-label={`${model.title} ${model.documentNumber}`}>
    <header className="doc__head">
      <h1 className="doc__title">{model.title}</h1>
      {copyLabel && <p className="doc__copy">{copyLabel}</p>}
      {model.legacyNotice && <p className="doc__legacy" role="note">{model.legacyNotice}</p>}
    </header>

    <section className="doc__seller">
      {seller
        ? <>
            <p className="doc__seller-name">{seller.tradeName ?? seller.legalName}</p>
            {seller.tradeName && seller.legalName !== seller.tradeName && <p>{seller.legalName}</p>}
            <p>{[seller.addressLine1, seller.addressLine2, seller.city, seller.postalCode].filter(Boolean).join(", ")}</p>
            {seller.stateName && <p>{seller.stateName}{seller.stateCode ? ` (${seller.stateCode})` : ""}</p>}
            {(seller.phone || seller.email) && <p>{[seller.phone, seller.email].filter(Boolean).join(" · ")}</p>}
            <p>{seller.gstin ? `GSTIN: ${seller.gstin}` : "Not GST-registered"}</p>
            <p>{seller.licenceText}</p>
            {model.retailMemoLicenceText && <p className="doc__licence">Drug sale licence: {model.retailMemoLicenceText}</p>}
          </>
        : <p className="doc__unrecorded">The pharmacy's own particulars were not recorded on this sale.</p>}
    </section>

    <section className="doc__meta">
      <p><span>{model.legacy ? "Number" : "No."}</span> <strong>{model.documentNumber}</strong></p>
      <p><span>Date</span> <strong>{model.businessDate}</strong></p>
      {!model.legacy && <p><span>Year</span> <strong>{model.financialYear}</strong></p>}
      {model.reverseCharge !== null && <p><span>Reverse charge</span> <strong>{model.reverseCharge ? "Yes" : "No"}</strong></p>}
    </section>

    <section className="doc__recipient">
      <p className="doc__section-label">Customer</p>
      {recipient.snapshotVersion < 1
        ? <p className="doc__unrecorded">The customer's particulars were not recorded on this sale.</p>
        : <>
            <p>{recipient.name ?? (recipient.walkIn ? "Walk-in" : "—")}</p>
            {recipient.address && <p>{[recipient.address.line1, recipient.address.line2, recipient.address.city, recipient.address.postalCode].filter(Boolean).join(", ")}</p>}
            {recipient.address?.stateName && <p>{recipient.address.stateName}{recipient.address.stateCode ? ` (${recipient.address.stateCode})` : ""}</p>}
            {recipient.gstin && <p>GSTIN: {recipient.gstin}</p>}
            {recipient.delivery?.sameAsRecipient === false && recipient.delivery.address && <p>
              Delivered to: {[recipient.delivery.address.line1, recipient.delivery.address.line2, recipient.delivery.address.city, recipient.delivery.address.postalCode, recipient.delivery.address.stateName].filter(Boolean).join(", ")}
            </p>}
          </>}
    </section>

    <table className="doc__lines">
      <thead>
        <tr>
          <th scope="col">#</th>
          <th scope="col">Item</th>
          <th scope="col" className="doc__hsn">HSN</th>
          <th scope="col" className="numeric">Qty</th>
          <th scope="col" className="numeric">Rate</th>
          {model.showTax && <th scope="col" className="numeric doc__taxable">Taxable</th>}
          {model.showTax && <th scope="col" className="numeric">GST</th>}
          <th scope="col" className="numeric">Amount</th>
        </tr>
      </thead>
      <tbody>
        {model.lines.map((line) => <tr key={line.lineNumber}>
          <td>{line.lineNumber}</td>
          <td>
            {line.description}
            {(line.packLabel || (showBatch && line.batchNumber)) && <small>
              {[line.packLabel, showBatch ? `Batch ${line.batchNumber}` : null, showBatch && line.expiresOn ? `Exp ${line.expiresOn}` : null].filter(Boolean).join(" · ")}
            </small>}
            {line.hsnCode && <small className="doc__inline-hsn">HSN {line.hsnCode}</small>}
          </td>
          <td className="doc__hsn">{line.hsnCode ?? ""}</td>
          <td className="numeric">{line.quantityText}</td>
          <td className="numeric">{paiseToAmountText(line.sellingRatePaise)}</td>
          {model.showTax && <td className="numeric doc__taxable">
            {treatmentLabel(line.taxTreatmentKind) ? "—" : paiseToAmountText(line.taxableValuePaise)}
          </td>}
          {model.showTax && <td className="numeric">
            {/* An exempted supply is not a supply taxed at zero, and a page that printed 0.00 for
                both would say it was. The treatment is named instead. */}
            {treatmentLabel(line.taxTreatmentKind)
              ?? paiseToAmountText(line.cgstPaise + line.sgstPaise + line.igstPaise + line.cessPaise)}
          </td>}
          <td className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
        </tr>)}
      </tbody>
    </table>

    <section className="doc__totals">
      {model.valueBands.map((band) => <p key={band.label}>
        <span>{band.label}</span> <strong>{paiseToAmountText(band.amountPaise)}</strong>
      </p>)}
      {model.showTax && model.totals.cgstPaise > 0 && <p><span>CGST</span> <strong>{paiseToAmountText(model.totals.cgstPaise)}</strong></p>}
      {model.showTax && model.totals.sgstPaise > 0 && <p><span>SGST</span> <strong>{paiseToAmountText(model.totals.sgstPaise)}</strong></p>}
      {model.showTax && model.totals.igstPaise > 0 && <p><span>IGST</span> <strong>{paiseToAmountText(model.totals.igstPaise)}</strong></p>}
      {model.showTax && model.totals.cessPaise > 0 && <p><span>Cess</span> <strong>{paiseToAmountText(model.totals.cessPaise)}</strong></p>}
      {!model.showTax && !model.legacy && <p><span>GST</span> <strong>Not charged</strong></p>}
      <p className="doc__grand"><span>Total</span> <strong>{paiseToAmountText(model.totals.grandTotalPaise)}</strong></p>
    </section>

    {model.payments.length > 0 && <section className="doc__payment">
      <p className="doc__section-label">Payment</p>
      {model.payments.map((payment, index) => <p key={`${payment.method}-${index}`}>
        {payment.method.toUpperCase()} · {paiseToAmountText(payment.amountPaise)}
        {payment.referenceText && <> · Ref {payment.referenceText}</>}
        <br />Recorded {payment.recordedAtUtc}
      </p>)}
    </section>}

    {model.declaration && <section className="doc__declaration">
      <p>{model.declaration}</p>
    </section>}

    {model.signatureRequired && <section className="doc__signature">
      <div className="doc__signature-space" />
      <p>Authorised Signatory</p>
    </section>}

    {model.unrecorded.length > 0 && <section className="doc__unrecorded-list">
      {model.unrecorded.map((fact) => <p key={fact}>{fact}</p>)}
    </section>}

    {reprintedAt && <p className="doc__reprint">Reprinted {reprintedAt}</p>}
  </article>;
}
