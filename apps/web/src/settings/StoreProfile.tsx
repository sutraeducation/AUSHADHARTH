import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type {
  GstRegistrationStatus,
  ReferenceMasterResponse,
  StoreLicence,
  StoreProfile
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { listReferences } from "../reference/referenceApi";
import { updateStoreTaxIdentity } from "./storeApi";
import {
  archiveStoreLicence,
  createStoreLicence,
  getStoreProfile,
  restoreStoreLicence,
  updateStoreAddress,
  updateStoreIdentity,
  updateStoreLicence
} from "./storeProfileApi";

const REGISTRATION_LABELS: Record<GstRegistrationStatus, string> = {
  registered: "Registered",
  unregistered: "Not registered",
  unknown: "Not recorded yet"
};

const PROFILE_KEY = ["store", "profile"] as const;

/**
 * Store Profile — who this pharmacy is.
 *
 * Phase 1G-0 recorded the Store's place of supply, which is what a GST-aware purchase needs. This
 * screen adds the rest of the seller, and the reason is narrower than "completeness": a pharmacy may
 * not hand a customer a bill without its own name, its address and its drug sale licence number on
 * it, so until those three are recorded here a Sale cannot be posted at all.
 *
 * The screen says that plainly rather than making somebody discover it at the counter.
 */
export function StoreProfilePage() {
  usePageTitle("Store Profile");
  const auth = useAuth();
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState<"identity" | "address" | "tax" | null>(null);
  const canMutate = auth.status?.user?.role === "owner_admin";

  const profile = useQuery({ queryKey: PROFILE_KEY, queryFn: getStoreProfile, retry: false });
  // Archived States are included so one assigned before it was archived still resolves to a label.
  const states = useQuery({
    queryKey: ["reference", "state-codes", "all"],
    queryFn: () => listReferences("state-codes", "", "all"),
    staleTime: 30_000,
    retry: false
  });
  useExpireOnAuthError(profile.error);
  const saved = (next: StoreProfile) => {
    queryClient.setQueryData(PROFILE_KEY, next);
    setEditing(null);
  };

  return <>
    <header className="page-header">
      <div><p className="eyebrow">CONFIGURATION</p><h1>Store Profile</h1><p>Who this pharmacy is, as it appears on every bill you issue and on your GST documents.</p></div>
      {!canMutate && <span className="read-only-note">Read-only access</span>}
    </header>

    {profile.isPending ? <section className="master-panel"><div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading store profile…</b></div></section>
      : profile.isError ? <section className="master-panel"><div className="empty-state" role="alert"><h3>Store profile could not be loaded</h3><p>The Local Store Service did not complete this request. This is not the same as an unrecorded profile.</p><button className="button button--secondary" type="button" onClick={() => void profile.refetch()}>Retry</button></div></section>
      : <>
        {!profile.data.sellerComplete && <div className="panel-callout panel-callout--blocking" role="status">
          <strong>Sales cannot be posted yet</strong>
          <small>A pharmacy bill must carry the seller's name, address and drug sale licence number. Record the following before selling — nothing else in the application is affected.</small>
          <ul>{profile.data.missingSellerFacts.map((fact) => <li key={fact.field}>{fact.message}</li>)}</ul>
        </div>}

        <section className="master-panel" aria-labelledby="store-identity-title">
          <div className="panel-heading">
            <h2 id="store-identity-title">Business Identity</h2>
            {canMutate && <button className="button button--secondary" type="button" onClick={() => setEditing("identity")}>Edit Identity</button>}
          </div>
          <dl className="detail-grid">
            <div><dt>Trading name</dt><dd>{profile.data.displayName}</dd></div>
            <div><dt>Registered name</dt><dd>{profile.data.legalName ?? <span className="row-subtext">Not recorded</span>}</dd></div>
          </dl>
          <p className="panel-note">The registered name is the one on your GST certificate and drug licence. It is often different from the name above the shop, and both are kept.</p>
        </section>

        <section className="master-panel" aria-labelledby="store-address-title">
          <div className="panel-heading">
            <h2 id="store-address-title">Address</h2>
            {canMutate && <button className="button button--secondary" type="button" onClick={() => setEditing("address")}>{profile.data.address ? "Edit Address" : "Record Address"}</button>}
          </div>
          {profile.data.address
            ? <dl className="detail-grid">
              <div><dt>Address</dt><dd>{[profile.data.address.line1, profile.data.address.line2].filter(Boolean).join(", ")}</dd></div>
              <div><dt>Town / city</dt><dd>{profile.data.address.city ?? "—"}</dd></div>
              <div><dt>State</dt><dd>{stateLabel(states.data, profile.data.address.stateId, states.isError, states.isPending)}</dd></div>
              <div><dt>PIN code</dt><dd>{profile.data.address.postalCode ?? "—"}</dd></div>
            </dl>
            : <div className="empty-state"><h3>No address recorded</h3><p>The address of the premises you supply from. It appears on every bill.</p></div>}
        </section>

        <section className="master-panel" aria-labelledby="store-contact-title">
          <h2 id="store-contact-title">Contact</h2>
          <dl className="detail-grid">
            <div><dt>Phone</dt><dd>{profile.data.primaryPhone ?? <span className="row-subtext">Not recorded</span>}</dd></div>
            <div><dt>Email</dt><dd>{profile.data.primaryEmail ?? <span className="row-subtext">Not recorded</span>}</dd></div>
          </dl>
          <p className="panel-note">Optional. A customer who needs to come back about a medicine has to be able to reach you.</p>
        </section>

        <section className="master-panel" aria-labelledby="store-tax-title">
          <div className="panel-heading">
            <h2 id="store-tax-title">GST</h2>
            {canMutate && <button className="button button--secondary" type="button" onClick={() => setEditing("tax")}>Edit Tax Identity</button>}
          </div>
          <dl className="detail-grid">
            <div><dt>GST registration</dt><dd>{REGISTRATION_LABELS[profile.data.gstRegistrationStatus]}</dd></div>
            <div><dt>GSTIN</dt><dd>{profile.data.normalizedGstin ?? "—"}</dd></div>
            <div><dt>Place of supply</dt><dd>{stateLabel(states.data, profile.data.placeOfSupplyStateId, states.isError, states.isPending)}</dd></div>
          </dl>
          {profile.data.taxComplete
            ? <p className="panel-note">Purchase documents can determine their tax treatment from this State.</p>
            : <div className="panel-callout" role="status"><strong>Place of supply not recorded</strong><small>Until a State is recorded here, a GST-aware purchase cannot decide between CGST and SGST or IGST, and posting one will be refused. Nothing else in the application is affected.</small></div>}
        </section>

        <LicencePanel profile={profile.data} canMutate={canMutate} onSaved={saved} />
      </>}

    {states.isError && <div className="catalog-inline-error" role="alert"><span>State names could not be loaded.</span><button className="button button--secondary" type="button" onClick={() => void states.refetch()}>Retry</button></div>}

    {editing === "identity" && profile.data && <IdentityDialog current={profile.data} onClose={() => setEditing(null)} onSaved={saved} />}
    {editing === "address" && profile.data && <AddressDialog current={profile.data} states={states.data} onClose={() => setEditing(null)} onSaved={saved} />}
    {editing === "tax" && profile.data && <TaxIdentityDialog current={profile.data} states={states.data} onClose={() => setEditing(null)} onSaved={saved} />}
  </>;
}

// ---------------------------------------------------------------------------------------------
// Pharmacy licences
// ---------------------------------------------------------------------------------------------

function LicencePanel({ profile, canMutate, onSaved }: {
  profile: StoreProfile;
  canMutate: boolean;
  onSaved: (next: StoreProfile) => void;
}) {
  const [editing, setEditing] = useState<StoreLicence | "new" | null>(null);
  const [notice, setNotice] = useState("");
  const active = profile.licences.filter((licence) => licence.status === "active");

  const lifecycle = useMutation({
    mutationFn: ({ licence, restore }: { licence: StoreLicence; restore: boolean }) =>
      restore
        ? restoreStoreLicence(licence.id, licence.revision)
        : archiveStoreLicence(licence.id, licence.revision, "Archived from Store Profile"),
    onSuccess: (next) => { setNotice(""); onSaved(next); },
    onError: (caught) => setNotice(caught instanceof LocalServiceError ? caught.message : "That licence could not be changed.")
  });

  return <section className="master-panel" aria-labelledby="store-licences-title">
    <div className="panel-heading">
      <h2 id="store-licences-title">Pharmacy Licences</h2>
      {canMutate && <button className="button button--secondary" type="button" onClick={() => setEditing("new")}>Add Licence</button>}
    </div>
    {notice && <div className="catalog-inline-error" role="alert"><span>{notice}</span></div>}
    {profile.licences.length === 0
      ? <div className="empty-state"><h3>No licence recorded</h3><p>Your drug sale licence number must appear on every bill you issue. Record at least one before selling.</p></div>
      : <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">Type</th><th scope="col">Number</th><th scope="col">Valid</th><th scope="col">Status</th>{canMutate && <th scope="col"><span className="sr-only">Actions</span></th>}</tr></thead>
        <tbody>{profile.licences.map((licence) => <tr key={licence.id}>
          <td data-label="Type">{licence.licenceType}</td>
          <td data-label="Number"><code>{licence.licenceNumber}</code>{licence.issuingAuthority && <small className="row-subtext">{licence.issuingAuthority}</small>}</td>
          <td data-label="Valid">{validity(licence)}</td>
          <td data-label="Status"><span className={`status-badge status-badge--${licence.status}`}>{licence.status === "active" ? "Active" : "Archived"}</span></td>
          {canMutate && <td className="row-actions">
            {licence.status === "active"
              ? <>
                <button className="button button--secondary" type="button" onClick={() => setEditing(licence)}>Edit</button>
                <button className="button button--secondary" type="button" disabled={lifecycle.isPending} onClick={() => lifecycle.mutate({ licence, restore: false })}>Archive</button>
              </>
              : <button className="button button--secondary" type="button" disabled={lifecycle.isPending} onClick={() => lifecycle.mutate({ licence, restore: true })}>Restore</button>}
          </td>}
        </tr>)}</tbody>
      </table></div>}
    <p className="panel-note">{active.length === 0
      ? "An archived licence does not appear on a bill. At least one active licence is needed to sell."
      : `Every active licence is printed on the bill, in a fixed order. ${active.length === 1 ? "One licence is" : `${active.length} licences are`} active.`}</p>

    {editing && <LicenceDialog
      current={editing === "new" ? null : editing}
      onClose={() => setEditing(null)}
      onSaved={(next) => { setEditing(null); onSaved(next); }}
    />}
  </section>;
}

function validity(licence: StoreLicence): string {
  if (!licence.validFrom && !licence.validUpto) return "Not recorded";
  if (licence.validFrom && licence.validUpto) return `${licence.validFrom} to ${licence.validUpto}`;
  return licence.validUpto ? `Until ${licence.validUpto}` : `From ${licence.validFrom}`;
}

function LicenceDialog({ current, onClose, onSaved }: {
  current: StoreLicence | null;
  onClose: () => void;
  onSaved: (next: StoreProfile) => void;
}) {
  const [licenceType, setLicenceType] = useState(current?.licenceType ?? "");
  const [licenceNumber, setLicenceNumber] = useState(current?.licenceNumber ?? "");
  const [issuingAuthority, setIssuingAuthority] = useState(current?.issuingAuthority ?? "");
  const [validFrom, setValidFrom] = useState(current?.validFrom ?? "");
  const [validUpto, setValidUpto] = useState(current?.validUpto ?? "");
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () => {
      const input = {
        licenceType: licenceType.trim(),
        licenceNumber: licenceNumber.trim(),
        issuingAuthority: issuingAuthority.trim() || null,
        validFrom: validFrom || null,
        validUpto: validUpto || null
      };
      return current
        ? updateStoreLicence(current.id, { ...input, expectedRevision: current.revision })
        : createStoreLicence(input);
    },
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "That licence could not be saved.")
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!licenceType.trim()) { setError("Enter the licence type as printed on the certificate."); return; }
    if (!licenceNumber.trim()) { setError("Enter the licence number."); return; }
    mutation.mutate();
  };

  return <CatalogDialog title={current ? "Edit Licence" : "Add Licence"} description="Transcribe the licence exactly as it is printed. Nothing here is reformatted." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="licence-type">Licence type<span aria-hidden="true"> *</span></label>
        <input id="licence-type" value={licenceType} onChange={(event) => setLicenceType(event.target.value)} placeholder="Form 20" />
        <small>As printed — for example “Form 20”, “Form 21B”, or the wording your State uses.</small>
      </div>
      <div className="field">
        <label htmlFor="licence-number">Licence number<span aria-hidden="true"> *</span></label>
        <input id="licence-number" value={licenceNumber} onChange={(event) => setLicenceNumber(event.target.value)} />
        <small>Kept exactly as you type it, including spacing and punctuation.</small>
      </div>
      <div className="field">
        <label htmlFor="licence-authority">Issuing authority</label>
        <input id="licence-authority" value={issuingAuthority} onChange={(event) => setIssuingAuthority(event.target.value)} />
      </div>
      <div className="field-row">
        <div className="field"><label htmlFor="licence-from">Valid from</label><input id="licence-from" type="date" value={validFrom} onChange={(event) => setValidFrom(event.target.value)} /></div>
        <div className="field"><label htmlFor="licence-upto">Valid until</label><input id="licence-upto" type="date" value={validUpto} onChange={(event) => setValidUpto(event.target.value)} /></div>
      </div>
      {error && <div className="inline-notice inline-notice--error" role="alert">{error}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save Licence"}</button></div>
    </form>
  </CatalogDialog>;
}

// ---------------------------------------------------------------------------------------------
// Identity and address
// ---------------------------------------------------------------------------------------------

function IdentityDialog({ current, onClose, onSaved }: {
  current: StoreProfile;
  onClose: () => void;
  onSaved: (next: StoreProfile) => void;
}) {
  const [displayName, setDisplayName] = useState(current.displayName);
  const [legalName, setLegalName] = useState(current.legalName ?? "");
  const [phone, setPhone] = useState(current.primaryPhone ?? "");
  const [email, setEmail] = useState(current.primaryEmail ?? "");
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () => updateStoreIdentity({
      expectedRevision: current.revision,
      displayName: displayName.trim(),
      legalName: legalName.trim() || null,
      primaryPhone: phone.trim() || null,
      primaryEmail: email.trim() || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The store details could not be saved.")
  });
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!displayName.trim()) { setError("Enter the name above the shop."); return; }
    mutation.mutate();
  };

  return <CatalogDialog title="Edit Business Identity" description="The trading name, the registered name, and how a customer reaches you." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="store-display-name">Trading name<span aria-hidden="true"> *</span></label>
        <input id="store-display-name" value={displayName} onChange={(event) => setDisplayName(event.target.value)} />
        <small>The name above the shop, shown throughout the application.</small>
      </div>
      <div className="field">
        <label htmlFor="store-legal-name">Registered name</label>
        <input id="store-legal-name" value={legalName} onChange={(event) => setLegalName(event.target.value)} />
        <small>As printed on your GST certificate and drug licence. Required before a sale can be posted.</small>
      </div>
      <div className="field">
        <label htmlFor="store-phone">Phone</label>
        <input id="store-phone" value={phone} onChange={(event) => setPhone(event.target.value)} inputMode="tel" />
      </div>
      <div className="field">
        <label htmlFor="store-email">Email</label>
        <input id="store-email" value={email} onChange={(event) => setEmail(event.target.value)} inputMode="email" />
      </div>
      {error && <div className="inline-notice inline-notice--error" role="alert">{stale ? "The store profile changed after you opened it. Close this and try again." : error}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save Identity"}</button></div>
    </form>
  </CatalogDialog>;
}

function AddressDialog({ current, states, onClose, onSaved }: {
  current: StoreProfile;
  states: ReferenceMasterResponse[] | undefined;
  onClose: () => void;
  onSaved: (next: StoreProfile) => void;
}) {
  const existing = current.address;
  const [line1, setLine1] = useState(existing?.line1 ?? "");
  const [line2, setLine2] = useState(existing?.line2 ?? "");
  const [city, setCity] = useState(existing?.city ?? "");
  const [stateId, setStateId] = useState(existing?.stateId ?? "");
  const [postalCode, setPostalCode] = useState(existing?.postalCode ?? "");
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () => updateStoreAddress({
      expectedRevision: existing?.revision,
      line1: line1.trim(),
      line2: line2.trim() || null,
      city: city.trim() || null,
      stateId: stateId || null,
      postalCode: postalCode.trim() || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The address could not be saved.")
  });
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (!line1.trim()) { setError("Enter the first line of the address."); return; }
    mutation.mutate();
  };

  const selectable = (states ?? []).filter((state) => state.status === "active" || state.id === existing?.stateId);

  return <CatalogDialog title={existing ? "Edit Address" : "Record Address"} description="The premises you supply from. This appears on every bill." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="store-line1">Address line 1<span aria-hidden="true"> *</span></label>
        <input id="store-line1" value={line1} onChange={(event) => setLine1(event.target.value)} autoComplete="address-line1" />
      </div>
      <div className="field">
        <label htmlFor="store-line2">Address line 2</label>
        <input id="store-line2" value={line2} onChange={(event) => setLine2(event.target.value)} autoComplete="address-line2" />
      </div>
      <div className="field-row">
        <div className="field"><label htmlFor="store-city">Town / city</label><input id="store-city" value={city} onChange={(event) => setCity(event.target.value)} autoComplete="address-level2" /></div>
        <div className="field"><label htmlFor="store-pin">PIN code</label><input id="store-pin" value={postalCode} onChange={(event) => setPostalCode(event.target.value)} inputMode="numeric" autoComplete="postal-code" /></div>
      </div>
      <div className="field">
        <label htmlFor="store-address-state">State</label>
        <select id="store-address-state" value={stateId} onChange={(event) => setStateId(event.target.value)} disabled={!states}>
          <option value="">{states ? "No State selected" : "Loading…"}</option>
          {selectable.map((state) => <option key={state.id} value={state.id}>{stateText(state)}{state.status === "archived" ? " (archived)" : ""}</option>)}
        </select>
        <small>The State the premises are in. It is recorded as entered and never changed to match your GSTIN.</small>
      </div>
      {error && <div className="inline-notice inline-notice--error" role="alert">{stale ? "The address changed after you opened it. Close this and try again." : error}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save Address"}</button></div>
    </form>
  </CatalogDialog>;
}

function TaxIdentityDialog({ current, states, onClose, onSaved }: {
  current: StoreProfile;
  states: ReferenceMasterResponse[] | undefined;
  onClose: () => void;
  onSaved: (next: StoreProfile) => void;
}) {
  const [status, setStatus] = useState<GstRegistrationStatus>(current.gstRegistrationStatus);
  const [gstin, setGstin] = useState(current.gstin ?? "");
  const [stateId, setStateId] = useState(current.placeOfSupplyStateId ?? "");
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState(false);
  const registered = status === "registered";
  const normalized = gstin.replace(/\s+/g, "").toUpperCase();

  const mutation = useMutation({
    mutationFn: async () => {
      await updateStoreTaxIdentity(current.revision, {
        gstRegistrationStatus: status,
        // A GSTIN is sent only when the status claims one, so the two can never disagree.
        gstin: registered ? gstin.trim() || null : null,
        placeOfSupplyStateId: stateId || null
      });
      // The tax endpoint answers with its own narrower shape; the screen wants the whole profile.
      return getStoreProfile();
    },
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The store profile could not be saved.")
  });
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  const reloadLatest = async () => {
    setReloading(true);
    try {
      const latest = await getStoreProfile();
      setStatus(latest.gstRegistrationStatus);
      setGstin(latest.gstin ?? "");
      setStateId(latest.placeOfSupplyStateId ?? "");
      setError(null);
      onSaved(latest);
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
