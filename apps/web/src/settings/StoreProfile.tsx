import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  GstRegistrationStatus,
  ReferenceMasterResponse,
  StoreTaxIdentity
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { listReferences } from "../reference/referenceApi";
import { getStoreTaxIdentity, updateStoreTaxIdentity } from "./storeApi";

const REGISTRATION_LABELS: Record<GstRegistrationStatus, string> = {
  registered: "Registered",
  unregistered: "Not registered",
  unknown: "Not recorded yet"
};

/**
 * Phase 1G-0 Store Profile.
 *
 * The Store's place of supply is one half of the comparison that decides whether a purchase is
 * taxed CGST + SGST or IGST. Until it is recorded, a GST-aware document cannot be posted, so this
 * screen says so plainly rather than letting the operator discover it at the counter.
 */
export function StoreProfilePage() {
  usePageTitle("Store Profile");
  const auth = useAuth();
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState(false);
  const canMutate = auth.status?.user?.role === "owner_admin";

  const profile = useQuery({ queryKey: ["store", "tax-identity"], queryFn: getStoreTaxIdentity, retry: false });
  // Archived States are included so one assigned before it was archived still resolves to a label.
  const states = useQuery({
    queryKey: ["reference", "state-codes", "all"],
    queryFn: () => listReferences("state-codes", "", "all"),
    staleTime: 30_000,
    retry: false
  });
  useExpireOnAuthError(profile.error);

  return <>
    <header className="page-header">
      <div><p className="eyebrow">CONFIGURATION</p><h1>Store Profile</h1><p>Your own GST registration and place of supply. Purchase documents compare this with the supplier's State to decide the tax treatment.</p></div>
      {canMutate && !profile.isPending && !profile.isError && <button className="button button--primary" type="button" onClick={() => setEditing(true)}>Edit Tax Identity</button>}
      {!canMutate && <span className="read-only-note">Read-only access</span>}
    </header>

    <section className="master-panel" aria-labelledby="store-tax-title">
      <h2 id="store-tax-title">Tax Identity</h2>
      {profile.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading store profile…</b></div>
        : profile.isError ? <div className="empty-state" role="alert"><h3>Store profile could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as an unrecorded profile.</p><button className="button button--secondary" type="button" onClick={() => void profile.refetch()}>Retry</button></div>
        : <>
          {states.isError && <div className="catalog-inline-error" role="alert"><span>State names could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void states.refetch()}>Retry</button></div>}
          <dl className="detail-grid">
            <div><dt>Store</dt><dd>{profile.data.displayName}</dd></div>
            <div><dt>GST registration</dt><dd>{REGISTRATION_LABELS[profile.data.gstRegistrationStatus]}</dd></div>
            <div><dt>GSTIN</dt><dd>{profile.data.normalizedGstin ?? "—"}</dd></div>
            <div><dt>Place of supply</dt><dd>{stateLabel(states.data, profile.data.placeOfSupplyStateId, states.isError, states.isPending)}</dd></div>
          </dl>
          {profile.data.complete
            ? <p className="panel-note">Purchase documents can determine their tax treatment from this State.</p>
            : <div className="panel-callout" role="status"><strong>Place of supply not recorded</strong><small>Until a State is recorded here, a GST-aware purchase cannot decide between CGST and SGST or IGST, and posting one will be refused. Nothing else in the application is affected.</small></div>}
        </>}
    </section>

    {editing && profile.data && <TaxIdentityDialog current={profile.data} states={states.data} onClose={() => setEditing(false)} onSaved={() => { setEditing(false); void queryClient.invalidateQueries({ queryKey: ["store", "tax-identity"] }); }} />}
  </>;
}

function TaxIdentityDialog({ current, states, onClose, onSaved }: {
  current: StoreTaxIdentity;
  states: ReferenceMasterResponse[] | undefined;
  onClose: () => void;
  onSaved: () => void;
}) {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<GstRegistrationStatus>(current.gstRegistrationStatus);
  const [gstin, setGstin] = useState(current.gstin ?? "");
  const [stateId, setStateId] = useState(current.placeOfSupplyStateId ?? "");
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const registered = status === "registered";
  const normalized = gstin.replace(/\s+/g, "").toUpperCase();

  const mutation = useMutation({
    mutationFn: () => updateStoreTaxIdentity(current.revision, {
      gstRegistrationStatus: status,
      // A GSTIN is sent only when the status claims one, so the two can never disagree.
      gstin: registered ? gstin.trim() || null : null,
      placeOfSupplyStateId: stateId || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The store profile could not be saved.")
  });
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  const reloadLatest = async () => {
    setReloading(true);
    try {
      const latest = await getStoreTaxIdentity();
      queryClient.setQueryData(["store", "tax-identity"], latest);
      setStatus(latest.gstRegistrationStatus);
      setGstin(latest.gstin ?? "");
      setStateId(latest.placeOfSupplyStateId ?? "");
      setError(null);
      onSaved();
    } finally { setReloading(false); }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (registered && normalized.length !== 15) { setError("A GSTIN is 15 characters."); return; }
    if (registered && !stateId) { setError("Select the State this registration belongs to."); return; }
    mutation.mutate();
  };

  const selectable = (states ?? []).filter((state) => state.status === "active" || state.id === current.placeOfSupplyStateId);

  return <CatalogDialog title="Edit Store Tax Identity" description="Your registration status, GSTIN, and the State you supply from." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="store-status">GST registration</label>
        <select id="store-status" value={status} onChange={(event) => setStatus(event.target.value as GstRegistrationStatus)}>
          <option value="unknown">Not recorded yet</option>
          <option value="registered">Registered</option>
          <option value="unregistered">Not registered</option>
        </select>
        <small>“Not registered” states this store has no GSTIN; “not recorded yet” means nobody has captured it.</small>
      </div>
      {registered && <div className="field">
        <label htmlFor="store-gstin">GSTIN<span aria-hidden="true"> *</span></label>
        <input id="store-gstin" value={gstin} onChange={(event) => setGstin(event.target.value)} aria-describedby="store-gstin-help" />
        <small id="store-gstin-help">{normalized.length === 15 ? `Will be recorded as ${normalized}` : "15 characters. The check digit is verified before saving."}</small>
      </div>}
      <div className="field">
        <label htmlFor="store-state">Place of supply (State){registered && <span aria-hidden="true"> *</span>}</label>
        <select id="store-state" value={stateId} onChange={(event) => setStateId(event.target.value)} disabled={!states}>
          <option value="">{states ? "No State selected" : "Loading…"}</option>
          {selectable.map((state) => <option key={state.id} value={state.id}>{stateText(state)}{state.status === "archived" ? " (archived)" : ""}</option>)}
        </select>
      </div>
      {error && <div className="inline-notice inline-notice--error" role="alert">{stale ? "The store profile changed after you opened it. Reload the latest version before saving." : error}{stale && <button type="button" onClick={() => void reloadLatest()} disabled={reloading}>{reloading ? "Reloading…" : "Reload latest"}</button>}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save Tax Identity"}</button></div>
    </form>
  </CatalogDialog>;
}

function stateText(state: ReferenceMasterResponse): string {
  const attributes = state.attributes as Record<string, unknown>;
  return `${String(attributes.stateCode)} · ${String(attributes.displayName)}`;
}

function stateLabel(states: ReferenceMasterResponse[] | undefined, id: string | null, failed: boolean, loading: boolean): string {
  if (!id) return "Not recorded";
  if (!states) return failed ? "State unavailable" : loading ? "Loading…" : id;
  const match = states.find((state) => state.id === id);
  if (!match) return "State unavailable";
  return `${stateText(match)}${match.status === "archived" ? " (archived)" : ""}`;
}

function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
