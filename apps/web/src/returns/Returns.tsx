import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";
import type {
  GstRoute,
  ReturnDetail,
  ReturnDisposition,
  ReturnKind,
  ReturnLine,
  ReturnableLine,
  TaxAdjustmentStatus
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { businessToday } from "../platform/businessDate";
import { LocalServiceError } from "../platform/localService";
import { newIdempotencyKey } from "../inventory/inventoryApi";
import {
  addReturnLine,
  createReturn,
  getReturn,
  listReturnablePurchaseLines,
  listReturnableSaleLines,
  listReturns,
  paiseToAmountText,
  postReturn,
  quantityToInteger,
  quoteReturn,
  removeReturnLine,
  unitLabelFor,
  type ReturnKindFilter
} from "./returnApi";

const KIND_LABELS: Record<ReturnKind, string> = {
  sales_return: "Sales return",
  purchase_return: "Purchase return"
};

/**
 * What each disposition means in the words a pharmacist would use.
 *
 * `sellable` is absent on purpose and cannot be chosen here: no medicine a customer brought back
 * goes straight onto the shelf. Releasing it is a separate decision, made by a pharmacist, from the
 * Inventory screen.
 */
const DISPOSITION_LABELS: Record<ReturnDisposition, string> = {
  quarantined: "Hold in quarantine — a pharmacist decides later whether it can be sold",
  non_sellable: "Write off — it will never be sold"
};

const TAX_ADJUSTMENT_LABELS: Record<TaxAdjustmentStatus, string> = {
  commercial_only: "Refund only — do not reduce GST already charged",
  tax_adjustable: "Credit note that also reduces the GST charged"
};

const GST_ROUTE_LABELS: Record<GstRoute, string> = {
  supplier_credit_note: "Send under a delivery challan and await the supplier's credit note",
  fresh_supply: "Treat as a fresh supply and raise our own invoice"
};

const STALE_DOCUMENT_MESSAGE =
  "This return changed after you opened it. Reload the latest version before saving.";

// ---------------------------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------------------------

export function ReturnListPage() {
  usePageTitle("Returns");
  const [kind, setKind] = useState<ReturnKindFilter>("all");
  const returns = useQuery({
    queryKey: ["returns", "list", kind],
    queryFn: () => listReturns(kind),
    retry: false
  });
  useExpireOnAuthError(returns.error);

  return <>
    <header className="page-header">
      <div><p className="eyebrow">OPERATIONS</p><h1>Returns</h1><p>Goods coming back. A return never changes the original document — it is a separate compensating document that reverses part of it.</p></div>
    </header>

    <section className="master-panel" aria-labelledby="return-list-title">
      <h2 id="return-list-title">Return documents</h2>
      <div className="master-toolbar">
        <div className="filter-field">
          <label htmlFor="return-kind">Kind</label>
          <select id="return-kind" value={kind} onChange={(event) => setKind(event.target.value as ReturnKindFilter)}>
            <option value="all">All</option>
            <option value="sales_return">From customers</option>
            <option value="purchase_return">To suppliers</option>
          </select>
        </div>
      </div>
      {returns.isPending ? <Loading label="Loading returns…" />
        : returns.isError ? <QueryError label="Returns could not be loaded" onRetry={() => void returns.refetch()} />
        : returns.data.length === 0 ? <div className="empty-state"><h3>No returns yet</h3><p>Start one from a posted sale or a posted purchase.</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Document</th><th scope="col">Kind</th><th scope="col">Against</th><th scope="col">Date</th><th scope="col">Status</th><th scope="col" className="numeric">Value</th></tr></thead>
            <tbody>{returns.data.map((document) => <tr key={document.id}>
              <td data-label="Document"><Link to={`/app/returns/${document.id}`}>{document.documentNumber ?? "Draft"}</Link></td>
              <td data-label="Kind">{KIND_LABELS[document.returnKind]}</td>
              <td data-label="Against">{document.originalDocumentNumber ?? "—"}</td>
              <td data-label="Date">{document.businessDate}</td>
              <td data-label="Status"><span className={`status-badge status-badge--${document.status === "posted" ? "active" : "archived"}`}>{document.status === "posted" ? "Posted" : "Draft"}</span></td>
              <td data-label="Value" className="numeric">{document.status === "posted" ? paiseToAmountText(document.grandTotalPaise) : <span className="row-subtext">Not posted</span>}</td>
            </tr>)}</tbody>
          </table></div>}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// Raising a return from a posted document
// ---------------------------------------------------------------------------------------------

/**
 * There is deliberately no blank return form.
 *
 * A return is always raised *from* the document it corrects, so the operator picks quantities out of
 * real lines rather than re-entering a product, a batch and a price that must match an original
 * exactly. It also means the screen can show what is genuinely left to return.
 */
export function ReturnCreatePage({ kind }: { kind: ReturnKind }) {
  const { id: originalId } = useParams();
  usePageTitle(KIND_LABELS[kind]);
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const isSale = kind === "sales_return";
  const [notice, setNotice] = useState<string | null>(null);
  const [quantities, setQuantities] = useState<Record<string, string>>({});
  const [dispositions, setDispositions] = useState<Record<string, ReturnDisposition>>({});

  const original = useQuery({
    queryKey: ["returns", "returnable", kind, originalId],
    queryFn: () => (isSale ? listReturnableSaleLines(originalId!) : listReturnablePurchaseLines(originalId!)),
    enabled: Boolean(originalId),
    retry: false
  });
  useExpireOnAuthError(original.error);

  const start = useMutation({
    mutationFn: async () => {
      const chosen = (original.data?.lines ?? []).filter((line) => (quantities[line.originalLineId] ?? "").trim() !== "");
      const draft = await createReturn({
        returnKind: kind,
        originalDocumentId: originalId!,
        businessDate: businessToday()
      });
      let revision = draft.revision;
      let detail = draft;
      for (const line of chosen) {
        detail = await addReturnLine(draft.id, {
          expectedRevision: revision,
          originalLineId: line.originalLineId,
          quantity: quantityToInteger(quantities[line.originalLineId] ?? "")!,
          disposition: isSale ? (dispositions[line.originalLineId] ?? "quarantined") : null
        });
        revision = detail.revision;
      }
      return detail;
    },
    onSuccess: (detail) => {
      void queryClient.invalidateQueries({ queryKey: ["returns", "list"] });
      void navigate(`/app/returns/${detail.id}`);
    },
    onError: (caught) => setNotice(messageFor(caught, "The return could not be started."))
  });

  const submit = () => {
    setNotice(null);
    const lines = original.data?.lines ?? [];
    const chosen = lines.filter((line) => (quantities[line.originalLineId] ?? "").trim() !== "");
    if (chosen.length === 0) {
      setNotice("Enter how much of at least one line is coming back.");
      return;
    }
    for (const line of chosen) {
      const typed = quantities[line.originalLineId] ?? "";
      const quantity = quantityToInteger(typed);
      const unit = unitLabelFor(line);
      if (quantity === null) {
        setNotice(`Enter a whole number of ${unit} for ${line.productDisplayName ?? "this line"}.`);
        document.getElementById(`return-quantity-${line.originalLineId}`)?.focus();
        return;
      }
      if (quantity > line.returnableQuantity) {
        setNotice(`Only ${line.returnableQuantity} ${unit} of ${line.productDisplayName ?? "this line"} can still be returned.`);
        document.getElementById(`return-quantity-${line.originalLineId}`)?.focus();
        return;
      }
    }
    start.mutate();
  };

  if (original.isPending) return <section className="master-panel"><Loading label="Loading the original document…" /></section>;
  if (original.isError) return <section className="master-panel"><QueryError label="This document could not be loaded" onRetry={() => void original.refetch()} /></section>;

  const lines = original.data.lines;
  const nothingLeft = lines.every((line) => line.returnableAtoms === 0);

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{KIND_LABELS[kind]}</h1>
        <p>Against {original.data.documentNumber ?? "this document"} of {original.data.documentDate}{original.data.counterpartyDisplayName ? ` · ${original.data.counterpartyDisplayName}` : ""}. The original is never changed.</p>
      </div>
      <Link className="button button--secondary" to={isSale ? `/app/sales/${originalId}` : `/app/purchases/${originalId}`}>Back to the document</Link>
    </header>

    <section className="master-panel" aria-labelledby="returnable-title">
      <h2 id="returnable-title">What is coming back</h2>
      {nothingLeft
        ? <div className="empty-state"><h3>Nothing is left to return</h3><p>Every line of this document has already been returned in full.</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Item</th><th scope="col">Batch</th><th scope="col" className="numeric">Sold</th><th scope="col" className="numeric">Already back</th><th scope="col" className="numeric">Can return</th><th scope="col">Returning</th>{isSale && <th scope="col">Where it goes</th>}</tr></thead>
            <tbody>{lines.map((line) => <ReturnableRow
              key={line.originalLineId}
              line={line}
              isSale={isSale}
              quantity={quantities[line.originalLineId] ?? ""}
              disposition={dispositions[line.originalLineId] ?? "quarantined"}
              onQuantity={(value) => setQuantities({ ...quantities, [line.originalLineId]: value })}
              onDisposition={(value) => setDispositions({ ...dispositions, [line.originalLineId]: value })}
            />)}</tbody>
          </table></div>}

      {notice && <div className="inline-notice inline-notice--error" role="alert">{notice}</div>}

      {!nothingLeft && <div className="form-actions">
        <Link className="button button--secondary" to={isSale ? `/app/sales/${originalId}` : `/app/purchases/${originalId}`}>Cancel</Link>
        <button className="button button--primary" type="button" onClick={submit} disabled={start.isPending}>{start.isPending ? "Starting…" : "Start return"}</button>
      </div>}
    </section>
  </>;
}

function ReturnableRow({ line, isSale, quantity, disposition, onQuantity, onDisposition }: {
  line: ReturnableLine;
  isSale: boolean;
  quantity: string;
  disposition: ReturnDisposition;
  onQuantity: (value: string) => void;
  onDisposition: (value: ReturnDisposition) => void;
}) {
  const unit = unitLabelFor(line);
  const expired = Boolean(line.batchExpiresOn && line.batchExpiresOn < businessToday());
  return <tr>
    <td data-label="Item">{line.productDisplayName ?? line.productId}<br /><small className="row-subtext">{line.packDisplayLabel}</small></td>
    <td data-label="Batch">{line.batchNumber ?? "—"}{line.batchExpiresOn ? <><br /><small className={expired ? "pos-warning" : "row-subtext"}>{expired ? `Expired ${line.batchExpiresOn}` : `Expires ${line.batchExpiresOn}`}</small></> : null}</td>
    <td data-label="Sold" className="numeric">{line.originalQuantity} {unit}</td>
    <td data-label="Already back" className="numeric">{line.alreadyReturnedAtoms === 0 ? "—" : `${line.originalQuantity - line.returnableQuantity} ${unit}`}</td>
    <td data-label="Can return" className="numeric">{line.returnableQuantity} {unit}</td>
    <td data-label="Returning">
      {line.returnableAtoms === 0
        ? <span className="row-subtext">Fully returned</span>
        : <input id={`return-quantity-${line.originalLineId}`} className="numeric" inputMode="numeric" value={quantity} onChange={(event) => onQuantity(event.target.value)} aria-label={`Quantity of ${line.productDisplayName ?? "this item"} being returned, in ${unit}`} />}
    </td>
    {isSale && <td data-label="Where it goes">
      {line.returnableAtoms === 0 ? <span className="row-subtext">—</span>
        : expired
          ? <span className="row-subtext">Written off — this batch has expired</span>
          : <select value={disposition} onChange={(event) => onDisposition(event.target.value as ReturnDisposition)} aria-label={`Where the returned ${line.productDisplayName ?? "goods"} are put`}>
              {(Object.keys(DISPOSITION_LABELS) as ReturnDisposition[]).map((value) => <option key={value} value={value}>{DISPOSITION_LABELS[value]}</option>)}
            </select>}
    </td>}
  </tr>;
}

// ---------------------------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------------------------

export function ReturnDetailPage() {
  const { id } = useParams();
  const document = useQuery({
    queryKey: ["returns", "detail", id],
    queryFn: () => getReturn(id!),
    enabled: Boolean(id),
    retry: false
  });
  useExpireOnAuthError(document.error);
  usePageTitle(document.data?.documentNumber ?? "Return");

  if (document.isPending) return <section className="master-panel"><Loading label="Loading return…" /></section>;
  if (document.isError) return <section className="master-panel"><QueryError label="This return could not be loaded" onRetry={() => void document.refetch()} /></section>;
  return document.data.status === "draft"
    ? <ReturnDraft document={document.data} />
    : <PostedReturn document={document.data} />;
}

function ReturnDraft({ document }: { document: ReturnDetail }) {
  const auth = useAuth();
  const queryClient = useQueryClient();
  const isSale = document.returnKind === "sales_return";
  const [notice, setNotice] = useState<string | null>(null);
  const [taxStatus, setTaxStatus] = useState<TaxAdjustmentStatus>("commercial_only");
  const [taxReason, setTaxReason] = useState("");
  const [route, setRoute] = useState<GstRoute>("supplier_credit_note");
  // A ref, not the mutation's pending flag: `disabled` only applies on the next render, and three
  // clicks in one tick all reach the handler before React has painted once.
  const inFlight = useRef(false);

  const role = auth.status?.user?.role;
  const mayPost = isSale
    ? role === "owner_admin" || role === "pharmacist"
    : role === "owner_admin";

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["returns", "detail", document.id] });
    void queryClient.invalidateQueries({ queryKey: ["returns", "quote", document.id] });
    void queryClient.invalidateQueries({ queryKey: ["returns", "list"] });
  };

  const quote = useQuery({
    queryKey: ["returns", "quote", document.id, document.revision],
    queryFn: () => quoteReturn(document.id),
    retry: false
  });

  const remove = useMutation({
    mutationFn: (lineId: string) => removeReturnLine(lineId, document.revision),
    onSuccess: () => { refresh(); setNotice(null); },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "The line could not be removed."))
  });

  const post = useMutation({
    mutationFn: () => postReturn(document.id, {
      expectedRevision: document.revision,
      idempotencyKey: newIdempotencyKey(),
      taxAdjustmentStatus: isSale ? taxStatus : null,
      taxAdjustmentReason: isSale ? (taxReason.trim() || null) : null,
      gstRoute: isSale ? null : route
    }),
    onSuccess: () => { refresh(); setNotice(null); },
    onSettled: () => { inFlight.current = false; },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "This return could not be posted."))
  });

  const submit = () => {
    setNotice(null);
    if (inFlight.current) return;
    if (document.lines.length === 0) {
      setNotice("Add at least one line before posting.");
      return;
    }
    inFlight.current = true;
    post.mutate();
  };

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{KIND_LABELS[document.returnKind]}</h1>
        <p>Against {document.originalDocumentNumber ?? "the original"} of {document.originalDocumentDate ?? "—"}{document.counterpartyDisplayName ? ` · ${document.counterpartyDisplayName}` : ""}. Nothing is reversed and no stock moves until this is posted.</p>
      </div>
      <Link className="button button--secondary" to="/app/returns">Back to Returns</Link>
    </header>

    <section className="master-panel" aria-labelledby="return-draft-title">
      <h2 id="return-draft-title">Lines coming back</h2>
      {document.lines.length === 0
        ? <div className="empty-state"><h3>Nothing on this return</h3><p>Start again from the original document to choose what is coming back.</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Batch</th><th scope="col" className="numeric">Quantity</th>{isSale && <th scope="col">Where it goes</th>}<th scope="col" className="numeric">Value</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead>
            <tbody>{document.lines.map((line) => <tr key={line.id}>
              <td data-label="#">{line.lineNumber}</td>
              <td data-label="Item">{line.productDisplayName ?? line.productId}</td>
              <td data-label="Batch">{line.batchNumber ?? "—"}</td>
              <td data-label="Quantity" className="numeric">{quantityText(line)}</td>
              {isSale && <td data-label="Where it goes">{line.disposition ? DISPOSITION_LABELS[line.disposition].split(" — ")[0] : "—"}</td>}
              <td data-label="Value" className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
              <td data-label="Actions"><button className="button button--secondary" type="button" onClick={() => remove.mutate(line.id)} disabled={remove.isPending}>Remove</button></td>
            </tr>)}</tbody>
          </table></div>}

      {quote.data && <dl className="totals-grid">
        <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(quote.data.taxableValuePaise)}</dd></div>
        <div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(quote.data.cgstPaise)}</dd></div>
        <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(quote.data.sgstPaise)}</dd></div>
        <div className="totals-grand"><dt>{isSale ? "To refund" : "Value returned"}</dt><dd className="numeric">{paiseToAmountText(quote.data.grandTotalPaise)}</dd></div>
      </dl>}

      {isSale
        ? <div className="master-form">
            <div className="field">
              <label htmlFor="return-tax-status">GST treatment</label>
              <select id="return-tax-status" value={taxStatus} onChange={(event) => setTaxStatus(event.target.value as TaxAdjustmentStatus)}>
                {(Object.keys(TAX_ADJUSTMENT_LABELS) as TaxAdjustmentStatus[]).map((value) => <option key={value} value={value}>{TAX_ADJUSTMENT_LABELS[value]}</option>)}
              </select>
              <small>AUSHADHARTH records what you decide here; it does not decide it for you. Whether the GST charged can be reduced depends on facts this software does not hold.</small>
            </div>
            <div className="field">
              <label htmlFor="return-tax-reason">Why (optional)</label>
              <input id="return-tax-reason" value={taxReason} onChange={(event) => setTaxReason(event.target.value)} />
            </div>
          </div>
        : <div className="master-form">
            <div className="field">
              <label htmlFor="return-gst-route">How the goods are going back</label>
              <select id="return-gst-route" value={route} onChange={(event) => setRoute(event.target.value as GstRoute)}>
                {(Object.keys(GST_ROUTE_LABELS) as GstRoute[]).map((value) => <option key={value} value={value}>{GST_ROUTE_LABELS[value]}</option>)}
              </select>
              <small>This is a purchase return, not a debit note — under GST a debit note is issued by the supplier, never by us.</small>
            </div>
          </div>}

      {notice && <div className="inline-notice inline-notice--error" role="alert">{notice}</div>}

      <div className="form-actions">
        {mayPost
          ? <button className="button button--primary" type="button" onClick={submit} disabled={post.isPending || document.lines.length === 0}>{post.isPending ? "Posting…" : "Post return"}</button>
          : <span className="read-only-note">{isSale ? "A pharmacist or the owner posts a sales return." : "The owner posts a purchase return."}</span>}
      </div>
      <p className="pos-summary__note">Posting issues the return's own document number, reverses the original's tax exactly as it was charged, and moves the stock. It cannot be undone.</p>
    </section>
  </>;
}

function PostedReturn({ document }: { document: ReturnDetail }) {
  const isSale = document.returnKind === "sales_return";
  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{document.documentNumber}</h1>
        <p>{KIND_LABELS[document.returnKind]} against {document.originalDocumentNumber} · {document.counterpartyDisplayName ?? "Walk-in"}</p>
      </div>
      <Link className="button button--secondary" to="/app/returns">Back to Returns</Link>
    </header>

    <section className="master-panel" aria-labelledby="posted-return-title">
      <h2 id="posted-return-title">Return</h2>
      <dl className="detail-grid">
        <div><dt>Document number</dt><dd>{document.documentNumber}</dd></div>
        <div><dt>Against</dt><dd>{document.originalDocumentNumber} of {document.originalDocumentDate}</dd></div>
        <div><dt>Date</dt><dd>{document.businessDate}</dd></div>
        <div><dt>Store GSTIN</dt><dd>{document.storeNormalizedGstin ?? "Not registered"}</dd></div>
        {isSale
          ? <div><dt>GST treatment</dt><dd>{document.taxAdjustmentStatus ? TAX_ADJUSTMENT_LABELS[document.taxAdjustmentStatus] : "—"}</dd></div>
          : <div><dt>Returned as</dt><dd>{document.gstRoute ? GST_ROUTE_LABELS[document.gstRoute] : "—"}</dd></div>}
        {document.taxAdjustmentReason && <div><dt>Reason recorded</dt><dd>{document.taxAdjustmentReason}</dd></div>}
      </dl>

      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Batch</th><th scope="col">HSN</th><th scope="col" className="numeric">Quantity</th>{isSale && <th scope="col">Where it went</th>}<th scope="col" className="numeric">Taxable</th><th scope="col" className="numeric">GST</th><th scope="col" className="numeric">Total</th></tr></thead>
        <tbody>{document.lines.map((line) => <tr key={line.id}>
          <td data-label="#">{line.lineNumber}</td>
          <td data-label="Item">{line.productDisplayName}<br /><small className="row-subtext">{line.packDisplayLabel}</small></td>
          <td data-label="Batch">{line.batchNumber}</td>
          <td data-label="HSN">{line.hsnCode ?? "—"}</td>
          <td data-label="Quantity" className="numeric">{quantityText(line)}</td>
          {isSale && <td data-label="Where it went">{line.disposition ? DISPOSITION_LABELS[line.disposition].split(" — ")[0] : "—"}</td>}
          <td data-label="Taxable" className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
          <td data-label="GST" className="numeric">{paiseToAmountText(line.cgstPaise + line.sgstPaise + line.igstPaise + line.cessPaise)}</td>
          <td data-label="Total" className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
        </tr>)}</tbody>
      </table></div>

      <dl className="totals-grid">
        <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(document.taxableValuePaise)}</dd></div>
        <div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(document.cgstPaise)}</dd></div>
        <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(document.sgstPaise)}</dd></div>
        <div className="totals-grand"><dt>{isSale ? "Refunded" : "Value returned"}</dt><dd className="numeric">{paiseToAmountText(document.grandTotalPaise)}</dd></div>
      </dl>

      {isSale && document.lines.some((line) => line.disposition === "quarantined") && <p className="pos-summary__note">
        Goods on this return are in quarantine. They are not available to sell until a pharmacist releases them from the Inventory screen.
      </p>}

      {document.supplierCreditNotes.length > 0 && <p className="pos-summary__note">
        Supplier credit note {document.supplierCreditNotes[0].creditNoteNumber} of {document.supplierCreditNotes[0].creditNoteDate} · {paiseToAmountText(document.supplierCreditNotes[0].creditNoteAmountPaise)}
      </p>}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/** A quantity always says what it is counted in: three strips and three tablets differ. */
function quantityText(line: ReturnLine): string {
  const unit = line.quantityBasis === "pack"
    ? (line.packDisplayLabel ?? "pack")
    : (line.baseUnitLabel ?? "unit");
  const count = line.quantityBasis === "pack" ? (line.quantityPacks ?? line.quantityAtoms) : line.quantityAtoms;
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

function messageFor(caught: unknown, fallback: string): string {
  return caught instanceof LocalServiceError ? caught.message : fallback;
}

function isStale(error: unknown): boolean {
  return error instanceof LocalServiceError && error.code === "revision_conflict";
}

function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }): ReactNode { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = window.document.title; window.document.title = `${title} | AUSHADHARTH`; return () => { window.document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
