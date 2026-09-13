import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode
} from "react";
import { createPortal } from "react-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Navigate, useParams } from "react-router";
import type { ReferenceKind, ReferenceMasterResponse } from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { LocalServiceError } from "../platform/localService";
import {
  basisPointsToPercent,
  changeReferenceLifecycle,
  createReference,
  listReferences,
  percentToBasisPoints,
  updateReference,
  type ReferenceStatusFilter
} from "./referenceApi";

type Attributes = Record<string, unknown>;
type FormValues = Record<string, string | boolean>;

const SECTIONS = [
  { slug: "units", kind: "units", title: "Units of Measure", short: "Units", description: "Inventory dimensions, discrete handling, and quantity precision." },
  { slug: "dosage-forms", kind: "dosage-forms", title: "Dosage Forms", short: "Dosage Forms", description: "Reusable medicine presentation and administration hints." },
  { slug: "companies", kind: "companies", title: "Pharmaceutical Companies", short: "Companies", description: "Manufacturers, marketers, and their optional verified identifiers." },
  { slug: "brands", kind: "brands", title: "Brands", short: "Brands", description: "Trade names with optional owning companies." },
  { slug: "hsn", kind: "hsn-codes", title: "HSN Codes", short: "HSN", description: "Jurisdiction-scoped classification references." },
  { slug: "tax", kind: "tax-categories", title: "Tax Categories", short: "Tax Categories", description: "Tax treatment and effective-dated percentage versions." },
  { slug: "regulatory", kind: "regulatory-categories", title: "Regulatory References", short: "Regulatory", description: "Verified reference metadata without legal enforcement." },
  { slug: "ingredients", kind: "ingredients", title: "Ingredients", short: "Ingredients", description: "Active moieties used in medicine composition, independent of salt form." },
  { slug: "salt-forms", kind: "salt-forms", title: "Salt Forms", short: "Salt Forms", description: "Chemical form modifiers applied to an ingredient." },
  { slug: "strength-units", kind: "strength-units", title: "Strength Units", short: "Strength Units", description: "Units a stated strength may use. Separate from inventory units of measure." },
  { slug: "states", kind: "state-codes", title: "States", short: "States", description: "Jurisdiction State codes used as the place of supply and on addresses." }
] as const;

type Section = (typeof SECTIONS)[number];

export function ReferenceDataLandingPage() {
  usePageTitle("Reference Data");
  return <>
    <PageHeading eyebrow="MASTERS" title="Reference Data" description="Maintain the shared classifications used by pharmacy records. Every value shown here comes from the Local Store Service." />
    <section className="reference-landing" aria-label="Reference master groups">
      {SECTIONS.map((section) => <Link className="reference-card" key={section.slug} to={`/app/reference/${section.slug}`}>
        <span className="reference-card__mark" aria-hidden="true">{section.short.slice(0, 1)}</span>
        <span><strong>{section.title}</strong><small>{section.description}</small></span>
        <b aria-hidden="true">→</b>
      </Link>)}
    </section>
  </>;
}

export function ReferenceDataSectionPage() {
  const { section: slug } = useParams();
  const section = SECTIONS.find((candidate) => candidate.slug === slug);
  if (!section) return <Navigate to="/app/reference" replace />;
  return <MasterPage section={section} />;
}

function MasterPage({ section }: { section: Section }) {
  usePageTitle(section.title);
  const auth = useAuth();
  const queryClient = useQueryClient();
  const [searchInput, setSearchInput] = useState("");
  const search = useDebouncedValue(searchInput, 250);
  const [status, setStatus] = useState<ReferenceStatusFilter>("active");
  const [editing, setEditing] = useState<ReferenceMasterResponse | "new" | null>(null);
  const [lifecycle, setLifecycle] = useState<ReferenceMasterResponse | null>(null);
  const [related, setRelated] = useState<ReferenceMasterResponse | null>(null);
  const canMutate = auth.status?.user?.role === "owner_admin";
  const queryKey = ["reference", section.kind, search, status];
  const records = useQuery({
    queryKey,
    queryFn: () => listReferences(section.kind, search, status),
    retry: false
  });
  useEffect(() => {
    if (records.error instanceof LocalServiceError && ["authentication_required", "session_expired"].includes(records.error.code)) void auth.expireSession();
  }, [records.error]);
  const companies = useQuery({
    queryKey: ["reference", "companies", "selector"],
    queryFn: () => listReferences("companies", "", "active"),
    enabled: section.kind === "brands",
    retry: false
  });
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["reference"] });

  return <>
    <nav className="breadcrumbs" aria-label="Breadcrumb"><Link to="/app/reference">Reference Data</Link><span aria-hidden="true">/</span><span>{section.title}</span></nav>
    <PageHeading eyebrow="REFERENCE MASTER" title={section.title} description={section.description}>
      {canMutate ? <button className="button button--primary" type="button" onClick={() => setEditing("new")}>Add {singular(section.kind)}</button> : <span className="read-only-note">Read-only access</span>}
    </PageHeading>
    <section className="master-panel" aria-labelledby="master-list-title">
      <div className="master-toolbar">
        <div className="search-field"><label htmlFor="reference-search">Search</label><span><input id="reference-search" value={searchInput} onChange={(event) => setSearchInput(event.target.value)} placeholder={`Search ${section.short.toLowerCase()}`} />{searchInput && <button type="button" onClick={() => setSearchInput("")}>Clear</button>}</span></div>
        <div className="filter-field"><label htmlFor="reference-status">Status</label><select id="reference-status" value={status} onChange={(event) => setStatus(event.target.value as ReferenceStatusFilter)}><option value="active">Active</option><option value="archived">Archived</option><option value="all">All</option></select></div>
      </div>
      <h2 id="master-list-title" className="sr-only">{section.title} records</h2>
      {records.isPending ? <TableLoading /> : records.isError ? <QueryError onRetry={() => void records.refetch()} /> : records.data.length === 0 ? <EmptyState searching={Boolean(search)} archived={status === "archived"} /> :
        <ReferenceTable records={records.data} kind={section.kind} canMutate={canMutate} onEdit={setEditing} onLifecycle={setLifecycle} onRelated={setRelated} companies={companies.data ?? []} />}
    </section>
    {editing && <MasterEditor kind={section.kind} record={editing === "new" ? undefined : editing} companies={companies.data ?? []} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void refresh(); }} />}
    {lifecycle && <LifecycleDialog record={lifecycle} onClose={() => setLifecycle(null)} onSaved={() => { setLifecycle(null); void refresh(); }} />}
    {related && <RelatedRecordsDialog parent={related} parentKind={section.kind} canMutate={canMutate} onClose={() => setRelated(null)} />}
  </>;
}

function usePageTitle(title: string) {
  useEffect(() => {
    const previousTitle = document.title;
    document.title = `${title} | AUSHADHARTH`;
    return () => {
      document.title = previousTitle;
    };
  }, [title]);
}

function ReferenceTable({ records, kind, canMutate, onEdit, onLifecycle, onRelated, companies }: {
  records: ReferenceMasterResponse[];
  kind: ReferenceKind;
  canMutate: boolean;
  onEdit: (record: ReferenceMasterResponse) => void;
  onLifecycle: (record: ReferenceMasterResponse) => void;
  onRelated: (record: ReferenceMasterResponse) => void;
  companies: ReferenceMasterResponse[];
}) {
  const columns = columnsFor(kind);
  return <div className="table-scroll"><table className="data-table"><thead><tr>{columns.map((column) => <th key={column.key} scope="col">{column.label}</th>)}<th scope="col">Status</th><th scope="col"><span className="sr-only">Actions</span></th></tr></thead><tbody>{records.map((record) => {
    const attributes = record.attributes as unknown as Attributes;
    return <tr key={record.id}>{columns.map((column) => <td key={column.key} data-label={column.label}>{displayValue(attributes, column.key, companies)}</td>)}<td data-label="Status"><StatusBadge status={record.status} /></td><td className="row-actions"><div>
      {(kind === "companies" || kind === "tax-categories") && <button type="button" onClick={() => onRelated(record)}>{kind === "companies" ? "Identifiers" : "Rate history"}</button>}
      {canMutate && record.status === "active" && kind !== "tax-rate-versions" && <button type="button" onClick={() => onEdit(record)}>Edit</button>}
      {canMutate && <button type="button" onClick={() => onLifecycle(record)}>{record.status === "active" ? "Archive" : "Restore"}</button>}
    </div></td></tr>;
  })}</tbody></table></div>;
}

function RelatedRecordsDialog({ parent, parentKind, canMutate, onClose }: { parent: ReferenceMasterResponse; parentKind: ReferenceKind; canMutate: boolean; onClose: () => void }) {
  const childKind: ReferenceKind = parentKind === "companies" ? "company-identifiers" : "tax-rate-versions";
  const title = parentKind === "companies" ? "Company identifiers" : "Tax rate history";
  const [editing, setEditing] = useState<ReferenceMasterResponse | "new" | null>(null);
  const [lifecycle, setLifecycle] = useState<ReferenceMasterResponse | null>(null);
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: ["reference", childKind, parent.id], queryFn: () => listReferences(childKind, "", "all", parent.id), retry: false });
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["reference"] });
  return <Modal title={title} description={`Managed separately from ${primaryName(parent)}.`} onClose={onClose} wide suspended={Boolean(editing || lifecycle)}>
    <div className="related-heading"><p>{parentKind === "companies" ? "Identifiers are optional. Verified namespace/value pairs must be unique." : "Rates are stored as exact basis points. Active periods use [from, to), so adjacent dates are allowed."}</p>{canMutate && <button className="button button--primary" type="button" onClick={() => setEditing("new")}>Add {parentKind === "companies" ? "identifier" : "rate version"}</button>}</div>
    {query.isPending ? <TableLoading /> : query.isError ? <QueryError onRetry={() => void query.refetch()} /> : query.data.length === 0 ? <EmptyState searching={false} archived={false} /> : <ReferenceTable records={query.data} kind={childKind} canMutate={canMutate} onEdit={setEditing} onLifecycle={setLifecycle} onRelated={() => undefined} companies={[]} />}
    {editing && <MasterEditor kind={childKind} record={editing === "new" ? undefined : editing} parentId={parent.id} companies={[]} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void refresh(); }} />}
    {lifecycle && <LifecycleDialog record={lifecycle} onClose={() => setLifecycle(null)} onSaved={() => { setLifecycle(null); void refresh(); }} />}
  </Modal>;
}

function MasterEditor({ kind, record, parentId, companies, onClose, onSaved }: { kind: ReferenceKind; record?: ReferenceMasterResponse; parentId?: string; companies: ReferenceMasterResponse[]; onClose: () => void; onSaved: () => void }) {
  const formRef = useRef<HTMLFormElement>(null);
  const [values, setValues] = useState<FormValues>(() => initialValues(kind, record, parentId));
  const [submitted, setSubmitted] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const mutation = useMutation({
    mutationFn: async () => {
      const validation = validateForm(kind, values);
      if (Object.keys(validation).length) { setFieldErrors(validation); throw new ClientValidationError(); }
      const attributes = toAttributes(kind, values, parentId);
      return record ? updateReference(record, attributes) : createReference(kind, attributes);
    },
    onSuccess: onSaved,
    onError: (error) => {
      if (error instanceof ClientValidationError) return;
      if (error instanceof LocalServiceError) {
        setFieldErrors(Object.fromEntries(error.issues.map((issue) => [issue.field, issue.message])));
        setNotice(error.message);
      } else setNotice("The Local Store Service could not save this record.");
    }
  });
  const submit = (event: FormEvent) => { event.preventDefault(); setSubmitted(true); setNotice(null); mutation.mutate(); };
  const set = (field: string, value: string | boolean) => { setValues((current) => ({ ...current, [field]: value })); setFieldErrors((current) => { const next = { ...current }; delete next[field]; return next; }); };
  const errors = submitted ? { ...validateForm(kind, values), ...fieldErrors } : fieldErrors;
  useEffect(() => {
    if (submitted && Object.keys(errors).length) formRef.current?.querySelector<HTMLElement>('[aria-invalid="true"]')?.focus();
  }, [submitted, Object.keys(errors).join("|")]);
  return <Modal title={`${record ? "Edit" : "Add"} ${singular(kind)}`} description={record ? `Revision ${record.revision}. Saving requires this revision to remain current.` : undefined} onClose={onClose}>
    <form ref={formRef} className="master-form" onSubmit={submit} noValidate>
      <FormFields kind={kind} values={values} set={set} errors={errors} companies={companies} />
      {notice && <div className="inline-notice inline-notice--error" role="alert" aria-live="assertive">{notice}{mutation.error instanceof LocalServiceError && mutation.error.code === "revision_conflict" && <button type="button" onClick={onClose}>Reload latest</button>}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={mutation.isPending}>{mutation.isPending ? "Saving…" : "Save"}</button></div>
    </form>
  </Modal>;
}

function LifecycleDialog({ record, onClose, onSaved }: { record: ReferenceMasterResponse; onClose: () => void; onSaved: () => void }) {
  const action = record.status === "active" ? "archive" : "restore";
  const [reason, setReason] = useState(action === "restore" ? "Required again" : "");
  const mutation = useMutation({ mutationFn: () => changeReferenceLifecycle(record, action, reason), onSuccess: onSaved });
  return <Modal title={`${action === "archive" ? "Archive" : "Restore"} ${primaryName(record)}`} description={action === "archive" ? "Archiving retains this record and its complete history." : "Restore reruns current duplicate and integrity checks."} onClose={onClose}>
    <form className="master-form" onSubmit={(event) => { event.preventDefault(); if (reason.trim()) mutation.mutate(); }}><Field label="Reason" field="reason" value={reason} onChange={setReason} required error={!reason.trim() ? "A reason is required." : undefined} />
      {mutation.error && <div className="inline-notice inline-notice--error" role="alert">{mutation.error instanceof LocalServiceError ? mutation.error.message : "The lifecycle action failed."}</div>}
      <div className="form-actions"><button className="button button--secondary" type="button" onClick={onClose}>Cancel</button><button className="button button--primary" type="submit" disabled={!reason.trim() || mutation.isPending}>{mutation.isPending ? "Saving…" : action === "archive" ? "Archive record" : "Restore record"}</button></div>
    </form>
  </Modal>;
}

function FormFields({ kind, values, set, errors, companies }: { kind: ReferenceKind; values: FormValues; set: (field: string, value: string | boolean) => void; errors: Record<string, string>; companies: ReferenceMasterResponse[] }) {
  const field = (label: string, name: string, required = false, type = "text", hint?: string) => <Field label={label} field={name} value={String(values[name] ?? "")} onChange={(value) => set(name, value)} required={required} type={type} hint={hint} error={errors[name]} />;
  switch (kind) {
    case "units": return <>{field("Canonical code", "canonicalCode", true)}{field("Display name", "displayName", true)}<SelectField label="Dimension" field="dimension" value={String(values.dimension)} onChange={(value) => set("dimension", value)} options={[["count", "Count"], ["container", "Container"], ["volume", "Volume"], ["mass", "Mass"]]} /><label className="check-field"><input type="checkbox" checked={Boolean(values.isDiscrete)} onChange={(event) => { set("isDiscrete", event.target.checked); if (event.target.checked) set("allowedScale", "0"); }} /> Discrete unit</label>{field("Allowed decimal scale", "allowedScale", true, "number", "Discrete units must use scale 0; continuous units support 0–6.")}</>;
    case "dosage-forms": return <>{field("Canonical code", "canonicalCode", true)}{field("Display name", "displayName", true)}{field("Description", "description")}{field("Route hint", "routeHint")}{field("Release hint", "releaseHint")}</>;
    case "companies": return <>{field("Display / trade name", "displayName", true)}{field("Legal name", "legalName")}{field("City", "city")}{field("State", "state")}{field("Country code", "countryCode", false, "text", "Two-letter code, for example IN.")}</>;
    case "company-identifiers": return <>{field("Namespace", "namespace", true)}{field("Identifier value", "normalizedValue", true)}<SelectField label="Verification state" field="verificationState" value={String(values.verificationState)} onChange={(value) => set("verificationState", value)} options={[["unverified", "Unverified"], ["verified", "Verified"], ["rejected", "Rejected"]]} /></>;
    case "brands": return <>{field("Display / trade name", "displayName", true)}<SelectField label="Owner company (optional)" field="brandOwnerCompanyId" value={String(values.brandOwnerCompanyId)} onChange={(value) => set("brandOwnerCompanyId", value)} options={[["", "No owner selected"], ...companies.map((company) => [company.id, primaryName(company)] as [string, string])]} /></>;
    case "hsn-codes": return <>{field("Jurisdiction", "jurisdiction", true)}{field("HSN code", "hsnCode", true)}{field("Description", "description", true)}</>;
    case "tax-categories": return <>{field("Jurisdiction", "jurisdiction", true)}{field("Category code", "categoryCode", true)}{field("Display name", "displayName", true)}<SelectField label="Tax treatment" field="taxTreatment" value={String(values.taxTreatment)} onChange={(value) => set("taxTreatment", value)} options={[["taxable", "Taxable"], ["exempt", "Exempt"], ["nil_rated", "Nil rated"], ["non_gst", "Non-GST"]]} /></>;
    case "tax-rate-versions": return <>{field("Effective from", "effectiveFrom", true, "date")}{field("Effective to (exclusive)", "effectiveTo", false, "date", "Leave empty for an open-ended period. The end date itself belongs to the next period.")}{field("CGST (%)", "cgstPercent", true)}{field("SGST (%)", "sgstPercent", true)}{field("IGST (%)", "igstPercent", true)}{field("Cess (%)", "cessPercent", true)}</>;
    case "regulatory-categories": return <><div className="reference-disclaimer" role="note">Reference metadata only — not legal advice or an enforcement rule.</div>{field("Jurisdiction", "jurisdiction", true)}{field("Category system", "categorySystem", true)}{field("Code", "categoryCode", true)}{field("Display name", "displayName", true)}{field("Effective from", "effectiveFrom", false, "date")}{field("Effective to", "effectiveTo", false, "date")}{field("Source / reference", "sourceReference")}<SelectField label="Verification state" field="verificationState" value={String(values.verificationState)} onChange={(value) => set("verificationState", value)} options={[["unverified", "Unverified"], ["verified", "Verified"], ["rejected", "Rejected"]]} /></>;
  }
}

function Modal({ title, description, onClose, wide = false, suspended = false, children }: { title: string; description?: string; onClose: () => void; wide?: boolean; suspended?: boolean; children: ReactNode }) {
  const panel = useRef<HTMLDivElement>(null);
  const previousFocus = useRef(document.activeElement as HTMLElement | null);
  const titleId = useId();
  const closeRef = useRef(onClose);
  const suspendedRef = useRef(suspended);
  suspendedRef.current = suspended;
  useEffect(() => { closeRef.current = onClose; }, [onClose]);
  useEffect(() => {
    const element = panel.current;
    const focusable = () => Array.from(element?.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href]') ?? []);
    focusable()[0]?.focus();
    const keydown = (event: KeyboardEvent) => {
      if (suspendedRef.current) return;
      if (event.key === "Escape") { event.preventDefault(); closeRef.current(); return; }
      if (event.key !== "Tab") return;
      const items = focusable(); if (!items.length) return;
      const first = items[0]; const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", keydown);
    return () => { document.removeEventListener("keydown", keydown); previousFocus.current?.focus(); };
  }, []);
  return createPortal(<div className="modal-backdrop"><div ref={panel} className={`modal-panel ${wide ? "modal-panel--wide" : ""}`} role="dialog" aria-modal="true" inert={suspended || undefined} aria-hidden={suspended || undefined} aria-labelledby={titleId}><header><div><h2 id={titleId}>{title}</h2>{description && <p>{description}</p>}</div><button type="button" className="modal-close" onClick={onClose} aria-label="Close dialog">×</button></header>{children}</div></div>, document.body);
}

function Field({ label, field, value, onChange, required, type = "text", hint, error }: { label: string; field: string; value: string; onChange: (value: string) => void; required?: boolean; type?: string; hint?: string; error?: string }) {
  const id = `reference-${field}`; const help = error || hint ? `${id}-help` : undefined;
  return <div className="field"><label htmlFor={id}>{label}{required && <span aria-hidden="true"> *</span>}</label><input id={id} name={field} type={type} value={value} onChange={(event) => onChange(event.target.value)} required={required} aria-invalid={Boolean(error)} aria-describedby={help} />{help && <small id={help} className={error ? "field-error" : ""}>{error ?? hint}</small>}</div>;
}

function SelectField({ label, field, value, onChange, options }: { label: string; field: string; value: string; onChange: (value: string) => void; options: Array<readonly [string, string]> }) {
  const id = `reference-${field}`;
  return <div className="field"><label htmlFor={id}>{label}</label><select id={id} value={value} onChange={(event) => onChange(event.target.value)}>{options.map(([option, text]) => <option key={option} value={option}>{text}</option>)}</select></div>;
}

function PageHeading({ eyebrow, title, description, children }: { eyebrow: string; title: string; description: string; children?: ReactNode }) { return <header className="page-header"><div><p className="eyebrow">{eyebrow}</p><h1>{title}</h1><p>{description}</p></div>{children}</header>; }
function TableLoading() { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading reference data…</b></div>; }
function QueryError({ onRetry }: { onRetry: () => void }) { return <div className="empty-state" role="alert"><h3>Reference data could not be loaded</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={onRetry}>Retry</button></div>; }
function EmptyState({ searching, archived }: { searching: boolean; archived: boolean }) { return <div className="empty-state"><h3>{searching ? "No matching records" : archived ? "No archived records" : "No records yet"}</h3><p>{searching ? "Clear or change the search to see more results." : "Records created in this master will appear here."}</p></div>; }
function StatusBadge({ status }: { status: string }) { return <span className={`status-badge status-badge--${status}`}>{status === "active" ? "Active" : "Archived"}</span>; }
function useDebouncedValue(value: string, milliseconds: number) { const [debounced, setDebounced] = useState(value); useEffect(() => { const timer = window.setTimeout(() => setDebounced(value), milliseconds); return () => window.clearTimeout(timer); }, [value, milliseconds]); return debounced; }
class ClientValidationError extends Error {}

function singular(kind: ReferenceKind): string { return ({ units: "unit", "dosage-forms": "dosage form", companies: "company", "company-identifiers": "identifier", brands: "brand", "hsn-codes": "HSN code", "tax-categories": "tax category", "tax-rate-versions": "tax rate version", "regulatory-categories": "regulatory reference", ingredients: "ingredient", "salt-forms": "salt form", "strength-units": "strength unit", "state-codes": "State" })[kind]; }
function attrs(record: ReferenceMasterResponse): Attributes { return record.attributes as unknown as Attributes; }
function primaryName(record: ReferenceMasterResponse): string { const value = attrs(record); return String(value.displayName ?? value.canonicalCode ?? value.hsnCode ?? value.categoryCode ?? value.normalizedValue ?? "Reference record"); }

function columnsFor(kind: ReferenceKind): Array<{ key: string; label: string }> {
  return ({
    units: [{ key: "canonicalCode", label: "Code" }, { key: "displayName", label: "Name" }, { key: "dimension", label: "Dimension" }, { key: "allowedScale", label: "Scale" }],
    "dosage-forms": [{ key: "canonicalCode", label: "Code" }, { key: "displayName", label: "Name" }, { key: "routeHint", label: "Route hint" }],
    companies: [{ key: "displayName", label: "Trade name" }, { key: "legalName", label: "Legal name" }, { key: "location", label: "Location" }],
    "company-identifiers": [{ key: "namespace", label: "Namespace" }, { key: "normalizedValue", label: "Value" }, { key: "verificationState", label: "Verification" }],
    brands: [{ key: "displayName", label: "Brand" }, { key: "brandOwnerCompanyId", label: "Owner company" }],
    "hsn-codes": [{ key: "jurisdiction", label: "Jurisdiction" }, { key: "hsnCode", label: "HSN code" }, { key: "description", label: "Description" }],
    "tax-categories": [{ key: "jurisdiction", label: "Jurisdiction" }, { key: "categoryCode", label: "Code" }, { key: "displayName", label: "Name" }, { key: "taxTreatment", label: "Treatment" }],
    "tax-rate-versions": [{ key: "period", label: "Effective period" }, { key: "cgstBasisPoints", label: "CGST" }, { key: "sgstBasisPoints", label: "SGST" }, { key: "igstBasisPoints", label: "IGST" }, { key: "cessBasisPoints", label: "Cess" }],
    "regulatory-categories": [{ key: "jurisdiction", label: "Jurisdiction" }, { key: "categorySystem", label: "System" }, { key: "categoryCode", label: "Code" }, { key: "displayName", label: "Name" }, { key: "verificationState", label: "Verification" }],
    ingredients: [{ key: "canonicalCode", label: "Code" }, { key: "displayName", label: "Ingredient" }, { key: "description", label: "Description" }],
    "salt-forms": [{ key: "canonicalCode", label: "Code" }, { key: "displayName", label: "Salt / Form" }],
    "strength-units": [{ key: "canonicalCode", label: "Code" }, { key: "displayName", label: "Name" }, { key: "dimension", label: "Dimension" }, { key: "allowedScale", label: "Scale" }],
    "state-codes": [{ key: "jurisdiction", label: "Jurisdiction" }, { key: "stateCode", label: "Code" }, { key: "displayName", label: "State" }]
  })[kind];
}

function displayValue(attributes: Attributes, key: string, companies: ReferenceMasterResponse[]): string {
  if (key === "location") return [attributes.city, attributes.state, attributes.countryCode].filter(Boolean).join(", ") || "—";
  if (key === "period") return `${String(attributes.effectiveFrom)} → ${attributes.effectiveTo ? String(attributes.effectiveTo) : "Open ended"}`;
  if (key.endsWith("BasisPoints")) return `${basisPointsToPercent(Number(attributes[key]))}%`;
  if (key === "brandOwnerCompanyId") return companies.find((company) => company.id === attributes[key]) ? primaryName(companies.find((company) => company.id === attributes[key])!) : "—";
  const value = attributes[key];
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (value === null || value === undefined || value === "") return "—";
  const labels: Record<string, string> = { taxable: "Taxable", exempt: "Exempt", nil_rated: "Nil rated", non_gst: "Non-GST", unverified: "Unverified", verified: "Verified", rejected: "Rejected" };
  return labels[String(value)] ?? String(value);
}

function initialValues(kind: ReferenceKind, record?: ReferenceMasterResponse, parentId?: string): FormValues {
  const existing = record ? attrs(record) : {};
  const defaults: Record<ReferenceKind, FormValues> = {
    units: { canonicalCode: "", displayName: "", dimension: "count", isDiscrete: true, allowedScale: "0" },
    "dosage-forms": { canonicalCode: "", displayName: "", description: "", routeHint: "", releaseHint: "" },
    companies: { displayName: "", legalName: "", city: "", state: "", countryCode: "IN" },
    "company-identifiers": { companyId: parentId ?? "", namespace: "", normalizedValue: "", verificationState: "unverified" },
    brands: { displayName: "", brandOwnerCompanyId: "" },
    "hsn-codes": { jurisdiction: "IN", hsnCode: "", description: "" },
    "tax-categories": { jurisdiction: "IN", categoryCode: "", displayName: "", taxTreatment: "taxable" },
    "tax-rate-versions": { taxCategoryId: parentId ?? "", effectiveFrom: "", effectiveTo: "", cgstPercent: "0.00", sgstPercent: "0.00", igstPercent: "0.00", cessPercent: "0.00" },
    "regulatory-categories": { jurisdiction: "IN", categorySystem: "", categoryCode: "", displayName: "", effectiveFrom: "", effectiveTo: "", sourceReference: "", verificationState: "unverified" },
    ingredients: { canonicalCode: "", displayName: "", description: "" },
    "salt-forms": { canonicalCode: "", displayName: "" },
    "strength-units": { canonicalCode: "", displayName: "", dimension: "mass", allowedScale: "3" },
    "state-codes": { jurisdiction: "IN", stateCode: "", displayName: "" }
  };
  const value = { ...defaults[kind], ...existing } as FormValues;
  if (kind === "tax-rate-versions" && record) {
    value.cgstPercent = basisPointsToPercent(Number(existing.cgstBasisPoints)); value.sgstPercent = basisPointsToPercent(Number(existing.sgstBasisPoints)); value.igstPercent = basisPointsToPercent(Number(existing.igstBasisPoints)); value.cessPercent = basisPointsToPercent(Number(existing.cessBasisPoints));
  }
  return value;
}

function validateForm(kind: ReferenceKind, values: FormValues): Record<string, string> {
  const errors: Record<string, string> = {};
  const required: Partial<Record<ReferenceKind, string[]>> = { units: ["canonicalCode", "displayName", "allowedScale"], "dosage-forms": ["canonicalCode", "displayName"], companies: ["displayName"], "company-identifiers": ["namespace", "normalizedValue"], brands: ["displayName"], "hsn-codes": ["jurisdiction", "hsnCode", "description"], "tax-categories": ["jurisdiction", "categoryCode", "displayName"], "tax-rate-versions": ["effectiveFrom", "cgstPercent", "sgstPercent", "igstPercent", "cessPercent"], "regulatory-categories": ["jurisdiction", "categorySystem", "categoryCode", "displayName"], ingredients: ["canonicalCode", "displayName"], "salt-forms": ["canonicalCode", "displayName"], "strength-units": ["canonicalCode", "displayName", "allowedScale"], "state-codes": ["jurisdiction", "stateCode", "displayName"] };
  for (const field of required[kind] ?? []) if (!String(values[field] ?? "").trim()) errors[field] = "This field is required.";
  if (kind === "units") { const scale = Number(values.allowedScale); if (!Number.isInteger(scale) || scale < 0 || scale > 6 || (values.isDiscrete && scale !== 0)) errors.allowedScale = values.isDiscrete ? "Discrete units must use scale 0." : "Use an integer from 0 to 6."; }
  if (kind === "companies" && values.countryCode && !/^[A-Za-z]{2}$/.test(String(values.countryCode))) errors.countryCode = "Use a two-letter country code.";
  if (kind === "tax-rate-versions") for (const field of ["cgstPercent", "sgstPercent", "igstPercent", "cessPercent"]) if (percentToBasisPoints(String(values[field])) === null) errors[field] = "Enter a percentage from 0.00 to 100.00 with at most two decimals.";
  if ((kind === "tax-rate-versions" || kind === "regulatory-categories") && values.effectiveFrom && values.effectiveTo && String(values.effectiveTo) <= String(values.effectiveFrom)) errors.effectiveTo = "End date must be later than start date.";
  return errors;
}

function toAttributes(kind: ReferenceKind, values: FormValues, parentId?: string): Attributes {
  const nullable = (field: string) => String(values[field] ?? "").trim() || null;
  switch (kind) {
    case "units": return { canonicalCode: values.canonicalCode, displayName: values.displayName, dimension: values.dimension, isDiscrete: Boolean(values.isDiscrete), allowedScale: Number(values.allowedScale) };
    case "dosage-forms": return { canonicalCode: values.canonicalCode, displayName: values.displayName, description: nullable("description"), routeHint: nullable("routeHint"), releaseHint: nullable("releaseHint") };
    case "companies": return { displayName: values.displayName, legalName: nullable("legalName"), city: nullable("city"), state: nullable("state"), countryCode: nullable("countryCode") };
    case "company-identifiers": return { companyId: parentId, namespace: values.namespace, normalizedValue: values.normalizedValue, verificationState: values.verificationState };
    case "brands": return { displayName: values.displayName, brandOwnerCompanyId: nullable("brandOwnerCompanyId") };
    case "hsn-codes": return { jurisdiction: values.jurisdiction, hsnCode: values.hsnCode, description: values.description };
    case "tax-categories": return { jurisdiction: values.jurisdiction, categoryCode: values.categoryCode, displayName: values.displayName, taxTreatment: values.taxTreatment };
    case "tax-rate-versions": return { taxCategoryId: parentId, effectiveFrom: values.effectiveFrom, effectiveTo: nullable("effectiveTo"), cgstBasisPoints: percentToBasisPoints(String(values.cgstPercent)), sgstBasisPoints: percentToBasisPoints(String(values.sgstPercent)), igstBasisPoints: percentToBasisPoints(String(values.igstPercent)), cessBasisPoints: percentToBasisPoints(String(values.cessPercent)) };
    case "regulatory-categories": return { jurisdiction: values.jurisdiction, categorySystem: values.categorySystem, categoryCode: values.categoryCode, displayName: values.displayName, effectiveFrom: nullable("effectiveFrom"), effectiveTo: nullable("effectiveTo"), sourceReference: nullable("sourceReference"), verificationState: values.verificationState };
    case "ingredients": return { canonicalCode: values.canonicalCode, displayName: values.displayName, description: nullable("description") };
    case "salt-forms": return { canonicalCode: values.canonicalCode, displayName: values.displayName };
    case "strength-units": return { canonicalCode: values.canonicalCode, displayName: values.displayName, dimension: values.dimension, allowedScale: Number(values.allowedScale) };
    case "state-codes": return { jurisdiction: values.jurisdiction, stateCode: values.stateCode, displayName: values.displayName };
  }
}
