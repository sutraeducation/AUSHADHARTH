import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, Navigate, useParams } from "react-router";
import type { Batch, InventoryMovement, ProductDetail, StockBalance, StockStatus } from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import {
  atomsToQuantity,
  getProduct,
  listBatches,
  listProducts,
  paiseToRupees,
  quantityToAtoms
} from "../products/productApi";
import { listMovements, listStock, newIdempotencyKey, postMovement } from "./inventoryApi";
import { createStockDisposition } from "../returns/returnApi";
import {
  StockOperationDialog,
  StockOperationHistory,
  kindsForRole
} from "./StockOperations";
import { OPERATION_KIND_LABELS } from "./stockOperationApi";
import type { StockOperationKind } from "@aushadharth/contracts";

const SECTIONS = [
  { slug: "stock", title: "Stock Overview", description: "Derived balances by Product, Pack, Batch, and stock status. Only sellable stock can be billed." },
  { slug: "operations", title: "Stock Operations", description: "Counts, damage, expiry, quarantine and disposals the store has recorded." },
  { slug: "ledger", title: "Stock Ledger", description: "Every posted movement, newest first." }
] as const;

export function InventoryPage() {
  const { section } = useParams();
  const slug = section ?? "stock";
  const auth = useAuth();
  const canPost = auth.status?.user?.role === "owner_admin";
  const kinds = kindsForRole(auth.status?.user?.role);
  const [operation, setOperation] = useState<StockOperationKind | null>(null);
  const queryClient = useQueryClient();
  const [posting, setPosting] = useState(false);
  usePageTitle(SECTIONS.find((item) => item.slug === slug)?.title ?? "Inventory");
  if (!SECTIONS.some((item) => item.slug === slug)) return <Navigate to="/app/inventory/stock" replace />;
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["inventory", "stock"] });
    void queryClient.invalidateQueries({ queryKey: ["inventory", "movements"] });
    void queryClient.invalidateQueries({ queryKey: ["stock-operations"] });
  };
  return <>
    <header className="page-header"><div><p className="eyebrow">INVENTORY</p><h1>Inventory</h1><p>Quantity is derived from the movement ledger. No balance is ever stored.</p></div>
      <div className="page-header__actions">
        {kinds.map((kind) => <button key={kind} className="button button--secondary" type="button" onClick={() => setOperation(kind)}>{OPERATION_KIND_LABELS[kind]}</button>)}
        {canPost && <button className="button button--primary" type="button" onClick={() => setPosting(true)}>Post Opening Stock</button>}
        {kinds.length === 0 && <span className="read-only-note">Read-only access</span>}
      </div>
    </header>
    <nav className="inventory-tabs" aria-label="Inventory sections">{SECTIONS.map((item) => <NavLink key={item.slug} to={`/app/inventory/${item.slug}`} className={({ isActive }) => `inventory-tab ${isActive ? "inventory-tab--active" : ""}`}>{item.title}</NavLink>)}</nav>
    {slug === "stock" ? <StockOverview /> : slug === "operations" ? <StockOperationHistory /> : <StockLedger />}
    {operation && <StockOperationDialog kind={operation} onClose={() => { setOperation(null); refresh(); }} />}
    {posting && <PostMovementDialog onClose={() => setPosting(false)} onPosted={() => { setPosting(false); refresh(); }} />}
  </>;
}

function StockOverview() {
  const stock = useQuery({ queryKey: ["inventory", "stock"], queryFn: () => listStock(), retry: false });
  const products = useQuery({ queryKey: ["products", "", "all"], queryFn: () => listProducts("", "all"), retry: false });
  useExpireOnAuthError(stock.error);
  if (stock.isPending) return <Loading label="Loading stock balances…" />;
  if (stock.isError) return <QueryError label="Stock balances could not be loaded" onRetry={() => void stock.refetch()} />;
  if (stock.data.length === 0) return <Empty title="No stock recorded" text="Post opening stock to establish starting quantities." />;
  return <section className="master-panel" aria-labelledby="stock-title"><h2 id="stock-title" className="sr-only">Stock balances</h2>
    {products.isError && <InlineQueryError label="Product names could not be loaded." onRetry={() => void products.refetch()} />}
    <CustodySummary rows={stock.data} />
    <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Product</th><th scope="col">Pack</th><th scope="col">Batch</th><th scope="col">Status</th><th scope="col">Balance</th><th scope="col">Decision</th></tr></thead><tbody>{stock.data.map((row) => <BalanceRow key={`${row.productPackId}-${row.batchId ?? "none"}-${row.stockStatus}`} row={row} productName={productName(products.data, row.productId, products.isError)} />)}</tbody></table></div>
  </section>;
}

/**
 * What the store physically holds, split by what it may do with it.
 *
 * Sellable, quarantined and non-sellable are all goods in the building; only the first may be
 * billed. Showing the three without their total invites the reading that written-off stock has
 * gone somewhere, which is exactly the confusion Phase 1J exists to remove — goods leave only
 * when somebody records that they left.
 */
function CustodySummary({ rows }: { rows: StockBalance[] }) {
  const totals = { sellable: 0, quarantined: 0, non_sellable: 0 };
  for (const row of rows) totals[row.stockStatus] += row.balanceAtoms;
  const custody = totals.sellable + totals.quarantined + totals.non_sellable;
  return <dl className="detail-grid" aria-label="Stock in the building">
    <div><dt>Sellable</dt><dd>{totals.sellable}</dd></div>
    <div><dt>Quarantined</dt><dd>{totals.quarantined}</dd></div>
    <div><dt>Not sellable</dt><dd>{totals.non_sellable}</dd></div>
    <div><dt>In the building</dt><dd><strong>{custody}</strong></dd></div>
  </dl>;
}

function BalanceRow({ row, productName }: { row: StockBalance; productName: string }) {
  const auth = useAuth();
  const canDecide = auth.status?.user?.role === "owner_admin" || auth.status?.user?.role === "pharmacist";
  const [deciding, setDeciding] = useState(false);
  const detail = useQuery({ queryKey: ["product", row.productId], queryFn: () => getProduct(row.productId), retry: false });
  const pack = detail.data?.packs.find((item) => item.id === row.productPackId);
  const batches = useQuery({ queryKey: ["pack-batches", row.productPackId], queryFn: () => listBatches(row.productPackId), enabled: Boolean(row.batchId), retry: false });
  const batch = row.batchId ? batches.data?.find((item) => item.id === row.batchId) : undefined;
  const scale = detail.data?.quantityScale ?? 0;
  return <><tr><td data-label="Product">{productName}</td><td data-label="Pack">{pack?.displayLabel || pack?.skuCode || (detail.isError ? "Pack unavailable" : detail.isPending ? "Loading…" : "—")}</td><td data-label="Batch">{row.batchId ? batch?.batchNumber ?? (batches.isError ? "Batch unavailable" : "Loading…") : "No batch"}</td><td data-label="Status"><span className={`stock-status stock-status--${row.stockStatus}`}>{STOCK_STATUS_LABELS[row.stockStatus]}</span></td><td data-label="Balance">{atomsToQuantity(row.balanceAtoms, scale)}</td><td data-label="Decision">{decisionCell(row, canDecide, () => setDeciding(true))}</td></tr>
    {deciding && row.batchId && <QuarantineDecisionDialog row={row} batchId={row.batchId} productName={productName} packLabel={pack?.displayLabel || pack?.skuCode || "this pack"} batchNumber={batch?.batchNumber ?? "this batch"} expiresOn={batch?.expiresOn ?? null} packAtoms={pack?.baseQuantityAtoms ?? 0} scale={scale} onClose={() => setDeciding(false)} />}</>;
}

/**
 * Quarantined goods are waiting on a person, and the table has to say so.
 *
 * A quarantined line with no visible next step reads as stock that has simply vanished from sale,
 * which is how quarantine turns into a shelf nobody ever clears. A cashier sees who the decision is
 * waiting for rather than a button that would only be refused.
 */
function decisionCell(row: StockBalance, canDecide: boolean, onDecide: () => void) {
  if (row.stockStatus !== "quarantined") return <span className="row-subtext">—</span>;
  if (!row.batchId) return <span className="row-subtext">No batch to decide</span>;
  if (!canDecide) return <span className="row-subtext">Waiting on a pharmacist</span>;
  return <button className="button button--secondary" type="button" onClick={onDecide}>Decide…</button>;
}

function StockLedger() {
  const movements = useQuery({ queryKey: ["inventory", "movements"], queryFn: () => listMovements(), retry: false });
  const products = useQuery({ queryKey: ["products", "", "all"], queryFn: () => listProducts("", "all"), retry: false });
  useExpireOnAuthError(movements.error);
  if (movements.isPending) return <Loading label="Loading stock ledger…" />;
  if (movements.isError) return <QueryError label="The stock ledger could not be loaded" onRetry={() => void movements.refetch()} />;
  if (movements.data.length === 0) return <Empty title="No movements posted" text="Every quantity change appears here as an immutable entry." />;
  return <section className="master-panel" aria-labelledby="ledger-title"><h2 id="ledger-title" className="sr-only">Stock ledger</h2>
    <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Date</th><th scope="col">Type</th><th scope="col">Product</th><th scope="col">Status</th><th scope="col">Quantity</th><th scope="col">Reason</th></tr></thead><tbody>{movements.data.map((movement) => <LedgerRow key={movement.id} movement={movement} productName={productName(products.data, movement.productId, products.isError)} />)}</tbody></table></div>
  </section>;
}

function LedgerRow({ movement, productName }: { movement: InventoryMovement; productName: string }) {
  const detail = useQuery({ queryKey: ["product", movement.productId], queryFn: () => getProduct(movement.productId), retry: false });
  const scale = detail.data?.quantityScale ?? 0;
  const signed = `${movement.quantityDeltaAtoms > 0 ? "+" : "−"}${atomsToQuantity(Math.abs(movement.quantityDeltaAtoms), scale)}`;
  return <tr><td data-label="Date">{movement.occurredOn}</td><td data-label="Type">{MOVEMENT_LABELS[movement.movementType]}</td><td data-label="Product">{productName}</td><td data-label="Status"><span className={`stock-status stock-status--${movement.stockStatus}`}>{STOCK_STATUS_LABELS[movement.stockStatus]}</span></td><td data-label="Quantity"><span className={movement.quantityDeltaAtoms > 0 ? "ledger-in" : "ledger-out"}>{signed}</span></td><td data-label="Reason">{movementReason(movement)}</td></tr>;
}

/**
 * Every movement type must name itself. A purchase inward shown as "Adjustment" would tell an
 * auditor that someone corrected the stock by hand when a supplier invoice was in fact posted.
 */
/**
 * Where a movement came from, in the operator's words.
 *
 * A movement whose Reason column is blank reads as an unexplained correction, which is exactly what
 * an auditor should never see beside stock that a document actually moved.
 */
function movementReason(movement: InventoryMovement) {
  if (movement.purchaseLineId) return <span>Purchase inward<small className="row-subtext">Recorded by posting a purchase</small></span>;
  if (movement.saleLineId) return <span>Sold at the counter<small className="row-subtext">Recorded by posting a sale</small></span>;
  if (movement.returnLineId) return <span>{movement.movementType === "sales_return" ? "Returned by a customer" : "Returned to the supplier"}<small className="row-subtext">Recorded by posting a return</small></span>;
  if (movement.stockDispositionId) return <span>Stock status change<small className="row-subtext">{movement.reason || "Authorised by a pharmacist"}</small></span>;
  if (movement.stockOperationLineId) return <span>{movement.movementType === "stock_count" ? "Counted on the shelf" : movement.movementType === "stock_removal" ? "Physically removed" : "Stock operation"}<small className="row-subtext">{movement.reason || "Recorded by a stock operation"}</small></span>;
  return movement.reason || "—";
}

const MOVEMENT_LABELS: Record<InventoryMovement["movementType"], string> = {
  opening_stock: "Opening stock",
  adjustment: "Adjustment",
  purchase: "Purchase",
  sale: "Sale",
  sales_return: "Sales return",
  purchase_return: "Purchase return",
  disposition_transfer: "Stock status change",
  stock_count: "Physical count",
  stock_removal: "Removed from the store"
};

/**
 * What each stock status means at the counter.
 *
 * Quarantined and non-sellable quantity is real and in the building, and the ledger must show it —
 * but it is never added to what can be sold. Merging the two figures would tell a pharmacist they
 * have stock they are not allowed to hand over.
 */
const STOCK_STATUS_LABELS: Record<StockStatus, string> = {
  sellable: "Sellable",
  quarantined: "Quarantined",
  non_sellable: "Not sellable"
};

function PostMovementDialog({ onClose, onPosted }: { onClose: () => void; onPosted: () => void }) {
  const [productId, setProductId] = useState("");
  const [packId, setPackId] = useState("");
  const [batchId, setBatchId] = useState("");
  const [entryMode, setEntryMode] = useState<"packs" | "base">("packs");
  const [amount, setAmount] = useState("");
  const [occurredOn, setOccurredOn] = useState(() => todayIso());
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  // One key per posting attempt, so a retry of this attempt can never double-post.
  const [idempotencyKey, setIdempotencyKey] = useState(() => newIdempotencyKey());

  const products = useQuery({ queryKey: ["products", "", "active"], queryFn: () => listProducts("", "active"), retry: false });
  const detail = useQuery({ queryKey: ["product", productId], queryFn: () => getProduct(productId), enabled: Boolean(productId), retry: false });
  const batches = useQuery({ queryKey: ["pack-batches", packId], queryFn: () => listBatches(packId), enabled: Boolean(packId), retry: false });
  const packs = useMemo(() => detail.data?.packs.filter((pack) => pack.status === "active") ?? [], [detail.data]);
  const pack = packs.find((item) => item.id === packId);
  const scale = detail.data?.quantityScale ?? 0;

  useEffect(() => { setPackId(""); setBatchId(""); }, [productId]);
  useEffect(() => { setBatchId(""); }, [packId]);

  // The authoritative quantity is always shown before posting, so a pack entry can never be
  // silently reinterpreted.
  const atoms = resolveAtoms(entryMode, amount, pack?.baseQuantityAtoms ?? 0, scale);

  const mutation = useMutation({
    mutationFn: () => postMovement({
      idempotencyKey,
      movementType: "opening_stock",
      productPackId: packId,
      batchId: batchId || null,
      quantityDeltaAtoms: atoms!,
      occurredOn,
      reason: reason.trim() || null
    }),
    onSuccess: onPosted,
    onError: (caught) => {
      setError(caught instanceof LocalServiceError ? caught.message : caught instanceof Error ? caught.message : "The movement could not be posted.");
      // A failed attempt gets a fresh key so a corrected resubmission is a new posting.
      setIdempotencyKey(newIdempotencyKey());
    }
  });

  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (!packId) { setError("Select the Product and Pack this stock belongs to."); return; }
    if (atoms === null) { setError("Enter a positive quantity using the precision this product allows."); return; }
    if (!occurredOn) { setError("Enter the date this opening stock applies from."); return; }
    mutation.mutate();
  };

  return <CatalogDialog title="Post Opening Stock" description="Opening stock is a ledger entry, not an editable quantity. Correct it later with an adjustment." onClose={onClose}><form className="master-form" onSubmit={submit}>
    <SelectField label="Product" name="productId" value={productId} onChange={setProductId} disabled={products.isPending} options={[["", products.isPending ? "Loading…" : products.isError ? "Products unavailable" : "Select a Product"], ...(products.data ?? []).map((item) => [item.id, item.displayName] as const)]} />
    {products.isError && <InlineQueryError label="Products could not be loaded." onRetry={() => void products.refetch()} />}
    <SelectField label="Pack" name="packId" value={packId} onChange={setPackId} disabled={!productId || detail.isPending} options={[["", !productId ? "Select a Product first" : detail.isPending ? "Loading…" : detail.isError ? "Packs unavailable" : "Select a Pack"], ...packs.map((item) => [item.id, `${item.displayLabel || item.skuCode || "Pack"} · ${atomsToQuantity(item.baseQuantityAtoms, scale)} per pack`] as const)]} />
    {detail.isError && <InlineQueryError label="Packs could not be loaded." onRetry={() => void detail.refetch()} />}
    <SelectField label="Batch (optional)" name="batchId" value={batchId} onChange={setBatchId} disabled={!packId || batches.isPending} options={[["", !packId ? "Select a Pack first" : batches.isPending ? "Loading…" : batches.isError ? "Batches unavailable" : "No batch"], ...(batches.data ?? []).filter((item) => item.status === "active").map((item) => [item.id, batchLabel(item)] as const)]} />
    {batches.isError && <InlineQueryError label="Batches could not be loaded." onRetry={() => void batches.refetch()} />}
    <SelectField label="Quantity entered in" name="entryMode" value={entryMode} onChange={(value) => setEntryMode(value as "packs" | "base")} options={[["packs", "Packs"], ["base", "Base units"]]} />
    <TextField label="Quantity" name="amount" value={amount} onChange={setAmount} required hint={entryMode === "packs" && pack ? `One pack is ${atomsToQuantity(pack.baseQuantityAtoms, scale)} base units.` : undefined} />
    <TextField label="Opening date" name="occurredOn" type="date" value={occurredOn} onChange={setOccurredOn} required />
    <TextField label="Reason (optional)" name="reason" value={reason} onChange={setReason} />
    <div className="field catalog-span"><span className="field-label">Will be posted as</span><strong>{atoms === null ? "—" : `${atomsToQuantity(atoms, scale)} base units`}</strong><small>The ledger records exact base-unit quantities, never a pack count.</small></div>
    {error && <div className="inline-notice inline-notice--error" role="alert">{error}</div>}
    <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Posting…" : "Post Opening Stock"}</button></div>
  </form></CatalogDialog>;
}

/**
 * What happens to goods sitting in quarantine.
 *
 * A customer return never comes back as sellable stock by itself — a pharmacist has to look at it
 * first. This is where that judgement is recorded, and it is a transfer rather than an edit: the
 * Store Service writes two opposite movements within the lot, so the physical total is untouched
 * and the ledger keeps both the quarantine and the decision that ended it.
 *
 * Releasing is deliberately the harder-sounding option of the two. Written off is terminal, so the
 * form says as much before it is submitted rather than after.
 */
function QuarantineDecisionDialog({ row, batchId, productName, packLabel, batchNumber, expiresOn, packAtoms, scale, onClose }: {
  row: StockBalance; batchId: string; productName: string; packLabel: string; batchNumber: string;
  expiresOn: string | null; packAtoms: number; scale: number; onClose: () => void;
}) {
  const queryClient = useQueryClient();
  // An expired lot has only one outcome left, and the form says so instead of offering a release
  // the Store Service would refuse. The service is still the authority — this only spares the
  // pharmacist a refusal they can do nothing about.
  const expired = Boolean(expiresOn && expiresOn < todayIso());
  const [outcome, setOutcome] = useState<"sellable" | "non_sellable">(expired ? "non_sellable" : "sellable");
  // The batch may still have been loading when the row was clicked.
  useEffect(() => { if (expired) setOutcome("non_sellable"); }, [expired]);
  const [entryMode, setEntryMode] = useState<"packs" | "base">("base");
  const [amount, setAmount] = useState(() => atomsToQuantity(row.balanceAtoms, scale));
  const [occurredOn, setOccurredOn] = useState(() => todayIso());
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  // One key per attempt, so retrying this decision can never transfer the stock twice.
  const [idempotencyKey, setIdempotencyKey] = useState(() => newIdempotencyKey());

  const atoms = resolveAtoms(entryMode, amount, packAtoms, scale);

  const mutation = useMutation({
    mutationFn: () => createStockDisposition({
      idempotencyKey,
      productPackId: row.productPackId,
      batchId,
      quantityAtoms: atoms!,
      fromStatus: "quarantined",
      toStatus: outcome,
      reason: reason.trim(),
      occurredOn
    }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["inventory", "stock"] });
      void queryClient.invalidateQueries({ queryKey: ["inventory", "movements"] });
      onClose();
    },
    onError: (caught) => {
      setError(caught instanceof Error ? caught.message : "The decision could not be recorded.");
      setIdempotencyKey(newIdempotencyKey());
    }
  });

  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (atoms === null || atoms > row.balanceAtoms) { setError(`Enter a quantity between one and ${atomsToQuantity(row.balanceAtoms, scale)} base units.`); return; }
    if (!reason.trim()) { setError("Say what was checked. This is the record of why the stock was released or written off."); return; }
    if (!occurredOn) { setError("Enter the date this decision was made."); return; }
    mutation.mutate();
  };

  return <CatalogDialog title="Decide quarantined stock" description="Quarantined goods are in the building but cannot be sold. Recording a decision moves them; it never changes what the return said." onClose={onClose}><form className="master-form" onSubmit={submit}>
    <div className="field catalog-span"><span className="field-label">In quarantine</span><strong>{atomsToQuantity(row.balanceAtoms, scale)} base units</strong><small>{productName} · {packLabel} · Batch {batchNumber}{expiresOn ? ` · ${expired ? "Expired" : "Expires"} ${expiresOn}` : ""}</small></div>
    {expired && <div className="panel-callout catalog-span" role="alert"><strong>This batch has expired</strong><small>Expired stock can never be released for sale, whatever condition it is in. Writing it off is the only outcome left.</small></div>}
    <SelectField label="Decision" name="outcome" value={outcome} onChange={(value) => setOutcome(value as "sellable" | "non_sellable")} disabled={expired} options={expired ? [["non_sellable", "Write off — cannot be sold"]] : [["sellable", "Release for sale"], ["non_sellable", "Write off — cannot be sold"]]} />
    <SelectField label="Quantity entered in" name="decisionEntryMode" value={entryMode} onChange={(value) => setEntryMode(value as "packs" | "base")} options={[["base", "Base units"], ["packs", "Packs"]]} />
    <TextField label="Quantity" name="decisionAmount" value={amount} onChange={setAmount} required hint={entryMode === "packs" && packAtoms > 0 ? `One pack is ${atomsToQuantity(packAtoms, scale)} base units.` : undefined} />
    <TextField label="Decision date" name="decisionOccurredOn" type="date" value={occurredOn} onChange={setOccurredOn} required />
    <TextField label="What was checked" name="decisionReason" value={reason} onChange={setReason} required />
    <div className="field catalog-span"><span className="field-label">Will be recorded as</span><strong>{atoms === null ? "—" : `${atomsToQuantity(atoms, scale)} base units ${outcome === "sellable" ? "released for sale" : "written off"}`}</strong><small>{outcome === "sellable" ? "Released stock becomes available at the counter immediately." : "Writing off is final. Stock written off is never transferred back."}</small></div>
    {error && <div className="inline-notice inline-notice--error" role="alert">{error}</div>}
    <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Recording…" : "Record decision"}</button></div>
  </form></CatalogDialog>;
}

/** Exact integer conversion. A pack entry multiplies; a base entry parses against the scale. */
function resolveAtoms(mode: "packs" | "base", amount: string, packAtoms: number, scale: number): number | null {
  if (mode === "base") return quantityToAtoms(amount, scale);
  const packs = quantityToAtoms(amount, 0);
  if (packs === null || packAtoms <= 0) return null;
  const atoms = packs * packAtoms;
  return Number.isSafeInteger(atoms) && atoms > 0 ? atoms : null;
}

function batchLabel(batch: Batch) {
  const parts = [batch.batchNumber];
  if (batch.expiresOn) parts.push(`exp ${batch.expiresOn}`);
  if (batch.mrpPaise != null) parts.push(`₹${paiseToRupees(batch.mrpPaise)}`);
  return parts.join(" · ");
}

function productName(products: Array<{ id: string; displayName: string }> | undefined, id: string, failed: boolean) {
  if (!products) return failed ? "Name unavailable" : "Loading…";
  return products.find((product) => product.id === id)?.displayName ?? "Unknown product";
}

function todayIso() { const now = new Date(); return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`; }

function TextField({ label, name, value, onChange, required, type = "text", hint }: { label: string; name: string; value: string; onChange: (value: string) => void; required?: boolean; type?: string; hint?: string }) {
  const id = `inventory-${name}`;
  return <div className="field"><label htmlFor={id}>{label}{required && <span aria-hidden="true"> *</span>}</label><input id={id} name={name} type={type} value={value} onChange={(event) => onChange(event.target.value)} required={required} aria-describedby={hint ? `${id}-help` : undefined} />{hint && <small id={`${id}-help`}>{hint}</small>}</div>;
}
function SelectField({ label, name, value, onChange, options, disabled }: { label: string; name: string; value: string; onChange: (value: string) => void; options: ReadonlyArray<readonly [string, string]>; disabled?: boolean }) {
  const id = `inventory-${name}`;
  return <div className="field"><label htmlFor={id}>{label}</label><select id={id} name={name} value={value} onChange={(event) => onChange(event.target.value)} disabled={disabled}>{options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}</select></div>;
}
function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function InlineQueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="catalog-inline-error" role="alert"><span>{label}</span><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function Empty({ title, text }: { title: string; text: string }) { return <div className="empty-state"><h3>{title}</h3><p>{text}</p></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }

export type { ReactNode };
