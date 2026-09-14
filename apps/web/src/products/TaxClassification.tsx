import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ProductDetail, ReferenceMasterResponse } from "@aushadharth/contracts";
import { LocalServiceError } from "../platform/localService";
import { listReferences } from "../reference/referenceApi";
import { CatalogDialog } from "./CatalogDialog";
import {
  basisPointsToPercentText,
  getTaxClassification,
  updateTaxClassification
} from "./productApi";

/**
 * Phase 1F Product tax classification.
 *
 * The Product identifies its HSN and Tax Category. The rate shown is resolved by the Store Service
 * for a specific date and is labelled as such, because a rate belongs to the Tax Category's version
 * history and never to the Product. There is deliberately no rate selector.
 */
export function TaxClassificationSection({ product, canMutate }: { product: ProductDetail; canMutate: boolean }) {
  const [editing, setEditing] = useState(false);
  const queryClient = useQueryClient();
  // The business date, taken from this PC's calendar exactly as Phase 1D's ledger does. The service
  // falls back to UTC when no date is supplied, which is a day behind in India for the first hours
  // of every business day — long enough to show yesterday's rate after a rate change.
  const asOf = businessToday();
  const classification = useQuery({
    queryKey: ["product-tax", product.id, asOf],
    queryFn: () => getTaxClassification(product.id, asOf),
    retry: false
  });
  // Archived masters are included so a reference assigned before it was archived still resolves to
  // a real label instead of a raw identifier.
  const references = useQuery({
    queryKey: ["catalog", "tax-references"],
    queryFn: async () => {
      const [hsnCodes, taxCategories] = await Promise.all([
        listReferences("hsn-codes", "", "all"),
        listReferences("tax-categories", "", "all")
      ]);
      return { hsnCodes, taxCategories };
    },
    staleTime: 30_000,
    retry: false
  });

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["product-tax", product.id] });
    void queryClient.invalidateQueries({ queryKey: ["product", product.id] });
  };

  return <section className="master-panel" aria-labelledby="tax-classification-title">
    <div className="panel-header">
      <div><h2 id="tax-classification-title">Tax Classification</h2><p>Identifies which HSN and Tax Category apply. The rate itself is held by the Tax Category and resolved by date, never stored on the Product.</p></div>
      {canMutate && product.status === "active" && !classification.isPending && !classification.isError && <button className="button button--secondary" type="button" onClick={() => setEditing(true)}>Edit Classification</button>}
    </div>

    {classification.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading tax classification…</b></div>
      : classification.isError ? <div className="empty-state" role="alert"><h3>Tax classification could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as an unclassified Product.</p><button className="button button--secondary" type="button" onClick={() => void classification.refetch()}>Retry</button></div>
      : <>
        {references.isError && <div className="catalog-inline-error" role="alert"><span>HSN and Tax Category names could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void references.refetch()}>Retry</button></div>}
        <dl className="detail-grid">
          <div><dt>HSN code</dt><dd>{referenceLabel(references.data?.hsnCodes, classification.data.hsnCodeId, references.isError, references.isPending)}</dd></div>
          <div><dt>Tax Category</dt><dd>{referenceLabel(references.data?.taxCategories, classification.data.taxCategoryId, references.isError, references.isPending)}</dd></div>
          <div><dt>Status</dt><dd>{classification.data.complete ? <span className="status-badge status-badge--active">Complete</span> : <span className="status-badge status-badge--archived">Incomplete</span>}</dd></div>
        </dl>
        {classification.data.applicableRate
          ? <div className="tax-rate-note"><strong>Rate in force on {classification.data.asOf}</strong>
              <ul>
                <li>CGST {basisPointsToPercentText(classification.data.applicableRate.cgstBasisPoints)}%</li>
                <li>SGST {basisPointsToPercentText(classification.data.applicableRate.sgstBasisPoints)}%</li>
                <li>IGST {basisPointsToPercentText(classification.data.applicableRate.igstBasisPoints)}%</li>
                {classification.data.applicableRate.cessBasisPoints > 0 && <li>Cess {basisPointsToPercentText(classification.data.applicableRate.cessBasisPoints)}%</li>}
              </ul>
              <small>Effective {classification.data.applicableRate.effectiveFrom} to {classification.data.applicableRate.effectiveTo ?? "further notice"}. Whether CGST and SGST or IGST applies is decided by the document, from the places of supply — not by the Product.</small>
            </div>
          : <p className="panel-note">{classification.data.taxCategoryId ? `No rate version is in force on ${classification.data.asOf} for this Tax Category.` : "Assign a Tax Category to resolve the applicable rate."}</p>}
      </>}

    {editing && classification.data && <ClassificationDialog product={product} current={classification.data} references={references.data} onClose={() => setEditing(false)} onSaved={() => { setEditing(false); refresh(); }} />}
  </section>;
}

function ClassificationDialog({ product, current, references, onClose, onSaved }: {
  product: ProductDetail;
  current: { revision: number; hsnCodeId?: string | null; taxCategoryId?: string | null };
  references: { hsnCodes: ReferenceMasterResponse[]; taxCategories: ReferenceMasterResponse[] } | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const queryClient = useQueryClient();
  const [hsnCodeId, setHsnCodeId] = useState(current.hsnCodeId ?? "");
  const [taxCategoryId, setTaxCategoryId] = useState(current.taxCategoryId ?? "");
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const mutation = useMutation({
    mutationFn: () => updateTaxClassification(product.id, current.revision, {
      hsnCodeId: hsnCodeId || null,
      taxCategoryId: taxCategoryId || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The classification could not be saved.")
  });
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  const reloadLatest = async () => {
    setReloading(true);
    try {
      const latest = await getTaxClassification(product.id, businessToday());
      queryClient.setQueryData(["product-tax", product.id, businessToday()], latest);
      void queryClient.invalidateQueries({ queryKey: ["product", product.id] });
      setHsnCodeId(latest.hsnCodeId ?? "");
      setTaxCategoryId(latest.taxCategoryId ?? "");
      setError(null);
      onSaved();
    } finally { setReloading(false); }
  };

  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); mutation.mutate(); };

  return <CatalogDialog title="Edit Tax Classification" description="Select which HSN and Tax Category apply. Rates belong to the Tax Category and are resolved by date." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <ReferenceSelect label="HSN code" name="hsnCodeId" value={hsnCodeId} onChange={setHsnCodeId} options={references?.hsnCodes} assigned={current.hsnCodeId} loading={!references} />
      <ReferenceSelect label="Tax Category" name="taxCategoryId" value={taxCategoryId} onChange={setTaxCategoryId} options={references?.taxCategories} assigned={current.taxCategoryId} loading={!references} />
      <p className="panel-note catalog-span">Leaving a field unselected clears it. Tax classification is optional until a document needs it.</p>
      {error && <div className="inline-notice inline-notice--error" role="alert">{stale ? "This Product changed after you opened it. Reload the latest version before saving." : error}{stale && <button type="button" onClick={() => void reloadLatest()} disabled={reloading}>{reloading ? "Reloading…" : "Reload latest"}</button>}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save Classification"}</button></div>
    </form>
  </CatalogDialog>;
}

/**
 * Only active records are offered for a new assignment, but a record already assigned and since
 * archived stays selectable so saving an unrelated change cannot silently clear it.
 */
function ReferenceSelect({ label, name, value, onChange, options, assigned, loading }: {
  label: string; name: string; value: string; onChange: (value: string) => void;
  options: ReferenceMasterResponse[] | undefined; assigned: string | null | undefined; loading: boolean;
}) {
  const id = `catalog-${name}`;
  const selectable = (options ?? []).filter((option) => option.status === "active" || option.id === assigned);
  return <div className="field">
    <label htmlFor={id}>{label}</label>
    <select id={id} name={name} value={value} onChange={(event) => onChange(event.target.value)} disabled={loading}>
      <option value="">{loading ? "Loading…" : `No ${label} selected`}</option>
      {selectable.map((option) => <option key={option.id} value={option.id}>{referenceText(option)}{option.status === "archived" ? " (archived)" : ""}</option>)}
    </select>
    {!loading && selectable.length === 0 && <small>No {label} records exist yet. Add them in Reference Data.</small>}
  </div>;
}

function referenceText(record: ReferenceMasterResponse): string {
  const attributes = record.attributes as Record<string, unknown>;
  if (record.kind === "hsn-codes") return `${String(attributes.hsnCode)} · ${String(attributes.description)}`;
  return `${String(attributes.categoryCode)} · ${String(attributes.displayName)}`;
}

function referenceLabel(
  options: ReferenceMasterResponse[] | undefined,
  id: string | null | undefined,
  failed: boolean,
  loading: boolean
): string {
  if (!id) return "Not classified";
  if (!options) return failed ? "Name unavailable" : loading ? "Loading…" : id;
  const match = options.find((option) => option.id === id);
  if (!match) return "Name unavailable";
  return `${referenceText(match)}${match.status === "archived" ? " (archived)" : ""}`;
}


/** This PC's calendar date, matching how the Phase 1D ledger derives its business date. */
function businessToday(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}
