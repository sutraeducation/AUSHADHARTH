import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  Comparability,
  PriceControlStatus,
  ProductDetail,
  ReferenceMasterResponse
} from "@aushadharth/contracts";
import { businessToday } from "../platform/businessDate";
import { LocalServiceError } from "../platform/localService";
import { listReferences } from "../reference/referenceApi";
import { CatalogDialog } from "./CatalogDialog";
import { getPriceControl, paiseToRupees, updatePriceControl } from "./productApi";

const STATUS_LABELS: Record<PriceControlStatus, string> = {
  unknown: "Not assessed yet",
  not_applicable: "Not price-controlled",
  controlled: "Price-controlled"
};

const COMPARABILITY_NOTES: Record<Comparability, string> = {
  comparable: "This ceiling can be compared with a selling rate.",
  incomparable_pack_basis:
    "This ceiling is quoted per pack. Which pack is not stated, so it cannot be compared with a per-unit rate — dividing it by an assumed pack size would invent a legal figure.",
  incomparable_unit:
    "This ceiling is quoted in a different unit than this product's base unit. No conversion is attempted, because converting between units of different kinds cannot be done safely."
};

/**
 * Phase 1H-0 Product price control.
 *
 * The Product asserts whether it is price-controlled and, when it is, which notified formulation its
 * ceiling comes from. The ceiling shown is resolved by the Store Service for a specific date and is
 * labelled as such, because a ceiling belongs to the formulation's version history and never to the
 * Product. There is deliberately no ceiling-price field here.
 *
 * The ceiling is **not** the Batch MRP: a ceiling is a notified maximum exclusive of GST, while an
 * MRP is a lot's printed price inclusive of it. The panel keeps them visibly separate.
 */
export function PriceControlSection({ product, canMutate }: { product: ProductDetail; canMutate: boolean }) {
  const [editing, setEditing] = useState(false);
  const queryClient = useQueryClient();
  // This PC's calendar date, for the same reason Phase 1F uses it: the service falls back to UTC,
  // which is a day behind in India for the first hours of every business day.
  const asOf = businessToday();
  const control = useQuery({
    queryKey: ["product-price-control", product.id, asOf],
    queryFn: () => getPriceControl(product.id, asOf),
    retry: false
  });
  // Archived formulations are included so one assigned before it was archived still resolves to a
  // real name instead of a raw identifier.
  const formulations = useQuery({
    queryKey: ["catalog", "controlled-formulations"],
    queryFn: () => listReferences("controlled-formulations", "", "all"),
    staleTime: 30_000,
    retry: false
  });

  return <section className="master-panel" aria-labelledby="price-control-title">
    <div className="panel-header">
      <div>
        <h2 id="price-control-title">Price Control</h2>
        <p>Whether a notified ceiling price applies to this medicine. Separate from the printed MRP on a batch.</p>
      </div>
      {canMutate && !control.isPending && !control.isError && <button className="button button--secondary" type="button" onClick={() => setEditing(true)}>Edit Price Control</button>}
    </div>

    {control.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading price control…</b></div>
      : control.isError ? <div className="empty-state" role="alert"><h3>Price control could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as a product without price control.</p><button className="button button--secondary" type="button" onClick={() => void control.refetch()}>Retry</button></div>
      : <>
        <dl className="detail-grid">
          <div><dt>Applicability</dt><dd>{STATUS_LABELS[control.data.priceControlStatus]}</dd></div>
          <div><dt>Controlled formulation</dt><dd>{formulationLabel(formulations.data, control.data.controlledFormulationId, formulations.isError, formulations.isPending)}</dd></div>
          {control.data.applicableCeiling && <>
            <div><dt>Ceiling price</dt><dd>{paiseToRupees(control.data.applicableCeiling.ceilingPricePaise)} {basisLabel(control.data.applicableCeiling.ceilingBasis)}</dd></div>
            <div><dt>Notification</dt><dd>{control.data.applicableCeiling.notificationReference ?? "Not recorded"}</dd></div>
            <div><dt>In force</dt><dd>{control.data.applicableCeiling.effectiveFrom} → {control.data.applicableCeiling.effectiveTo ?? "Open ended"}</dd></div>
          </>}
        </dl>

        {control.data.priceControlStatus === "unknown" && <div className="panel-callout" role="status">
          <strong>Nobody has assessed this product yet</strong>
          <small>This is not the same as saying it is uncontrolled. Until someone records an assessment, a future sale cannot know whether a notified ceiling applies, and will say so rather than assume one does not.</small>
        </div>}

        {control.data.priceControlStatus === "controlled" && !control.data.applicableCeiling && <div className="panel-callout" role="alert">
          <strong>No ceiling is in force on {asOf}</strong>
          <small>This product is marked price-controlled, but its formulation has no effective ceiling version covering today. A future sale must refuse rather than treat the product as unconstrained. Add the notified version under Reference Data · Medicine Price Control.</small>
        </div>}

        {control.data.comparability && control.data.comparability !== "comparable" && <div className="panel-callout" role="alert">
          <strong>This ceiling cannot be compared with a selling rate</strong>
          <small>{COMPARABILITY_NOTES[control.data.comparability]}</small>
        </div>}

        <p className="panel-note">
          The ceiling is resolved for {asOf} and belongs to the formulation's version history, never to this product.
          A notified ceiling is exclusive of GST; a batch's MRP is the printed price including it. The two are different facts and are never compared with each other.
        </p>
      </>}

    {editing && control.data && <PriceControlDialog
      product={product}
      current={control.data}
      formulations={formulations.data}
      onClose={() => setEditing(false)}
      onSaved={() => { setEditing(false); void queryClient.invalidateQueries({ queryKey: ["product-price-control", product.id] }); void queryClient.invalidateQueries({ queryKey: ["catalog", "product", product.id] }); }}
    />}
  </section>;
}

function PriceControlDialog({ product, current, formulations, onClose, onSaved }: {
  product: ProductDetail;
  current: { revision: number; priceControlStatus: PriceControlStatus; controlledFormulationId: string | null };
  formulations: ReferenceMasterResponse[] | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [status, setStatus] = useState<PriceControlStatus>(current.priceControlStatus);
  const [formulationId, setFormulationId] = useState(current.controlledFormulationId ?? "");
  const [notice, setNotice] = useState<string | null>(null);
  const controlled = status === "controlled";

  const save = useMutation({
    mutationFn: () => updatePriceControl(product.id, current.revision, {
      priceControlStatus: status,
      // A formulation is sent only when the status claims one, so the two can never disagree.
      controlledFormulationId: controlled ? formulationId || null : null
    }),
    onSuccess: onSaved,
    onError: (caught) => setNotice(caught instanceof LocalServiceError ? caught.message : "Price control could not be saved.")
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setNotice(null);
    if (controlled && !formulationId) {
      setNotice("Select the notified formulation this product's ceiling comes from.");
      document.getElementById("price-control-formulation")?.focus();
      return;
    }
    save.mutate();
  };

  const selectable = (formulations ?? []).filter((item) => item.status === "active" || item.id === current.controlledFormulationId);

  return <CatalogDialog title="Edit Price Control" description="Whether a notified ceiling applies to this medicine, and which formulation it comes from." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="price-control-status">Applicability</label>
        <select id="price-control-status" value={status} onChange={(event) => setStatus(event.target.value as PriceControlStatus)}>
          <option value="unknown">Not assessed yet</option>
          <option value="not_applicable">Not price-controlled</option>
          <option value="controlled">Price-controlled</option>
        </select>
        <small>Being a medicine does not make a product price-controlled. “Not assessed yet” means nobody has checked; it is never read as “not controlled”.</small>
      </div>
      {controlled && <div className="field">
        <label htmlFor="price-control-formulation">Controlled formulation<span aria-hidden="true"> *</span></label>
        <select id="price-control-formulation" value={formulationId} onChange={(event) => setFormulationId(event.target.value)} disabled={!formulations}>
          <option value="">{formulations ? "Select a formulation" : "Loading…"}</option>
          {selectable.map((item) => <option key={item.id} value={item.id}>{formulationText(item)}{item.status === "archived" ? " (archived)" : ""}</option>)}
        </select>
        <small>Chosen deliberately. A product is never matched to a notification by its name.</small>
      </div>}
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{notice}</div>}
      <div className="form-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={save.isPending}>{save.isPending ? "Saving…" : "Save Price Control"}</button>
      </div>
    </form>
  </CatalogDialog>;
}

function basisLabel(basis: string): string {
  return basis === "per_base_unit" ? "per base unit" : "per pack";
}

function formulationText(record: ReferenceMasterResponse): string {
  const attributes = record.attributes as Record<string, unknown>;
  const strength = attributes.strengthText ? ` · ${String(attributes.strengthText)}` : "";
  return `${String(attributes.formulationCode)} · ${String(attributes.displayName)}${strength}`;
}

function formulationLabel(
  records: ReferenceMasterResponse[] | undefined,
  id: string | null,
  failed: boolean,
  loading: boolean
): string {
  if (!id) return "Not assigned";
  if (!records) return failed ? "Formulation unavailable" : loading ? "Loading…" : id;
  const match = records.find((record) => record.id === id);
  if (!match) return "Formulation unavailable";
  return `${formulationText(match)}${match.status === "archived" ? " (archived)" : ""}`;
}
