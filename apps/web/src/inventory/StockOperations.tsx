import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  Batch,
  ProductDetail,
  StockOperationDetail,
  StockOperationKind,
  StockOperationLine,
  StockOperationReason,
  StockStatus
} from "@aushadharth/contracts";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { atomsToQuantity, getProduct, listBatches, listProducts } from "../products/productApi";
import { newIdempotencyKey } from "./inventoryApi";
import {
  OPERATION_KIND_BLURBS,
  OPERATION_KIND_LABELS,
  REASON_LABELS,
  addStockOperationLine,
  createStockOperation,
  discardStockOperation,
  listStockOperations,
  postStockOperation,
  quantityToInteger,
  quoteStockOperation,
  removeStockOperationLine
} from "./stockOperationApi";

/**
 * Phase 1J stock operations.
 *
 * Six workflows rather than one "adjust" box, because a pharmacist never means "change this
 * number" — they mean a shelf was counted, a strip was crushed, a lot expired, or a contractor
 * took the written-off carton away. Those sentences have different authorities, different
 * destinations and different consequences, and a single ambiguous form would lose all of it.
 */

/** What each workflow asks for, and what it does with the answer. */
type KindShape = {
  /** The status the line acts on. Fixed per kind, because the kind is the intent. */
  source: StockStatus;
  /** Where a transfer sends the quantity, when the kind transfers. */
  targets?: { value: StockStatus; label: string }[];
  reasons: { value: StockOperationReason; label: string }[];
  /** Whether the operator chooses the direction (only a generic adjustment does). */
  choosesDirection?: boolean;
  /** Whether the operator enters a counted quantity rather than a quantity to move. */
  counts?: boolean;
  quantityLabel: string;
};

const SHAPES: Record<StockOperationKind, KindShape> = {
  physical_count: {
    source: "sellable",
    reasons: [{ value: "physical_count_gain", label: "Counted" }],
    counts: true,
    quantityLabel: "Counted quantity"
  },
  adjustment: {
    source: "sellable",
    reasons: [
      { value: "data_correction", label: REASON_LABELS.data_correction },
      { value: "theft_or_loss", label: REASON_LABELS.theft_or_loss }
    ],
    choosesDirection: true,
    quantityLabel: "Quantity"
  },
  damage: {
    source: "sellable",
    targets: [
      { value: "non_sellable", label: "Write off — it can never be sold" },
      { value: "quarantined", label: "Hold in quarantine — a pharmacist will decide" }
    ],
    reasons: [
      { value: "damage", label: REASON_LABELS.damage },
      { value: "breakage", label: REASON_LABELS.breakage }
    ],
    quantityLabel: "Damaged quantity"
  },
  expiry: {
    source: "sellable",
    targets: [{ value: "non_sellable", label: "Write off — it can never be sold" }],
    reasons: [{ value: "expiry", label: REASON_LABELS.expiry }],
    quantityLabel: "Expired quantity"
  },
  quarantine: {
    source: "sellable",
    targets: [{ value: "quarantined", label: "Hold in quarantine" }],
    reasons: [
      { value: "quality_hold", label: REASON_LABELS.quality_hold },
      { value: "damage", label: REASON_LABELS.damage }
    ],
    quantityLabel: "Quantity to hold"
  },
  removal: {
    source: "non_sellable",
    reasons: [{ value: "disposal", label: REASON_LABELS.disposal }],
    quantityLabel: "Quantity leaving"
  }
};

/** Which kinds each role may start. Matches the Store Service, which is the authority. */
export function kindsForRole(role: string | undefined): StockOperationKind[] {
  if (role === "owner_admin") {
    return ["physical_count", "adjustment", "damage", "expiry", "quarantine", "removal"];
  }
  if (role === "pharmacist") return ["physical_count", "damage", "expiry", "quarantine"];
  return [];
}

export function StockOperationDialog({
  kind,
  onClose
}: {
  kind: StockOperationKind;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const shape = SHAPES[kind];
  const [draft, setDraft] = useState<StockOperationDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [posting, setPosting] = useState(false);

  // Line entry.
  const [productId, setProductId] = useState("");
  const [packId, setPackId] = useState("");
  const [batchId, setBatchId] = useState("");
  const [target, setTarget] = useState<StockStatus>(shape.targets?.[0]?.value ?? "non_sellable");
  const [reason, setReason] = useState<StockOperationReason>(shape.reasons[0].value);
  const [direction, setDirection] = useState<"increase" | "decrease">("decrease");
  const [entryMode, setEntryMode] = useState<"packs" | "base">("base");
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [businessDate] = useState(() => todayIso());

  const products = useQuery({
    queryKey: ["products", "", "active"],
    queryFn: () => listProducts("", "active"),
    retry: false
  });
  const detail = useQuery({
    queryKey: ["product", productId],
    queryFn: () => getProduct(productId),
    enabled: Boolean(productId),
    retry: false
  });
  const batches = useQuery({
    queryKey: ["pack-batches", packId],
    queryFn: () => listBatches(packId),
    enabled: Boolean(packId),
    retry: false
  });
  const packs = useMemo(
    () => detail.data?.packs.filter((pack) => pack.status === "active") ?? [],
    [detail.data]
  );
  const pack = packs.find((item) => item.id === packId);
  const scale = detail.data?.quantityScale ?? 0;
  const batch = batches.data?.find((item) => item.id === batchId);

  useEffect(() => {
    setPackId("");
    setBatchId("");
  }, [productId]);
  useEffect(() => setBatchId(""), [packId]);
  // One pack is the common case; making the operator choose it is a click that says nothing.
  useEffect(() => {
    if (packs.length === 1) setPackId(packs[0].id);
  }, [packs]);

  const quote = useQuery({
    queryKey: ["stock-operation-quote", draft?.id, draft?.revision],
    queryFn: () => quoteStockOperation(draft!.id),
    enabled: Boolean(draft && draft.lines.length > 0),
    retry: false
  });

  // Exactly what the operator typed. The Store Service holds the pack size and does the
  // conversion; a browser that converted too would be applying it twice.
  const entered = quantityToInteger(amount);

  const addLine = useMutation({
    mutationFn: async () => {
      const document = draft ?? (await createStockOperation({ operationKind: kind, businessDate }));
      return addStockOperationLine(document.id, {
        expectedRevision: document.revision,
        productPackId: packId,
        batchId: batchId || null,
        stockStatus: shape.source,
        targetStockStatus: shape.targets ? target : null,
        reasonCode: reason,
        countedQuantity: shape.counts ? entered! : null,
        quantity: shape.counts ? null : entered!,
        quantityBasis: entryMode === "packs" ? "pack" : "base_unit",
        direction: shape.choosesDirection ? direction : null,
        note: note.trim() || null
      });
    },
    onSuccess: (document) => {
      setDraft(document);
      setAmount("");
      setNote("");
      setError(null);
    },
    onError: (caught) => setError(safeText(caught))
  });

  const post = useMutation({
    mutationFn: () => postStockOperation(draft!.id, draft!.revision, newIdempotencyKey()),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["inventory"] });
      void queryClient.invalidateQueries({ queryKey: ["stock-operations"] });
      onClose();
    },
    onError: (caught) => {
      setError(safeText(caught));
      setPosting(false);
    }
  });

  /** Backing out of a draft nobody posted leaves nothing behind. */
  const cancel = async () => {
    if (draft) {
      try {
        await discardStockOperation(draft.id, draft.revision);
      } catch {
        // A draft that could not be discarded is inert; the operator should not be held here.
      }
      void queryClient.invalidateQueries({ queryKey: ["stock-operations"] });
    }
    onClose();
  };

  const submitLine = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!packId) {
      setError("Choose the product and pack this applies to.");
      return;
    }
    if (!batchId && kind !== "physical_count" && kind !== "adjustment") {
      setError("Choose the batch. This operation always applies to one batch.");
      return;
    }
    if (entered === null || (!shape.counts && entered < 1)) {
      setError(
        shape.counts
          ? "Enter the counted quantity. Zero is a valid answer."
          : "Enter a quantity of at least one."
      );
      return;
    }
    if ((reason === "quality_hold" || reason === "data_correction") && !note.trim()) {
      setError("Say what was found. This is the record of why the stock was moved.");
      return;
    }
    addLine.mutate();
  };

  const lines = draft?.lines ?? [];

  return (
    <CatalogDialog
      title={OPERATION_KIND_LABELS[kind]}
      description={OPERATION_KIND_BLURBS[kind]}
      onClose={() => void cancel()}
    >
      <form className="master-form" onSubmit={submitLine}>
        <SelectField
          label="Product"
          name="so-product"
          value={productId}
          onChange={setProductId}
          disabled={products.isPending}
          options={[
            ["", products.isPending ? "Loading…" : products.isError ? "Products unavailable" : "Select a product"],
            ...(products.data ?? []).map((item) => [item.id, item.displayName] as const)
          ]}
        />
        <SelectField
          label="Pack"
          name="so-pack"
          value={packId}
          onChange={setPackId}
          disabled={!productId || detail.isPending}
          options={[
            ["", !productId ? "Select a product first" : detail.isPending ? "Loading…" : "Select a pack"],
            ...packs.map(
              (item) =>
                [item.id, `${item.displayLabel || item.skuCode || "Pack"} · ${item.baseQuantityAtoms} per pack`] as const
            )
          ]}
        />
        <SelectField
          label="Batch"
          name="so-batch"
          value={batchId}
          onChange={setBatchId}
          disabled={!packId || batches.isPending}
          options={[
            ["", !packId ? "Select a pack first" : batches.isPending ? "Loading…" : "Select a batch"],
            ...(batches.data ?? [])
              .filter((item) => item.status === "active")
              .map((item) => [item.id, batchLabel(item)] as const)
          ]}
        />
        {batch?.expiresOn && (
          <div className="field catalog-span">
            <span className="field-label">This batch</span>
            <strong>
              {batch.batchNumber} · {batch.expiresOn < todayIso() ? "Expired" : "Expires"} {batch.expiresOn}
            </strong>
          </div>
        )}

        {shape.choosesDirection && (
          <SelectField
            label="Change"
            name="so-direction"
            value={direction}
            onChange={(value) => setDirection(value as "increase" | "decrease")}
            options={[
              ["decrease", "Reduce the recorded quantity"],
              ["increase", "Increase the recorded quantity"]
            ]}
          />
        )}
        {shape.targets && shape.targets.length > 1 && (
          <SelectField
            label="What happens to them"
            name="so-target"
            value={target}
            onChange={(value) => setTarget(value as StockStatus)}
            options={shape.targets.map((item) => [item.value, item.label] as const)}
          />
        )}
        {shape.reasons.length > 1 && (
          <SelectField
            label="Reason"
            name="so-reason"
            value={reason}
            onChange={(value) => setReason(value as StockOperationReason)}
            options={shape.reasons.map((item) => [item.value, item.label] as const)}
          />
        )}

        <SelectField
          label="Quantity entered in"
          name="so-basis"
          value={entryMode}
          onChange={(value) => setEntryMode(value as "packs" | "base")}
          options={[
            ["base", "Base units"],
            ["packs", "Packs"]
          ]}
        />
        <TextField
          label={shape.quantityLabel}
          name="so-amount"
          value={amount}
          onChange={setAmount}
          required
          hint={
            entryMode === "packs" && pack
              ? `One pack is ${pack.baseQuantityAtoms} base units.`
              : shape.counts
                ? "Count what is physically there. AUSHADHARTH works out the difference."
                : undefined
          }
        />
        <TextField
          label={reason === "quality_hold" || reason === "data_correction" ? "What was found *" : "What was found"}
          name="so-note"
          value={note}
          onChange={setNote}
        />

        {error && (
          <div className="inline-notice inline-notice--error" role="alert">
            {error}
          </div>
        )}
        <div className="form-actions">
          <button className="button button--secondary" type="button" onClick={() => void cancel()}>
            Cancel
          </button>
          <button className="button button--secondary" type="submit" disabled={addLine.isPending}>
            {addLine.isPending ? "Adding…" : lines.length ? "Add another line" : "Add line"}
          </button>
        </div>
      </form>

      {lines.length > 0 && (
        <section className="master-panel" aria-label="Lines on this operation">
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th scope="col">Item</th>
                  <th scope="col">Batch</th>
                  <th scope="col">{shape.counts ? "Counted" : "Quantity"}</th>
                  <th scope="col">{shape.counts ? "Now recorded" : "Effect"}</th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {lines.map((line) => (
                  <LineRow
                    key={line.id}
                    line={line}
                    quote={quote.data?.lines.find((item) => item.lineId === line.id)}
                    counts={Boolean(shape.counts)}
                    onRemove={async () => {
                      try {
                        setDraft(await removeStockOperationLine(line.id, draft!.revision));
                      } catch (caught) {
                        setError(safeText(caught));
                      }
                    }}
                  />
                ))}
              </tbody>
            </table>
          </div>
          {quote.data && !quote.data.postable && (
            <div className="inline-notice inline-notice--error" role="alert">
              One of these lines asks for more stock than there is. Change it before posting.
            </div>
          )}
          <div className="form-actions">
            <button
              className="button button--primary"
              type="button"
              disabled={post.isPending || posting || quote.data?.postable === false}
              onClick={() => {
                setPosting(true);
                post.mutate();
              }}
            >
              {post.isPending ? "Posting…" : `Post ${OPERATION_KIND_LABELS[kind].toLowerCase()}`}
            </button>
          </div>
          <p className="panel-note">
            Posting writes the stock movements and cannot be undone. Correct a mistake by recording a
            later operation, never by editing this one.
          </p>
        </section>
      )}
    </CatalogDialog>
  );
}

function LineRow({
  line,
  quote,
  counts,
  onRemove
}: {
  line: StockOperationLine;
  quote?: { currentAtoms: number; resultingAtoms: number; sufficient: boolean };
  counts: boolean;
  onRemove: () => void;
}) {
  const entered = counts ? line.countedAtoms : line.quantityAtoms;
  return (
    <tr>
      <td data-label="Item">
        {line.productDisplayName ?? "Unknown product"}
        <small className="row-subtext">{line.packDisplayLabel}</small>
      </td>
      <td data-label="Batch">{line.batchNumber ?? "No batch"}</td>
      <td data-label={counts ? "Counted" : "Quantity"}>
        {atomsToQuantity(entered ?? 0, line.quantityScale)}
      </td>
      <td data-label={counts ? "Now recorded" : "Effect"}>
        {quote ? (
          <span className={quote.sufficient ? "" : "ledger-out"}>
            {atomsToQuantity(quote.currentAtoms, line.quantityScale)} →{" "}
            {atomsToQuantity(quote.resultingAtoms, line.quantityScale)}
          </span>
        ) : (
          "—"
        )}
      </td>
      <td data-label="Actions">
        <button className="button button--secondary" type="button" onClick={onRemove}>
          Remove
        </button>
      </td>
    </tr>
  );
}

/**
 * What the store did to its own stock, in the operator's words.
 *
 * The Stock Ledger below it is the underlying audit truth and stays available, but nobody running a
 * pharmacy should have to read a movement table to find out who wrote off what.
 */
export function StockOperationHistory() {
  const queryClient = useQueryClient();
  // A draft left behind by closing a tab is still nobody's record of anything, so it stays
  // clearable from the list rather than sitting there forever.
  const discard = useMutation({
    mutationFn: (operation: { id: string; revision: number }) =>
      discardStockOperation(operation.id, operation.revision),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["stock-operations"] })
  });
  const operations = useQuery({
    queryKey: ["stock-operations", "all"],
    queryFn: () => listStockOperations("all"),
    retry: false
  });
  if (operations.isPending) return <Loading label="Loading stock operations…" />;
  if (operations.isError) {
    return (
      <QueryError
        label="Stock operations could not be loaded"
        onRetry={() => void operations.refetch()}
      />
    );
  }
  if (operations.data.length === 0) {
    return (
      <Empty
        title="No stock operations yet"
        text="Counts, damage, expiry, quarantine and disposals appear here once they are posted."
      />
    );
  }
  return (
    <section className="master-panel" aria-labelledby="operations-title">
      <h2 id="operations-title" className="sr-only">
        Stock operations
      </h2>
      <div className="table-scroll">
        <table className="data-table">
          <thead>
            <tr>
              <th scope="col">Date</th>
              <th scope="col">What was done</th>
              <th scope="col">Status</th>
              <th scope="col">Note</th>
              <th scope="col"><span className="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {operations.data.map((operation) => (
              <tr key={operation.id}>
                <td data-label="Date">{operation.businessDate}</td>
                <td data-label="What was done">{OPERATION_KIND_LABELS[operation.operationKind]}</td>
                <td data-label="Status">
                  {operation.status === "posted" ? (
                    "Posted"
                  ) : (
                    <span className="row-subtext">Not posted</span>
                  )}
                </td>
                <td data-label="Note">{operation.note ?? "—"}</td>
                <td data-label="Actions">{operation.status === "draft"
                  ? <button className="button button--secondary" type="button" onClick={() => discard.mutate(operation)}>Discard</button>
                  : <span className="row-subtext">—</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}


function batchLabel(batch: Batch) {
  const parts = [batch.batchNumber];
  if (batch.expiresOn) parts.push(`exp ${batch.expiresOn}`);
  return parts.join(" · ");
}

function safeText(caught: unknown) {
  if (caught instanceof LocalServiceError) return caught.message;
  if (caught instanceof Error) return caught.message;
  return "That could not be recorded.";
}

function todayIso() {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}

function TextField({
  label,
  name,
  value,
  onChange,
  required,
  hint
}: {
  label: string;
  name: string;
  value: string;
  onChange: (value: string) => void;
  required?: boolean;
  hint?: string;
}) {
  const id = `stockop-${name}`;
  return (
    <div className="field">
      <label htmlFor={id}>
        {label}
        {required && <span aria-hidden="true"> *</span>}
      </label>
      <input
        id={id}
        name={name}
        type="text"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        required={required}
        aria-describedby={hint ? `${id}-help` : undefined}
      />
      {hint && <small id={`${id}-help`}>{hint}</small>}
    </div>
  );
}

function SelectField({
  label,
  name,
  value,
  onChange,
  options,
  disabled
}: {
  label: string;
  name: string;
  value: string;
  onChange: (value: string) => void;
  options: ReadonlyArray<readonly [string, string]>;
  disabled?: boolean;
}) {
  const id = `stockop-${name}`;
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <select
        id={id}
        name={name}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        disabled={disabled}
      >
        {options.map(([option, text]) => (
          <option key={option} value={option}>
            {text}
          </option>
        ))}
      </select>
    </div>
  );
}

function Loading({ label }: { label: string }) {
  return (
    <div className="table-loading" role="status" aria-live="polite">
      <span />
      <span />
      <span />
      <b>{label}</b>
    </div>
  );
}
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }) {
  return (
    <div className="empty-state" role="alert">
      <h3>{label}</h3>
      <p>The Local Store Service did not complete this request.</p>
      <button className="button button--secondary" type="button" onClick={onRetry}>
        Retry
      </button>
    </div>
  );
}
function Empty({ title, text }: { title: string; text: string }) {
  return (
    <div className="empty-state">
      <h3>{title}</h3>
      <p>{text}</p>
    </div>
  );
}

export type { ProductDetail };
