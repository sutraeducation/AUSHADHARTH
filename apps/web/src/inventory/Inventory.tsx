import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, Navigate, useParams } from "react-router";
import type { Batch, InventoryMovement, ProductDetail, StockBalance } from "@aushadharth/contracts";
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

const SECTIONS = [
  { slug: "stock", title: "Stock Overview", description: "Derived balances by Product, Pack, and Batch." },
  { slug: "ledger", title: "Stock Ledger", description: "Every posted movement, newest first." }
] as const;

export function InventoryPage() {
  const { section } = useParams();
  const slug = section ?? "stock";
  const auth = useAuth();
  const canPost = auth.status?.user?.role === "owner_admin";
  const queryClient = useQueryClient();
  const [posting, setPosting] = useState(false);
  usePageTitle(SECTIONS.find((item) => item.slug === slug)?.title ?? "Inventory");
  if (!SECTIONS.some((item) => item.slug === slug)) return <Navigate to="/app/inventory/stock" replace />;
  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["inventory", "stock"] });
    void queryClient.invalidateQueries({ queryKey: ["inventory", "movements"] });
  };
  return <>
    <header className="page-header"><div><p className="eyebrow">INVENTORY</p><h1>Inventory</h1><p>Quantity is derived from the movement ledger. No balance is ever stored.</p></div>
      {canPost ? <button className="button button--primary" type="button" onClick={() => setPosting(true)}>Post Opening Stock</button> : <span className="read-only-note">Read-only access</span>}
    </header>
    <nav className="inventory-tabs" aria-label="Inventory sections">{SECTIONS.map((item) => <NavLink key={item.slug} to={`/app/inventory/${item.slug}`} className={({ isActive }) => `inventory-tab ${isActive ? "inventory-tab--active" : ""}`}>{item.title}</NavLink>)}</nav>
    {slug === "stock" ? <StockOverview /> : <StockLedger />}
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
    <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Product</th><th scope="col">Pack</th><th scope="col">Batch</th><th scope="col">Balance</th></tr></thead><tbody>{stock.data.map((row) => <BalanceRow key={`${row.productPackId}-${row.batchId ?? "none"}`} row={row} productName={productName(products.data, row.productId, products.isError)} />)}</tbody></table></div>
  </section>;
}

function BalanceRow({ row, productName }: { row: StockBalance; productName: string }) {
  const detail = useQuery({ queryKey: ["product", row.productId], queryFn: () => getProduct(row.productId), retry: false });
  const pack = detail.data?.packs.find((item) => item.id === row.productPackId);
  const batches = useQuery({ queryKey: ["pack-batches", row.productPackId], queryFn: () => listBatches(row.productPackId), enabled: Boolean(row.batchId), retry: false });
  const batch = row.batchId ? batches.data?.find((item) => item.id === row.batchId) : undefined;
  const scale = detail.data?.quantityScale ?? 0;
  return <tr><td data-label="Product">{productName}</td><td data-label="Pack">{pack?.displayLabel || pack?.skuCode || (detail.isError ? "Pack unavailable" : detail.isPending ? "Loading…" : "—")}</td><td data-label="Batch">{row.batchId ? batch?.batchNumber ?? (batches.isError ? "Batch unavailable" : "Loading…") : "No batch"}</td><td data-label="Balance">{atomsToQuantity(row.balanceAtoms, scale)}</td></tr>;
}

function StockLedger() {
  const movements = useQuery({ queryKey: ["inventory", "movements"], queryFn: () => listMovements(), retry: false });
  const products = useQuery({ queryKey: ["products", "", "all"], queryFn: () => listProducts("", "all"), retry: false });
  useExpireOnAuthError(movements.error);
  if (movements.isPending) return <Loading label="Loading stock ledger…" />;
  if (movements.isError) return <QueryError label="The stock ledger could not be loaded" onRetry={() => void movements.refetch()} />;
  if (movements.data.length === 0) return <Empty title="No movements posted" text="Every quantity change appears here as an immutable entry." />;
  return <section className="master-panel" aria-labelledby="ledger-title"><h2 id="ledger-title" className="sr-only">Stock ledger</h2>
    <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Date</th><th scope="col">Type</th><th scope="col">Product</th><th scope="col">Quantity</th><th scope="col">Reason</th></tr></thead><tbody>{movements.data.map((movement) => <LedgerRow key={movement.id} movement={movement} productName={productName(products.data, movement.productId, products.isError)} />)}</tbody></table></div>
  </section>;
}

function LedgerRow({ movement, productName }: { movement: InventoryMovement; productName: string }) {
  const detail = useQuery({ queryKey: ["product", movement.productId], queryFn: () => getProduct(movement.productId), retry: false });
  const scale = detail.data?.quantityScale ?? 0;
  const signed = `${movement.quantityDeltaAtoms > 0 ? "+" : "−"}${atomsToQuantity(Math.abs(movement.quantityDeltaAtoms), scale)}`;
  return <tr><td data-label="Date">{movement.occurredOn}</td><td data-label="Type">{movement.movementType === "opening_stock" ? "Opening stock" : "Adjustment"}</td><td data-label="Product">{productName}</td><td data-label="Quantity"><span className={movement.quantityDeltaAtoms > 0 ? "ledger-in" : "ledger-out"}>{signed}</span></td><td data-label="Reason">{movement.reason || "—"}</td></tr>;
}

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
