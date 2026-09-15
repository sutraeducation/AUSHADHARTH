import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";
import type {
  Batch,
  Party,
  Product,
  ProductPack,
  PurchaseDetail,
  PurchaseLine,
  PurchaseLineInput,
  PurchaseLineTaxKind,
  PurchaseTaxTreatment
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { businessToday } from "../platform/businessDate";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { listParties } from "../parties/partyApi";
import { newIdempotencyKey } from "../inventory/inventoryApi";
import {
  basisPointsToPercentText,
  getProduct,
  listBatches,
  listProducts,
  paiseToRupees
} from "../products/productApi";
import {
  addPurchaseLine,
  createPurchaseDraft,
  getPurchase,
  listPurchases,
  paiseToAmountText,
  postPurchase,
  ratePerPackToPaise,
  removePurchaseLine,
  updatePurchaseDraft,
  updatePurchaseLine,
  type PurchaseStatusFilter
} from "./purchaseApi";

const TREATMENT_LABELS: Record<PurchaseTaxTreatment, string> = {
  intra_state: "CGST + SGST (same State)",
  inter_state: "IGST (different States)"
};

const TAX_KIND_LABELS: Record<PurchaseLineTaxKind, string> = {
  taxable: "Taxable",
  exempt: "Exempt",
  nil_rated: "Nil rated",
  non_gst: "Outside GST"
};

const STALE_DOCUMENT_MESSAGE =
  "This purchase changed after you opened it. Reload the latest version before saving.";

/** The tax a draft will attract is not known yet, and the screen must never imply that it is. */
const DRAFT_TAX_NOTE =
  "GST is determined when this purchase is posted, from the tax rate in force on the invoice date and from the two States involved. A draft shows the taxable value only.";

export function PurchaseListPage() {
  usePageTitle("Purchases");
  const auth = useAuth();
  const [status, setStatus] = useState<PurchaseStatusFilter>("all");
  const [supplierId, setSupplierId] = useState("");
  const canMutate = auth.status?.user?.role === "owner_admin";

  const purchases = useQuery({
    queryKey: ["purchases", "list", status, supplierId],
    queryFn: () => listPurchases(status, supplierId || undefined),
    retry: false
  });
  const suppliers = useQuery({
    queryKey: ["parties", "suppliers", "active"],
    queryFn: () => listParties("", "active", "supplier"),
    staleTime: 30_000,
    retry: false
  });
  useExpireOnAuthError(purchases.error);

  return <>
    <header className="page-header">
      <div><p className="eyebrow">OPERATIONS</p><h1>Purchases</h1><p>Goods received from suppliers. A draft can be corrected freely; posting records the stock and the GST and cannot be undone.</p></div>
      {canMutate ? <Link className="button button--primary" to="/app/purchases/new">New Purchase</Link> : <span className="read-only-note">Read-only access</span>}
    </header>

    <section className="master-panel" aria-labelledby="purchase-list-title">
      <h2 id="purchase-list-title">Purchase documents</h2>
      <div className="master-toolbar">
        <div className="filter-field">
          <label htmlFor="purchase-supplier-filter">Supplier</label>
          <select id="purchase-supplier-filter" value={supplierId} onChange={(event) => setSupplierId(event.target.value)} disabled={!suppliers.data}>
            <option value="">{suppliers.data ? "Every supplier" : "Loading…"}</option>
            {(suppliers.data ?? []).map((supplier) => <option key={supplier.id} value={supplier.id}>{supplier.displayName}</option>)}
          </select>
        </div>
        <div className="filter-field">
          <label htmlFor="purchase-status">Status</label>
          <select id="purchase-status" value={status} onChange={(event) => setStatus(event.target.value as PurchaseStatusFilter)}>
            <option value="all">All</option>
            <option value="draft">Drafts</option>
            <option value="posted">Posted</option>
          </select>
        </div>
      </div>
      {purchases.isPending ? <Loading label="Loading purchases…" />
        : purchases.isError ? <QueryError label="Purchases could not be loaded" onRetry={() => void purchases.refetch()} />
        : purchases.data.length === 0 ? <div className="empty-state"><h3>No purchase documents yet</h3><p>{canMutate ? "Record a supplier invoice to bring stock in." : "Nothing has been recorded for this filter."}</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Invoice</th><th scope="col">Supplier</th><th scope="col">Invoice date</th><th scope="col">Status</th><th scope="col" className="numeric">Invoice total</th></tr></thead>
            <tbody>{purchases.data.map((purchase) => <tr key={purchase.id}>
              <td data-label="Invoice"><Link to={`/app/purchases/${purchase.id}`}>{purchase.supplierInvoiceNumber}</Link></td>
              <td data-label="Supplier">{purchase.supplierDisplayName ?? supplierName(suppliers.data, purchase.supplierPartyId)}</td>
              <td data-label="Invoice date">{purchase.invoiceDate}</td>
              <td data-label="Status"><span className={`status-badge status-badge--${purchase.status === "posted" ? "active" : "archived"}`}>{purchase.status === "posted" ? "Posted" : "Draft"}</span></td>
              <td data-label="Invoice total" className="numeric">{purchase.status === "posted" ? paiseToAmountText(purchase.grandTotalPaise) : <span className="row-subtext">Not posted</span>}</td>
            </tr>)}</tbody>
          </table></div>}
    </section>
  </>;
}

export function PurchaseCreatePage() {
  usePageTitle("New Purchase");
  const auth = useAuth();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [values, setValues] = useState({ supplierPartyId: "", supplierInvoiceNumber: "", invoiceDate: businessToday() });
  const [notice, setNotice] = useState<string | null>(null);

  const suppliers = useQuery({
    queryKey: ["parties", "suppliers", "active"],
    queryFn: () => listParties("", "active", "supplier"),
    retry: false
  });

  const create = useMutation({
    mutationFn: () => createPurchaseDraft(values),
    onSuccess: (draft) => {
      void queryClient.invalidateQueries({ queryKey: ["purchases", "list"] });
      void navigate(`/app/purchases/${draft.id}`);
    },
    onError: (caught) => setNotice(caught instanceof LocalServiceError ? caught.message : "The draft could not be created.")
  });

  if (auth.status?.user?.role !== "owner_admin") return <NotPermitted />;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setNotice(null);
    const reject = rejectInto(setNotice);
    if (!values.supplierPartyId) return reject("purchase-supplier", "Select the supplier who issued this invoice.");
    if (!values.supplierInvoiceNumber.trim()) return reject("purchase-invoice-number", "Enter the supplier's invoice number.");
    if (!values.invoiceDate) return reject("purchase-invoice-date", "Enter the invoice date.");
    create.mutate();
  };

  return <>
    <header className="page-header"><div><p className="eyebrow">OPERATIONS</p><h1>New Purchase</h1><p>Start from the supplier's invoice header. Lines are added next, and nothing affects stock until the document is posted.</p></div></header>
    <section className="master-panel" aria-labelledby="purchase-new-title">
      <h2 id="purchase-new-title">Invoice header</h2>
      <form className="master-form" onSubmit={submit}>
        <div className="field">
          <label htmlFor="purchase-supplier">Supplier<span aria-hidden="true"> *</span></label>
          <select id="purchase-supplier" value={values.supplierPartyId} onChange={(event) => setValues({ ...values, supplierPartyId: event.target.value })} disabled={!suppliers.data}>
            <option value="">{suppliers.data ? "Select a supplier" : "Loading…"}</option>
            {(suppliers.data ?? []).map((supplier) => <option key={supplier.id} value={supplier.id}>{supplier.displayName}</option>)}
          </select>
          <small>Only active suppliers appear here. Add one under Suppliers if it is missing.</small>
        </div>
        <div className="field">
          <label htmlFor="purchase-invoice-number">Supplier invoice number<span aria-hidden="true"> *</span></label>
          <input id="purchase-invoice-number" value={values.supplierInvoiceNumber} onChange={(event) => setValues({ ...values, supplierInvoiceNumber: event.target.value })} />
          <small>Exactly as printed on the supplier's invoice. The same number cannot be recorded twice for one supplier.</small>
        </div>
        <div className="field">
          <label htmlFor="purchase-invoice-date">Invoice date<span aria-hidden="true"> *</span></label>
          <input id="purchase-invoice-date" type="date" value={values.invoiceDate} onChange={(event) => setValues({ ...values, invoiceDate: event.target.value })} />
          <small>The GST rate in force on this date is the one applied when the purchase is posted.</small>
        </div>
        {suppliers.isError && <div className="catalog-inline-error catalog-span" role="alert"><span>Suppliers could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void suppliers.refetch()}>Retry</button></div>}
        {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{notice}</div>}
        <div className="form-actions">
          <Link className="button button--secondary" to="/app/purchases">Cancel</Link>
          <button className="button button--primary" type="submit" disabled={create.isPending}>{create.isPending ? "Creating…" : "Create Draft"}</button>
        </div>
      </form>
    </section>
  </>;
}

export function PurchaseDetailPage() {
  const { id } = useParams();
  const auth = useAuth();
  const purchase = useQuery({
    queryKey: ["purchases", "detail", id],
    queryFn: () => getPurchase(id!),
    enabled: Boolean(id),
    retry: false
  });
  useExpireOnAuthError(purchase.error);
  usePageTitle(purchase.data ? `Purchase ${purchase.data.supplierInvoiceNumber}` : "Purchase");

  if (purchase.isPending) return <Loading label="Loading purchase…" />;
  if (purchase.isError) return <QueryError label="This purchase could not be loaded" onRetry={() => void purchase.refetch()} />;

  const canMutate = auth.status?.user?.role === "owner_admin";
  return purchase.data.status === "draft" && canMutate
    ? <DraftEditor draft={purchase.data} />
    : <PostedDetail purchase={purchase.data} readOnlyRole={!canMutate} />;
}

/**
 * The draft editor. Every write is a separate Store Service operation carrying the document's
 * expected revision, which is exactly how the server behaves — the screen does not pretend the
 * whole draft is saved at once.
 */
function DraftEditor({ draft }: { draft: PurchaseDetail }) {
  const queryClient = useQueryClient();
  const [editingHeader, setEditingHeader] = useState(false);
  const [lineEditor, setLineEditor] = useState<{ line: PurchaseLine | null } | null>(null);
  const [posting, setPosting] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = (latest: PurchaseDetail) => {
    queryClient.setQueryData(["purchases", "detail", draft.id], latest);
    void queryClient.invalidateQueries({ queryKey: ["purchases", "list"] });
  };

  const suppliers = useQuery({
    queryKey: ["parties", "suppliers", "all"],
    queryFn: () => listParties("", "all", "supplier"),
    staleTime: 30_000,
    retry: false
  });

  const remove = useMutation({
    mutationFn: (line: PurchaseLine) => removePurchaseLine(line.id, draft.revision),
    onSuccess: refresh,
    onError: (caught) => setNotice(messageFor(caught, "The line could not be removed."))
  });

  const taxableTotal = draft.lines.reduce((total, line) => total + line.taxableValuePaise, 0);

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow"><Link to="/app/purchases">Purchases</Link> · DRAFT</p>
        <h1>{draft.supplierInvoiceNumber}</h1>
        <p>{supplierName(suppliers.data, draft.supplierPartyId, draft.supplierDisplayName)} · Invoice dated {draft.invoiceDate}</p>
      </div>
      <div className="header-actions">
        <button className="button button--secondary" type="button" onClick={() => setEditingHeader(true)}>Edit Header</button>
        <button className="button button--primary" type="button" onClick={() => setPosting(true)} disabled={draft.lines.length === 0}>Post Purchase</button>
      </div>
    </header>

    <div className="panel-callout" role="status"><strong>This purchase is a draft</strong><small>Nothing has entered stock and no GST has been recorded. {DRAFT_TAX_NOTE}</small></div>

    <section className="master-panel" aria-labelledby="draft-lines-title">
      <div className="panel-header">
        <h2 id="draft-lines-title">Lines</h2>
        <div className="header-actions">
          {/* The running total belongs where the operator is working. At ten lines the table's own
              footer is below the fold, so entering an invoice meant scrolling to check the figure
              against the supplier's printed total. */}
          {draft.lines.length > 0 && <p className="running-total"><span>{draft.lines.length} {draft.lines.length === 1 ? "line" : "lines"}</span><strong>{paiseToAmountText(taxableTotal)}</strong></p>}
          <button className="button button--secondary" type="button" onClick={() => setLineEditor({ line: null })}>Add Line</button>
        </div>
      </div>
      {notice && <div className="inline-notice inline-notice--error" role="alert">{notice}</div>}
      {draft.lines.length === 0
        ? <div className="empty-state"><h3>No lines yet</h3><p>Add each product on the supplier's invoice with the quantity and rate printed against it.</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">#</th><th scope="col">Product</th><th scope="col">Batch</th><th scope="col" className="numeric">Quantity (Pack)</th><th scope="col" className="numeric">Rate per Pack</th><th scope="col" className="numeric">Taxable value</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead>
            <tbody>{draft.lines.map((line) => <tr key={line.id}>
              <td data-label="#">{line.lineNumber}</td>
              <td data-label="Product"><LineProductName line={line} /></td>
              <td data-label="Batch"><BatchCell line={line} /></td>
              <td data-label="Quantity (Pack)" className="numeric">{line.quantityPacks}</td>
              <td data-label="Rate per Pack" className="numeric">{paiseToAmountText(line.ratePerPackPaise)}</td>
              <td data-label="Taxable value" className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
              <td className="row-actions"><div>
                <button type="button" onClick={() => setLineEditor({ line })}>Edit</button>
                <button type="button" onClick={() => remove.mutate(line)} disabled={remove.isPending}>Remove</button>
              </div></td>
            </tr>)}</tbody>
            <tfoot><tr><th scope="row" colSpan={5}>Taxable value</th><td className="numeric"><strong>{paiseToAmountText(taxableTotal)}</strong></td><td /></tr></tfoot>
          </table></div>}
    </section>

    {editingHeader && <HeaderDialog draft={draft} suppliers={suppliers.data} onClose={() => setEditingHeader(false)} onSaved={(latest) => { refresh(latest); setEditingHeader(false); }} />}
    {lineEditor && <LineDialog draft={draft} line={lineEditor.line} onClose={() => setLineEditor(null)} onSaved={(latest) => { refresh(latest); setLineEditor(null); }} onSavedAndContinue={refresh} />}
    {posting && <PostDialog draft={draft} onClose={() => setPosting(false)} onPosted={(latest) => { refresh(latest); setPosting(false); }} />}
  </>;
}

function HeaderDialog({ draft, suppliers, onClose, onSaved }: {
  draft: PurchaseDetail;
  suppliers: Party[] | undefined;
  onClose: () => void;
  onSaved: (latest: PurchaseDetail) => void;
}) {
  const queryClient = useQueryClient();
  const [values, setValues] = useState({
    supplierPartyId: draft.supplierPartyId,
    supplierInvoiceNumber: draft.supplierInvoiceNumber,
    invoiceDate: draft.invoiceDate
  });
  const [notice, setNotice] = useState<string | null>(null);

  const save = useMutation({
    mutationFn: () => updatePurchaseDraft(draft.id, draft.revision, values),
    onSuccess: onSaved,
    onError: (caught) => setNotice(messageFor(caught, "The invoice header could not be saved."))
  });
  const stale = isStale(save.error);

  const selectable = (suppliers ?? []).filter((supplier) => supplier.status === "active" || supplier.id === draft.supplierPartyId);

  return <CatalogDialog title="Edit Invoice Header" description="The supplier, their invoice number, and the date it was issued." onClose={onClose}>
    <form className="master-form" onSubmit={(event) => { event.preventDefault(); setNotice(null); save.mutate(); }}>
      <div className="field">
        <label htmlFor="edit-supplier">Supplier</label>
        <select id="edit-supplier" value={values.supplierPartyId} onChange={(event) => setValues({ ...values, supplierPartyId: event.target.value })} disabled={!suppliers}>
          {selectable.map((supplier) => <option key={supplier.id} value={supplier.id}>{supplier.displayName}{supplier.status === "archived" ? " (archived)" : ""}</option>)}
        </select>
      </div>
      <div className="field">
        <label htmlFor="edit-invoice-number">Supplier invoice number</label>
        <input id="edit-invoice-number" value={values.supplierInvoiceNumber} onChange={(event) => setValues({ ...values, supplierInvoiceNumber: event.target.value })} />
      </div>
      <div className="field">
        <label htmlFor="edit-invoice-date">Invoice date</label>
        <input id="edit-invoice-date" type="date" value={values.invoiceDate} onChange={(event) => setValues({ ...values, invoiceDate: event.target.value })} />
        <small>Changing this changes which GST rate will apply when the purchase is posted.</small>
      </div>
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{stale ? STALE_DOCUMENT_MESSAGE : notice}{stale && <ReloadLatest id={draft.id} onReloaded={(latest) => { queryClient.setQueryData(["purchases", "detail", draft.id], latest); onSaved(latest); }} />}</div>}
      <div className="form-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={save.isPending}>{save.isPending ? "Saving…" : "Save Header"}</button>
      </div>
    </form>
  </CatalogDialog>;
}

/**
 * One line. The Pack list is filtered to the chosen Product and the Batch list to the chosen Pack,
 * so an impossible combination cannot be assembled here — the server refuses it either way, but the
 * operator should never be offered it.
 */
function LineDialog({ draft, line, onClose, onSaved, onSavedAndContinue }: {
  draft: PurchaseDetail;
  line: PurchaseLine | null;
  onClose: () => void;
  onSaved: (latest: PurchaseDetail) => void;
  /** Records the saved line but leaves the dialog open for the next one. */
  onSavedAndContinue?: (latest: PurchaseDetail) => void;
}) {
  const queryClient = useQueryClient();
  const [productId, setProductId] = useState(line?.productId ?? "");
  const [packId, setPackId] = useState(line?.productPackId ?? "");
  const [batchMode, setBatchMode] = useState<"existing" | "new">(line && line.newBatchNumber ? "new" : "existing");
  const [batchId, setBatchId] = useState(line?.batchId ?? "");
  const [newBatch, setNewBatch] = useState({
    number: line?.newBatchNumber ?? "",
    expiresOn: line?.newBatchExpiresOn ?? "",
    mrp: line?.newBatchMrpPaise == null ? "" : paiseToRupees(line.newBatchMrpPaise)
  });
  const [quantity, setQuantity] = useState(line ? String(line.quantityPacks) : "");
  const [rate, setRate] = useState(line ? paiseToRupees(line.ratePerPackPaise) : "");
  const [notice, setNotice] = useState<string | null>(null);

  const products = useQuery({ queryKey: ["products", "list", "", "active"], queryFn: () => listProducts("", "active"), staleTime: 30_000, retry: false });
  const product = useQuery({ queryKey: ["products", "detail", productId], queryFn: () => getProduct(productId), enabled: Boolean(productId), retry: false });
  const batches = useQuery({ queryKey: ["batches", packId], queryFn: () => listBatches(packId), enabled: Boolean(packId), retry: false });

  // A Pack belongs to exactly one Product, so changing the Product invalidates the Pack and, with
  // it, the batch chosen underneath that Pack.
  const packs = useMemo(
    () => (product.data?.packs ?? []).filter((pack: ProductPack) => pack.status === "active" || pack.id === line?.productPackId),
    [product.data, line?.productPackId]
  );
  const selectableBatches = useMemo(
    () => (batches.data ?? []).filter((batch: Batch) => batch.status === "active" || batch.id === line?.batchId),
    [batches.data, line?.batchId]
  );

  /**
   * A supplier invoice arrives with many lines, and reopening the dialog for each one costs a
   * click and a full tab traversal every time. Saving and continuing keeps the dialog open and
   * clears only what changes between lines, so the operator stays on the keyboard.
   */
  const [keepOpen, setKeepOpen] = useState(false);
  const save = useMutation({
    mutationFn: (body: PurchaseLineInput) => line ? updatePurchaseLine(line.id, body) : addPurchaseLine(draft.id, body),
    onSuccess: (latest) => {
      if (!keepOpen || !onSavedAndContinue) { onSaved(latest); return; }
      onSavedAndContinue(latest);
      // The product and pack usually repeat down a distributor's invoice, so they are kept; the
      // batch, quantity and rate are specific to the line just saved and must not carry over.
      setBatchId("");
      setNewBatch({ number: "", expiresOn: "", mrp: "" });
      setQuantity("");
      setRate("");
      setKeepOpen(false);
      document.getElementById("line-quantity")?.focus();
    },
    onError: (caught) => { setKeepOpen(false); setNotice(messageFor(caught, "The line could not be saved.")); }
  });
  const stale = isStale(save.error);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setNotice(null);
    // Parsed from the digits alone: `parseInt` would read "2.5" as 2 and record a quantity the
    // operator never typed.
    const packsOrdered = /^\d+$/.test(quantity.trim()) ? Number.parseInt(quantity.trim(), 10) : Number.NaN;
    const ratePaise = ratePerPackToPaise(rate);
    const mrpPaise = newBatch.mrp.trim() ? ratePerPackToPaise(newBatch.mrp) : null;
    const reject = rejectInto(setNotice);
    if (!productId) return reject("line-product", "Select the product on this line.");
    if (!packId) return reject("line-pack", "Select the pack this quantity is counted in.");
    if (batchMode === "existing" && !batchId) return reject("line-batch", "Select the batch received, or switch to recording a new batch.");
    if (batchMode === "new" && !newBatch.number.trim()) return reject("line-batch-number", "Enter the batch number printed on the pack.");
    if (batchMode === "new" && newBatch.mrp.trim() && (mrpPaise === null || mrpPaise === 0)) return reject("line-batch-mrp", "MRP must be an amount in rupees above zero, with at most two decimal places.");
    if (!Number.isSafeInteger(packsOrdered) || packsOrdered <= 0) return reject("line-quantity", "Quantity (Pack) must be a whole number of packs, at least one.");
    if (ratePaise === null) return reject("line-rate", "Rate per Pack must be an amount in rupees with at most two decimal places.");
    save.mutate({
      expectedRevision: draft.revision,
      productId,
      productPackId: packId,
      // Mutually exclusive by construction: the unselected mode is sent as null, never omitted, so
      // switching modes on an existing line clears the other side on the server too.
      batchId: batchMode === "existing" ? batchId : null,
      newBatchNumber: batchMode === "new" ? newBatch.number.trim() : null,
      newBatchExpiresOn: batchMode === "new" ? newBatch.expiresOn || null : null,
      newBatchMrpPaise: batchMode === "new" ? mrpPaise : null,
      quantityPacks: packsOrdered,
      ratePerPackPaise: ratePaise
    });
  };

  return <CatalogDialog title={line ? `Edit Line ${line.lineNumber}` : "Add Line"} description="What was received, in which pack and batch, and at what price." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="line-product">Product<span aria-hidden="true"> *</span></label>
        <select id="line-product" value={productId} onChange={(event) => { setProductId(event.target.value); setPackId(""); setBatchId(""); }} disabled={!products.data}>
          <option value="">{products.data ? "Select a product" : "Loading…"}</option>
          {(products.data ?? []).map((item: Product) => <option key={item.id} value={item.id}>{item.displayName}</option>)}
        </select>
      </div>
      <div className="field">
        <label htmlFor="line-pack">Pack<span aria-hidden="true"> *</span></label>
        <select id="line-pack" value={packId} onChange={(event) => { setPackId(event.target.value); setBatchId(""); }} disabled={!productId || product.isPending}>
          <option value="">{!productId ? "Select a product first" : product.isPending ? "Loading…" : "Select a pack"}</option>
          {packs.map((pack: ProductPack) => <option key={pack.id} value={pack.id}>{packText(pack)}{pack.status === "archived" ? " (archived)" : ""}</option>)}
        </select>
        <small>Quantity and rate below are both counted per this pack.</small>
      </div>

      <fieldset className="field catalog-span">
        <legend>Batch received<span aria-hidden="true"> *</span></legend>
        <div className="choice-row">
          <label><input type="radio" name="batch-mode" value="existing" checked={batchMode === "existing"} onChange={() => setBatchMode("existing")} /> An existing batch</label>
          <label><input type="radio" name="batch-mode" value="new" checked={batchMode === "new"} onChange={() => setBatchMode("new")} /> A batch not recorded yet</label>
        </div>
        <small>A purchase line carries one batch. Recording a new one here creates it when the purchase is posted.</small>
      </fieldset>

      {batchMode === "existing"
        ? <div className="field">
            <label htmlFor="line-batch">Batch<span aria-hidden="true"> *</span></label>
            <select id="line-batch" value={batchId} onChange={(event) => setBatchId(event.target.value)} disabled={!packId || batches.isPending}>
              <option value="">{!packId ? "Select a pack first" : batches.isPending ? "Loading…" : selectableBatches.length ? "Select a batch" : "No batches recorded for this pack"}</option>
              {selectableBatches.map((batch: Batch) => <option key={batch.id} value={batch.id}>{batchText(batch)}{batch.status === "archived" ? " (archived)" : ""}</option>)}
            </select>
          </div>
        : <>
            <div className="field">
              <label htmlFor="line-batch-number">Batch number<span aria-hidden="true"> *</span></label>
              <input id="line-batch-number" value={newBatch.number} onChange={(event) => setNewBatch({ ...newBatch, number: event.target.value })} />
            </div>
            <div className="field">
              <label htmlFor="line-batch-expiry">Expires on</label>
              <input id="line-batch-expiry" type="date" value={newBatch.expiresOn} onChange={(event) => setNewBatch({ ...newBatch, expiresOn: event.target.value })} />
            </div>
            <div className="field">
              <label htmlFor="line-batch-mrp">MRP per pack</label>
              <input id="line-batch-mrp" inputMode="decimal" value={newBatch.mrp} onChange={(event) => setNewBatch({ ...newBatch, mrp: event.target.value })} />
              <small>Printed retail price. Not the purchase rate.</small>
            </div>
          </>}

      <div className="field">
        <label htmlFor="line-quantity">Quantity (Pack)<span aria-hidden="true"> *</span></label>
        <input id="line-quantity" inputMode="numeric" value={quantity} onChange={(event) => setQuantity(event.target.value)} />
        <small>Whole packs received, as invoiced.</small>
      </div>
      <div className="field">
        <label htmlFor="line-rate">Rate per Pack<span aria-hidden="true"> *</span></label>
        <input id="line-rate" inputMode="decimal" value={rate} onChange={(event) => setRate(event.target.value)} />
        <small>The supplier's price for one pack, before GST. Zero is allowed for free goods.</small>
      </div>

      {products.isError && <div className="catalog-inline-error catalog-span" role="alert"><span>Products could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void products.refetch()}>Retry</button></div>}
      {batches.isError && <div className="catalog-inline-error catalog-span" role="alert"><span>Batches could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void batches.refetch()}>Retry</button></div>}
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{stale ? STALE_DOCUMENT_MESSAGE : notice}{stale && <ReloadLatest id={draft.id} onReloaded={(latest) => { queryClient.setQueryData(["purchases", "detail", draft.id], latest); onSaved(latest); }} />}</div>}
      <div className="form-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        {!line && onSavedAndContinue && <button className="button button--secondary" type="submit" disabled={save.isPending} onClick={() => setKeepOpen(true)}>Add and Next Line</button>}
        <button className="button button--primary" type="submit" disabled={save.isPending}>{save.isPending ? "Saving…" : line ? "Save Line" : "Add Line"}</button>
      </div>
    </form>
  </CatalogDialog>;
}

/**
 * Posting confirmation.
 *
 * The idempotency key is generated once, when the dialog opens, and held for the life of the
 * dialog. A retry after a timeout therefore reuses it and the Store Service returns the original
 * document rather than posting the invoice a second time.
 */
function PostDialog({ draft, onClose, onPosted }: {
  draft: PurchaseDetail;
  onClose: () => void;
  onPosted: (latest: PurchaseDetail) => void;
}) {
  const queryClient = useQueryClient();
  const postingKey = useRef(newIdempotencyKey());
  const [notice, setNotice] = useState<string | null>(null);

  // A disabled attribute is applied on the next render, which is too late for a double-click: three
  // rapid clicks really did issue three postings. The idempotency key still made the invoice post
  // exactly once, but the browser should not be sending work it knows is already in flight, so the
  // guard is set synchronously and released only when the attempt has settled.
  const inFlight = useRef(false);
  const post = useMutation({
    mutationFn: () => postPurchase(draft.id, draft.revision, postingKey.current),
    onSuccess: onPosted,
    onError: (caught) => setNotice(messageFor(caught, "This purchase could not be posted.")),
    onSettled: () => { inFlight.current = false; }
  });
  const startPosting = () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setNotice(null);
    post.mutate();
  };
  const stale = isStale(post.error);
  const taxableTotal = draft.lines.reduce((total, line) => total + line.taxableValuePaise, 0);

  return <CatalogDialog title="Post Purchase" description="Posting records the stock received and the GST charged." onClose={onClose}>
    <div className="master-form">
      <dl className="detail-grid catalog-span">
        <div><dt>Supplier invoice</dt><dd>{draft.supplierInvoiceNumber}</dd></div>
        <div><dt>Invoice date</dt><dd>{draft.invoiceDate}</dd></div>
        <div><dt>Lines</dt><dd>{draft.lines.length}</dd></div>
        <div><dt>Taxable value</dt><dd>{paiseToAmountText(taxableTotal)}</dd></div>
      </dl>
      <div className="panel-callout catalog-span" role="status"><strong>This cannot be undone</strong><small>A posted purchase is permanent. A mistake is corrected with a later document, never by editing this one. The GST is calculated now, from the rate in force on {draft.invoiceDate} and from the two States involved.</small></div>
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{stale ? STALE_DOCUMENT_MESSAGE : notice}{stale && <ReloadLatest id={draft.id} onReloaded={(latest) => { queryClient.setQueryData(["purchases", "detail", draft.id], latest); onPosted(latest); }} />}</div>}
      <div className="form-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="button" onClick={startPosting} disabled={post.isPending}>{post.isPending ? "Posting…" : "Post Purchase"}</button>
      </div>
    </div>
  </CatalogDialog>;
}

/**
 * A posted purchase, read-only for everyone. It shows the snapshots the Store Service recorded at
 * posting — not today's supplier or store profile — because that is what the document means.
 */
function PostedDetail({ purchase, readOnlyRole }: { purchase: PurchaseDetail; readOnlyRole: boolean }) {
  const draft = purchase.status === "draft";
  // Sending goods back to a supplier is the owner’s commercial decision, so only the owner is
  // offered the door into it.
  const canReturn = !readOnlyRole;
  /**
   * A draft has no supplier snapshot — that is written at posting — so its name must come from the
   * current party record. Falling through to the raw party id put a UUID on the screen where the
   * supplier's name belongs, which is never something to show a pharmacist.
   */
  const suppliers = useQuery({
    queryKey: ["parties", "suppliers", "all"],
    queryFn: () => listParties("", "all", "supplier"),
    enabled: draft && !purchase.supplierDisplayName,
    staleTime: 30_000,
    retry: false
  });
  const currentName = suppliers.data?.find((supplier) => supplier.id === purchase.supplierPartyId)?.displayName;
  const shownSupplier = purchase.supplierDisplayName
    ?? currentName
    ?? (suppliers.isError ? "Supplier unavailable" : "Loading…");

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow"><Link to="/app/purchases">Purchases</Link> · {draft ? "DRAFT" : "POSTED"}</p>
        <h1>{purchase.supplierInvoiceNumber}</h1>
        <p>{shownSupplier} · Invoice dated {purchase.invoiceDate}</p>
      </div>
      <div className="page-header__actions">
        {!draft && canReturn && <Link className="button button--secondary" to={`/app/purchases/${purchase.id}/return`}>Return Items</Link>}
        <span className="read-only-note">{draft ? "Read-only access" : "Posted · read-only"}</span>
      </div>
    </header>

    {draft && readOnlyRole && <div className="panel-callout" role="status"><strong>This purchase is still a draft</strong><small>Nothing has entered stock and no GST has been recorded. Only an Owner/Admin can change or post it.</small></div>}

    {/* A draft has no snapshots yet, so the absent ones are described as undetermined rather than
        as facts. "Not registered" would be an answer this document has never been given. */}
    <section className="master-panel" aria-labelledby="posted-header-title">
      <h2 id="posted-header-title">{draft ? "Invoice header" : "As recorded"}</h2>
      <dl className="detail-grid">
        <div><dt>Supplier</dt><dd>{shownSupplier}</dd></div>
        <div><dt>Invoice date</dt><dd>{purchase.invoiceDate}</dd></div>
        {!draft && <>
          <div><dt>Supplier GSTIN</dt><dd>{purchase.supplierNormalizedGstin ?? "Not registered"}</dd></div>
          <div><dt>Supplier State</dt><dd>{purchase.supplierStateCode ?? "Not recorded"}</dd></div>
          <div><dt>Store GSTIN</dt><dd>{purchase.storeNormalizedGstin ?? "Not registered"}</dd></div>
          <div><dt>Store State</dt><dd>{purchase.storeStateCode ?? "Not recorded"}</dd></div>
          <div><dt>Tax treatment</dt><dd>{purchase.taxTreatment ? TREATMENT_LABELS[purchase.taxTreatment] : "Not determined"}</dd></div>
          <div><dt>Posted at</dt><dd>{purchase.postedAtUtc ? new Date(purchase.postedAtUtc).toLocaleString("en-IN") : "Not posted"}</dd></div>
        </>}
      </dl>
      {!draft && <p className="panel-note">These are the facts as they stood when this purchase was posted. Later changes to the supplier or store profile do not alter this document.</p>}
    </section>

    <section className="master-panel" aria-labelledby="posted-lines-title">
      <h2 id="posted-lines-title">Lines</h2>
      {/* An unposted document carries no tax at all, so its tax columns are absent rather than
          filled with zeros that would read as "no GST applies to this purchase". */}
      <div className="table-scroll"><table className="data-table">
        <thead><tr>
          <th scope="col">#</th><th scope="col">Product</th><th scope="col">Batch</th>
          {!draft && <th scope="col">HSN</th>}
          <th scope="col" className="numeric">Quantity (Pack)</th><th scope="col" className="numeric">Rate per Pack</th>
          <th scope="col" className="numeric">Taxable value</th>
          {!draft && <><th scope="col">GST</th><th scope="col" className="numeric">Line total</th></>}
        </tr></thead>
        <tbody>{purchase.lines.map((line) => <tr key={line.id}>
          <td data-label="#">{line.lineNumber}</td>
          <td data-label="Product"><LineProductName line={line} /></td>
          <td data-label="Batch"><BatchCell line={line} /></td>
          {!draft && <td data-label="HSN">{line.hsnCode ?? "—"}</td>}
          <td data-label="Quantity (Pack)" className="numeric">{line.quantityPacks}</td>
          <td data-label="Rate per Pack" className="numeric">{paiseToAmountText(line.ratePerPackPaise)}</td>
          <td data-label="Taxable value" className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
          {!draft && <>
            <td data-label="GST"><LineTax line={line} treatment={purchase.taxTreatment} /></td>
            <td data-label="Line total" className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
          </>}
        </tr>)}</tbody>
      </table></div>
      {draft && <p className="panel-note">{DRAFT_TAX_NOTE}</p>}
    </section>

    {!draft && <section className="master-panel" aria-labelledby="posted-totals-title">
      <h2 id="posted-totals-title">Invoice totals</h2>
      <dl className="detail-grid totals-grid">
        <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(purchase.taxableValuePaise)}</dd></div>
        {purchase.taxTreatment === "inter_state"
          ? <div><dt>IGST</dt><dd className="numeric">{paiseToAmountText(purchase.igstPaise)}</dd></div>
          : <>
              <div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(purchase.cgstPaise)}</dd></div>
              <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(purchase.sgstPaise)}</dd></div>
            </>}
        {purchase.cessPaise > 0 && <div><dt>Cess</dt><dd className="numeric">{paiseToAmountText(purchase.cessPaise)}</dd></div>}
        <div className="totals-grand"><dt>Invoice total</dt><dd className="numeric"><strong>{paiseToAmountText(purchase.grandTotalPaise)}</strong></dd></div>
      </dl>
      <p className="panel-note">Each line's tax is rounded once, to the paise, and the totals are the sum of those rounded lines. No further rounding is applied.</p>
    </section>}
  </>;
}

function LineTax({ line, treatment }: { line: PurchaseLine; treatment: PurchaseTaxTreatment | null }) {
  if (line.taxTreatmentKind && line.taxTreatmentKind !== "taxable") {
    return <span className="row-subtext">{TAX_KIND_LABELS[line.taxTreatmentKind]}</span>;
  }
  const parts: Array<{ label: string; basisPoints: number; paise: number }> = treatment === "inter_state"
    ? [{ label: "IGST", basisPoints: line.igstBasisPoints, paise: line.igstPaise }]
    : [
        { label: "CGST", basisPoints: line.cgstBasisPoints, paise: line.cgstPaise },
        { label: "SGST", basisPoints: line.sgstBasisPoints, paise: line.sgstPaise }
      ];
  // Cess is shown only when the rate actually carries one, so an ordinary medicine line stays plain.
  if (line.cessBasisPoints > 0 || line.cessPaise > 0) parts.push({ label: "Cess", basisPoints: line.cessBasisPoints, paise: line.cessPaise });
  return <ul className="tax-breakdown">{parts.map((part) => <li key={part.label}><span>{part.label} {basisPointsToPercentText(part.basisPoints)}%</span><b>{paiseToAmountText(part.paise)}</b></li>)}</ul>;
}

function LineProductName({ line }: { line: PurchaseLine }) {
  const product = useQuery({ queryKey: ["products", "detail", line.productId], queryFn: () => getProduct(line.productId), staleTime: 60_000, retry: false });
  const pack = product.data?.packs.find((item: ProductPack) => item.id === line.productPackId);
  return <>
    <Link to={`/app/products/${line.productId}`}>{product.data?.displayName ?? (product.isError ? "Product unavailable" : "Loading…")}</Link>
    {pack && <small className="row-subtext">{packText(pack)}</small>}
  </>;
}

function BatchCell({ line }: { line: PurchaseLine }) {
  const batches = useQuery({ queryKey: ["batches", line.productPackId], queryFn: () => listBatches(line.productPackId), enabled: Boolean(line.batchId), staleTime: 60_000, retry: false });
  if (line.newBatchNumber) {
    return <>{line.newBatchNumber}<small className="row-subtext">New batch{line.newBatchExpiresOn ? ` · expires ${line.newBatchExpiresOn}` : ""}</small></>;
  }
  const match = batches.data?.find((batch: Batch) => batch.id === line.batchId);
  return <>{match ? batchText(match) : batches.isError ? "Batch unavailable" : "Loading…"}</>;
}

function ReloadLatest({ id, onReloaded }: { id: string; onReloaded: (latest: PurchaseDetail) => void }) {
  const [reloading, setReloading] = useState(false);
  return <button type="button" disabled={reloading} onClick={async () => {
    setReloading(true);
    try { onReloaded(await getPurchase(id)); } finally { setReloading(false); }
  }}>{reloading ? "Reloading…" : "Reload latest"}</button>;
}

function packText(pack: ProductPack): string {
  return pack.displayLabel ?? pack.skuCode ?? `${pack.baseQuantityAtoms} per pack`;
}

function batchText(batch: Batch): string {
  return batch.expiresOn ? `${batch.batchNumber} · expires ${batch.expiresOn}` : batch.batchNumber;
}

function supplierName(suppliers: Party[] | undefined, id: string, snapshot?: string | null): string {
  return snapshot ?? suppliers?.find((supplier) => supplier.id === id)?.displayName ?? id;
}

/**
 * Reports a validation failure and puts the cursor on the field that caused it.
 *
 * Announcing the problem is not enough on its own: without the focus move, an operator working by
 * keyboard hears what is wrong and is then left on the submit button, having to tab backwards
 * through the form to find out where.
 */
function rejectInto(setNotice: (message: string) => void) {
  return (fieldId: string, message: string) => {
    setNotice(message);
    const field = document.getElementById(fieldId);
    if (field instanceof HTMLElement) field.focus();
  };
}

function messageFor(caught: unknown, fallback: string): string {
  return caught instanceof LocalServiceError ? caught.message : fallback;
}

function isStale(error: unknown): boolean {
  return error instanceof LocalServiceError && error.code === "revision_conflict";
}

function NotPermitted(): ReactNode {
  return <div className="empty-state" role="alert"><h3>Not permitted</h3><p>Only an Owner/Admin can record a purchase.</p><Link className="button button--secondary" to="/app/purchases">Back to Purchases</Link></div>;
}

function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
