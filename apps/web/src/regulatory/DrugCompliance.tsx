import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  LicenceFormSchema,
  LicenceLegalStatusSchema,
  LicenceValidityBasisSchema,
  ProfessionalCapacitySchema,
  RecordElectionKindSchema,
  type ComplianceLicence,
  type LicenceDrugCoverage,
  type LicenceForm,
  type LicenceLegalStatus,
  type LicenceValidityBasis,
  type ProfessionalCapacity,
  type RecordElectionKind,
  type RecordElectionMethod,
  type StoreProfessional
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { ScrollableTable } from "../components/Ui";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { listProducts } from "../products/productApi";
import { PrescribersPanel } from "../prescriptions/Prescriptions";
import {
  CAPACITY_LABELS,
  ELECTION_LABELS,
  LEGAL_STATUS_LABELS,
  LICENCE_FORM_LABELS,
  METHODS_FOR,
  METHOD_LABELS,
  VALIDITY_BASIS_LABELS,
  archiveProfessional,
  closeDrugCoverage,
  createComplianceLicence,
  createDrugCoverage,
  createProfessional,
  createRecordElection,
  getDrugCompliance,
  updateLicenceAuthority
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
  const [dialog, setDialog] = useState<"professional" | "licence" | "coverage" | RecordElectionKind | null>(null);
  const [archiving, setArchiving] = useState<StoreProfessional | null>(null);
  const [authority, setAuthority] = useState<ComplianceLicence | null>(null);
  const [closingCoverage, setClosingCoverage] = useState<LicenceDrugCoverage | null>(null);
  const form20fLicences = (compliance.data?.complianceLicences ?? [])
    .filter((licence) => licence.licenceForm === "form_20f" && licence.status === "active");

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

        <PrescribersPanel />

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
            : <ScrollableTable><table className="data-table">
                <thead><tr><th scope="col">Form</th><th scope="col">Number</th><th scope="col">Standing</th><th scope="col">Runs</th><th scope="col">Status</th>{canMutate && <th scope="col"><span className="visually-hidden">Actions</span></th>}</tr></thead>
                <tbody>{compliance.data.complianceLicences.map((licence) => <tr key={licence.id} data-testid={`licence-${licence.id}`}>
                  <td>{LICENCE_FORM_LABELS[licence.licenceForm]}</td>
                  <td>{licence.licenceNumber}{licence.issuingAuthority && <small>{licence.issuingAuthority}</small>}</td>
                  <td><span className={`regulatory-answer regulatory-answer--${licence.legalStatus === "in_force" ? "does_not_apply" : licence.legalStatus === "unknown" ? "unknown" : "applies"}`}>{LEGAL_STATUS_LABELS[licence.legalStatus]}</span></td>
                  <td>{VALIDITY_BASIS_LABELS[licence.validityBasis]}<small>{runsFrom(licence.validFrom, licence.validUpto)}</small></td>
                  <td><span className={`status-badge status-badge--${licence.status}`}>{licence.status === "active" ? "Active" : "Archived"}</span></td>
                  {canMutate && <td className="table-actions">{licence.status === "active" && <button className="button button--secondary" type="button" onClick={() => setAuthority(licence)}>Standing</button>}</td>}
                </tr>)}</tbody>
              </table></ScrollableTable>}
        </section>

        <section className="master-panel" aria-labelledby="drug-coverage-title" data-testid="drug-coverage">
          <div className="panel-header">
            <div>
              <h2 id="drug-coverage-title">Schedule X Drug Coverage</h2>
              <p>Item 2 of Form 20F is "Names of drugs": the licence covers the drugs written on it and no others. Record each drug from the certificate or its endorsement. A drug being within Schedule X is what the law calls it, and says nothing about what this pharmacy is licensed to supply.</p>
            </div>
            {canMutate && form20fLicences.length > 0 && <button className="button button--secondary" type="button" onClick={() => setDialog("coverage")}>Add Drug</button>}
          </div>
          {form20fLicences.length === 0
            ? <p className="panel-note">No Form 20F is recorded, so there is no retail Schedule X licence to write drugs onto.</p>
            : compliance.data.drugCoverage.length === 0
            ? <p className="panel-note">No drug is recorded against this pharmacy's Form 20F.</p>
            : <ScrollableTable><table className="data-table">
                <thead><tr><th scope="col">Drug</th><th scope="col">Licence</th><th scope="col">In force</th><th scope="col">Authority</th><th scope="col">Status</th>{canMutate && <th scope="col"><span className="visually-hidden">Actions</span></th>}</tr></thead>
                <tbody>{compliance.data.drugCoverage.map((row) => <tr key={row.id} data-testid={`coverage-${row.id}`}>
                  <td>{row.productDisplayName}</td>
                  <td>{row.licenceNumber}</td>
                  <td>{row.effectiveFrom} to {row.effectiveTo ? `${row.effectiveTo} (exclusive)` : "further notice"}</td>
                  <td><span className="regulatory-source">{row.sourceCitation}</span>{row.reason && <small>{row.reason}</small>}</td>
                  <td><span className={`status-badge status-badge--${row.status}`}>{row.status === "active" ? "Active" : "Archived"}</span></td>
                  {canMutate && <td className="table-actions">{row.status === "active" && row.effectiveTo === null && <button className="button button--secondary" type="button" onClick={() => setClosingCoverage(row)}>End</button>}</td>}
                </tr>)}</tbody>
              </table></ScrollableTable>}
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
    {dialog && dialog !== "professional" && dialog !== "licence" && dialog !== "coverage" && <ElectionDialog election={dialog} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); refresh(); }} />}
    {archiving && <ArchiveProfessionalDialog person={archiving} onClose={() => setArchiving(null)} onSaved={() => { setArchiving(null); refresh(); }} />}
    {authority && <LicenceAuthorityDialog licence={authority} onClose={() => setAuthority(null)} onSaved={() => { setAuthority(null); refresh(); }} />}
    {dialog === "coverage" && <DrugCoverageDialog licences={form20fLicences} onClose={() => setDialog(null)} onSaved={() => { setDialog(null); refresh(); }} />}
    {closingCoverage && <CloseCoverageDialog coverage={closingCoverage} onClose={() => setClosingCoverage(null)} onSaved={() => { setClosingCoverage(null); refresh(); }} />}
  </div>;
}

/** The dates a licence runs between, said plainly, with "not recorded" where nothing was. */
function runsFrom(validFrom: string | null, validUpto: string | null): string {
  if (!validFrom && !validUpto) return "No dates recorded";
  return `${validFrom ?? "start not recorded"} to ${validUpto ?? "no end date"}`;
}

/**
 * A licence's standing, restated.
 *
 * The two answers are asked separately because they are separate facts, and the form refuses the
 * two combinations that cannot both be true rather than quietly fixing them: a fixed term needs
 * the date it runs to, and a perpetual licence — which is what a Form 20F is — has none.
 */
function LicenceAuthorityDialog({ licence, onClose, onSaved }: { licence: ComplianceLicence; onClose: () => void; onSaved: () => void }) {
  const [legalStatus, setLegalStatus] = useState<LicenceLegalStatus>(licence.legalStatus);
  const [validityBasis, setValidityBasis] = useState<LicenceValidityBasis>(licence.validityBasis);
  const [validFrom, setValidFrom] = useState(licence.validFrom ?? "");
  const [validUpto, setValidUpto] = useState(licence.validUpto ?? "");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => updateLicenceAuthority(licence.id, {
      expectedRevision: licence.revision,
      legalStatus,
      validityBasis,
      validFrom: validFrom || null,
      validUpto: validityBasis === "fixed_term" ? validUpto || null : null,
      reason: reason || null
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = validityBasis !== "fixed_term" || validUpto !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Licence Standing" description={`${LICENCE_FORM_LABELS[licence.licenceForm]} — ${licence.licenceNumber}. Record what the certificate and any later order say. Leaving this unrecorded is itself an answer: the software treats an unrecorded licence as no authority at all.`} onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="licence-legal-status">Legal status</label>
        <select id="licence-legal-status" value={legalStatus} onChange={(event) => setLegalStatus(event.target.value as LicenceLegalStatus)}>
          {LicenceLegalStatusSchema.options.map((option) => <option key={option} value={option}>{LEGAL_STATUS_LABELS[option]}</option>)}
        </select>
      </div>
      <div className="field">
        <label htmlFor="licence-validity-basis">Validity basis</label>
        <select id="licence-validity-basis" value={validityBasis} onChange={(event) => setValidityBasis(event.target.value as LicenceValidityBasis)}>
          {LicenceValidityBasisSchema.options.map((option) => <option key={option} value={option}>{VALIDITY_BASIS_LABELS[option]}</option>)}
        </select>
        <small>A licence in Form 20F is granted without an expiry: it stands until it is suspended or cancelled.</small>
      </div>
      <div className="field"><label htmlFor="licence-authority-from">Valid from (optional)</label><input id="licence-authority-from" type="date" value={validFrom} onChange={(event) => setValidFrom(event.target.value)} /></div>
      {validityBasis === "fixed_term" && <div className="field">
        <label htmlFor="licence-authority-upto">Valid up to</label>
        <input id="licence-authority-upto" type="date" value={validUpto} onChange={(event) => setValidUpto(event.target.value)} required />
      </div>}
      <div className="field"><label htmlFor="licence-authority-reason">Reason (optional)</label><input id="licence-authority-reason" value={reason} onChange={(event) => setReason(event.target.value)} placeholder="e.g. Order of the Licensing Authority dated…" maxLength={500} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Save Standing</button>
      </div>
    </form>
  </CatalogDialog>;
}

/** One drug written onto the Form 20F, from the date the licence or its endorsement says. */
function DrugCoverageDialog({ licences, onClose, onSaved }: { licences: ComplianceLicence[]; onClose: () => void; onSaved: () => void }) {
  const [licenceId, setLicenceId] = useState(licences.length === 1 ? licences[0]!.id : "");
  const [search, setSearch] = useState("");
  const [productId, setProductId] = useState("");
  const [effectiveFrom, setEffectiveFrom] = useState("");
  const [sourceCitation, setSourceCitation] = useState("");
  const [error, setError] = useState<string | null>(null);
  const products = useQuery({
    queryKey: ["products", "coverage", search],
    queryFn: () => listProducts(search, "active"),
    retry: false
  });
  const mutation = useMutation({
    mutationFn: () => createDrugCoverage({ licenceId, productId, effectiveFrom, effectiveTo: null, sourceCitation }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = licenceId !== "" && productId !== "" && effectiveFrom !== "" && sourceCitation.trim() !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Add Drug to Form 20F" description="Record a drug that this pharmacy's Form 20F names, with the date it took effect and where you read it. Nothing is inferred from the product's schedule classification." onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field">
        <label htmlFor="coverage-licence">Form 20F</label>
        <select id="coverage-licence" value={licenceId} onChange={(event) => setLicenceId(event.target.value)} required>
          <option value="">Choose a licence</option>
          {licences.map((licence) => <option key={licence.id} value={licence.id}>{licence.licenceNumber}</option>)}
        </select>
      </div>
      <div className="field"><label htmlFor="coverage-search">Find a drug</label><input id="coverage-search" value={search} onChange={(event) => { setSearch(event.target.value); setProductId(""); }} placeholder="Type part of the product name" /></div>
      <div className="field">
        <label htmlFor="coverage-product">Drug</label>
        <select id="coverage-product" value={productId} onChange={(event) => setProductId(event.target.value)} required>
          <option value="">{products.isPending ? "Loading products…" : "Choose a drug"}</option>
          {(products.data ?? []).map((product) => <option key={product.id} value={product.id}>{product.displayName}</option>)}
        </select>
      </div>
      <div className="field"><label htmlFor="coverage-from">In force from</label><input id="coverage-from" type="date" value={effectiveFrom} onChange={(event) => setEffectiveFrom(event.target.value)} required /></div>
      <div className="field"><label htmlFor="coverage-citation">Where this was read</label><input id="coverage-citation" value={sourceCitation} onChange={(event) => setSourceCitation(event.target.value)} required maxLength={300} placeholder="e.g. Form 20F item 2, names of drugs" /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Add Drug</button>
      </div>
    </form>
  </CatalogDialog>;
}

function CloseCoverageDialog({ coverage, onClose, onSaved }: { coverage: LicenceDrugCoverage; onClose: () => void; onSaved: () => void }) {
  const [effectiveTo, setEffectiveTo] = useState("");
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => closeDrugCoverage(coverage.id, { expectedRevision: coverage.revision, effectiveTo, reason }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = effectiveTo !== "" && reason.trim() !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="End Drug Coverage" description={`${coverage.productDisplayName} on ${coverage.licenceNumber}. The coverage keeps answering for every day it governed; it stops on the date you give, which is not itself covered.`} onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field"><label htmlFor="coverage-to">Covered up to (exclusive)</label><input id="coverage-to" type="date" value={effectiveTo} onChange={(event) => setEffectiveTo(event.target.value)} required /></div>
      <div className="field"><label htmlFor="coverage-reason">Reason</label><input id="coverage-reason" value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={300} placeholder="e.g. Struck off the licence on renewal" /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>End Coverage</button>
      </div>
    </form>
  </CatalogDialog>;
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
    if (caught.code === "licence_drug_coverage_period_overlaps") return "This drug already has Form 20F coverage over part of that period.";
    if (caught.code === "licence_validity_basis_incoherent") return "A fixed-term licence needs an expiry date, and a perpetual licence must not have one.";
    if (caught.code === "licence_drug_coverage_incoherent") return "Coverage must name an active drug and this pharmacy's own active Form 20F.";
    if (caught.code === "revision_conflict") return "This record changed after it was read. Reload and try again.";
    if (caught.code === "record_conflict") return "That is already recorded.";
    return caught.message;
  }
  return "This could not be saved.";
}
