import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";
import type {
  Party,
  Product,
  ProductPack,
  QuantityBasis,
  SaleDetail,
  SaleLine,
  SaleLineTaxKind,
  SaleTaxTreatment,
  SellableBatch,
  TenderMethod
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { businessToday } from "../platform/businessDate";
import { LocalServiceError } from "../platform/localService";
import { listParties } from "../parties/partyApi";
import { newIdempotencyKey } from "../inventory/inventoryApi";
import { getProduct, listProducts } from "../products/productApi";
import { listReferences } from "../reference/referenceApi";
import { getStoreTaxIdentity } from "../settings/storeApi";
import {
  addSaleLine,
  basisUnitLabel,
  createSaleDraft,
  getSale,
  listSales,
  listSellableBatches,
  paiseToAmountText,
  postSale,
  quantityToInteger,
  quoteSale,
  removeSaleLine,
  sellingRateToPaise,
  updateSaleDraft,
  type SaleStatusFilter
} from "./saleApi";

const TREATMENT_LABELS: Record<SaleTaxTreatment, string> = {
  intra_state: "CGST + SGST (same State)",
  inter_state: "IGST (different States)"
};

const TAX_KIND_LABELS: Record<SaleLineTaxKind, string> = {
  taxable: "Taxable",
  exempt: "Exempt",
  nil_rated: "Nil rated",
  non_gst: "Outside GST"
};

const TENDER_LABELS: Record<TenderMethod, string> = {
  cash: "Cash",
  card: "Card",
  upi: "UPI"
};

const STALE_DOCUMENT_MESSAGE =
  "This sale changed after you opened it. Reload the latest version before saving.";

// ---------------------------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------------------------

export function SaleListPage() {
  usePageTitle("Sales");
  const [status, setStatus] = useState<SaleStatusFilter>("all");
  const sales = useQuery({
    queryKey: ["sales", "list", status],
    queryFn: () => listSales(status),
    retry: false
  });
  useExpireOnAuthError(sales.error);

  return <>
    <header className="page-header">
      <div><p className="eyebrow">OPERATIONS</p><h1>Sales</h1><p>Goods sold over the counter. A draft can be corrected freely; posting issues the invoice number, records the GST and takes the stock out.</p></div>
      <Link className="button button--primary" to="/app/sales/new">New Sale</Link>
    </header>

    <section className="master-panel" aria-labelledby="sale-list-title">
      <h2 id="sale-list-title">Sale documents</h2>
      <div className="master-toolbar">
        <div className="filter-field">
          <label htmlFor="sale-status">Status</label>
          <select id="sale-status" value={status} onChange={(event) => setStatus(event.target.value as SaleStatusFilter)}>
            <option value="all">All</option>
            <option value="draft">Drafts</option>
            <option value="posted">Posted</option>
          </select>
        </div>
      </div>
      {sales.isPending ? <Loading label="Loading sales…" />
        : sales.isError ? <QueryError label="Sales could not be loaded" onRetry={() => void sales.refetch()} />
        : sales.data.length === 0 ? <div className="empty-state"><h3>No sales yet</h3><p>Start a sale to bill a customer at the counter.</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Invoice</th><th scope="col">Customer</th><th scope="col">Date</th><th scope="col">Status</th><th scope="col" className="numeric">Invoice total</th></tr></thead>
            <tbody>{sales.data.map((sale) => <tr key={sale.id}>
              <td data-label="Invoice"><Link to={`/app/sales/${sale.id}`}>{sale.documentNumber ?? "Draft"}</Link></td>
              <td data-label="Customer">{sale.customerDisplayName ?? sale.customerNameText ?? <span className="row-subtext">Walk-in</span>}</td>
              <td data-label="Date">{sale.businessDate}</td>
              <td data-label="Status"><span className={`status-badge status-badge--${sale.status === "posted" ? "active" : "archived"}`}>{sale.status === "posted" ? "Posted" : "Draft"}</span></td>
              <td data-label="Invoice total" className="numeric">{sale.status === "posted" ? paiseToAmountText(sale.grandTotalPaise) : <span className="row-subtext">Not posted</span>}</td>
            </tr>)}</tbody>
          </table></div>}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// New sale
// ---------------------------------------------------------------------------------------------

/**
 * A counter does not fill in a header before it can start billing.
 *
 * The draft is created immediately as a walk-in on today's business date, and the operator is put
 * straight onto the POS screen. Naming a customer is an edit made afterwards, on the rare bill that
 * needs one, rather than a gate in front of every sale.
 */
export function SaleCreatePage() {
  usePageTitle("New Sale");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [notice, setNotice] = useState<string | null>(null);
  const started = useRef(false);

  const create = useMutation({
    mutationFn: () => createSaleDraft({ businessDate: businessToday() }),
    onSuccess: (draft) => {
      void queryClient.invalidateQueries({ queryKey: ["sales", "list"] });
      void navigate(`/app/sales/${draft.id}`, { replace: true });
    },
    onError: (caught) => setNotice(messageFor(caught, "The sale could not be started."))
  });

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    create.mutate();
  }, []);

  return <>
    <header className="page-header"><div><p className="eyebrow">OPERATIONS</p><h1>New Sale</h1><p>Opening the counter screen.</p></div></header>
    <section className="master-panel">
      {notice
        ? <div className="empty-state" role="alert"><h3>The sale could not be started</h3><p>{notice}</p><Link className="button button--secondary" to="/app/sales">Back to Sales</Link></div>
        : <Loading label="Starting a new sale…" />}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// Detail — the POS screen for a draft, the invoice for a posted sale
// ---------------------------------------------------------------------------------------------

export function SaleDetailPage() {
  const { id } = useParams();
  const sale = useQuery({
    queryKey: ["sales", "detail", id],
    queryFn: () => getSale(id!),
    enabled: Boolean(id),
    retry: false
  });
  useExpireOnAuthError(sale.error);
  usePageTitle(sale.data?.documentNumber ?? "Sale");

  if (sale.isPending) return <section className="master-panel"><Loading label="Loading sale…" /></section>;
  if (sale.isError) return <section className="master-panel"><QueryError label="This sale could not be loaded" onRetry={() => void sale.refetch()} /></section>;
  return sale.data.status === "draft"
    ? <PointOfSale sale={sale.data} />
    : <PostedInvoice sale={sale.data} />;
}

// ---------------------------------------------------------------------------------------------
// Point of sale
// ---------------------------------------------------------------------------------------------

type EntryState = {
  search: string;
  productId: string;
  packId: string;
  batchId: string;
  basis: QuantityBasis;
  quantity: string;
  rate: string;
};

const EMPTY_ENTRY: EntryState = {
  search: "",
  productId: "",
  packId: "",
  batchId: "",
  basis: "pack",
  quantity: "1",
  rate: ""
};

/**
 * The counter screen.
 *
 * Everything happens on one page and in one tab order: search, pick the lot, type a quantity and a
 * price, press Enter, and the focus returns to the search box for the next line. There is no dialog
 * anywhere — the Phase 1G modal editor suits an admin filling in a supplier invoice, and it does not
 * suit somebody billing a queue.
 */
function PointOfSale({ sale }: { sale: SaleDetail }) {
  const queryClient = useQueryClient();
  const [entry, setEntry] = useState<EntryState>(EMPTY_ENTRY);
  const [notice, setNotice] = useState<string | null>(null);
  const [tenderMethod, setTenderMethod] = useState<TenderMethod>("cash");
  const [tenderReference, setTenderReference] = useState("");
  const searchBox = useRef<HTMLInputElement>(null);
  // A ref, not the mutation's own pending flag: `disabled` only takes effect on the next render, and
  // a triple-click reaches the handler three times before React has painted once. Phase 1G proved
  // this with a test that sent three postings.
  const inFlight = useRef(false);

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["sales", "detail", sale.id] });
    void queryClient.invalidateQueries({ queryKey: ["sales", "quote", sale.id] });
    void queryClient.invalidateQueries({ queryKey: ["sales", "list"] });
  };

  const store = useQuery({ queryKey: ["store", "tax-identity"], queryFn: getStoreTaxIdentity, staleTime: 300_000, retry: false });
  // The Product carries a unit id; a counter needs the unit's NAME, so a loose line is labelled in
  // tablets or millilitres rather than in the word "units".
  const units = useQuery({
    queryKey: ["reference", "units", "active"],
    queryFn: () => listReferences("units", "", "active"),
    staleTime: 300_000,
    retry: false
  });
  const unitName = (unitId: string | undefined): string => {
    const match = (units.data ?? []).find((unit) => unit.id === unitId);
    return match && match.kind === "units" ? match.attributes.displayName : "units";
  };
  const products = useQuery({
    queryKey: ["products", "sale-search", entry.search],
    queryFn: () => listProducts(entry.search, "active"),
    enabled: entry.search.trim().length >= 2,
    retry: false
  });
  const product = useQuery({
    queryKey: ["products", "detail", entry.productId],
    queryFn: () => getProduct(entry.productId),
    enabled: Boolean(entry.productId),
    retry: false
  });
  const batches = useQuery({
    queryKey: ["sales", "sellable-batches", entry.packId, sale.businessDate],
    queryFn: () => listSellableBatches(entry.packId, sale.businessDate),
    enabled: Boolean(entry.packId),
    retry: false
  });
  /**
   * What this bill comes to, resolved by the Store Service.
   *
   * The browser deliberately does not add up the tax itself: it asks the same code that will post
   * the invoice, so the figure read out at the counter cannot drift from the one that gets charged.
   */
  const quote = useQuery({
    queryKey: ["sales", "quote", sale.id, sale.revision],
    queryFn: () => quoteSale(sale.id),
    retry: false
  });

  const packs = useMemo(
    () => (product.data?.packs ?? []).filter((pack) => pack.status === "active"),
    [product.data]
  );
  // Choosing the product is enough when there is only one thing it could mean: a single active pack
  // selects itself. With several, the operator still chooses, because guessing which presentation a
  // customer asked for would be inventing an answer.
  useEffect(() => {
    if (entry.productId && !entry.packId && packs.length === 1) {
      setEntry((current) => (current.packId ? current : { ...current, packId: packs[0].id }));
    }
  }, [entry.productId, entry.packId, packs]);
  const selectedPack = packs.find((pack) => pack.id === entry.packId) ?? null;
  const selectedBatch = (batches.data ?? []).find((batch) => batch.id === entry.batchId) ?? null;
  const baseUnitName = product.data ? unitName(product.data.baseUnitId) : "units";
  const unitLabel = basisUnitLabel(entry.basis, selectedPack?.displayLabel ?? null, baseUnitName);

  const addLine = useMutation({
    mutationFn: (input: Parameters<typeof addSaleLine>[1]) => addSaleLine(sale.id, input),
    onSuccess: () => {
      refresh();
      setEntry(EMPTY_ENTRY);
      setNotice(null);
      searchBox.current?.focus();
    },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "The line could not be added."))
  });

  const removeLine = useMutation({
    mutationFn: (lineId: string) => removeSaleLine(lineId, sale.revision),
    onSuccess: () => { refresh(); setNotice(null); },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "The line could not be removed."))
  });

  const post = useMutation({
    mutationFn: (amountPaise: number) => postSale(sale.id, sale.revision, newIdempotencyKey(), [{
      method: tenderMethod,
      amountPaise,
      referenceText: tenderReference.trim() || null
    }]),
    onSuccess: () => { refresh(); setNotice(null); },
    onSettled: () => { inFlight.current = false; },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "This sale could not be posted."))
  });

  const submitLine = (event: FormEvent) => {
    event.preventDefault();
    setNotice(null);
    const reject = rejectInto(setNotice);
    if (!entry.productId) return reject("pos-search", "Find the product being sold.");
    if (!entry.packId) return reject("pos-pack", "Choose the pack this is sold in.");
    if (!entry.batchId) return reject("pos-batch", "Choose the batch the goods come from.");
    if (selectedBatch?.expired) return reject("pos-batch", "That batch has expired and cannot be sold.");
    const quantity = quantityToInteger(entry.quantity);
    if (quantity === null) return reject("pos-quantity", `Enter a whole number of ${unitLabel}.`);
    const rate = sellingRateToPaise(entry.rate);
    if (rate === null) return reject("pos-rate", `Enter the price per ${unitLabel} in rupees, to at most two decimals.`);
    addLine.mutate({
      expectedRevision: sale.revision,
      productId: entry.productId,
      productPackId: entry.packId,
      batchId: entry.batchId,
      quantityBasis: entry.basis,
      quantity,
      sellingRatePaise: rate
    });
  };

  const submitPost = () => {
    setNotice(null);
    if (inFlight.current) return;
    if (sale.lines.length === 0) {
      setNotice("Add at least one line before taking payment.");
      return;
    }
    if (!quote.data) {
      setNotice("The amount payable is not known yet. Wait for the total, or fix the line it refuses.");
      return;
    }
    inFlight.current = true;
    post.mutate(quote.data.grandTotalPaise);
  };

  const chooseProduct = (chosen: Product) => {
    setEntry({ ...EMPTY_ENTRY, search: chosen.displayName, productId: chosen.id });
    setNotice(null);
  };

  /**
   * Enter in the search box takes the match, rather than submitting a line that has no product yet.
   *
   * A scanner types the code and sends Enter; so does anyone typing a name quickly. Letting that
   * Enter fall through to the form produced "Find the product being sold" for a product the operator
   * had just found, which at a counter reads as the software not listening.
   */
  const searchKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "Enter") return;
    const matches = products.data ?? [];
    if (entry.productId || matches.length === 0) return;
    event.preventDefault();
    // Exactly one match is what a scan produces, and it is taken without asking. Several means the
    // operator typed something ambiguous, so the first is offered rather than chosen for them.
    chooseProduct(matches[0]);
  };

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>Counter sale</h1>
        <p>Nothing is billed and no stock moves until this sale is posted. GST and the invoice number are issued by the Store Service at that moment.</p>
      </div>
      <Link className="button button--secondary" to="/app/sales">Back to Sales</Link>
    </header>

    <section className="pos" aria-labelledby="pos-title">
      <h2 id="pos-title" className="sr-only">Point of sale</h2>

      <form className="pos-entry" onSubmit={submitLine} aria-label="Add a line">
        <div className="field pos-entry__search">
          <label htmlFor="pos-search">Product or barcode</label>
          <input
            id="pos-search"
            ref={searchBox}
            autoFocus
            autoComplete="off"
            value={entry.search}
            onKeyDown={searchKeyDown}
            onChange={(event) => setEntry({ ...EMPTY_ENTRY, search: event.target.value, basis: entry.basis })}
            placeholder="Type a name, or scan"
          />
          {entry.search.trim().length >= 2 && !entry.productId && <ul className="pos-results" role="listbox" aria-label="Matching products">
            {products.isPending ? <li className="pos-results__note">Searching…</li>
              : products.isError ? <li className="pos-results__note">Products could not be searched.</li>
              : products.data.length === 0 ? <li className="pos-results__note">Nothing matches that.</li>
              : products.data.slice(0, 8).map((match) => <li key={match.id}>
                  <button type="button" onClick={() => chooseProduct(match)}>{match.displayName}</button>
                </li>)}
          </ul>}
        </div>

        <div className="field">
          <label htmlFor="pos-pack">Pack</label>
          <select id="pos-pack" value={entry.packId} disabled={!entry.productId} onChange={(event) => setEntry({ ...entry, packId: event.target.value, batchId: "" })}>
            <option value="">{entry.productId ? (product.isPending ? "Loading…" : "Choose a pack") : "Find a product first"}</option>
            {packs.map((pack) => <option key={pack.id} value={pack.id}>{packLabel(pack)}</option>)}
          </select>
        </div>

        <div className="field">
          <label htmlFor="pos-batch">Batch</label>
          <select id="pos-batch" value={entry.batchId} disabled={!entry.packId} onChange={(event) => setEntry({ ...entry, batchId: event.target.value })}>
            <option value="">{entry.packId ? (batches.isPending ? "Loading…" : "Choose a batch") : "Choose a pack first"}</option>
            {(batches.data ?? []).map((batch) => <option key={batch.id} value={batch.id}>{batchLabel(batch)}</option>)}
          </select>
          {selectedBatch && <small className={selectedBatch.expired ? "pos-warning" : undefined}>{batchNote(selectedBatch)}</small>}
        </div>

        <fieldset className="field pos-entry__basis">
          <legend>Sold as</legend>
          <div className="choice-row">
            <label><input type="radio" name="pos-basis" value="pack" checked={entry.basis === "pack"} onChange={() => setEntry({ ...entry, basis: "pack" })} /> Whole {selectedPack?.displayLabel ?? "packs"}</label>
            <label><input type="radio" name="pos-basis" value="base_unit" checked={entry.basis === "base_unit"} onChange={() => setEntry({ ...entry, basis: "base_unit" })} /> Loose {baseUnitName}</label>
          </div>
          <small>{entry.basis === "pack" ? "A whole-pack count, priced per pack." : "A count of individual units taken out of the pack, priced per unit."}</small>
        </fieldset>

        <div className="field">
          <label htmlFor="pos-quantity">Quantity ({unitLabel})</label>
          <input id="pos-quantity" className="numeric" inputMode="numeric" value={entry.quantity} onChange={(event) => setEntry({ ...entry, quantity: event.target.value })} />
        </div>

        <div className="field">
          <label htmlFor="pos-rate">Price per {unitLabel} (₹)</label>
          <input id="pos-rate" className="numeric" inputMode="decimal" value={entry.rate} onChange={(event) => setEntry({ ...entry, rate: event.target.value })} />
          {selectedBatch?.mrpPaise != null && <small>Printed MRP {paiseToAmountText(selectedBatch.mrpPaise)} a {selectedPack?.displayLabel ?? "pack"}, inclusive of GST.</small>}
          {selectedBatch != null && selectedBatch.mrpPaise == null && <small>No MRP is recorded for this batch, so no printed ceiling applies.</small>}
        </div>

        <div className="pos-entry__submit">
          <button className="button button--primary" type="submit" disabled={addLine.isPending}>{addLine.isPending ? "Adding…" : "Add line"}</button>
        </div>
      </form>

      {notice && <div className="inline-notice inline-notice--error" role="alert">{notice}</div>}

      <div className="pos-lines">
        {sale.lines.length === 0
          ? <div className="empty-state"><h3>Nothing on this bill yet</h3><p>Find a product above to add the first line.</p></div>
          : <div className="table-scroll"><table className="data-table">
              <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Batch</th><th scope="col" className="numeric">Quantity</th><th scope="col" className="numeric">Rate</th><th scope="col" className="numeric">Value</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead>
              <tbody>{sale.lines.map((line) => <tr key={line.id}>
                <td data-label="#">{line.lineNumber}</td>
                <td data-label="Item">{line.productDisplayName ?? line.currentProductDisplayName ?? line.productId}</td>
                <td data-label="Batch">{line.batchNumber ?? line.currentBatchNumber ?? <span className="row-subtext">Not found</span>}</td>
                <td data-label="Quantity" className="numeric">{quantityText(line)}</td>
                <td data-label="Rate" className="numeric">{paiseToAmountText(line.sellingRatePaise)}</td>
                <td data-label="Value" className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
                <td data-label="Actions"><button className="button button--secondary" type="button" onClick={() => removeLine.mutate(line.id)} disabled={removeLine.isPending}>Remove</button></td>
              </tr>)}</tbody>
            </table></div>}
      </div>

      <aside className="pos-summary" aria-label="Bill total">
        <p className="pos-summary__count">{sale.lines.length} {sale.lines.length === 1 ? "line" : "lines"}</p>
        {quote.isPending && sale.lines.length > 0 ? <p className="pos-summary__pending" role="status">Working out the total…</p>
          : quote.isError ? <div className="inline-notice inline-notice--error" role="alert">{quoteProblem(quote.error)}</div>
          : quote.data ? <dl className="totals-grid">
              <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(quote.data.taxableValuePaise)}</dd></div>
              {quote.data.taxTreatment === "intra_state"
                ? <><div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(quote.data.cgstPaise)}</dd></div>
                    <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(quote.data.sgstPaise)}</dd></div></>
                : <div><dt>IGST</dt><dd className="numeric">{paiseToAmountText(quote.data.igstPaise)}</dd></div>}
              {quote.data.cessPaise > 0 && <div><dt>Cess</dt><dd className="numeric">{paiseToAmountText(quote.data.cessPaise)}</dd></div>}
              <div className="totals-grand"><dt>Amount payable</dt><dd className="numeric">{paiseToAmountText(quote.data.grandTotalPaise)}</dd></div>
            </dl>
          : null}

        <div className="field">
          <label htmlFor="pos-tender-method">Paid by</label>
          <select id="pos-tender-method" value={tenderMethod} onChange={(event) => setTenderMethod(event.target.value as TenderMethod)}>
            {(Object.keys(TENDER_LABELS) as TenderMethod[]).map((method) => <option key={method} value={method}>{TENDER_LABELS[method]}</option>)}
          </select>
        </div>
        {tenderMethod !== "cash" && <div className="field">
          <label htmlFor="pos-tender-reference">Reference</label>
          <input id="pos-tender-reference" value={tenderReference} onChange={(event) => setTenderReference(event.target.value)} placeholder="Approval or UPI reference" />
        </div>}

        <button className="button button--primary button--full" type="button" onClick={submitPost} disabled={post.isPending || sale.lines.length === 0 || !quote.data}>
          {post.isPending ? "Posting…" : quote.data ? `Take ${paiseToAmountText(quote.data.grandTotalPaise)} and post` : "Post"}
        </button>
        <p className="pos-summary__note">Posting issues the invoice number, charges the GST and takes the stock out. It cannot be undone.</p>
      </aside>

      <CustomerPanel sale={sale} onSaved={refresh} storeName={store.data?.displayName ?? null} />
    </section>
  </>;
}

/**
 * Naming a customer, out of the way of billing.
 *
 * Most counter sales are walk-ins, so this sits below the bill rather than in front of it. A name
 * typed here is invoice text and nothing more; only a real party row carries identity.
 */
function CustomerPanel({ sale, onSaved, storeName }: { sale: SaleDetail; onSaved: () => void; storeName: string | null }) {
  const [open, setOpen] = useState(false);
  const [customerPartyId, setCustomerPartyId] = useState(sale.customerPartyId ?? "");
  const [customerNameText, setCustomerNameText] = useState(sale.customerNameText ?? "");
  const [notice, setNotice] = useState<string | null>(null);

  const customers = useQuery({
    queryKey: ["parties", "customers", "active"],
    queryFn: () => listParties("", "active", "customer"),
    enabled: open,
    staleTime: 30_000,
    retry: false
  });

  const save = useMutation({
    mutationFn: () => updateSaleDraft(sale.id, sale.revision, {
      customerPartyId: customerPartyId || null,
      customerNameText: customerNameText.trim() || null,
      businessDate: sale.businessDate
    }),
    onSuccess: () => { setNotice(null); setOpen(false); onSaved(); },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "The customer could not be recorded."))
  });

  const current = sale.customerDisplayName ?? sale.customerNameText ?? "Walk-in";
  return <section className="pos-customer" aria-labelledby="pos-customer-title">
    <h3 id="pos-customer-title">Customer</h3>
    <p className="pos-customer__current">{current}{storeName ? ` · billed at ${storeName}` : ""}</p>
    {open
      ? <div className="master-form">
          <div className="field">
            <label htmlFor="pos-customer-party">Registered customer</label>
            <select id="pos-customer-party" value={customerPartyId} onChange={(event) => setCustomerPartyId(event.target.value)} disabled={!customers.data}>
              <option value="">{customers.data ? "Walk-in (no party)" : "Loading…"}</option>
              {(customers.data ?? []).map((party: Party) => <option key={party.id} value={party.id}>{party.displayName}</option>)}
            </select>
            <small>Only a party holding an active customer role can be billed.</small>
          </div>
          <div className="field">
            <label htmlFor="pos-customer-name">Name on the bill</label>
            <input id="pos-customer-name" value={customerNameText} onChange={(event) => setCustomerNameText(event.target.value)} />
            <small>Printed on the invoice for a walk-in who asks for a name. It creates no customer record.</small>
          </div>
          {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{notice}</div>}
          <div className="form-actions">
            <button className="button button--secondary" type="button" onClick={() => { setOpen(false); setNotice(null); }}>Cancel</button>
            <button className="button button--primary" type="button" onClick={() => save.mutate()} disabled={save.isPending}>{save.isPending ? "Saving…" : "Save customer"}</button>
          </div>
        </div>
      : <button className="button button--secondary" type="button" onClick={() => setOpen(true)}>Change customer</button>}
  </section>;
}

// ---------------------------------------------------------------------------------------------
// Posted invoice
// ---------------------------------------------------------------------------------------------

function PostedInvoice({ sale }: { sale: SaleDetail }) {
  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{sale.documentNumber}</h1>
        <p>Posted {sale.postedAtUtc?.slice(0, 10)} · {sale.customerDisplayName ?? sale.customerNameText ?? "Walk-in"}</p>
      </div>
      <div className="page-header__actions">
        <Link className="button button--secondary" to={`/app/sales/${sale.id}/return`}>Return Items</Link>
        <Link className="button button--secondary" to="/app/sales">Back to Sales</Link>
      </div>
    </header>

    <section className="master-panel" aria-labelledby="sale-invoice-title">
      <h2 id="sale-invoice-title">Invoice</h2>
      <dl className="detail-grid">
        <div><dt>Invoice number</dt><dd>{sale.documentNumber}</dd></div>
        <div><dt>Business date</dt><dd>{sale.businessDate}</dd></div>
        <div><dt>Financial year</dt><dd>{sale.financialYear}</dd></div>
        <div><dt>Tax treatment</dt><dd>{sale.taxTreatment ? TREATMENT_LABELS[sale.taxTreatment] : "—"}</dd></div>
        <div><dt>Store GSTIN</dt><dd>{sale.storeNormalizedGstin ?? "Not registered"}</dd></div>
        <div><dt>Customer GSTIN</dt><dd>{sale.customerNormalizedGstin ?? "—"}</dd></div>
      </dl>

      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Batch</th><th scope="col">HSN</th><th scope="col" className="numeric">Quantity</th><th scope="col" className="numeric">Rate</th><th scope="col" className="numeric">Taxable</th><th scope="col" className="numeric">GST</th><th scope="col" className="numeric">Total</th></tr></thead>
        <tbody>{sale.lines.map((line) => <tr key={line.id}>
          <td data-label="#">{line.lineNumber}</td>
          <td data-label="Item">{line.productDisplayName}<br /><small className="row-subtext">{line.packDisplayLabel}{line.taxTreatmentKind ? ` · ${TAX_KIND_LABELS[line.taxTreatmentKind]}` : ""}</small></td>
          <td data-label="Batch">{line.batchNumber}{line.batchExpiresOn ? <><br /><small className="row-subtext">Expires {line.batchExpiresOn}</small></> : null}</td>
          <td data-label="HSN">{line.hsnCode ?? "—"}</td>
          <td data-label="Quantity" className="numeric">{quantityText(line)}</td>
          <td data-label="Rate" className="numeric">{paiseToAmountText(line.sellingRatePaise)}</td>
          <td data-label="Taxable" className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
          <td data-label="GST" className="numeric">{paiseToAmountText(line.cgstPaise + line.sgstPaise + line.igstPaise + line.cessPaise)}</td>
          <td data-label="Total" className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
        </tr>)}</tbody>
      </table></div>

      <dl className="totals-grid">
        <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(sale.taxableValuePaise)}</dd></div>
        {sale.taxTreatment === "intra_state"
          ? <><div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(sale.cgstPaise)}</dd></div>
              <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(sale.sgstPaise)}</dd></div></>
          : <div><dt>IGST</dt><dd className="numeric">{paiseToAmountText(sale.igstPaise)}</dd></div>}
        {sale.cessPaise > 0 && <div><dt>Cess</dt><dd className="numeric">{paiseToAmountText(sale.cessPaise)}</dd></div>}
        <div className="totals-grand"><dt>Invoice total</dt><dd className="numeric">{paiseToAmountText(sale.grandTotalPaise)}</dd></div>
      </dl>

      {sale.tenders.length > 0 && <p className="pos-summary__note">Paid by {TENDER_LABELS[sale.tenders[0].method]} · {paiseToAmountText(sale.tenders[0].amountPaise)}{sale.tenders[0].referenceText ? ` · ${sale.tenders[0].referenceText}` : ""}</p>}

      {sale.lines.some((line) => line.priceControlStatus === "controlled") && <p className="pos-summary__note">
        A line on this invoice is a price-controlled medicine; the notified ceiling in force on the sale date was checked and recorded against it.
      </p>}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/**
 * A quantity always says what it is counted in.
 *
 * "3" alone is the ambiguity the dual-basis model exists to remove: three strips and three tablets
 * are different sales, and a bill that shows neither is unreadable.
 */
function quantityText(line: SaleLine): string {
  const unit = line.quantityBasis === "pack"
    ? (line.packDisplayLabel ?? line.currentPackDisplayLabel ?? "pack")
    : (line.baseUnitLabel ?? line.currentBaseUnitLabel ?? "unit");
  const count = line.quantityBasis === "pack" ? (line.quantityPacks ?? line.quantityAtoms) : line.quantityAtoms;
  return `${count} ${unit}${count === 1 ? "" : "s"}`;
}

function packLabel(pack: ProductPack): string {
  return pack.displayLabel ?? `Pack of ${pack.baseQuantityAtoms}`;
}

function batchLabel(batch: SellableBatch): string {
  const parts = [batch.batchNumber];
  if (batch.expiresOn) parts.push(`exp ${batch.expiresOn}`);
  parts.push(batch.expired ? "EXPIRED" : `${batch.availableAtoms} on hand`);
  return parts.join(" · ");
}

function batchNote(batch: SellableBatch): string {
  if (batch.expired) return "This batch has expired and cannot be sold.";
  if (batch.availableAtoms <= 0) return "Nothing of this batch is left in stock.";
  return `${batch.availableAtoms} units on hand, by the ledger.`;
}

function quoteProblem(error: unknown): string {
  return error instanceof LocalServiceError
    ? `${error.message} The bill cannot be totalled until that line is corrected.`
    : "The total could not be worked out.";
}

/**
 * Reports a validation failure and puts the cursor on the field that caused it.
 *
 * At a counter this matters more than anywhere else: the operator is not looking at the screen while
 * typing, and an error that leaves the focus where it was costs a whole re-entry.
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

function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }): ReactNode { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
