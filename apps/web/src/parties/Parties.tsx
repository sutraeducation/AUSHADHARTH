import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Navigate, useNavigate, useParams } from "react-router";
import type {
  DuplicateCandidate,
  GstRegistrationStatus,
  Party,
  PartyAddress,
  PartyDetail,
  PartyFields,
  PartyRole,
  ReferenceMasterResponse
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { listReferences } from "../reference/referenceApi";
import {
  addPartyAddress,
  addPartyRole,
  changeAddressLifecycle,
  changePartyLifecycle,
  changeRoleLifecycle,
  createParty,
  findDuplicateCandidates,
  getParty,
  listParties,
  panInsideGstin,
  previewNormalizedGstin,
  updateParty,
  type PartyStatusFilter
} from "./partyApi";

const REGISTRATION_LABELS: Record<GstRegistrationStatus, string> = {
  registered: "Registered",
  unregistered: "Not registered",
  unknown: "Not recorded yet"
};

const REASON_LABELS: Record<string, string> = {
  gstin_match: "same GSTIN",
  pan_match: "same PAN",
  drug_licence_match: "same drug licence",
  name_match: "same name",
  phone_match: "same phone",
  email_match: "same email"
};

const STALE_RECORD_MESSAGE = "This supplier changed after you opened it. Reload the latest version before saving.";

type FormValues = {
  displayName: string;
  legalName: string;
  gstRegistrationStatus: GstRegistrationStatus;
  gstin: string;
  pan: string;
  placeOfSupplyStateId: string;
  primaryPhone: string;
  primaryEmail: string;
  drugLicenceNumber: string;
  drugLicenceValidUpto: string;
};

const EMPTY: FormValues = { displayName: "", legalName: "", gstRegistrationStatus: "unknown", gstin: "", pan: "", placeOfSupplyStateId: "", primaryPhone: "", primaryEmail: "", drugLicenceNumber: "", drugLicenceValidUpto: "" };

function useStates() {
  return useQuery({ queryKey: ["reference", "state-codes", "selector"], queryFn: () => listReferences("state-codes", "", "active"), retry: false });
}

function stateLabel(states: ReferenceMasterResponse[] | undefined, id: string | null | undefined, failed: boolean) {
  if (!id) return "—";
  if (!states) return failed ? "State unavailable" : "Loading…";
  const match = states.find((state) => state.id === id);
  if (!match || match.kind !== "state-codes") return "Unknown State";
  return `${match.attributes.stateCode} · ${match.attributes.displayName}`;
}

export function PartyListPage() {
  usePageTitle("Suppliers");
  const auth = useAuth();
  const [input, setInput] = useState("");
  const search = useDebouncedValue(input, 250);
  const [status, setStatus] = useState<PartyStatusFilter>("active");
  const parties = useQuery({ queryKey: ["parties", search, status], queryFn: () => listParties(search, status), retry: false });
  const states = useStates();
  useExpireOnAuthError(parties.error);
  const canMutate = auth.status?.user?.role === "owner_admin";
  return <>
    <PageHeading eyebrow="MASTERS" title="Suppliers" description="Business parties this pharmacy buys from. A supplier record is identity only — it carries no balance, credit limit, or outstanding amount.">
      {canMutate ? <Link className="button button--primary" to="/app/parties/new">Add Supplier</Link> : <span className="read-only-note">Read-only access</span>}
    </PageHeading>
    <section className="master-panel" aria-labelledby="party-list-title">
      <div className="master-toolbar">
        <div className="search-field"><label htmlFor="party-search">Search</label><span><input id="party-search" value={input} onChange={(event) => setInput(event.target.value)} placeholder="Name, GSTIN, or PAN" />{input && <button type="button" onClick={() => setInput("")}>Clear</button>}</span></div>
        <div className="filter-field"><label htmlFor="party-status">Status</label><select id="party-status" value={status} onChange={(event) => setStatus(event.target.value as PartyStatusFilter)}><option value="active">Active</option><option value="archived">Archived</option><option value="all">All</option></select></div>
      </div>
      <h2 id="party-list-title" className="sr-only">Supplier records</h2>
      {parties.isPending ? <Loading label="Loading suppliers…" /> : parties.isError ? <QueryError label="Suppliers could not be loaded" onRetry={() => void parties.refetch()} /> : parties.data.length === 0 ? <Empty search={Boolean(search)} archived={status === "archived"} /> :
        <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Supplier</th><th scope="col">GST registration</th><th scope="col">GSTIN</th><th scope="col">Place of supply</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{parties.data.map((party: Party) => <tr key={party.id}><td data-label="Supplier"><Link to={`/app/parties/${party.id}`}>{party.displayName}</Link>{party.legalName && <small className="row-subtext">{party.legalName}</small>}</td><td data-label="GST registration">{REGISTRATION_LABELS[party.gstRegistrationStatus]}</td><td data-label="GSTIN">{party.normalizedGstin ?? "—"}</td><td data-label="Place of supply">{stateLabel(states.data, party.placeOfSupplyStateId, states.isError)}</td><td data-label="Status"><Status value={party.status} /></td><td className="row-actions"><div><Link to={`/app/parties/${party.id}`}>View</Link>{canMutate && party.status === "active" && <Link to={`/app/parties/${party.id}/edit`}>Edit</Link>}</div></td></tr>)}</tbody></table></div>}
    </section>
  </>;
}

export function PartyCreatePage() { return <PartyForm />; }

export function PartyEditPage() {
  const { id } = useParams();
  const party = useQuery({ queryKey: ["party", id], queryFn: () => getParty(id!), enabled: Boolean(id), retry: false });
  useExpireOnAuthError(party.error);
  if (party.isPending) return <Loading label="Loading supplier…" />;
  if (party.isError) return <QueryError label="Supplier could not be loaded" onRetry={() => void party.refetch()} />;
  if (party.data.status !== "active") return <Navigate to={`/app/parties/${party.data.id}`} replace />;
  return <PartyForm record={party.data} />;
}

function PartyForm({ record }: { record?: PartyDetail }) {
  usePageTitle(record ? "Edit Supplier" : "Add Supplier");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const states = useStates();
  const [values, setValues] = useState<FormValues>(() => record ? toValues(record) : EMPTY);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [candidates, setCandidates] = useState<DuplicateCandidate[] | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [reloading, setReloading] = useState(false);
  const set = (field: keyof FormValues, value: string) => setValues((current) => ({ ...current, [field]: value }));

  const registered = values.gstRegistrationStatus === "registered";
  const normalizedGstin = previewNormalizedGstin(values.gstin);
  const derivedPan = registered ? panInsideGstin(normalizedGstin) : null;

  const candidateNames = useQuery({ queryKey: ["parties", "", "all"], queryFn: () => listParties("", "all"), enabled: Boolean(candidates?.length), retry: false });

  const mutation = useMutation({
    mutationFn: async () => {
      const fields = toFields(values);
      if (!record) {
        // Advisory duplicate review happens before the first create attempt, never after.
        if (!confirmed) {
          const found = await findDuplicateCandidates({ party: fields, roles: [{ role: "supplier" }], addresses: [] });
          if (found.length) { setCandidates(found); return null; }
        }
        return createParty({ party: fields, roles: [{ role: "supplier" }], addresses: [] });
      }
      return updateParty(record.id, record.revision, fields);
    },
    onSuccess: (saved) => {
      if (!saved) return;
      void queryClient.invalidateQueries({ queryKey: ["parties"] });
      void queryClient.invalidateQueries({ queryKey: ["party", saved.id] });
      navigate(`/app/parties/${saved.id}`);
    }
  });

  const reloadLatest = async () => {
    if (!record) return;
    setReloading(true);
    try {
      const latest = await getParty(record.id);
      setValues(toValues(latest));
      queryClient.setQueryData(["party", record.id], latest);
      void queryClient.invalidateQueries({ queryKey: ["parties"] });
    } finally { setReloading(false); }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const found = validate(values);
    setErrors(found);
    if (Object.keys(found).length) return;
    mutation.mutate();
  };

  const notice = mutation.error instanceof LocalServiceError ? mutation.error.message : mutation.error ? "The supplier could not be saved." : null;
  const stale = mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict";

  return <>
    <nav className="breadcrumbs" aria-label="Breadcrumb"><Link to="/app/parties">Suppliers</Link><span aria-hidden="true">/</span><span>{record ? "Edit" : "Add Supplier"}</span></nav>
    <PageHeading eyebrow="MASTERS" title={record ? `Edit ${record.displayName}` : "Add Supplier"} description="Identity and tax registration only. Purchases, payments, and balances belong to later modules." />
    <form className="master-form catalog-form" onSubmit={submit} noValidate>
      <TextField label="Supplier name" name="displayName" value={values.displayName} onChange={(value) => set("displayName", value)} required error={errors.displayName} hint="What you will search for and see in every list." />
      <TextField label="Legal name" name="legalName" value={values.legalName} onChange={(value) => set("legalName", value)} error={errors.legalName} hint="As printed on the GST certificate, if it differs." />
      <SelectField label="GST registration" name="gstRegistrationStatus" value={values.gstRegistrationStatus} onChange={(value) => set("gstRegistrationStatus", value)} options={[["unknown", "Not recorded yet"], ["registered", "Registered"], ["unregistered", "Not registered"]]} hint="“Not registered” states there is no GSTIN; “not recorded yet” means nobody has captured it." />
      {registered && <>
        <TextField label="GSTIN" name="gstin" value={values.gstin} onChange={(value) => set("gstin", value)} required error={errors.gstin} hint="15 characters. The check digit is verified before saving." />
        <SelectField label="Place of supply (State)" name="placeOfSupplyStateId" value={values.placeOfSupplyStateId} onChange={(value) => set("placeOfSupplyStateId", value)} required error={errors.placeOfSupplyStateId} disabled={states.isPending} options={[["", states.isPending ? "Loading…" : states.isError ? "States unavailable" : "Select a State"], ...stateOptions(states.data)]} />
        <div className="field catalog-span"><span className="field-label">Will be recorded as</span><strong>{normalizedGstin || "—"}</strong><small>{derivedPan ? `The GSTIN carries PAN ${derivedPan}; a PAN you type must match it.` : "Spacing and case are normalised before comparison."}</small></div>
      </>}
      {states.isError && <InlineQueryError label="States could not be loaded." onRetry={() => void states.refetch()} />}
      <TextField label="PAN" name="pan" value={values.pan} onChange={(value) => set("pan", value)} error={errors.pan} hint="Five letters, four digits, then a letter." />
      <TextField label="Phone" name="primaryPhone" value={values.primaryPhone} onChange={(value) => set("primaryPhone", value)} error={errors.primaryPhone} />
      <TextField label="Email" name="primaryEmail" type="email" value={values.primaryEmail} onChange={(value) => set("primaryEmail", value)} error={errors.primaryEmail} />
      <TextField label="Drug licence number" name="drugLicenceNumber" value={values.drugLicenceNumber} onChange={(value) => set("drugLicenceNumber", value)} error={errors.drugLicenceNumber} hint="Exactly as printed, including both licences if the supplier holds two." />
      <TextField label="Drug licence valid up to" name="drugLicenceValidUpto" type="date" value={values.drugLicenceValidUpto} onChange={(value) => set("drugLicenceValidUpto", value)} error={errors.drugLicenceValidUpto} />
      {candidates?.length ? <div className="duplicate-warning catalog-span" role="alert"><strong>A similar supplier may already exist.</strong><p>These matches are advisory. Review them, then continue if this really is a different business.</p><ul>{candidates.map((candidate) => <li key={candidate.candidateId}><Link to={`/app/parties/${candidate.candidateId}`}>{candidateNames.data?.find((item) => item.id === candidate.candidateId)?.displayName ?? candidate.candidateId}</Link> — {candidate.reasonCodes.map((code) => REASON_LABELS[code] ?? code).join(", ")}</li>)}</ul><button className="button button--secondary" type="button" onClick={() => setConfirmed(true)}>Continue with a new supplier</button></div> : null}
      {notice && <div className="inline-notice inline-notice--error catalog-span" role="alert">{stale && record ? STALE_RECORD_MESSAGE : notice}{stale && record && <button type="button" onClick={() => void reloadLatest()} disabled={reloading}>{reloading ? "Reloading…" : "Reload latest"}</button>}</div>}
      <div className="form-actions catalog-span"><Link className="button button--secondary" to={record ? `/app/parties/${record.id}` : "/app/parties"}>Cancel</Link><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : candidates?.length && confirmed ? "Create Supplier Anyway" : record ? "Save Supplier" : "Review & Create Supplier"}</button></div>
    </form>
  </>;
}

export function PartyDetailPage() {
  const { id } = useParams();
  const auth = useAuth();
  const queryClient = useQueryClient();
  const party = useQuery({ queryKey: ["party", id], queryFn: () => getParty(id!), enabled: Boolean(id), retry: false });
  const states = useStates();
  useExpireOnAuthError(party.error);
  const [addingAddress, setAddingAddress] = useState(false);
  const [lifecycle, setLifecycle] = useState<{ kind: "party" | "role" | "address"; id: string; revision: number; archived: boolean; label: string } | null>(null);
  usePageTitle(party.data?.displayName ?? "Supplier");
  const canMutate = auth.status?.user?.role === "owner_admin";
  const refresh = () => { void queryClient.invalidateQueries({ queryKey: ["party", id] }); void queryClient.invalidateQueries({ queryKey: ["parties"] }); };

  const addRole = useMutation({ mutationFn: () => addPartyRole(id!, { role: "supplier" }), onSuccess: refresh });

  if (party.isPending) return <Loading label="Loading supplier…" />;
  if (party.isError) return <QueryError label="Supplier could not be loaded" onRetry={() => void party.refetch()} />;
  const record = party.data;
  const activeSupplier = record.roles.some((role: PartyRole) => role.role === "supplier" && role.status === "active");

  return <>
    <nav className="breadcrumbs" aria-label="Breadcrumb"><Link to="/app/parties">Suppliers</Link><span aria-hidden="true">/</span><span>{record.displayName}</span></nav>
    <PageHeading eyebrow="SUPPLIER" title={record.displayName} description={record.legalName ?? "Identity and tax registration only."}>
      {canMutate ? <div className="header-actions">{record.status === "active" && <Link className="button button--secondary" to={`/app/parties/${record.id}/edit`}>Edit</Link>}<button className="button button--secondary" type="button" onClick={() => setLifecycle({ kind: "party", id: record.id, revision: record.revision, archived: record.status === "archived", label: record.displayName })}>{record.status === "active" ? "Archive supplier" : "Restore supplier"}</button></div> : <span className="read-only-note">Read-only access</span>}
    </PageHeading>

    <section className="master-panel" aria-labelledby="party-identity-title">
      <h2 id="party-identity-title">Identity</h2>
      <dl className="detail-grid">
        <div><dt>Status</dt><dd><Status value={record.status} /></dd></div>
        <div><dt>GST registration</dt><dd>{REGISTRATION_LABELS[record.gstRegistrationStatus]}</dd></div>
        <div><dt>GSTIN</dt><dd>{record.normalizedGstin ?? "—"}</dd></div>
        <div><dt>PAN</dt><dd>{record.normalizedPan ?? "—"}</dd></div>
        <div><dt>Place of supply</dt><dd>{stateLabel(states.data, record.placeOfSupplyStateId, states.isError)}</dd></div>
        <div><dt>Phone</dt><dd>{record.primaryPhone ?? "—"}</dd></div>
        <div><dt>Email</dt><dd>{record.primaryEmail ?? "—"}</dd></div>
        <div><dt>Drug licence</dt><dd>{record.drugLicenceNumber ?? "—"}{record.drugLicenceValidUpto && <small className="row-subtext">Valid to {record.drugLicenceValidUpto}</small>}</dd></div>
      </dl>
      <p className="panel-note">A supplier record carries no balance or credit limit. Amounts appear only when purchase and accounting modules are enabled.</p>
    </section>

    <section className="master-panel" aria-labelledby="party-roles-title">
      <div className="panel-header"><h2 id="party-roles-title">Roles</h2>{canMutate && record.status === "active" && !activeSupplier && <button className="button button--secondary" type="button" onClick={() => addRole.mutate()} disabled={addRole.isPending}>{addRole.isPending ? "Adding…" : "Add supplier role"}</button>}</div>
      {addRole.error && <div className="inline-notice inline-notice--error" role="alert">{addRole.error instanceof LocalServiceError ? addRole.error.message : "The role could not be added."}</div>}
      {record.roles.length === 0 ? <p>No roles recorded.</p> :
        <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Role</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{record.roles.map((role: PartyRole) => <tr key={role.id}><td data-label="Role">{role.role === "supplier" ? "Supplier" : "Customer"}</td><td data-label="Status"><Status value={role.status} /></td><td className="row-actions"><div>{canMutate && <button type="button" onClick={() => setLifecycle({ kind: "role", id: role.id, revision: role.revision, archived: role.status === "archived", label: `${role.role} role` })}>{role.status === "active" ? "Archive" : "Restore"}</button>}</div></td></tr>)}</tbody></table></div>}
    </section>

    <section className="master-panel" aria-labelledby="party-addresses-title">
      <div className="panel-header"><h2 id="party-addresses-title">Addresses</h2>{canMutate && record.status === "active" && <button className="button button--secondary" type="button" onClick={() => setAddingAddress(true)}>Add address</button>}</div>
      {record.addresses.length === 0 ? <p>No addresses recorded.</p> :
        <div className="table-scroll"><table className="data-table"><thead><tr><th scope="col">Purpose</th><th scope="col">Address</th><th scope="col">State</th><th scope="col">Primary</th><th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{record.addresses.map((address: PartyAddress) => <tr key={address.id}><td data-label="Purpose">{address.addressRole === "billing" ? "Billing" : "Shipping"}</td><td data-label="Address">{[address.line1, address.line2, address.city, address.postalCode].filter(Boolean).join(", ")}</td><td data-label="State">{stateLabel(states.data, address.stateId, states.isError)}</td><td data-label="Primary">{address.isPrimary ? "Yes" : "No"}</td><td data-label="Status"><Status value={address.status} /></td><td className="row-actions"><div>{canMutate && <button type="button" onClick={() => setLifecycle({ kind: "address", id: address.id, revision: address.revision, archived: address.status === "archived", label: address.line1 })}>{address.status === "active" ? "Archive" : "Restore"}</button>}</div></td></tr>)}</tbody></table></div>}
    </section>

    {addingAddress && <AddressDialog partyId={record.id} states={states.data} onClose={() => setAddingAddress(false)} onSaved={() => { setAddingAddress(false); refresh(); }} />}
    {lifecycle && <LifecycleDialog target={lifecycle} onClose={() => setLifecycle(null)} onSaved={() => { setLifecycle(null); refresh(); }} />}
  </>;
}

function AddressDialog({ partyId, states, onClose, onSaved }: { partyId: string; states: ReferenceMasterResponse[] | undefined; onClose: () => void; onSaved: () => void }) {
  const [values, setValues] = useState({ addressRole: "billing", line1: "", line2: "", city: "", stateId: "", postalCode: "", isPrimary: false });
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => addPartyAddress(partyId, { addressRole: values.addressRole as "billing" | "shipping", line1: values.line1, line2: values.line2 || null, city: values.city || null, stateId: values.stateId || null, postalCode: values.postalCode || null, countryCode: "IN", isPrimary: values.isPrimary }),
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The address could not be saved.")
  });
  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (!values.line1.trim()) { setError("Enter at least the first line of the address."); return; }
    mutation.mutate();
  };
  return <CatalogDialog title="Add Address" description="Postal address only. Contacts and delivery routing belong to later modules." onClose={onClose}><form className="master-form" onSubmit={submit}>
    <SelectField label="Purpose" name="addressRole" value={values.addressRole} onChange={(value) => setValues({ ...values, addressRole: value })} options={[["billing", "Billing"], ["shipping", "Shipping"]]} />
    <TextField label="Address line 1" name="line1" value={values.line1} onChange={(value) => setValues({ ...values, line1: value })} required />
    <TextField label="Address line 2" name="line2" value={values.line2} onChange={(value) => setValues({ ...values, line2: value })} />
    <TextField label="City" name="city" value={values.city} onChange={(value) => setValues({ ...values, city: value })} />
    <SelectField label="State" name="stateId" value={values.stateId} onChange={(value) => setValues({ ...values, stateId: value })} options={[["", "No State selected"], ...stateOptions(states)]} />
    <TextField label="Postal code" name="postalCode" value={values.postalCode} onChange={(value) => setValues({ ...values, postalCode: value })} />
    <label className="check-field catalog-span"><input type="checkbox" checked={values.isPrimary} onChange={(event) => setValues({ ...values, isPrimary: event.target.checked })} /> Primary address for this purpose</label>
    {error && <div className="inline-notice inline-notice--error" role="alert">{error}</div>}
    <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Add Address"}</button></div>
  </form></CatalogDialog>;
}

function LifecycleDialog({ target, onClose, onSaved }: { target: { kind: "party" | "role" | "address"; id: string; revision: number; archived: boolean; label: string }; onClose: () => void; onSaved: () => void }) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const action = target.archived ? "restore" : "archive";
  const mutation = useMutation({
    // The three records differ in shape, so the caller refreshes rather than consuming a result.
    mutationFn: async (): Promise<void> => {
      if (target.kind === "party") await changePartyLifecycle(target.id, action, target.revision, reason);
      else if (target.kind === "role") await changeRoleLifecycle(target.id, action, target.revision, reason);
      else await changeAddressLifecycle(target.id, action, target.revision, reason);
    },
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The change could not be saved.")
  });
  const submit = (event: FormEvent) => {
    event.preventDefault(); setError(null);
    if (!reason.trim()) { setError("A reason is required and is recorded in the audit trail."); return; }
    mutation.mutate();
  };
  return <CatalogDialog title={`${target.archived ? "Restore" : "Archive"} ${target.label}`} description={target.archived ? "Restoring returns this record to active use." : "Archiving keeps the record and its history; nothing is deleted."} onClose={onClose}><form className="master-form" onSubmit={submit}>
    <TextField label="Reason" name="reason" value={reason} onChange={setReason} required hint="Recorded permanently in the audit trail." />
    {error && <div className="inline-notice inline-notice--error" role="alert">{error}</div>}
    <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : target.archived ? "Restore" : "Archive"}</button></div>
  </form></CatalogDialog>;
}

function stateOptions(states: ReferenceMasterResponse[] | undefined): ReadonlyArray<readonly [string, string]> {
  return (states ?? []).flatMap((state) => state.kind === "state-codes" ? [[state.id, `${state.attributes.stateCode} · ${state.attributes.displayName}`] as const] : []);
}

function toValues(record: PartyDetail): FormValues {
  return { displayName: record.displayName, legalName: record.legalName ?? "", gstRegistrationStatus: record.gstRegistrationStatus, gstin: record.gstin ?? "", pan: record.pan ?? "", placeOfSupplyStateId: record.placeOfSupplyStateId ?? "", primaryPhone: record.primaryPhone ?? "", primaryEmail: record.primaryEmail ?? "", drugLicenceNumber: record.drugLicenceNumber ?? "", drugLicenceValidUpto: record.drugLicenceValidUpto ?? "" };
}

function toFields(values: FormValues): PartyFields {
  const registered = values.gstRegistrationStatus === "registered";
  return {
    displayName: values.displayName.trim(),
    legalName: values.legalName.trim() || null,
    gstRegistrationStatus: values.gstRegistrationStatus,
    // A GSTIN is sent only when the status claims one, so the two can never disagree.
    gstin: registered ? values.gstin.trim() || null : null,
    pan: values.pan.trim() || null,
    placeOfSupplyStateId: registered ? values.placeOfSupplyStateId || null : null,
    primaryPhone: values.primaryPhone.trim() || null,
    primaryEmail: values.primaryEmail.trim() || null,
    drugLicenceNumber: values.drugLicenceNumber.trim() || null,
    drugLicenceValidUpto: values.drugLicenceValidUpto || null
  };
}

/** Local checks only. The Store Service remains the authority, including the GSTIN check digit. */
function validate(values: FormValues): Record<string, string> {
  const errors: Record<string, string> = {};
  if (!values.displayName.trim()) errors.displayName = "A supplier name is required.";
  if (values.gstRegistrationStatus === "registered") {
    if (previewNormalizedGstin(values.gstin).length !== 15) errors.gstin = "A GSTIN is 15 characters.";
    if (!values.placeOfSupplyStateId) errors.placeOfSupplyStateId = "Select the State this registration belongs to.";
  }
  if (values.pan.trim() && !/^[A-Za-z]{5}[0-9]{4}[A-Za-z]$/.test(values.pan.replace(/\s+/g, ""))) errors.pan = "A PAN is five letters, four digits, then a letter.";
  return errors;
}

function PageHeading({ eyebrow, title, description, children }: { eyebrow: string; title: string; description?: string; children?: ReactNode }) {
  return <header className="page-header"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1>{description && <p>{description}</p>}</div>{children}</header>;
}
function TextField({ label, name, value, onChange, required, type = "text", error, hint }: { label: string; name: string; value: string; onChange: (value: string) => void; required?: boolean; type?: string; error?: string; hint?: string }) {
  const id = `party-${name}`; const help = error || hint ? `${id}-help` : undefined;
  return <div className="field"><label htmlFor={id}>{label}{required && <span aria-hidden="true"> *</span>}</label><input id={id} name={name} type={type} value={value} onChange={(event) => onChange(event.target.value)} aria-invalid={Boolean(error)} aria-describedby={help} />{help && <small id={help} className={error ? "field-error" : ""}>{error || hint}</small>}</div>;
}
function SelectField({ label, name, value, onChange, options, disabled, required, error, hint }: { label: string; name: string; value: string; onChange: (value: string) => void; options: ReadonlyArray<readonly [string, string]>; disabled?: boolean; required?: boolean; error?: string; hint?: string }) {
  const id = `party-${name}`; const help = error || hint ? `${id}-help` : undefined;
  return <div className="field"><label htmlFor={id}>{label}{required && <span aria-hidden="true"> *</span>}</label><select id={id} name={name} value={value} onChange={(event) => onChange(event.target.value)} disabled={disabled} aria-invalid={Boolean(error)} aria-describedby={help}>{options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}</select>{help && <small id={help} className={error ? "field-error" : ""}>{error || hint}</small>}</div>;
}
function Status({ value }: { value: string }) { return <span className={`status-chip status-chip--${value}`}>{value === "active" ? "Active" : "Archived"}</span>; }
function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="empty-state" role="alert"><h3>{label}</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function InlineQueryError({ label, onRetry }: { label: string; onRetry: () => void }) { return <div className="catalog-inline-error" role="alert"><span>{label}</span><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function Empty({ search, archived }: { search: boolean; archived: boolean }) { return <div className="empty-state"><h3>{search ? "No matching suppliers" : archived ? "No archived suppliers" : "No suppliers yet"}</h3><p>{search ? "Try a different name, GSTIN, or PAN." : "Add the businesses this pharmacy buys from."}</p></div>; }
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
function useExpireOnAuthError(error: Error | null) { const auth = useAuth(); useEffect(() => { if (error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(error.code)) void auth.expireSession(); }, [error]); }
function useDebouncedValue<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => { const timer = window.setTimeout(() => setDebounced(value), delay); return () => window.clearTimeout(timer); }, [value, delay]);
  return useMemo(() => debounced, [debounced]);
}
