import { useEffect, useMemo, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams } from "react-router";
import type {
  Party,
  Product,
  ProductPack,
  QuantityBasis,
  RecipientRequirement,
  RecipientRequirementReason,
  ReferenceMasterResponse,
  SaleAddressInput,
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

/**
 * Why this invoice must carry the customer's particulars, in the counter's words. One line each for
 * CGST Rule 46(d), 46(e) and 46(f); the rule numbers stay out of the cashier's way.
 */
const REASON_TEXT: Record<RecipientRequirementReason, string> = {
  registered_recipient: "The customer is GST-registered, so their name, address and GSTIN go on this invoice.",
  taxable_value_threshold: "The taxable value is ₹50,000 or more, so the customer's name, address and State go on this invoice.",
  recipient_requested: "The customer asked for their details on the invoice."
};

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
  const [customerOpen, setCustomerOpen] = useState(false);
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
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : postingProblem(caught))
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

  // What posting will ask of the customer, from the same Store Service code posting runs. It changes
  // as lines are added and removed, because the ₹50,000 line is judged on the bill as it stands.
  const requirement = quote.data?.recipientParticulars;
  const particularsMissing = Boolean(requirement && requirement.missing.length > 0);
  // Phase 1L-A4. Notification No. 14/2020-CT as the Store Service judged it for this bill: null when
  // it cannot apply. Under "required" the invoice records the payment cross-reference, so a card or
  // UPI payment needs its transaction reference. The Store Service decides; this only says so first.
  const dynamicQr = quote.data?.dynamicQrApplicability ?? null;
  const dynamicQrUnknown = dynamicQr === "unknown";
  const referenceRequired = dynamicQr === "required" && tenderMethod !== "cash";
  const mixedSupply = quote.data?.registeredRecipientMixedSupply === true;

  const openCustomerDetails = () => {
    setCustomerOpen(true);
    // Not every environment implements scrolling an element into view; opening the form must not
    // depend on it.
    requestAnimationFrame(() => document.getElementById("pos-customer-title")?.scrollIntoView?.({ block: "start", behavior: "smooth" }));
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
    if (particularsMissing) {
      setNotice("Add the customer's details to this invoice before posting.");
      openCustomerDetails();
      return;
    }
    if (referenceRequired && !tenderReference.trim()) {
      setNotice("Enter the card or UPI transaction reference before posting.");
      document.getElementById("pos-tender-reference")?.focus();
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
          : quote.data && quote.data.sellerGstRegistrationStatus === "unregistered" ? <dl className="totals-grid">
              {/* Not a GST document: the pharmacy is recorded as unregistered, so no tax is charged. */}
              <div><dt>Value of goods</dt><dd className="numeric">{paiseToAmountText(quote.data.taxableValuePaise)}</dd></div>
              <div><dt>GST</dt><dd>Not charged</dd></div>
              <div className="totals-grand"><dt>Amount payable</dt><dd className="numeric">{paiseToAmountText(quote.data.grandTotalPaise)}</dd></div>
            </dl>
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

        {requirement?.required && particularsMissing && <div className="panel-callout panel-callout--blocking pos-recipient-callout" role="status">
          <strong>Customer details needed before posting</strong>
          <ul>{requirement.missing.map((fact) => <li key={fact.field}>{fact.message}</li>)}</ul>
          <button className="button button--secondary" type="button" onClick={openCustomerDetails}>Add customer details</button>
        </div>}

        {dynamicQrUnknown && <div className="panel-callout panel-callout--blocking" role="status">
          <strong>Dynamic QR requirement not recorded</strong>
          <small>This pharmacy has not recorded in Store Profile whether the Dynamic QR requirement for GST invoices to unregistered customers applies to it. This sale cannot be posted until the owner records it.</small>
        </div>}

        {mixedSupply && <div className="panel-callout panel-callout--blocking" role="status">
          <strong>Taxable and untaxed items for a GST-registered customer</strong>
          <small>AUSHADHARTH cannot issue one document for this mix to a GST-registered customer. Bill the taxable and untaxed items separately.</small>
        </div>}

        <div className="field">
          <label htmlFor="pos-tender-method">Paid by</label>
          <select id="pos-tender-method" value={tenderMethod} onChange={(event) => setTenderMethod(event.target.value as TenderMethod)}>
            {(Object.keys(TENDER_LABELS) as TenderMethod[]).map((method) => <option key={method} value={method}>{TENDER_LABELS[method]}</option>)}
          </select>
        </div>
        {tenderMethod !== "cash" && <div className="field">
          <label htmlFor="pos-tender-reference">{referenceRequired ? "Transaction reference" : "Reference"}{referenceRequired && <span aria-hidden="true"> *</span>}</label>
          <input id="pos-tender-reference" value={tenderReference} onChange={(event) => setTenderReference(event.target.value)} placeholder="Approval or UPI reference" aria-required={referenceRequired} aria-describedby={referenceRequired ? "pos-tender-reference-help" : undefined} />
          {referenceRequired && <small id="pos-tender-reference-help">Required. It is recorded on the invoice with the amount, mode and time as the payment details.</small>}
        </div>}

        <button className="button button--primary button--full" type="button" onClick={submitPost} disabled={post.isPending || sale.lines.length === 0 || !quote.data || particularsMissing || dynamicQrUnknown || mixedSupply}>
          {post.isPending ? "Posting…" : quote.data ? `Take ${paiseToAmountText(quote.data.grandTotalPaise)} and post` : "Post"}
        </button>
        <p className="pos-summary__note">{quote.data?.sellerGstRegistrationStatus === "unregistered"
          ? "Posting issues the cash memo number and takes the stock out. This pharmacy is recorded as not GST-registered, so no GST is charged. It cannot be undone."
          : "Posting issues the invoice number, charges the GST and takes the stock out. It cannot be undone."}</p>
      </aside>

      <CustomerPanel
        key={sale.id}
        sale={sale}
        requirement={requirement ?? null}
        sellerRegistered={quote.data ? quote.data.sellerGstRegistrationStatus === "registered" : null}
        open={customerOpen}
        onOpenChange={setCustomerOpen}
        onSaved={refresh}
        storeName={store.data?.displayName ?? null}
      />
    </section>
  </>;
}

type AddressForm = { line1: string; line2: string; city: string; postalCode: string; stateId: string };

function addressForm(line1: string | null, line2: string | null, city: string | null, postalCode: string | null, stateId: string | null): AddressForm {
  return { line1: line1 ?? "", line2: line2 ?? "", city: city ?? "", postalCode: postalCode ?? "", stateId: stateId ?? "" };
}

/** Blank fields are sent as nothing, and an untouched form as no address at all. */
function addressInput(form: AddressForm): SaleAddressInput | null {
  const input = {
    line1: form.line1.trim() || null,
    line2: form.line2.trim() || null,
    city: form.city.trim() || null,
    postalCode: form.postalCode.trim() || null,
    stateId: form.stateId || null
  };
  return Object.values(input).some((value) => value !== null) ? input : null;
}

function addressText(parts: Array<string | null | undefined>): string {
  return parts.map((part) => part?.trim()).filter(Boolean).join(", ");
}

function stateOptionText(state: ReferenceMasterResponse): string {
  const attributes = state.attributes as Record<string, unknown>;
  return `${String(attributes.displayName)} (${String(attributes.stateCode)})`;
}

/** The same five fields for the customer's address and for a delivery address. */
function AddressFields({ prefix, legend, value, onChange, states }: {
  prefix: string;
  legend: string;
  value: AddressForm;
  onChange: (next: AddressForm) => void;
  states: ReferenceMasterResponse[] | undefined;
}) {
  const selectable = (states ?? []).filter((state) => state.status === "active" || state.id === value.stateId);
  return <fieldset className="pos-address catalog-span">
    <legend>{legend}</legend>
    <div className="field pos-address__wide">
      <label htmlFor={`${prefix}-line1`}>Address line 1</label>
      <input id={`${prefix}-line1`} maxLength={200} autoComplete="off" value={value.line1} onChange={(event) => onChange({ ...value, line1: event.target.value })} />
    </div>
    <div className="field pos-address__wide">
      <label htmlFor={`${prefix}-line2`}>Address line 2 <span className="row-subtext">(optional)</span></label>
      <input id={`${prefix}-line2`} maxLength={200} autoComplete="off" value={value.line2} onChange={(event) => onChange({ ...value, line2: event.target.value })} />
    </div>
    <div className="field">
      <label htmlFor={`${prefix}-city`}>City <span className="row-subtext">(optional)</span></label>
      <input id={`${prefix}-city`} maxLength={100} autoComplete="off" value={value.city} onChange={(event) => onChange({ ...value, city: event.target.value })} />
    </div>
    <div className="field">
      <label htmlFor={`${prefix}-postal`}>PIN code <span className="row-subtext">(optional)</span></label>
      <input id={`${prefix}-postal`} maxLength={16} inputMode="numeric" autoComplete="off" value={value.postalCode} onChange={(event) => onChange({ ...value, postalCode: event.target.value })} />
    </div>
    <div className="field pos-address__wide">
      <label htmlFor={`${prefix}-state`}>State</label>
      <select id={`${prefix}-state`} value={value.stateId} disabled={!states} onChange={(event) => onChange({ ...value, stateId: event.target.value })}>
        <option value="">{states ? "Choose a State" : "Loading…"}</option>
        {selectable.map((state) => <option key={state.id} value={state.id}>{stateOptionText(state)}</option>)}
      </select>
    </div>
  </fieldset>;
}

/**
 * Naming a customer, out of the way of billing.
 *
 * Most counter sales are walk-ins, so this sits below the bill rather than in front of it. A name
 * typed here is invoice text and nothing more; only a real party row carries identity.
 */
function CustomerPanel({ sale, requirement, sellerRegistered, open, onOpenChange, onSaved, storeName }: {
  sale: SaleDetail;
  requirement: RecipientRequirement | null;
  /**
   * Whether this pharmacy issues GST tax invoices, from the quote; null until a quote is available.
   * Rule 46 — and so every recipient particular below — belongs to a registered seller's tax invoice.
   */
  sellerRegistered: boolean | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved: () => void;
  storeName: string | null;
}) {
  const [customerPartyId, setCustomerPartyId] = useState(sale.customerPartyId ?? "");
  const [customerNameText, setCustomerNameText] = useState(sale.customerNameText ?? "");
  const [requested, setRequested] = useState(sale.recipientParticularsRequested ?? false);
  const [address, setAddress] = useState<AddressForm>(() => addressForm(
    sale.recipientAddressLine1, sale.recipientAddressLine2, sale.recipientCity, sale.recipientPostalCode, sale.recipientStateId
  ));
  const [deliverySame, setDeliverySame] = useState(sale.deliverySameAsRecipient ?? true);
  const [delivery, setDelivery] = useState<AddressForm>(() => addressForm(
    sale.deliveryAddressLine1, sale.deliveryAddressLine2, sale.deliveryCity, sale.deliveryPostalCode, sale.deliveryStateId
  ));
  const [notice, setNotice] = useState<string | null>(null);

  // Also loaded when the draft already names a customer: a draft's `customerDisplayName` is a posting
  // snapshot and stays empty until then, so the name the counter reads has to come from the record.
  const customers = useQuery({
    queryKey: ["parties", "customers", "active"],
    queryFn: () => listParties("", "active", "customer"),
    enabled: open || Boolean(sale.customerPartyId),
    staleTime: 30_000,
    retry: false
  });
  const states = useQuery({
    queryKey: ["reference", "state-codes", "all"],
    queryFn: () => listReferences("state-codes", "", "all"),
    enabled: open,
    staleTime: 300_000,
    retry: false
  });

  // Which Rule 46 path this form is on. The saved draft's requirement comes from the Store Service;
  // a customer chosen in the form but not yet saved is judged from their own record, so the right
  // fields appear before saving rather than after a round trip. Posting decides either way.
  const savedCustomer = customerPartyId === (sale.customerPartyId ?? "");
  const selectedParty = (customers.data ?? []).find((party) => party.id === customerPartyId) ?? null;
  const registeredPath = customerPartyId !== "" && (selectedParty
    ? selectedParty.gstRegistrationStatus === "registered"
    : savedCustomer && Boolean(requirement?.reasons.includes("registered_recipient")));
  const thresholdReached = requirement !== null && requirement.taxableSupplyValuePaise >= requirement.thresholdPaise;
  // An unregistered seller issues a retail cash memo, not a tax invoice: no Rule 46 reason applies.
  const gstInvoice = sellerRegistered !== false;
  const reasons: RecipientRequirementReason[] = gstInvoice ? [
    ...(registeredPath ? ["registered_recipient" as const] : thresholdReached ? ["taxable_value_threshold" as const] : []),
    ...(requested ? ["recipient_requested" as const] : [])
  ] : [];
  const statutory = reasons.length > 0;
  const walkIn = customerPartyId === "";
  const partyMissing = savedCustomer && !walkIn
    ? (requirement?.missing ?? []).filter((fact) => fact.field.startsWith("customer."))
    : [];

  const save = useMutation({
    mutationFn: () => updateSaleDraft(sale.id, sale.revision, {
      customerPartyId: customerPartyId || null,
      customerNameText: customerNameText.trim() || null,
      businessDate: sale.businessDate,
      // Never recorded as a GST-invoice request where no GST invoice exists to honour it.
      recipientParticularsRequested: gstInvoice ? requested : false,
      // A named customer's address is read from their record at posting and is never sent.
      recipientAddress: walkIn ? addressInput(address) : null,
      // Rule 46(d) asks for no address of delivery, so a registered customer carries none.
      deliverySameAsRecipient: registeredPath ? true : deliverySame,
      deliveryAddress: registeredPath || deliverySame ? null : addressInput(delivery)
    }),
    onSuccess: () => { setNotice(null); onOpenChange(false); onSaved(); },
    onError: (caught) => setNotice(isStale(caught) ? STALE_DOCUMENT_MESSAGE : messageFor(caught, "The customer could not be recorded."))
  });

  const namedParty = (customers.data ?? []).find((party) => party.id === sale.customerPartyId) ?? null;
  const current = sale.customerDisplayName
    ?? namedParty?.displayName
    ?? sale.customerNameText
    ?? (sale.customerPartyId ? "Customer on record" : "Walk-in");
  const recordedAddress = sale.customerPartyId ? "" : addressText([sale.recipientAddressLine1, sale.recipientAddressLine2, sale.recipientCity, sale.recipientPostalCode]);
  const recordedDelivery = sale.deliverySameAsRecipient === false
    ? addressText([sale.deliveryAddressLine1, sale.deliveryAddressLine2, sale.deliveryCity, sale.deliveryPostalCode])
    : "";
  const savedMissing = Boolean(requirement && requirement.missing.length > 0);

  return <section className="pos-customer" aria-labelledby="pos-customer-title">
    <h3 id="pos-customer-title">Customer</h3>
    <p className="pos-customer__current">{current}{storeName ? ` · billed at ${storeName}` : ""}</p>
    {!open && requirement?.required && <ul className="pos-customer__reasons">
      {requirement.reasons.map((reason) => <li key={reason}>{REASON_TEXT[reason]}</li>)}
    </ul>}
    {!open && recordedAddress && <p className="pos-customer__address">Address: {recordedAddress}</p>}
    {!open && recordedDelivery && <p className="pos-customer__address">Delivered to: {recordedDelivery}</p>}
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
            <input id="pos-customer-name" maxLength={200} value={customerNameText} onChange={(event) => setCustomerNameText(event.target.value)} />
            <small>Printed on the invoice for a walk-in who asks for a name. It creates no customer record.</small>
          </div>
          {gstInvoice
            ? <label className="check-field catalog-span">
                <input id="pos-recipient-requested" type="checkbox" checked={requested} onChange={(event) => setRequested(event.target.checked)} />
                <span>Customer asked for their details on the invoice<small>Their name and address are then recorded on the invoice at any amount.</small></span>
              </label>
            : <p className="panel-note catalog-span">This pharmacy is recorded as not GST-registered, so this sale is a retail cash memo, not a GST tax invoice. No GST-invoice particulars are asked for.</p>}

          {statutory && <div className="pos-recipient catalog-span">
            <ul className="pos-customer__reasons">{reasons.map((reason) => <li key={reason}>{REASON_TEXT[reason]}</li>)}</ul>
            {walkIn
              ? <AddressFields prefix="pos-recipient" legend="Customer's address" value={address} onChange={setAddress} states={states.data} />
              : <div className="pos-recipient__party">
                  <p>The address is taken from this customer's record in Parties when the sale is posted.</p>
                  {partyMissing.length > 0 && <ul className="pos-recipient__missing">{partyMissing.map((fact) => <li key={fact.field}>{fact.message}</li>)}</ul>}
                </div>}
            {!registeredPath && <>
              <label className="check-field">
                <input id="pos-delivery-same" type="checkbox" checked={deliverySame} onChange={(event) => setDeliverySame(event.target.checked)} />
                <span>Goods delivered to the same address</span>
              </label>
              {!deliverySame && <AddressFields prefix="pos-delivery" legend="Delivery address" value={delivery} onChange={setDelivery} states={states.data} />}
            </>}
            {states.isError && <div className="inline-notice inline-notice--error" role="alert">State names could not be loaded. Close and reopen this panel to try again.</div>}
          </div>}

          {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{notice}</div>}
          <div className="form-actions">
            <button className="button button--secondary" type="button" onClick={() => { onOpenChange(false); setNotice(null); }}>Cancel</button>
            <button className="button button--primary" type="button" onClick={() => save.mutate()} disabled={save.isPending}>{save.isPending ? "Saving…" : "Save customer"}</button>
          </div>
        </div>
      : <button className="button button--secondary" type="button" onClick={() => onOpenChange(true)}>{savedMissing ? "Add customer details" : "Change customer"}</button>}
  </section>;
}

// ---------------------------------------------------------------------------------------------
// Posted invoice
// ---------------------------------------------------------------------------------------------

function PostedInvoice({ sale }: { sale: SaleDetail }) {
  // Worded from what was frozen: the seller's status at posting AND the tax actually charged. A
  // product's GST classification is catalogue metadata and says nothing about whether GST was
  // charged. A Sale posted before Phase 1L-A3 that did record GST keeps showing it, as recorded.
  const taxCharged = sale.cgstPaise + sale.sgstPaise + sale.igstPaise + sale.cessPaise;
  const noGst = sale.storeGstRegistrationStatus === "unregistered" && taxCharged === 0;
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
      <h2 id="sale-invoice-title">{noGst ? "Retail cash memo" : "Invoice"}</h2>
      <dl className="detail-grid">
        <div><dt>{noGst ? "Memo number" : "Invoice number"}</dt><dd>{sale.documentNumber}</dd></div>
        <div><dt>Business date</dt><dd>{sale.businessDate}</dd></div>
        <div><dt>Financial year</dt><dd>{sale.financialYear}</dd></div>
        {noGst
          ? <div><dt>GST</dt><dd>Not charged: this pharmacy was not GST-registered when the sale was posted</dd></div>
          : <div><dt>Tax treatment</dt><dd>{sale.taxTreatment ? TREATMENT_LABELS[sale.taxTreatment] : "—"}</dd></div>}
        <div><dt>Store GSTIN</dt><dd>{sale.storeNormalizedGstin ?? "Not registered"}</dd></div>
        <div><dt>Customer GSTIN</dt><dd>{sale.customerNormalizedGstin ?? "—"}</dd></div>
        {sale.recipientAddressLine1 && <div><dt>Customer address</dt><dd>{addressText([sale.recipientAddressLine1, sale.recipientAddressLine2, sale.recipientCity, sale.recipientPostalCode, sale.recipientStateName])}</dd></div>}
        {sale.deliverySameAsRecipient === true && <div><dt>Delivery</dt><dd>Same as customer address</dd></div>}
        {sale.deliverySameAsRecipient === false && <div><dt>Delivery address</dt><dd>{addressText([sale.deliveryAddressLine1, sale.deliveryAddressLine2, sale.deliveryCity, sale.deliveryPostalCode, sale.deliveryStateName])}</dd></div>}
      </dl>

      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Batch</th><th scope="col">HSN</th><th scope="col" className="numeric">Quantity</th><th scope="col" className="numeric">Rate</th><th scope="col" className="numeric">{noGst ? "Value" : "Taxable"}</th>{!noGst && <th scope="col" className="numeric">GST</th>}<th scope="col" className="numeric">Total</th></tr></thead>
        <tbody>{sale.lines.map((line) => <tr key={line.id}>
          <td data-label="#">{line.lineNumber}</td>
          <td data-label="Item">{line.productDisplayName}<br /><small className="row-subtext">{line.packDisplayLabel}{line.taxTreatmentKind && !noGst ? ` · ${TAX_KIND_LABELS[line.taxTreatmentKind]}` : ""}</small></td>
          <td data-label="Batch">{line.batchNumber}{line.batchExpiresOn ? <><br /><small className="row-subtext">Expires {line.batchExpiresOn}</small></> : null}</td>
          <td data-label="HSN">{line.hsnCode ?? "—"}</td>
          <td data-label="Quantity" className="numeric">{quantityText(line)}</td>
          <td data-label="Rate" className="numeric">{paiseToAmountText(line.sellingRatePaise)}</td>
          <td data-label={noGst ? "Value" : "Taxable"} className="numeric">{paiseToAmountText(line.taxableValuePaise)}</td>
          {!noGst && <td data-label="GST" className="numeric">{paiseToAmountText(line.cgstPaise + line.sgstPaise + line.igstPaise + line.cessPaise)}</td>}
          <td data-label="Total" className="numeric">{paiseToAmountText(line.lineTotalPaise)}</td>
        </tr>)}</tbody>
      </table></div>

      {noGst
        ? <dl className="totals-grid">
            <div><dt>Value of goods</dt><dd className="numeric">{paiseToAmountText(sale.taxableValuePaise)}</dd></div>
            <div><dt>GST</dt><dd>Not charged</dd></div>
            <div className="totals-grand"><dt>Memo total</dt><dd className="numeric">{paiseToAmountText(sale.grandTotalPaise)}</dd></div>
          </dl>
        : <dl className="totals-grid">
            <div><dt>Taxable value</dt><dd className="numeric">{paiseToAmountText(sale.taxableValuePaise)}</dd></div>
            {sale.taxTreatment === "intra_state"
              ? <><div><dt>CGST</dt><dd className="numeric">{paiseToAmountText(sale.cgstPaise)}</dd></div>
                  <div><dt>SGST</dt><dd className="numeric">{paiseToAmountText(sale.sgstPaise)}</dd></div></>
              : <div><dt>IGST</dt><dd className="numeric">{paiseToAmountText(sale.igstPaise)}</dd></div>}
            {sale.cessPaise > 0 && <div><dt>Cess</dt><dd className="numeric">{paiseToAmountText(sale.cessPaise)}</dd></div>}
            <div className="totals-grand"><dt>Invoice total</dt><dd className="numeric">{paiseToAmountText(sale.grandTotalPaise)}</dd></div>
          </dl>}

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
  if (!(error instanceof LocalServiceError)) return "The total could not be worked out.";
  // Not a line's fault: whether GST applies at all is a Store fact only the owner can record.
  if (error.code === "store_gst_status_unresolved") return `${error.message} The bill cannot be totalled until then.`;
  return `${error.message} The bill cannot be totalled until that line is corrected.`;
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

/**
 * A posting refusal that names what is missing says so item by item. For these codes the issue
 * messages are written for the counter by the Store Service; every other refusal keeps its one
 * plain sentence.
 */
const ITEMISED_REFUSALS = new Set([
  "store_legal_profile_incomplete",
  "recipient_particulars_incomplete",
  "sale_compliance_incomplete"
]);

function postingProblem(caught: unknown): string {
  const message = messageFor(caught, "This sale could not be posted.");
  if (!(caught instanceof LocalServiceError) || !ITEMISED_REFUSALS.has(caught.code) || caught.issues.length === 0) return message;
  return `${message} ${caught.issues.map((issue) => issue.message).join(" ")}`;
}

function isStale(error: unknown): boolean {
  return error instanceof LocalServiceError && error.code === "revision_conflict";
}

function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }): ReactNode { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
