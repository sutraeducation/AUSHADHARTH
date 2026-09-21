import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  RegulatorySchemeSchema,
  type ProductDetail,
  type RegulatoryClassification,
  type RegulatoryScheme
} from "@aushadharth/contracts";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import {
  ANSWER_LABELS,
  GATE_LABELS,
  SCHEME_LABELS,
  archiveClassification,
  closeClassification,
  createClassification,
  getProductRegulatory
} from "./regulatoryApi";

/**
 * Phase 1M-A — a product's position under the Drugs Rules.
 *
 * Every answer on this panel was recorded by a person with a citation, or it says "Unknown". There
 * is no checkbox whose empty state could be read as "not scheduled": saying a product is OUTSIDE a
 * schedule is a finding, and it is recorded exactly as saying it is inside one — with a date and an
 * authority. Nothing here is inferred from the product's name, brand, HSN or dosage form.
 */
export function RegulatoryClassificationSection({ product, canMutate }: { product: ProductDetail; canMutate: boolean }) {
  const asOf = businessToday();
  const queryClient = useQueryClient();
  const [recording, setRecording] = useState(false);
  const [closing, setClosing] = useState<RegulatoryClassification | null>(null);
  const [archiving, setArchiving] = useState<RegulatoryClassification | null>(null);
  const regulatory = useQuery({
    queryKey: ["product-regulatory", product.id, asOf],
    queryFn: () => getProductRegulatory(product.id, asOf),
    retry: false
  });
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["product-regulatory", product.id] });

  return <section className="master-panel" aria-labelledby="regulatory-title">
    <div className="panel-header">
      <div>
        <h2 id="regulatory-title">Drugs Rules Classification</h2>
        <p>Whether this product is within Schedule H, H1, X, C or C(1), recorded with its authority and the dates it governs. A Sale is judged by the classification in force on its own business date.</p>
      </div>
      {canMutate && product.status === "active" && regulatory.data && <button className="button button--secondary" type="button" onClick={() => setRecording(true)}>Record Finding</button>}
    </div>

    {regulatory.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading classification…</b></div>
      : regulatory.isError ? <div className="empty-state" role="alert"><h3>Classification could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as an unclassified product.</p><button className="button button--secondary" type="button" onClick={() => void regulatory.refetch()}>Retry</button></div>
      : <>
        <div className={`regulatory-gate regulatory-gate--${regulatory.data.saleGate}`} role="status" data-testid="regulatory-gate">
          <strong>{GATE_LABELS[regulatory.data.saleGate]}</strong>
          <small>{gateExplanation(regulatory.data.saleGate, regulatory.data.saleGateScheme, product.productKind)}</small>
        </div>
        <dl className="detail-grid" aria-label={`Position on ${regulatory.data.resolvedOn}`}>
          {regulatory.data.resolved.map((entry) => <div key={entry.scheme}>
            <dt>{SCHEME_LABELS[entry.scheme]}</dt>
            <dd><span className={`regulatory-answer regulatory-answer--${entry.answer}`}>{ANSWER_LABELS[entry.answer]}</span></dd>
          </div>)}
        </dl>
        <p className="panel-note">Position in force on {regulatory.data.resolvedOn}.</p>

        {regulatory.data.classifications.length === 0
          ? <p className="panel-note">No finding has been recorded for this product.</p>
          : <div className="table-scroll"><table className="data-table">
              <thead><tr><th scope="col">Scheme</th><th scope="col">Finding</th><th scope="col">In force</th><th scope="col">Authority</th><th scope="col">Status</th>{canMutate && <th scope="col"><span className="visually-hidden">Actions</span></th>}</tr></thead>
              <tbody>
                {regulatory.data.classifications.map((finding) => <tr key={finding.id}>
                  <td>{SCHEME_LABELS[finding.scheme]}</td>
                  <td>{finding.applies ? "Applies" : "Does not apply"}</td>
                  <td>{finding.effectiveFrom} to {finding.effectiveTo ? `${finding.effectiveTo} (exclusive)` : "further notice"}</td>
                  <td><span className="regulatory-source">{finding.sourceCitation}</span>{finding.reason && <small>{finding.reason}</small>}</td>
                  <td><span className={`status-badge status-badge--${finding.status}`}>{finding.status === "active" ? "Active" : "Archived"}</span></td>
                  {canMutate && <td className="table-actions">{finding.status === "active" && <>
                    {finding.effectiveTo === null && <button className="button button--secondary" type="button" onClick={() => setClosing(finding)}>End</button>}
                    <button className="button button--secondary" type="button" onClick={() => setArchiving(finding)}>Archive</button>
                  </>}</td>}
                </tr>)}
              </tbody>
            </table></div>}
      </>}

    {recording && <RecordFindingDialog productId={product.id} onClose={() => setRecording(false)} onSaved={() => { setRecording(false); refresh(); }} />}
    {closing && <CloseFindingDialog productId={product.id} finding={closing} onClose={() => setClosing(null)} onSaved={() => { setClosing(null); refresh(); }} />}
    {archiving && <ArchiveFindingDialog productId={product.id} finding={archiving} onClose={() => setArchiving(null)} onSaved={() => { setArchiving(null); refresh(); }} />}
  </section>;
}

function gateExplanation(gate: string, scheme: RegulatoryScheme | null, kind: string): string {
  if (gate === "workflow_unavailable" && scheme) {
    return `${SCHEME_LABELS[scheme]} applies. Its prescription or register requirements are not available in this version, so the counter cannot sell it yet.`;
  }
  if (gate === "unresolved") {
    return "A medicine cannot be sold until its position under Schedules H, H1, X, C and C(1) is recorded.";
  }
  return kind === "medicine"
    ? "Recorded outside every schedule that restricts a counter sale."
    : "Not a medicine, and not recorded within a schedule that restricts a counter sale.";
}

function RecordFindingDialog({ productId, onClose, onSaved }: { productId: string; onClose: () => void; onSaved: () => void }) {
  const [scheme, setScheme] = useState<RegulatoryScheme | "">("");
  // No default. An untouched control must never become a legal finding in either direction.
  const [applies, setApplies] = useState<"yes" | "no" | "">("");
  const [effectiveFrom, setEffectiveFrom] = useState("");
  const [effectiveTo, setEffectiveTo] = useState("");
  const [sourceCitation, setSourceCitation] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => createClassification(productId, {
      scheme: scheme as RegulatoryScheme,
      applies: applies === "yes",
      effectiveFrom,
      effectiveTo: effectiveTo || null,
      sourceCitation,
      reason: reason || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = scheme !== "" && applies !== "" && effectiveFrom !== "" && sourceCitation.trim().length >= 3;
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };

  return <CatalogDialog title="Record Finding" description="Record whether this product is inside or outside one scheme, from a date, on a named authority." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="finding-scheme">Scheme</label>
        <select id="finding-scheme" value={scheme} onChange={(event) => setScheme(event.target.value as RegulatoryScheme)} required>
          <option value="">Choose a scheme</option>
          {RegulatorySchemeSchema.options.map((option) => <option key={option} value={option}>{SCHEME_LABELS[option]}</option>)}
        </select>
      </div>
      <fieldset className="field">
        <legend>Finding</legend>
        <label className="choice"><input type="radio" name="finding-applies" value="yes" checked={applies === "yes"} onChange={() => setApplies("yes")} /> Applies — this product is within the scheme</label>
        <label className="choice"><input type="radio" name="finding-applies" value="no" checked={applies === "no"} onChange={() => setApplies("no")} /> Does not apply — this product is outside the scheme</label>
      </fieldset>
      <div className="field">
        <label htmlFor="finding-from">In force from</label>
        <input id="finding-from" type="date" value={effectiveFrom} onChange={(event) => setEffectiveFrom(event.target.value)} required />
      </div>
      <div className="field">
        <label htmlFor="finding-to">Until (optional)</label>
        <input id="finding-to" type="date" value={effectiveTo} onChange={(event) => setEffectiveTo(event.target.value)} />
        <small>Leave empty while the finding is still in force. The finding governs dates before this one.</small>
      </div>
      <div className="field">
        <label htmlFor="finding-source">Authority</label>
        <input id="finding-source" value={sourceCitation} onChange={(event) => setSourceCitation(event.target.value)} placeholder="e.g. G.S.R. 377(E) dated 13 May 2026" required maxLength={300} />
        <small>The notification or rule this finding rests on. A finding without an authority is not accepted.</small>
      </div>
      <div className="field">
        <label htmlFor="finding-reason">Note (optional)</label>
        <input id="finding-reason" value={reason} onChange={(event) => setReason(event.target.value)} maxLength={500} />
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>{mutation.isPending ? "Saving…" : "Record Finding"}</button>
      </div>
    </form>
  </CatalogDialog>;
}

function CloseFindingDialog({ productId, finding, onClose, onSaved }: { productId: string; finding: RegulatoryClassification; onClose: () => void; onSaved: () => void }) {
  const [effectiveTo, setEffectiveTo] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => closeClassification(productId, finding.id, { expectedRevision: finding.revision, effectiveTo, reason }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); mutation.mutate(); };
  return <CatalogDialog title={`End ${SCHEME_LABELS[finding.scheme]} Finding`} description="For a change in the law. The finding keeps answering for every date before the end date, so earlier Sales are judged exactly as before." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="close-to">Ends on</label>
        <input id="close-to" type="date" value={effectiveTo} onChange={(event) => setEffectiveTo(event.target.value)} required />
      </div>
      <div className="field">
        <label htmlFor="close-reason">Reason</label>
        <input id="close-reason" value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} />
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!effectiveTo || !reason.trim() || mutation.isPending}>End Finding</button>
      </div>
    </form>
  </CatalogDialog>;
}

function ArchiveFindingDialog({ productId, finding, onClose, onSaved }: { productId: string; finding: RegulatoryClassification; onClose: () => void; onSaved: () => void }) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => archiveClassification(productId, finding.id, { expectedRevision: finding.revision, reason }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); mutation.mutate(); };
  return <CatalogDialog title="Archive Finding" description="Only for a finding recorded in error. An archived finding answers for no date at all. To record a change in the law, end the finding instead." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="archive-reason">Reason</label>
        <input id="archive-reason" value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} />
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--danger" type="submit" disabled={!reason.trim() || mutation.isPending}>Archive Finding</button>
      </div>
    </form>
  </CatalogDialog>;
}

function describe(caught: unknown): string {
  if (caught instanceof LocalServiceError) {
    if (caught.code === "regulatory_period_overlaps") return "Another finding already governs this scheme over part of that period. End it first.";
    if (caught.code === "authorization_denied") return "Only an owner can record a classification.";
    return caught.message;
  }
  return "The finding could not be saved.";
}

function businessToday(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
}
