import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import type {
  Prescriber,
  PrescriptionDetail,
  PrescriptionInput,
  PrescriptionSubjectKind,
  PrescriptionSupplyRecord,
  Product,
  RepeatAuthority
} from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { businessToday } from "../platform/businessDate";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import { getProduct, listProducts } from "../products/productApi";
import { getSale, setLinePrescription } from "../sales/saleApi";
import {
  RECORD_METHOD_LABELS,
  REPEAT_LABELS,
  confirmSupplyRecord,
  voidSupplyRecord,
  RECORD_STATUS_LABELS,
  getSupplyRecord,
  archivePrescriber,
  archivePrescription,
  atomsToUnitsText,
  correctPrescription,
  createPrescriber,
  createPrescription,
  getPrescription,
  listPrescribers,
  listPrescriptions,
  repeatText,
  unitsToAtoms,
  updatePrescriber
} from "./prescriptionApi";
import "./record.css";

/**
 * Phase 1M-B — prescriptions.
 *
 * A prescription holds personal information, so these pages exist for the dispensing roles only:
 * the owner and the pharmacist. A cashier sees a Sale's prescription reference and quantities at the
 * counter and nothing else. The list is searched by the pharmacy's own reference, never by a
 * patient's name, and no patient's particulars ever go into a URL.
 */

function useDispenser(): { allowed: boolean; owner: boolean } {
  const role = useAuth().status?.user?.role;
  return { allowed: role === "owner_admin" || role === "pharmacist", owner: role === "owner_admin" };
}

function DispenserOnly({ children }: { children: ReactNode }) {
  const { allowed } = useDispenser();
  if (allowed) return <>{children}</>;
  return <section className="master-panel" role="alert">
    <div className="empty-state">
      <h3>Prescriptions are not available to this role</h3>
      <p>Prescriptions hold patients' particulars. Only a pharmacist or the owner can enter or read them.</p>
      <Link className="button button--secondary" to="/app/sales">Back to Sales</Link>
    </div>
  </section>;
}

// ---------------------------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------------------------

export function PrescriptionListPage() {
  usePageTitle("Prescriptions");
  return <DispenserOnly><PrescriptionList /></DispenserOnly>;
}

function PrescriptionList() {
  const [reference, setReference] = useState("");
  const [search, setSearch] = useState("");
  const prescriptions = useQuery({
    queryKey: ["prescriptions", "list", search],
    queryFn: () => listPrescriptions(search),
    retry: false
  });
  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>Prescriptions</h1>
        <p>Prescriptions entered at the counter, by the pharmacy's own reference. A dispensed prescription is history: it can be archived, never rewritten.</p>
      </div>
      <Link className="button button--primary" to="/app/prescriptions/new">New Prescription</Link>
    </header>
    <section className="master-panel" aria-labelledby="prescription-list-title">
      <h2 id="prescription-list-title">Prescription records</h2>
      <form className="master-toolbar" onSubmit={(event) => { event.preventDefault(); setSearch(reference); }}>
        <div className="filter-field">
          <label htmlFor="prescription-reference-search">Reference</label>
          <input id="prescription-reference-search" value={reference} onChange={(event) => setReference(event.target.value)} placeholder="RX-000001" autoComplete="off" />
        </div>
        <button className="button button--secondary" type="submit">Find</button>
      </form>
      {prescriptions.isPending ? <Loading label="Loading prescriptions…" />
        : prescriptions.isError ? <QueryError label="Prescriptions could not be loaded" error={prescriptions.error} onRetry={() => void prescriptions.refetch()} />
        : prescriptions.data.length === 0 ? <div className="empty-state"><h3>No prescription found</h3><p>{search ? "No prescription has that reference." : "Enter a prescription when a Schedule H medicine is supplied."}</p></div>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Reference</th><th scope="col">Dated</th><th scope="col">Prescriber</th><th scope="col" className="numeric">Items</th><th scope="col">Status</th></tr></thead>
            <tbody>{prescriptions.data.map((row) => <tr key={row.id}>
              <td data-label="Reference"><Link to={`/app/prescriptions/${row.id}`}>{row.reference}</Link></td>
              <td data-label="Dated">{row.prescribedOn}</td>
              <td data-label="Prescriber">{row.prescriberName}</td>
              <td data-label="Items" className="numeric">{row.itemCount}</td>
              <td data-label="Status"><span className={`status-badge status-badge--${row.status}`}>{row.status === "active" ? "Active" : "Archived"}</span></td>
            </tr>)}</tbody>
          </table></div>}
    </section>
  </>;
}

// ---------------------------------------------------------------------------------------------
// Entry and correction
// ---------------------------------------------------------------------------------------------

type ItemForm = {
  key: number;
  productId: string;
  productName: string;
  scale: number;
  search: string;
  writtenDescription: string;
  quantity: string;
  doseText: string;
};

type FormState = {
  prescribedOn: string;
  prescriberId: string;
  prescriberName: string;
  prescriberAddress: string;
  prescriberRegistrationNumber: string;
  prescriberRegisteringAuthority: string;
  saveAsPrescriber: boolean;
  subjectKind: PrescriptionSubjectKind;
  subjectName: string;
  subjectAddress: string;
  directionsText: string;
  repeatAuthority: RepeatAuthority;
  repeatTimes: string;
  repeatIntervalDays: string;
  attested: boolean;
  items: ItemForm[];
};

let itemKey = 0;
function blankItem(): ItemForm {
  itemKey += 1;
  return { key: itemKey, productId: "", productName: "", scale: 0, search: "", writtenDescription: "", quantity: "", doseText: "" };
}

function blankForm(): FormState {
  return {
    prescribedOn: businessToday(),
    prescriberId: "",
    prescriberName: "",
    prescriberAddress: "",
    prescriberRegistrationNumber: "",
    prescriberRegisteringAuthority: "",
    saveAsPrescriber: false,
    subjectKind: "human",
    subjectName: "",
    subjectAddress: "",
    directionsText: "",
    repeatAuthority: "once",
    repeatTimes: "",
    repeatIntervalDays: "",
    attested: false,
    items: [blankItem()]
  };
}

function formFrom(detail: PrescriptionDetail, scales: Map<string, number>): FormState {
  return {
    prescribedOn: detail.prescribedOn,
    prescriberId: detail.prescriberId ?? "",
    prescriberName: detail.prescriberName,
    prescriberAddress: detail.prescriberAddress,
    prescriberRegistrationNumber: detail.prescriberRegistrationNumber ?? "",
    prescriberRegisteringAuthority: detail.prescriberRegisteringAuthority ?? "",
    saveAsPrescriber: false,
    subjectKind: detail.subjectKind,
    subjectName: detail.subjectName,
    subjectAddress: detail.subjectAddress,
    directionsText: detail.directionsText ?? "",
    repeatAuthority: detail.repeatAuthority,
    repeatTimes: detail.repeatTimes === null ? "" : String(detail.repeatTimes),
    repeatIntervalDays: detail.repeatIntervalDays === null ? "" : String(detail.repeatIntervalDays),
    attested: false,
    items: detail.items.map((item) => {
      const scale = scales.get(item.productId) ?? 0;
      return {
        ...blankItem(),
        productId: item.productId,
        productName: item.productDisplayName ?? item.writtenDescription,
        scale,
        writtenDescription: item.writtenDescription,
        quantity: atomsToUnitsText(item.prescribedQuantityAtoms, scale),
        doseText: item.doseText
      };
    })
  };
}

export function PrescriptionCreatePage() {
  usePageTitle("New Prescription");
  return <DispenserOnly><PrescriptionCreate /></DispenserOnly>;
}

function PrescriptionCreate() {
  const [params] = useSearchParams();
  const saleId = params.get("saleId");
  const lineId = params.get("lineId");
  const productId = params.get("productId");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [initial, setInitial] = useState<FormState | null>(productId ? null : blankForm());

  // Opened from a Sale line: the item starts as that line's product, so the counter only adds what
  // the paper says about it.
  useEffect(() => {
    if (!productId) return;
    let cancelled = false;
    void getProduct(productId).then((product) => {
      if (cancelled) return;
      const form = blankForm();
      form.items = [{ ...form.items[0], productId: product.id, productName: product.displayName, scale: product.quantityScale, writtenDescription: product.displayName }];
      setInitial(form);
    }).catch(() => { if (!cancelled) setInitial(blankForm()); });
    return () => { cancelled = true; };
  }, [productId]);

  const [linkProblem, setLinkProblem] = useState<{ text: string; id: string } | null>(null);
  const save = async (input: PrescriptionInput) => {
    const created = await createPrescription(input);
    void queryClient.invalidateQueries({ queryKey: ["prescriptions"] });
    if (saleId && lineId) {
      try {
        const sale = await getSale(saleId);
        const line = sale.lines.find((each) => each.id === lineId);
        const item = line ? created.items.find((each) => each.productId === line.productId) : undefined;
        if (!line || !item) throw new Error("no matching item");
        await setLinePrescription(lineId, sale.revision, item.id);
        void queryClient.invalidateQueries({ queryKey: ["sales"] });
        void navigate(`/app/sales/${saleId}`);
        return;
      } catch (caught) {
        setLinkProblem({
          id: created.id,
          text: caught instanceof LocalServiceError
            ? `${created.reference} was saved, but could not be linked to the sale: ${caught.message}`
            : `${created.reference} was saved, but it names no item for that sale line's product. Link it from the sale.`
        });
        return;
      }
    }
    void navigate(`/app/prescriptions/${created.id}`);
  };

  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>New Prescription</h1>
        <p>Enter what the prescription says. The paper stays with the pharmacy; AUSHADHARTH records its particulars and checks every supply against them.</p>
      </div>
      <Link className="button button--secondary" to={saleId ? `/app/sales/${saleId}` : "/app/prescriptions"}>{saleId ? "Back to the sale" : "Back to Prescriptions"}</Link>
    </header>
    {linkProblem && <div className="inline-notice inline-notice--error" role="alert">{linkProblem.text} <Link to={`/app/prescriptions/${linkProblem.id}`}>Open the prescription</Link></div>}
    {linkProblem ? null : initial ? <PrescriptionForm initial={initial} submitLabel={saleId ? "Save and link to the sale" : "Save prescription"} onSubmit={save} /> : <section className="master-panel"><Loading label="Preparing the form…" /></section>}
  </>;
}

export function PrescriptionEditPage() {
  usePageTitle("Correct Prescription");
  return <DispenserOnly><PrescriptionEdit /></DispenserOnly>;
}

function PrescriptionEdit() {
  const { id } = useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const detail = useQuery({ queryKey: ["prescriptions", "detail", id], queryFn: () => getPrescription(id!), enabled: Boolean(id), retry: false });
  const [initial, setInitial] = useState<FormState | null>(null);
  useEffect(() => {
    if (!detail.data) return;
    let cancelled = false;
    void Promise.all(detail.data.items.map((item) => getProduct(item.productId).then((product) => [item.productId, product.quantityScale] as const).catch(() => [item.productId, 0] as const)))
      .then((pairs) => { if (!cancelled) setInitial(formFrom(detail.data, new Map(pairs))); });
    return () => { cancelled = true; };
  }, [detail.data]);

  if (detail.isPending) return <section className="master-panel"><Loading label="Loading prescription…" /></section>;
  if (detail.isError) return <section className="master-panel"><QueryError label="This prescription could not be loaded" error={detail.error} onRetry={() => void detail.refetch()} /></section>;
  const dispensed = detail.data.dispensings.length > 0;
  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>Correct {detail.data.reference}</h1>
        <p>A prescription entered wrongly may be corrected until it is first dispensed. The correction is recorded against it.</p>
      </div>
      <Link className="button button--secondary" to={`/app/prescriptions/${detail.data.id}`}>Back to {detail.data.reference}</Link>
    </header>
    {dispensed || detail.data.status === "archived"
      ? <section className="master-panel"><div className="empty-state" role="alert"><h3>This prescription can no longer be corrected</h3><p>{dispensed ? "It has been dispensed, so the facts that supply relied on must not change. Archive it and enter a new one." : "It is archived."}</p></div></section>
      : initial ? <PrescriptionForm initial={initial} submitLabel="Save correction" onSubmit={async (input) => {
          await correctPrescription(detail.data.id, { ...input, expectedRevision: detail.data.revision });
          void queryClient.invalidateQueries({ queryKey: ["prescriptions"] });
          void navigate(`/app/prescriptions/${detail.data.id}`);
        }} />
      : <section className="master-panel"><Loading label="Preparing the form…" /></section>}
  </>;
}

/**
 * One page, in reading order of the paper: date, prescriber, patient, items, repeat, attestation.
 * Keyboard-first — every control is a native input in document order, and a refused save puts the
 * cursor on the field the Store Service named.
 */
function PrescriptionForm({ initial, submitLabel, onSubmit }: {
  initial: FormState;
  submitLabel: string;
  onSubmit: (input: PrescriptionInput) => Promise<void>;
}) {
  const { owner, allowed } = useDispenser();
  const [form, setForm] = useState<FormState>(initial);
  const [problem, setProblem] = useState<string | null>(null);
  const [issues, setIssues] = useState<Array<{ field: string; message: string }>>([]);
  const [busy, setBusy] = useState(false);
  const prescribers = useQuery({ queryKey: ["prescribers"], queryFn: listPrescribers, retry: false, enabled: allowed });
  const active = (prescribers.data ?? []).filter((each) => each.status === "active");
  const set = (change: Partial<FormState>) => setForm((current) => ({ ...current, ...change }));
  const setItem = (key: number, change: Partial<ItemForm>) =>
    setForm((current) => ({ ...current, items: current.items.map((item) => (item.key === key ? { ...item, ...change } : item)) }));

  const choosePrescriber = (id: string) => {
    const chosen = active.find((each) => each.id === id);
    set(chosen
      ? { prescriberId: id, prescriberName: chosen.fullName, prescriberAddress: chosen.addressText, prescriberRegistrationNumber: chosen.registrationNumber ?? "", prescriberRegisteringAuthority: chosen.registeringAuthority ?? "", saveAsPrescriber: false }
      : { prescriberId: "" });
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setProblem(null);
    setIssues([]);
    const fail = (fieldId: string, message: string) => {
      setProblem(message);
      document.getElementById(fieldId)?.focus();
    };
    if (!form.attested) return fail("rx-attested", "Confirm the prescription is in writing, signed and dated by the prescriber.");
    const items = [];
    for (const [index, item] of form.items.entries()) {
      if (!item.productId) return fail(`rx-item-${item.key}-search`, `Choose the product for item ${index + 1}.`);
      const atoms = unitsToAtoms(item.quantity, item.scale);
      if (atoms === null) return fail(`rx-item-${item.key}-quantity`, `Enter the total quantity for item ${index + 1} as prescribed.`);
      items.push({ productId: item.productId, writtenDescription: item.writtenDescription.trim() || item.productName, prescribedQuantityAtoms: atoms, doseText: item.doseText });
    }
    const repeatTimes = form.repeatAuthority === "stated_times" ? Number.parseInt(form.repeatTimes, 10) : null;
    if (form.repeatAuthority === "stated_times" && !(Number.isInteger(repeatTimes) && (repeatTimes ?? 0) >= 2)) {
      return fail("rx-repeat-times", "Enter how many times in all the prescriber allowed, 2 or more.");
    }
    const interval = form.repeatAuthority !== "once" && form.repeatIntervalDays.trim() ? Number.parseInt(form.repeatIntervalDays, 10) : null;
    setBusy(true);
    try {
      let prescriberId = form.prescriberId || null;
      if (!prescriberId && form.saveAsPrescriber) {
        const saved = await createPrescriber({
          fullName: form.prescriberName,
          addressText: form.prescriberAddress,
          registrationNumber: form.prescriberRegistrationNumber.trim() || null,
          registeringAuthority: form.prescriberRegisteringAuthority.trim() || null
        });
        prescriberId = saved.id;
      }
      await onSubmit({
        prescribedOn: form.prescribedOn,
        prescriberId,
        prescriberName: form.prescriberName,
        prescriberAddress: form.prescriberAddress,
        prescriberRegistrationNumber: form.prescriberRegistrationNumber.trim() || null,
        prescriberRegisteringAuthority: form.prescriberRegisteringAuthority.trim() || null,
        subjectKind: form.subjectKind,
        subjectName: form.subjectName,
        subjectAddress: form.subjectAddress,
        directionsText: form.directionsText.trim() || null,
        repeatAuthority: form.repeatAuthority,
        repeatTimes,
        repeatIntervalDays: interval,
        writtenSignedDatedAttested: form.attested,
        items
      });
    } catch (caught) {
      if (caught instanceof LocalServiceError) {
        setProblem(caught.message);
        setIssues([...caught.issues]);
        const first = caught.issues[0]?.field;
        if (first) document.getElementById(fieldIdFor(first, form))?.focus();
      } else {
        setProblem("The prescription could not be saved.");
      }
    } finally {
      setBusy(false);
    }
  };

  const subjectWord = form.subjectKind === "human" ? "Patient" : "Animal owner";
  return <form className="master-panel prescription-form" onSubmit={(event) => void submit(event)} aria-label="Prescription" noValidate>
    <fieldset className="prescription-form__group">
      <legend>Prescription</legend>
      <div className="field">
        <label htmlFor="rx-date">Dated</label>
        <input id="rx-date" type="date" value={form.prescribedOn} max={businessToday()} onChange={(event) => set({ prescribedOn: event.target.value })} required />
        <small>The date the prescriber wrote on it.</small>
      </div>
    </fieldset>

    <fieldset className="prescription-form__group">
      <legend>Prescriber</legend>
      <div className="field">
        <label htmlFor="rx-prescriber">From the prescriber list</label>
        <select id="rx-prescriber" value={form.prescriberId} onChange={(event) => choosePrescriber(event.target.value)} disabled={!prescribers.data}>
          <option value="">{prescribers.data ? "Type the prescriber's details below" : "Loading…"}</option>
          {active.map((each) => <option key={each.id} value={each.id}>{each.fullName}</option>)}
        </select>
        <small>Choosing one fills in the details; the prescription keeps its own copy of them.</small>
      </div>
      <div className="field"><label htmlFor="rx-prescriber-name">Prescriber's name</label><input id="rx-prescriber-name" value={form.prescriberName} maxLength={120} onChange={(event) => set({ prescriberName: event.target.value, prescriberId: "" })} required /></div>
      <div className="field"><label htmlFor="rx-prescriber-address">Prescriber's address</label><input id="rx-prescriber-address" value={form.prescriberAddress} maxLength={300} onChange={(event) => set({ prescriberAddress: event.target.value, prescriberId: "" })} required /></div>
      <div className="field"><label htmlFor="rx-prescriber-registration">Registration number <span className="row-subtext">(optional, as written)</span></label><input id="rx-prescriber-registration" value={form.prescriberRegistrationNumber} maxLength={60} onChange={(event) => set({ prescriberRegistrationNumber: event.target.value, prescriberId: "" })} /></div>
      <div className="field"><label htmlFor="rx-prescriber-authority">Registering council <span className="row-subtext">(optional)</span></label><input id="rx-prescriber-authority" value={form.prescriberRegisteringAuthority} maxLength={160} onChange={(event) => set({ prescriberRegisteringAuthority: event.target.value, prescriberId: "" })} /></div>
      {!form.prescriberId && <label className="check-field">
        <input id="rx-save-prescriber" type="checkbox" checked={form.saveAsPrescriber} onChange={(event) => set({ saveAsPrescriber: event.target.checked })} />
        <span>Add to the prescriber list<small>Recorded as written. AUSHADHARTH does not verify a prescriber's registration.</small></span>
      </label>}
      {!owner && form.prescriberId && <small className="row-subtext">Only the owner can correct an entry in the prescriber list.</small>}
    </fieldset>

    <fieldset className="prescription-form__group">
      <legend>For whom</legend>
      <div className="choice-row" role="radiogroup" aria-label="Prescribed for">
        <label><input type="radio" name="rx-subject-kind" checked={form.subjectKind === "human"} onChange={() => set({ subjectKind: "human" })} /> A patient</label>
        <label><input type="radio" name="rx-subject-kind" checked={form.subjectKind === "animal"} onChange={() => set({ subjectKind: "animal" })} /> An animal (record its owner)</label>
      </div>
      <div className="field"><label htmlFor="rx-subject-name">{subjectWord}'s name</label><input id="rx-subject-name" value={form.subjectName} maxLength={120} autoComplete="off" onChange={(event) => set({ subjectName: event.target.value })} required /></div>
      <div className="field"><label htmlFor="rx-subject-address">{subjectWord}'s address</label><input id="rx-subject-address" value={form.subjectAddress} maxLength={300} autoComplete="off" onChange={(event) => set({ subjectAddress: event.target.value })} required /></div>
    </fieldset>

    <fieldset className="prescription-form__group prescription-form__items">
      <legend>Items prescribed</legend>
      {form.items.map((item, index) => <ItemFields
        key={item.key}
        index={index}
        item={item}
        onChange={(change) => setItem(item.key, change)}
        onRemove={form.items.length > 1 ? () => set({ items: form.items.filter((each) => each.key !== item.key) }) : null}
      />)}
      <button className="button button--secondary" type="button" onClick={() => set({ items: [...form.items, blankItem()] })}>Add another item</button>
    </fieldset>

    <fieldset className="prescription-form__group">
      <legend>Repeats</legend>
      <div className="field">
        <label htmlFor="rx-repeat">What the prescriber stated</label>
        <select id="rx-repeat" value={form.repeatAuthority} onChange={(event) => set({ repeatAuthority: event.target.value as RepeatAuthority })}>
          {(Object.keys(REPEAT_LABELS) as RepeatAuthority[]).map((option) => <option key={option} value={option}>{REPEAT_LABELS[option]}</option>)}
        </select>
        <small>Unless the prescriber wrote that it may be dispensed more than once, it is dispensed once.</small>
      </div>
      {form.repeatAuthority === "stated_times" && <div className="field"><label htmlFor="rx-repeat-times">Number of times in all</label><input id="rx-repeat-times" className="numeric" inputMode="numeric" value={form.repeatTimes} onChange={(event) => set({ repeatTimes: event.target.value })} /></div>}
      {form.repeatAuthority !== "once" && <div className="field"><label htmlFor="rx-repeat-interval">Days between supplies <span className="row-subtext">(if stated)</span></label><input id="rx-repeat-interval" className="numeric" inputMode="numeric" value={form.repeatIntervalDays} onChange={(event) => set({ repeatIntervalDays: event.target.value })} /></div>}
      <div className="field"><label htmlFor="rx-directions">Directions <span className="row-subtext">(optional)</span></label><input id="rx-directions" value={form.directionsText} maxLength={500} onChange={(event) => set({ directionsText: event.target.value })} /></div>
    </fieldset>

    <label className="check-field prescription-form__attest">
      <input id="rx-attested" type="checkbox" checked={form.attested} onChange={(event) => set({ attested: event.target.checked })} />
      <span>I have seen this prescription in writing, signed and dated by the prescriber</span>
    </label>

    {problem && <div className="inline-notice inline-notice--error" role="alert">{problem}{issues.length > 0 && <ul>{issues.map((issue) => <li key={issue.field}>{issue.message}</li>)}</ul>}</div>}
    <div className="form-actions">
      <button className="button button--primary" type="submit" disabled={busy}>{busy ? "Saving…" : submitLabel}</button>
    </div>
  </form>;
}

function ItemFields({ index, item, onChange, onRemove }: {
  index: number;
  item: ItemForm;
  onChange: (change: Partial<ItemForm>) => void;
  onRemove: (() => void) | null;
}) {
  const products = useQuery({
    queryKey: ["products", "prescription-search", item.search],
    queryFn: () => listProducts(item.search, "active"),
    enabled: !item.productId && item.search.trim().length >= 2,
    retry: false
  });
  const choose = (product: Product) => onChange({
    productId: product.id,
    productName: product.displayName,
    scale: product.quantityScale,
    search: "",
    writtenDescription: item.writtenDescription || product.displayName
  });
  const prefix = `rx-item-${item.key}`;
  return <div className="prescription-item" role="group" aria-label={`Item ${index + 1}`}>
    <div className="field prescription-item__product">
      <label htmlFor={`${prefix}-search`}>Item {index + 1} — product</label>
      {item.productId
        ? <div className="prescription-item__chosen"><strong>{item.productName}</strong><button id={`${prefix}-search`} className="button button--secondary" type="button" onClick={() => onChange({ productId: "", productName: "", search: "" })}>Change</button></div>
        : <>
            <input id={`${prefix}-search`} value={item.search} autoComplete="off" onChange={(event) => onChange({ search: event.target.value })} placeholder="Type the product's name" />
            {item.search.trim().length >= 2 && <ul className="pos-results" role="listbox" aria-label="Matching products">
              {products.isPending ? <li className="pos-results__note">Searching…</li>
                : products.isError ? <li className="pos-results__note">Products could not be searched.</li>
                : products.data.length === 0 ? <li className="pos-results__note">Nothing matches that.</li>
                : products.data.slice(0, 8).map((match) => <li key={match.id}><button type="button" onClick={() => choose(match)}>{match.displayName}</button></li>)}
            </ul>}
          </>}
      <small>The exact preparation prescribed. Another product cannot be supplied against it, whatever it contains.</small>
    </div>
    <div className="field"><label htmlFor={`${prefix}-written`}>As written</label><input id={`${prefix}-written`} value={item.writtenDescription} maxLength={200} onChange={(event) => onChange({ writtenDescription: event.target.value })} /></div>
    <div className="field"><label htmlFor={`${prefix}-quantity`}>Total quantity (base units)</label><input id={`${prefix}-quantity`} className="numeric" inputMode="decimal" value={item.quantity} onChange={(event) => onChange({ quantity: event.target.value })} /></div>
    <div className="field"><label htmlFor={`${prefix}-dose`}>Dose</label><input id={`${prefix}-dose`} value={item.doseText} maxLength={200} onChange={(event) => onChange({ doseText: event.target.value })} placeholder="e.g. 1 tablet twice daily" /></div>
    {onRemove && <button className="button button--secondary" type="button" onClick={onRemove}>Remove item {index + 1}</button>}
  </div>;
}

/** Maps a Store Service issue field to the input that holds it. */
function fieldIdFor(field: string, form: FormState): string {
  const item = /^items\.(\d+)\.(\w+)$/.exec(field);
  if (item) {
    const key = form.items[Number(item[1])]?.key;
    const part = { productId: "search", writtenDescription: "written", prescribedQuantityAtoms: "quantity", doseText: "dose" }[item[2]] ?? "search";
    return `rx-item-${key}-${part}`;
  }
  return {
    prescribedOn: "rx-date",
    prescriberId: "rx-prescriber",
    prescriberName: "rx-prescriber-name",
    prescriberAddress: "rx-prescriber-address",
    prescriberRegistrationNumber: "rx-prescriber-registration",
    prescriberRegisteringAuthority: "rx-prescriber-authority",
    subjectKind: "rx-subject-name",
    subjectName: "rx-subject-name",
    subjectAddress: "rx-subject-address",
    directionsText: "rx-directions",
    repeatAuthority: "rx-repeat",
    repeatIntervalDays: "rx-repeat-interval",
    writtenSignedDatedAttested: "rx-attested",
    items: "rx-date"
  }[field] ?? "rx-date";
}

// ---------------------------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------------------------

export function PrescriptionDetailPage() {
  usePageTitle("Prescription");
  return <DispenserOnly><PrescriptionDetailView /></DispenserOnly>;
}

function PrescriptionDetailView() {
  const { id } = useParams();
  const queryClient = useQueryClient();
  const detail = useQuery({ queryKey: ["prescriptions", "detail", id], queryFn: () => getPrescription(id!), enabled: Boolean(id), retry: false });
  const [archiving, setArchiving] = useState(false);
  if (detail.isPending) return <section className="master-panel"><Loading label="Loading prescription…" /></section>;
  if (detail.isError) return <section className="master-panel"><QueryError label="This prescription could not be loaded" error={detail.error} onRetry={() => void detail.refetch()} /></section>;
  const rx = detail.data;
  const dispensed = rx.dispensings.length > 0;
  const occasions = rx.occasionsAuthorised === null
    ? `${rx.occasionsUsed} so far; repeats stated without a number, limited by the total quantity`
    : `${rx.occasionsUsed} of ${rx.occasionsAuthorised}`;
  return <>
    <header className="page-header">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{rx.reference}</h1>
        <p>Dated {rx.prescribedOn} · {rx.prescriberName} · <span className={`status-badge status-badge--${rx.status}`}>{rx.status === "active" ? "Active" : "Archived"}</span></p>
      </div>
      <div className="page-header__actions">
        {!dispensed && rx.status === "active" && <Link className="button button--secondary" to={`/app/prescriptions/${rx.id}/edit`}>Correct</Link>}
        {rx.status === "active" && <button className="button button--secondary" type="button" onClick={() => setArchiving(true)}>Archive</button>}
        <Link className="button button--secondary" to="/app/prescriptions">Back to Prescriptions</Link>
      </div>
    </header>

    <section className="master-panel" aria-labelledby="rx-facts-title">
      <h2 id="rx-facts-title">What the prescription says</h2>
      <dl className="detail-grid">
        <div><dt>Prescriber</dt><dd>{rx.prescriberName}<br /><small className="row-subtext">{rx.prescriberAddress}</small></dd></div>
        <div><dt>Registration number</dt><dd>{rx.prescriberRegistrationNumber ?? "Not recorded"}{rx.prescriberRegisteringAuthority && <><br /><small className="row-subtext">{rx.prescriberRegisteringAuthority}</small></>}</dd></div>
        <div><dt>{rx.subjectKind === "human" ? "Patient" : "Animal owner"}</dt><dd>{rx.subjectName}<br /><small className="row-subtext">{rx.subjectAddress}</small></dd></div>
        <div><dt>Repeats</dt><dd>{repeatText(rx.repeatAuthority, rx.repeatTimes, rx.repeatIntervalDays)}</dd></div>
        <div><dt>Occasions dispensed</dt><dd>{occasions}</dd></div>
        {rx.directionsText && <div><dt>Directions</dt><dd>{rx.directionsText}</dd></div>}
        {rx.archiveReason && <div><dt>Archived because</dt><dd>{rx.archiveReason}</dd></div>}
      </dl>
      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">#</th><th scope="col">Item</th><th scope="col">Dose</th><th scope="col" className="numeric">Prescribed</th><th scope="col" className="numeric">Dispensed</th><th scope="col" className="numeric">Returned</th><th scope="col" className="numeric">Left</th></tr></thead>
        <tbody>{rx.items.map((item) => <tr key={item.id}>
          <td data-label="#">{item.lineNumber}</td>
          <td data-label="Item">{item.productDisplayName ?? item.writtenDescription}<br /><small className="row-subtext">As written: {item.writtenDescription}</small></td>
          <td data-label="Dose">{item.doseText}</td>
          <td data-label="Prescribed" className="numeric">{item.prescribedQuantityAtoms}</td>
          <td data-label="Dispensed" className="numeric">{item.dispensedAtoms}</td>
          <td data-label="Returned" className="numeric">{item.reinstatedAtoms}</td>
          <td data-label="Left" className="numeric">{item.prescribedQuantityAtoms - item.dispensedAtoms + item.reinstatedAtoms}</td>
        </tr>)}</tbody>
      </table></div>
      <p className="panel-note">Quantities are in each product's base units.</p>
    </section>

    {rx.records.length > 0 && <section className="master-panel" aria-labelledby="rx-records-title">
      <h2 id="rx-records-title">Prescription-supply entries</h2>
      <ul>{rx.records.map((entry) => <li key={entry.id}>
        <Link to={`/app/prescription-records/${entry.id}`}>{entry.serialNumber}</Link> · {RECORD_METHOD_LABELS[entry.recordMethod]} · {entry.dateOfSupply} · {RECORD_STATUS_LABELS[entry.status]}
      </li>)}</ul>
    </section>}

    <section className="master-panel" aria-labelledby="rx-dispensings-title">
      <h2 id="rx-dispensings-title">Dispensed</h2>
      {rx.dispensings.length === 0
        ? <p className="panel-note">Not dispensed yet. It can still be corrected.</p>
        : <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">Date</th><th scope="col">Sale</th><th scope="col" className="numeric">Quantity</th><th scope="col">Supervising pharmacist</th><th scope="col" className="numeric">Returned</th></tr></thead>
            <tbody>{rx.dispensings.map((entry) => <tr key={entry.id}>
              <td data-label="Date">{entry.dispensedOn}</td>
              <td data-label="Sale"><Link to={`/app/sales/${entry.saleDocumentId}`}>{entry.documentNumber ?? "Sale"}</Link></td>
              <td data-label="Quantity" className="numeric">{entry.quantityAtoms}</td>
              <td data-label="Supervising pharmacist">{entry.supervisingProfessionalName}</td>
              <td data-label="Returned" className="numeric">{entry.reversedAtoms}</td>
            </tr>)}</tbody>
          </table></div>}
    </section>

    {archiving && <ArchiveDialog
      title={`Archive ${rx.reference}`}
      description="An archived prescription cannot be dispensed again. Its record, and every supply made against it, is kept."
      onClose={() => setArchiving(false)}
      onArchive={async (reason) => {
        await archivePrescription(rx.id, rx.revision, reason);
        setArchiving(false);
        void queryClient.invalidateQueries({ queryKey: ["prescriptions"] });
      }}
    />}
  </>;
}

// ---------------------------------------------------------------------------------------------
// The rule 65(3)(1) entry
// ---------------------------------------------------------------------------------------------

export function PrescriptionRecordPage() {
  usePageTitle("Prescription-supply entry");
  return <DispenserOnly><PrescriptionRecordView /></DispenserOnly>;
}

/**
 * One entry of the prescription register, or one memo of the memo book, as it is to be kept.
 *
 * Every particular rule 65(3)(1) names, prepared before the supply. The registered pharmacist signs
 * the printed page by hand; AUSHADHARTH cannot sign, so it shows a blank signature box and records
 * only that a pharmacist confirmed the signature and the serial on the prescription. Only then may
 * the sale be posted, which finalizes the entry. An entry cancelled before posting is voided and its
 * serial is never given to another.
 */
function PrescriptionRecordView() {
  const { id } = useParams();
  const queryClient = useQueryClient();
  const record = useQuery({ queryKey: ["prescription-records", id], queryFn: () => getSupplyRecord(id!), enabled: Boolean(id), retry: false });
  const [signed, setSigned] = useState(false);
  const [serialWritten, setSerialWritten] = useState(false);
  const [voiding, setVoiding] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const settle = (updated: PrescriptionSupplyRecord) => {
    setProblem(null);
    queryClient.setQueryData(["prescription-records", id], updated);
    void queryClient.invalidateQueries({ queryKey: ["sales"] });
  };
  const confirm = useMutation({
    mutationFn: () => confirmSupplyRecord(id!),
    onSuccess: settle,
    onError: (caught) => setProblem(caught instanceof LocalServiceError ? caught.message : "The entry could not be confirmed.")
  });
  if (record.isPending) return <section className="master-panel"><Loading label="Loading the entry…" /></section>;
  if (record.isError) return <section className="master-panel"><QueryError label="This entry could not be loaded" error={record.error} onRetry={() => void record.refetch()} /></section>;
  const entry = record.data;
  const register = entry.recordMethod === "prescription_register";
  const live = entry.status === "prepared" || entry.status === "confirmed";
  const statusText = {
    prepared: "Prepared. Not signed: awaiting the registered pharmacist's handwritten signature on this entry, and this serial on the prescription. Nothing has been sold.",
    confirmed: `A pharmacist, ${entry.confirmedByDisplayName ?? "a user"}, confirmed at ${entry.confirmedAtUtc ?? ""} that the registered pharmacist signed this entry by hand and its serial is on the prescription. The sale can now be posted. AUSHADHARTH did not sign this entry.`,
    finalized: `Finalized with sale ${entry.documentNumber ?? ""} at ${entry.finalizedAtUtc ?? ""}. Handwritten signature and serial confirmed by ${entry.confirmedByDisplayName ?? "a user"} at ${entry.confirmedAtUtc ?? ""}, before the supply. AUSHADHARTH did not sign this entry.`,
    void: `Void — cancelled before the supply by ${entry.voidedByDisplayName ?? "a user"} at ${entry.voidedAtUtc ?? ""}: ${entry.voidReason ?? ""}. Nothing was supplied under it. Its serial is kept and not given to another entry.`
  }[entry.status];
  return <div className="record-page">
    <header className="page-header print-hide">
      <div>
        <p className="eyebrow">OPERATIONS</p>
        <h1>{entry.serialNumber}</h1>
        <p>{RECORD_METHOD_LABELS[entry.recordMethod]} entry for {entry.prescriptionReference}{entry.documentNumber ? ` · sale ${entry.documentNumber}` : ""} · {RECORD_STATUS_LABELS[entry.status]}</p>
      </div>
      <div className="page-header__actions">
        <button className="button button--primary" type="button" onClick={() => window.print()}>Print the entry</button>
        <Link className="button button--secondary" to={`/app/sales/${entry.saleDocumentId}`}>Back to the sale</Link>
      </div>
    </header>

    <article className="master-panel record-leaf" aria-labelledby="record-leaf-title">
      <h2 id="record-leaf-title">{register ? "Prescription register" : "Cash or credit memo"} — rule 65(3)(1), Drugs Rules, 1945{entry.status === "void" ? " — VOID" : ""}</h2>
      <dl className="record-leaf__particulars">
        <div><dt>(a) Serial number of the entry</dt><dd data-testid="record-serial">{entry.serialNumber}</dd></div>
        <div><dt>(b) Date of supply</dt><dd>{entry.dateOfSupply}</dd></div>
        <div><dt>(c) Prescriber</dt><dd>{entry.prescriberName}<br />{entry.prescriberAddress}</dd></div>
        <div><dt>(d) {entry.subjectKind === "human" ? "Patient" : "Owner of the animal"}</dt><dd>{entry.subjectName}<br />{entry.subjectAddress}</dd></div>
      </dl>
      <div className="table-scroll"><table className="data-table record-leaf__lines">
        <thead><tr><th scope="col">(e) Drug</th><th scope="col" className="numeric">(e) Quantity</th><th scope="col">(f) Manufacturer</th><th scope="col">(f) Batch</th><th scope="col">(f) Expiry</th></tr></thead>
        <tbody>{entry.lines.map((line, index) => <tr key={index}>
          <td data-label="Drug">{line.drugName}</td>
          <td data-label="Quantity" className="numeric">{line.quantityAtoms} {line.quantityUnitLabel ?? ""}{line.returnedAtoms > 0 && <small className="row-subtext">{line.returnedAtoms} returned since (recorded separately)</small>}</td>
          <td data-label="Manufacturer">{line.manufacturerName}</td>
          <td data-label="Batch">{line.batchNumber}</td>
          <td data-label="Expiry">{line.batchExpiresOn ?? "—"}</td>
        </tr>)}</tbody>
      </table></div>
      <dl className="record-leaf__particulars">
        <div><dt>Prescription</dt><dd>{entry.prescriptionReference}{entry.previousSerialNumber && <> · previous supply entered as {entry.previousSerialNumber}</>}</dd></div>
        {!register && <div><dt>Original container</dt><dd>{entry.originalContainerConfirmed ? "Attested: from or in the original container, not compounded here" : "Not attested"}</dd></div>}
        <div><dt>(g) Registered pharmacist</dt><dd>{entry.supervisingProfessionalName} · registration {entry.supervisingRegistrationNumber}</dd></div>
      </dl>
      <div className="record-leaf__signature">
        <span>Signature of the registered pharmacist (by hand)</span>
        <span className="record-leaf__signature-line" aria-hidden="true" />
      </div>
      <p className="record-leaf__status" data-testid="record-status">{statusText}</p>
    </article>

    {entry.status === "prepared" && <form className="master-panel print-hide record-complete" onSubmit={(event) => { event.preventDefault(); setProblem(null); if (signed && serialWritten) confirm.mutate(); }} aria-label="Confirm the signature">
      <h2>Before the sale is posted</h2>
      <ol className="panel-note">
        <li>Print this entry.</li>
        <li>The registered pharmacist, {entry.supervisingProfessionalName}, signs it by hand in the signature box. Keep the signed page.</li>
        <li>Write {entry.serialNumber} on the prescription.</li>
        <li>Confirm both here. Only a pharmacist or the owner can; a cashier cannot. Then post the sale.</li>
      </ol>
      <label className="check-field">
        <input id="record-signed" type="checkbox" checked={signed} onChange={(event) => setSigned(event.target.checked)} />
        <span>The registered pharmacist has signed this printed entry by hand</span>
      </label>
      <label className="check-field">
        <input id="record-serial-written" type="checkbox" checked={serialWritten} onChange={(event) => setSerialWritten(event.target.checked)} />
        <span>{entry.serialNumber} has been written on the prescription</span>
      </label>
      {problem && <div className="inline-notice inline-notice--error" role="alert">{problem}</div>}
      <div className="form-actions">
        <button className="button button--primary" type="submit" disabled={!signed || !serialWritten || confirm.isPending}>{confirm.isPending ? "Saving…" : "Confirm signature and serial"}</button>
      </div>
    </form>}

    {entry.status === "confirmed" && <section className="master-panel print-hide" aria-label="Post the sale">
      <p className="panel-note">The entry is confirmed. Post the sale from the sale screen; the posting finalizes this entry with the supply.</p>
      {problem && <div className="inline-notice inline-notice--error" role="alert">{problem}</div>}
      <Link className="button button--primary" to={`/app/sales/${entry.saleDocumentId}`}>Go to the sale to post it</Link>
    </section>}

    {live && <div className="print-hide form-actions">
      <button className="button button--danger" type="button" onClick={() => setVoiding(true)}>Void this entry</button>
    </div>}

    {voiding && <ArchiveDialog
      title={`Void ${entry.serialNumber}`}
      description="Void the entry if the supply will not happen as prepared. Nothing is sold under it. The entry is kept, marked void with your reason, and its serial is never given to another entry — mark the paper entry void too."
      actionLabel="Void the entry"
      onClose={() => setVoiding(false)}
      onArchive={async (reason) => {
        settle(await voidSupplyRecord(entry.id, reason));
        setVoiding(false);
      }}
    />}
  </div>;
}

// ---------------------------------------------------------------------------------------------
// Prescribers — shown on Drug Compliance
// ---------------------------------------------------------------------------------------------

/**
 * The prescriber list: a convenience for entry. A pharmacist may add to it; only the owner corrects
 * or archives an entry, because a correction reaches every prescription entered from it later. No
 * entry is ever described as verified: the software has no way to check a registration.
 */
export function PrescribersPanel() {
  const { allowed, owner } = useDispenser();
  const queryClient = useQueryClient();
  const prescribers = useQuery({ queryKey: ["prescribers"], queryFn: listPrescribers, retry: false, enabled: allowed });
  const [editing, setEditing] = useState<Prescriber | "new" | null>(null);
  const [archiving, setArchiving] = useState<Prescriber | null>(null);
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["prescribers"] });
  if (!allowed) return null;
  return <section className="master-panel" aria-labelledby="prescribers-title">
    <div className="panel-header">
      <div>
        <h2 id="prescribers-title">Prescribers</h2>
        <p>Doctors and veterinarians whose prescriptions the pharmacy has entered, recorded as written. A registration number is optional and is not verified by AUSHADHARTH.</p>
      </div>
      <button className="button button--secondary" type="button" onClick={() => setEditing("new")}>Add Prescriber</button>
    </div>
    {prescribers.isPending ? <Loading label="Loading prescribers…" />
      : prescribers.isError ? <QueryError label="Prescribers could not be loaded" error={prescribers.error} onRetry={() => void prescribers.refetch()} />
      : prescribers.data.length === 0 ? <p className="panel-note">No prescriber is recorded.</p>
      : <div className="table-scroll"><table className="data-table">
          <thead><tr><th scope="col">Name</th><th scope="col">Address</th><th scope="col">Registration number recorded</th><th scope="col">Status</th>{owner && <th scope="col"><span className="visually-hidden">Actions</span></th>}</tr></thead>
          <tbody>{prescribers.data.map((each) => <tr key={each.id}>
            <td data-label="Name">{each.fullName}</td>
            <td data-label="Address">{each.addressText}</td>
            <td data-label="Registration number recorded">{each.registrationNumber ?? "—"}{each.registeringAuthority && <small>{each.registeringAuthority}</small>}</td>
            <td data-label="Status"><span className={`status-badge status-badge--${each.status}`}>{each.status === "active" ? "Active" : "Archived"}</span></td>
            {owner && <td className="table-actions">{each.status === "active" && <>
              <button className="button button--secondary" type="button" onClick={() => setEditing(each)}>Edit</button>
              <button className="button button--secondary" type="button" onClick={() => setArchiving(each)}>Archive</button>
            </>}</td>}
          </tr>)}</tbody>
        </table></div>}
    {editing && <PrescriberDialog existing={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); refresh(); }} />}
    {archiving && <ArchiveDialog
      title={`Archive ${archiving.fullName}`}
      description="Archived from the list, never deleted. Prescriptions already entered keep their own copy of these details."
      onClose={() => setArchiving(null)}
      onArchive={async (reason) => { await archivePrescriber(archiving.id, archiving.revision, reason); setArchiving(null); refresh(); }}
    />}
  </section>;
}

function PrescriberDialog({ existing, onClose, onSaved }: { existing: Prescriber | null; onClose: () => void; onSaved: () => void }) {
  const [fullName, setFullName] = useState(existing?.fullName ?? "");
  const [addressText, setAddressText] = useState(existing?.addressText ?? "");
  const [registrationNumber, setRegistrationNumber] = useState(existing?.registrationNumber ?? "");
  const [registeringAuthority, setRegisteringAuthority] = useState(existing?.registeringAuthority ?? "");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => {
      const input = {
        fullName,
        addressText,
        registrationNumber: registrationNumber.trim() || null,
        registeringAuthority: registeringAuthority.trim() || null
      };
      return existing ? updatePrescriber(existing.id, { ...input, expectedRevision: existing.revision }) : createPrescriber(input);
    },
    onSuccess: onSaved,
    onError: (caught) => setError(caught instanceof LocalServiceError ? caught.message : "The prescriber could not be saved.")
  });
  const ready = fullName.trim() !== "" && addressText.trim() !== "";
  return <CatalogDialog title={existing ? "Edit Prescriber" : "Add Prescriber"} description="Recorded as written on the prescription. Prescriptions already entered keep their own copy." onClose={onClose}>
    <form className="master-form" onSubmit={(event) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); }}>
      <div className="field"><label htmlFor="prescriber-name">Name</label><input id="prescriber-name" value={fullName} onChange={(event) => setFullName(event.target.value)} maxLength={120} required /></div>
      <div className="field"><label htmlFor="prescriber-address">Address</label><input id="prescriber-address" value={addressText} onChange={(event) => setAddressText(event.target.value)} maxLength={300} required /></div>
      <div className="field"><label htmlFor="prescriber-registration">Registration number (optional)</label><input id="prescriber-registration" value={registrationNumber} onChange={(event) => setRegistrationNumber(event.target.value)} maxLength={60} /></div>
      <div className="field"><label htmlFor="prescriber-authority">Registering council (optional)</label><input id="prescriber-authority" value={registeringAuthority} onChange={(event) => setRegisteringAuthority(event.target.value)} maxLength={160} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>{existing ? "Save" : "Add Prescriber"}</button>
      </div>
    </form>
  </CatalogDialog>;
}

function ArchiveDialog({ title, description, actionLabel = "Archive", onClose, onArchive }: {
  title: string;
  description: string;
  actionLabel?: string;
  onClose: () => void;
  onArchive: (reason: string) => Promise<void>;
}) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return <CatalogDialog title={title} description={description} onClose={onClose}>
    <form className="master-form" onSubmit={(event) => {
      event.preventDefault();
      setError(null);
      setBusy(true);
      onArchive(reason).catch((caught) => setError(caught instanceof LocalServiceError ? caught.message : "This could not be archived.")).finally(() => setBusy(false));
    }}>
      <div className="field"><label htmlFor="archive-reason">Reason</label><input id="archive-reason" value={reason} onChange={(event) => setReason(event.target.value)} maxLength={500} required /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--danger" type="submit" disabled={!reason.trim() || busy}>{actionLabel}</button>
      </div>
    </form>
  </CatalogDialog>;
}

function Loading({ label }: { label: string }) { return <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>{label}</b></div>; }
function QueryError({ label, error, onRetry }: { label: string; error: unknown; onRetry: () => void }) {
  const denied = error instanceof LocalServiceError && error.code === "authorization_denied";
  return <div className="empty-state" role="alert"><h3>{label}</h3><p>{denied ? "Your role does not permit this." : "The Local Store Service did not complete this request."}</p>{!denied && <button className="button button--secondary" type="button" onClick={onRetry}>Retry</button>}</div>;
}
function usePageTitle(title: string) { useEffect(() => { const previous = document.title; document.title = `${title} | AUSHADHARTH`; return () => { document.title = previous; }; }, [title]); }
