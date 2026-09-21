import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  LicenceFormSchema,
  ProfessionalCapacitySchema,
  RecordElectionKindSchema,
  type LicenceForm,
  type ProfessionalCapacity,
  type RecordElectionKind,
  type RecordElectionMethod,
  type StoreProfessional
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import {
  CAPACITY_LABELS,
  ELECTION_LABELS,
  LICENCE_FORM_LABELS,
  METHODS_FOR,
  METHOD_LABELS,
  archiveProfessional,
  createComplianceLicence,
  createProfessional,
  createRecordElection,
  getDrugCompliance
} from "./regulatoryApi";

/**
 * Phase 1M-A — the store's Drugs Rules facts.
 *
 * Three things the counter will one day depend on, each kept apart from the thing it is easiest to
 * confuse it with:
 *
 *   * professionals — the registered pharmacist and competent person the Rules name, NOT the
 *     people who hold a login, and not inferred from a login's role;
 *   * licence forms — the typed assertion "this store holds a Form 20F", NOT the display licence
 *     text Store Profile prints on the memo, which is never reinterpreted;
 *   * the two rule 65 record elections — two separate facts, made once by the licensee in writing
 *     to the Licensing Authority, never chosen at the counter.
 */
export function DrugCompliancePage() {
  const auth = useAuth();
  const canMutate = auth.status?.user?.role === "owner_admin";
  const queryClient = useQueryClient();
  const compliance = useQuery({ queryKey: ["drug-compliance"], queryFn: getDrugCompliance, retry: false });
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["drug-compliance"] });
  const [dialog, setDialog] = useState<"professional" | "licence" | RecordElectionKind | null>(null);
  const [archiving, setArchiving] = useState<StoreProfessional | null>(null);

  return <div className="page-stack">
    <header className="page-header">
      <div>
        <p className="eyebrow">SETTINGS</p>
        <h1>Drug Compliance</h1>
        <p>The pharmacy's registered professionals, the licence forms it holds, and the record elections it made under rule 65.</p>
      </div>
    </header>

    {compliance.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading drug compliance…</b></div>
      : compliance.isError ? <div className="empty-state" role="alert"><h3>Drug compliance could not be loaded</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={() => void compliance.refetch()}>Retry</button></div>
      : <>
        <section className="master-panel" aria-labelledby="professionals-title">
          <div className="panel-header">
            <div>
              <h2 id="professionals-title">Professionals</h2>
              <p>The people the Drugs Rules name. A login's role is a permission in this software and is not evidence of registration under the Pharmacy Act, 1948.</p>
            </div>
            {canMutate && <button className="button button--secondary" type="button" onClick={() => setDialog("professional")}>Add Professional</button>}
          </div>
          {compliance.data.professionals.length === 0
            ? <p className="panel-note">No professional is recorded.</p>
            : <div className="table-scroll"><table className="data-table">
                <thead><tr><th scope="col">Name</th><th scope="col">Capacity</th>{canMutate && <th scope="col">Registration</th>}<th scope="col">Valid</th><th scope="col">Status</th>{canMutate && <th scope="col"><span className="visually-hidden">Actions</span></th>}</tr></thead>
                <tbody>{compliance.data.professionals.map((person) => <tr key={person.id}>
                  <td>{person.fullName}</td>
                  <td>{CAPACITY_LABELS[person.capacity]}</td>
                  {canMutate && <td>{person.registrationNumber ?? "—"}{person.registeringAuthority && <small>{person.registeringAuthority}</small>}</td>}
                  <td>{person.validFrom ?? "—"} to {person.validUpto ?? "—"}</td>
                  <td><span className={`status-badge status-badge--${person.status}`}>{person.status === "active" ? "Active" : "Archived"}</span></td>
                  {canMutate && <td className="table-actions">{person.status === "active" && <button className="button button--secondary" type="button" onClick={() => setArchiving(person)}>Archive</button>}</td>}
                </tr>)}</tbody>
              </table></div>}
        </section>

        <section className="master-panel" aria-labelledby="licence-forms-title">
          <div className="panel-header">
            <div>
              <h2 id="licence-forms-title">Licence Forms</h2>
              <p>The forms under rule 61 this pharmacy holds, as a typed assertion. The licence text on Store Profile is what prints on a memo; it is never read as one of these.</p>
            </div>
            {canMutate && <button className="button button--secondary" type="button" onClick={() => setDialog("licence")}>Add Licence Form</button>}
          </div>
          {compliance.data.complianceLicences.length === 0
            ? <p className="panel-note">No licence form is asserted.</p>
            : <div className="table-scroll"><table className="data-table">
                <thead><tr><th scope="col">Form</th><th scope="col">Number</th><th scope="col">Valid</th><th scope="col">Status</th></tr></thead>
                <tbody>{compliance.data.complianceLicences.map((licence) => <tr key={licence.id}>
                  <td>{LICENCE_FORM_LABELS[licence.licenceForm]}</td>
                  <td>{licence.licenceNumber}{licence.issuingAuthority && <small>{licence.issuingAuthority}</small>}</td>
                  <td>{licence.validFrom ?? "—"} to {licence.validUpto ?? "—"}</td>
                  <td><span className={`status-badge status-badge--${licence.status}`}>{licence.status === "active" ? "Active" : "Archived"}</span></td>
                </tr>)}</tbody>
              </table></div>}
        </section>

        {RecordElectionKindSchema.options.map((election) => {
          const recorded = compliance.data.recordElections.filter((entry) => entry.election === election);
          return <section className="master-panel" aria-labelledby={`election-${election}`} key={election} data-testid={`election-${election}`}>
            <div className="panel-header">
              <div>
                <h2 id={`election-${election}`}>{ELECTION_LABELS[election]}</h2>
                <p>{election === "rule_65_3_prescription_supply"
                  ? "Whether drugs supplied on prescription from or in the original container are recorded in a prescription register or a cash or credit memo book. Elected in writing to the Licensing Authority when applying for the licence."
                  : "Whether Schedule C drugs supplied without a prescription are recorded in a register or a cash or credit memo book. A separate election from the one above."}</p>
              </div>
              {canMutate && <button className="button button--secondary" type="button" onClick={() => setDialog(election)}>Record Election</button>}
            </div>
            {recorded.length === 0
              ? <p className="panel-note">Not recorded.</p>
              : <div className="table-scroll"><table className="data-table">
                  <thead><tr><th scope="col">Method</th><th scope="col">In force</th><th scope="col">Evidence</th><th scope="col">Status</th></tr></thead>
                  <tbody>{recorded.map((entry) => <tr key={entry.id}>
                    <td>{METHOD_LABELS[entry.method]}</td>
                    <td>{entry.effectiveFrom} to {entry.effectiveTo ?? "further notice"}</td>
                    <td>{entry.evidenceReference ?? "—"}</td>
                    <td><span className={`status-badge status-badge--${entry.status}`}>{entry.status === "active" ? "Active" : "Archived"}</span></td>
                  </tr>)}</tbody>
                </table></div>}
          </section>;
        })}
      </>}

    {dialog === "professional" && <ProfessionalDialog onClose={() => setDialog(null)} onSaved={() => { setDialog(null); refresh(); }} />}
    {dialog === "licence" && <LicenceFormDialog onClose={() => setDialog(null)} onSaved={() => { setDialog(null); refresh(); }} />}
    {dialog && dialog !== "professional" && dialog !== "licence" && <ElectionDialog election={dialog} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); refresh(); }} />}
    {archiving && <ArchiveProfessionalDialog person={archiving} onClose={() => setArchiving(null)} onSaved={() => { setArchiving(null); refresh(); }} />}
  </div>;
}

function ProfessionalDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [fullName, setFullName] = useState("");
  const [capacity, setCapacity] = useState<ProfessionalCapacity | "">("");
  const [registrationNumber, setRegistrationNumber] = useState("");
  const [registeringAuthority, setRegisteringAuthority] = useState("");
  const [validFrom, setValidFrom] = useState("");
  const [validUpto, setValidUpto] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => createProfessional({
      fullName,
      capacity: capacity as ProfessionalCapacity,
      registrationNumber: registrationNumber || null,
      registeringAuthority: registeringAuthority || null,
      validFrom: validFrom || null,
      validUpto: validUpto || null,
      linkedUserId: null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = fullName.trim() !== "" && capacity !== ""
    && (capacity !== "registered_pharmacist" || registrationNumber.trim() !== "");
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Add Professional" description="A person named by the Drugs Rules. A registered pharmacist is recorded with the registration that makes the claim checkable." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field"><label htmlFor="professional-name">Full name</label><input id="professional-name" value={fullName} onChange={(event) => setFullName(event.target.value)} required maxLength={120} /></div>
      <div className="field">
        <label htmlFor="professional-capacity">Capacity</label>
        <select id="professional-capacity" value={capacity} onChange={(event) => setCapacity(event.target.value as ProfessionalCapacity)} required>
          <option value="">Choose a capacity</option>
          {ProfessionalCapacitySchema.options.map((option) => <option key={option} value={option}>{CAPACITY_LABELS[option]}</option>)}
        </select>
      </div>
      <div className="field">
        <label htmlFor="professional-registration">Registration number{capacity === "registered_pharmacist" ? "" : " (optional)"}</label>
        <input id="professional-registration" value={registrationNumber} onChange={(event) => setRegistrationNumber(event.target.value)} maxLength={60} />
      </div>
      <div className="field"><label htmlFor="professional-authority">Registering council (optional)</label><input id="professional-authority" value={registeringAuthority} onChange={(event) => setRegisteringAuthority(event.target.value)} maxLength={160} /></div>
      <div className="field"><label htmlFor="professional-from">Valid from (optional)</label><input id="professional-from" type="date" value={validFrom} onChange={(event) => setValidFrom(event.target.value)} /></div>
      <div className="field"><label htmlFor="professional-upto">Valid up to (optional)</label><input id="professional-upto" type="date" value={validUpto} onChange={(event) => setValidUpto(event.target.value)} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Add Professional</button>
      </div>
    </form>
  </CatalogDialog>;
}

function LicenceFormDialog({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [licenceForm, setLicenceForm] = useState<LicenceForm | "">("");
  const [licenceNumber, setLicenceNumber] = useState("");
  const [issuingAuthority, setIssuingAuthority] = useState("");
  const [validFrom, setValidFrom] = useState("");
  const [validUpto, setValidUpto] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => createComplianceLicence({
      licenceForm: licenceForm as LicenceForm,
      licenceNumber,
      issuingAuthority: issuingAuthority || null,
      validFrom: validFrom || null,
      validUpto: validUpto || null,
      displayLicenceId: null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = licenceForm !== "" && licenceNumber.trim() !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Add Licence Form" description="Assert a licence form this pharmacy holds under rule 61, exactly as it appears on the certificate." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="licence-form">Form</label>
        <select id="licence-form" value={licenceForm} onChange={(event) => setLicenceForm(event.target.value as LicenceForm)} required>
          <option value="">Choose a form</option>
          {LicenceFormSchema.options.map((option) => <option key={option} value={option}>{LICENCE_FORM_LABELS[option]}</option>)}
        </select>
      </div>
      <div className="field"><label htmlFor="licence-number">Licence number</label><input id="licence-number" value={licenceNumber} onChange={(event) => setLicenceNumber(event.target.value)} required maxLength={100} /></div>
      <div className="field"><label htmlFor="licence-authority">Issuing authority (optional)</label><input id="licence-authority" value={issuingAuthority} onChange={(event) => setIssuingAuthority(event.target.value)} maxLength={160} /></div>
      <div className="field"><label htmlFor="licence-from">Valid from (optional)</label><input id="licence-from" type="date" value={validFrom} onChange={(event) => setValidFrom(event.target.value)} /></div>
      <div className="field"><label htmlFor="licence-upto">Valid up to (optional)</label><input id="licence-upto" type="date" value={validUpto} onChange={(event) => setValidUpto(event.target.value)} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Add Licence Form</button>
      </div>
    </form>
  </CatalogDialog>;
}

function ElectionDialog({ election, onClose, onSaved }: { election: RecordElectionKind; onClose: () => void; onSaved: () => void }) {
  const [method, setMethod] = useState<RecordElectionMethod | "">("");
  const [effectiveFrom, setEffectiveFrom] = useState("");
  const [evidenceReference, setEvidenceReference] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => createRecordElection({
      election,
      method: method as RecordElectionMethod,
      effectiveFrom,
      effectiveTo: null,
      evidenceReference: evidenceReference || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = method !== "" && effectiveFrom !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Record Election" description={`${ELECTION_LABELS[election]}. Record what the licensee elected in writing; this does not make the election.`} onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <fieldset className="field">
        <legend>Method elected</legend>
        {METHODS_FOR[election].map((option) => <label className="choice" key={option}>
          <input type="radio" name="election-method" value={option} checked={method === option} onChange={() => setMethod(option)} /> {METHOD_LABELS[option]}
        </label>)}
      </fieldset>
      <div className="field"><label htmlFor="election-from">In force from</label><input id="election-from" type="date" value={effectiveFrom} onChange={(event) => setEffectiveFrom(event.target.value)} required /></div>
      <div className="field"><label htmlFor="election-evidence">Evidence (optional)</label><input id="election-evidence" value={evidenceReference} onChange={(event) => setEvidenceReference(event.target.value)} placeholder="e.g. Election letter to Licensing Authority, dated…" maxLength={300} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Record Election</button>
      </div>
    </form>
  </CatalogDialog>;
}

function ArchiveProfessionalDialog({ person, onClose, onSaved }: { person: StoreProfessional; onClose: () => void; onSaved: () => void }) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => archiveProfessional(person.id, { expectedRevision: person.revision, reason }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); mutation.mutate(); };
  return <CatalogDialog title="Archive Professional" description="Archived, never deleted: a person who supervised a sale must stay nameable for as long as its record is kept." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field"><label htmlFor="professional-archive-reason">Reason</label><input id="professional-archive-reason" value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--danger" type="submit" disabled={!reason.trim() || mutation.isPending}>Archive</button>
      </div>
    </form>
  </CatalogDialog>;
}

function describe(caught: unknown): string {
  if (caught instanceof LocalServiceError) {
    if (caught.code === "authorization_denied") return "Only an owner can change drug compliance facts.";
    if (caught.code === "regulatory_period_overlaps") return "An election is already in force over part of that period.";
    if (caught.code === "record_conflict") return "That is already recorded.";
    return caught.message;
  }
  return "This could not be saved.";
}
