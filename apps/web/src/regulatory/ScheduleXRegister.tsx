import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ScheduleXRegisterEntry, StoreProfessional } from "@aushadharth/contracts";
import { useAuth } from "../auth/AuthContext";
import { ScrollableTable } from "../components/Ui";
import { LocalServiceError } from "../platform/localService";
import { CatalogDialog } from "../products/CatalogDialog";
import {
  NOT_RECORDED,
  SCHEDULE_X_ENTRY_KIND_LABELS,
  SCHEDULE_X_STATUS_LABELS,
  confirmScheduleXEntry,
  finalizeScheduleXEntry,
  getScheduleXRegister,
  listProfessionals,
  voidScheduleXEntry
} from "./regulatoryApi";

/**
 * Phase 1M-D2 — the Schedule X working record.
 *
 * Rule 65(21)(a) of the Drugs and Cosmetics Rules, 1945 requires the supply of a Schedule X drug to
 * be recorded at the time of supply in a register that is bound, serially page numbered, specially
 * maintained, with separate pages allotted for each drug. A bound book is not something software can
 * be, and the word "electronic" appears nowhere in rule 65.
 *
 * So this screen is careful about what it claims. It gathers the ten particulars of rule 65(21)(b),
 * frozen at the moment the Purchase was posted, so a person can write the register accurately — and
 * it says plainly, once and calmly, that the register is still the legal record. It allocates an
 * AUSHADHARTH reference and labels it as one. It never shows a page number, because rule 65(21) gives
 * software no safe rule for pages. And it never calls a click a signature: what it records is that a
 * named registered pharmacist did the physical acts, attested by the person who ticked the boxes.
 */
export function ScheduleXRegisterPage() {
  const auth = useAuth();
  const role = auth.status?.user?.role;
  const canRead = role === "owner_admin" || role === "pharmacist";
  const canMutate = canRead;
  const queryClient = useQueryClient();
  const register = useQuery({
    queryKey: ["schedule-x-register"],
    queryFn: getScheduleXRegister,
    retry: false
  });
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["schedule-x-register"] });
  const [confirming, setConfirming] = useState<ScheduleXRegisterEntry | null>(null);
  const [voiding, setVoiding] = useState<ScheduleXRegisterEntry | null>(null);

  if (!canRead) {
    return <div className="page-stack">
      <header className="page-header">
        <div>
          <p className="eyebrow">SETTINGS</p>
          <h1>Schedule X Working Record</h1>
        </div>
      </header>
      <div className="empty-state" role="alert">
        <h3>This record is not available to your role</h3>
        <p>Schedule X compliance records are kept for the owner and the pharmacist.</p>
      </div>
    </div>;
  }

  return <div className="page-stack">
    <header className="page-header">
      <div>
        <p className="eyebrow">SETTINGS</p>
        <h1>Schedule X Working Record</h1>
        <p>The receipt and supply particulars rule 65(21) asks for, gathered and frozen so you can write your register from them.</p>
      </div>
    </header>

    {/*
      The advisory. Calm, not alarming: this is the normal state of affairs, not an error. It appears
      once, at the top, and the screen never contradicts it further down.
    */}
    <section className="register-advisory" data-testid="schedule-x-advisory">
      <strong>This is an AUSHADHARTH working record.</strong>
      <p>The bound, serially page-numbered Schedule X register required by rule 65(21) remains the legal record. AUSHADHARTH does not replace it. Use the particulars below to write your register, then confirm here that you have done so.</p>
    </section>

    {register.isPending ? <div className="table-loading" role="status" aria-live="polite"><span /><span /><span /><b>Loading the working record…</b></div>
      : register.isError ? <div className="empty-state" role="alert"><h3>The working record could not be loaded</h3><p>The Local Store Service did not complete this request.</p><button className="button button--secondary" type="button" onClick={() => void register.refetch()}>Retry</button></div>
      : <>
        <section className="master-panel" aria-labelledby="schedule-x-entries-title">
          <div className="panel-header">
            <div>
              <h2 id="schedule-x-entries-title">Working entries</h2>
              <p>One entry for each Schedule X receipt, with the particulars as they stood when the purchase was posted. They do not change afterwards, even if a supplier or product is renamed.</p>
            </div>
          </div>
          {register.data.entries.length === 0
            ? <p className="panel-note">No Schedule X working entry yet. One is prepared for you when you post a purchase of a drug you have recorded as inside Schedule X.</p>
            : <ScrollableTable hint="Scroll sideways for the rest of the particulars">
                <table className="data-table schedule-x-table">
                  <thead><tr>
                    <th scope="col">AUSHADHARTH reference</th>
                    <th scope="col">Kind</th>
                    <th scope="col">Date</th>
                    <th scope="col">Drug</th>
                    <th scope="col">Quantity</th>
                    <th scope="col">Batch</th>
                    <th scope="col">Manufacturer</th>
                    <th scope="col">Supplier</th>
                    <th scope="col">Supplier address</th>
                    <th scope="col">Supplier licence</th>
                    <th scope="col">Bill</th>
                    <th scope="col">Physical register</th>
                    {canMutate && <th scope="col"><span className="visually-hidden">Actions</span></th>}
                  </tr></thead>
                  <tbody>{register.data.entries.map((entry) => <tr key={entry.id} data-testid={`schedule-x-entry-${entry.id}`}>
                    <td data-label="AUSHADHARTH reference" className="cell-reference"><span className="register-reference">{entry.reference}</span><small>AUSHADHARTH reference</small></td>
                    <td data-label="Kind">{SCHEDULE_X_ENTRY_KIND_LABELS[entry.entryKind]}</td>
                    <td data-label="Date">{entry.transactionDate}</td>
                    <td data-label="Drug" className="cell-wide">{entry.drugName}</td>
                    <td data-label="Quantity">{entry.quantityPacks !== null ? `${entry.quantityPacks} packs` : "—"}<small>{entry.quantityAtoms} units</small></td>
                    <td data-label="Batch">{entry.batchNumber ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                    <td data-label="Manufacturer" className="cell-wide">{entry.manufacturerName ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                    <td data-label="Supplier" className="cell-wide">{entry.supplierName ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                    <td data-label="Supplier address" className="cell-wide">{entry.supplierAddress ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                    <td data-label="Supplier licence">{entry.supplierLicenceNumber ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                    <td data-label="Bill">{entry.billNumber ?? <span className="not-recorded">{NOT_RECORDED}</span>}<small>{entry.billDate ?? ""}</small></td>
                    <td data-label="Physical register" className="cell-status">
                      <span className={`status-badge status-badge--${entry.status === "finalized" ? "posted" : entry.status === "void" ? "void" : "draft"}`}>{SCHEDULE_X_STATUS_LABELS[entry.status]}</span>
                      {entry.supervisingProfessionalName && <small>{entry.supervisingProfessionalName}</small>}
                      {entry.voidReason && <small>{entry.voidReason}</small>}
                    </td>
                    {canMutate && <td className="table-actions cell-actions" data-label="Actions">
                      {entry.status === "prepared" && <button className="button button--secondary" type="button" onClick={() => setConfirming(entry)}>Record in register</button>}
                      {entry.status === "confirmed" && <FinalizeButton entry={entry} onDone={refresh} />}
                      {(entry.status === "prepared" || entry.status === "confirmed") && <button className="button button--secondary" type="button" onClick={() => setVoiding(entry)}>Withdraw</button>}
                    </td>}
                  </tr>)}</tbody>
                </table>
              </ScrollableTable>}
        </section>

        {/*
          Receipts posted before this software kept a working record. They are shown, not repaired:
          the particulars are whatever was frozen at the time, and nothing is filled in from today's
          catalogue. There is deliberately no control here to mark one compliant.
        */}
        {register.data.legacyReceipts.length > 0 && <section className="master-panel" aria-labelledby="schedule-x-legacy-title" data-testid="schedule-x-legacy">
          <div className="panel-header">
            <div>
              <h2 id="schedule-x-legacy-title">Earlier receipts without a working entry</h2>
              <p>These Schedule X purchases were posted before a working entry was kept for them, or before you recorded the drug as inside Schedule X. Their particulars are not reconstructed here. Write your register from the purchase itself.</p>
            </div>
          </div>
          <ScrollableTable>
            <table className="data-table schedule-x-table">
              <thead><tr><th scope="col">Bill</th><th scope="col">Date</th><th scope="col">Drug name recorded at posting</th><th scope="col">Product (name today)</th></tr></thead>
              <tbody>{register.data.legacyReceipts.map((receipt) => <tr key={receipt.purchaseLineId}>
                <td data-label="Bill" className="cell-reference"><span className="register-reference">{receipt.supplierInvoiceNumber}</span></td>
                <td data-label="Date">{receipt.invoiceDate}</td>
                <td data-label="Drug name recorded at posting" className="cell-wide">{receipt.frozenDrugName ?? <span className="not-recorded">{NOT_RECORDED}</span>}</td>
                <td data-label="Product (name today)" className="cell-wide">{receipt.currentProductName}<small>Current catalogue name, for identification only</small></td>
              </tr>)}</tbody>
            </table>
          </ScrollableTable>
        </section>}
      </>}

    {confirming && <ConfirmDialog entry={confirming} onClose={() => setConfirming(null)} onSaved={() => { setConfirming(null); refresh(); }} />}
    {voiding && <VoidDialog entry={voiding} onClose={() => setVoiding(null)} onSaved={() => { setVoiding(null); refresh(); }} />}
  </div>;
}

function FinalizeButton({ entry, onDone }: { entry: ScheduleXRegisterEntry; onDone: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => finalizeScheduleXEntry(entry.id),
    onSuccess: onDone,
    onError: (caught) => setError(describe(caught))
  });
  return <>
    <button className="button button--secondary" type="button" disabled={mutation.isPending} onClick={() => { setError(null); mutation.mutate(); }}>Close entry</button>
    {error && <small role="alert">{error}</small>}
  </>;
}

/**
 * The two physical acts of rule 65(21), attested deliberately.
 *
 * Neither box is pre-checked. Rule 65(21)(b)(x) asks for the SIGNATURE of the person under whose
 * supervision the transaction happened — software cannot make that signature and this dialog does
 * not pretend to. What is recorded is that a person confirms the signing happened on the page, and
 * which registered pharmacist on the pharmacy's own record it was.
 */
function ConfirmDialog({ entry, onClose, onSaved }: { entry: ScheduleXRegisterEntry; onClose: () => void; onSaved: () => void }) {
  const [entered, setEntered] = useState(false);
  const [authenticated, setAuthenticated] = useState(false);
  const [professionalId, setProfessionalId] = useState("");
  const [error, setError] = useState<string | null>(null);
  const professionals = useQuery({ queryKey: ["professionals"], queryFn: listProfessionals, retry: false });
  const pharmacists = (professionals.data ?? []).filter(
    (person: StoreProfessional) => person.capacity === "registered_pharmacist" && person.status === "active"
  );
  const mutation = useMutation({
    mutationFn: () => confirmScheduleXEntry(entry.id, {
      supervisingProfessionalId: professionalId,
      particularsEnteredInPhysicalRegister: entered,
      physicalEntryAuthenticated: authenticated
    }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const ready = entered && authenticated && professionalId !== "";
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (ready) mutation.mutate(); };
  return <CatalogDialog title="Record in the physical register" description={`${entry.reference} — ${entry.drugName}. Confirm what you have already done in the bound Schedule X register. AUSHADHARTH records your confirmation; it does not make the register entry or the signature.`} onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <fieldset className="field">
        <legend>What has been done in the bound register</legend>
        <label className="choice">
          <input type="checkbox" checked={entered} onChange={(event) => setEntered(event.target.checked)} />
          {" "}I have entered these particulars in the bound, serially page-numbered Schedule X register.
        </label>
        <label className="choice">
          <input type="checkbox" checked={authenticated} onChange={(event) => setAuthenticated(event.target.checked)} />
          {" "}The entry in the register has been authenticated by the person under whose supervision the transaction took place.
        </label>
      </fieldset>
      <div className="field">
        <label htmlFor="schedule-x-professional">Supervising registered pharmacist</label>
        <select id="schedule-x-professional" value={professionalId} onChange={(event) => setProfessionalId(event.target.value)} required>
          <option value="">{professionals.isPending ? "Loading professionals…" : "Choose a registered pharmacist"}</option>
          {pharmacists.map((person: StoreProfessional) => <option key={person.id} value={person.id}>{person.fullName}</option>)}
        </select>
        {pharmacists.length === 0 && !professionals.isPending && <small>No active registered pharmacist is on record. Add one under Professionals first.</small>}
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--primary" type="submit" disabled={!ready || mutation.isPending}>Record confirmation</button>
      </div>
    </form>
  </CatalogDialog>;
}

function VoidDialog({ entry, onClose, onSaved }: { entry: ScheduleXRegisterEntry; onClose: () => void; onSaved: () => void }) {
  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const mutation = useMutation({
    mutationFn: () => voidScheduleXEntry(entry.id, { reason }),
    onSuccess: onSaved,
    onError: (caught) => setError(describe(caught))
  });
  const submit = (event: FormEvent) => { event.preventDefault(); setError(null); if (reason.trim()) mutation.mutate(); };
  return <CatalogDialog title="Withdraw working entry" description={`${entry.reference} — ${entry.drugName}. The entry and its reference are kept, so the reference is never reissued. This does not change anything you have already written in the bound register.`} onClose={onClose}>
    <form className="master-form" onSubmit={submit}>
      <div className="field"><label htmlFor="schedule-x-void-reason">Reason</label><input id="schedule-x-void-reason" value={reason} onChange={(event) => setReason(event.target.value)} required maxLength={500} /></div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="dialog-actions">
        <button className="button button--secondary" type="button" onClick={onClose}>Cancel</button>
        <button className="button button--danger" type="submit" disabled={!reason.trim() || mutation.isPending}>Withdraw</button>
      </div>
    </form>
  </CatalogDialog>;
}

function describe(caught: unknown): string {
  if (caught instanceof LocalServiceError) {
    if (caught.code === "authorization_denied") return "Schedule X compliance records are kept for the owner and the pharmacist.";
    if (caught.code === "schedule_x_entry_finalized") return "This working entry is closed and cannot be changed.";
    if (caught.code === "schedule_x_entry_void") return "This working entry was withdrawn.";
    if (caught.code === "schedule_x_physical_confirmation_required") return "Confirm the physical register entry before closing this working entry.";
    if (caught.code === "schedule_x_attestations_incomplete") return "Confirm both the register entry and its authentication.";
    if (caught.code === "schedule_x_pharmacist_required") return "Choose an active registered pharmacist on this pharmacy's record.";
    return caught.message;
  }
  return "This could not be saved.";
}
