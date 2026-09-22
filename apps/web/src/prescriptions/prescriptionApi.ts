import {
  PrescriberSchema,
  PrescriptionDetailSchema,
  PrescriptionSummarySchema,
  PrescriptionSupplyRecordSchema,
  type PrescriptionRecordMethod,
  type PrescriptionRecordStatus,
  type PrescriptionSupplyRecord,
  type Prescriber,
  type PrescriberInput,
  type PrescriptionDetail,
  type PrescriptionInput,
  type PrescriptionIssueCode,
  type PrescriptionSummary,
  type RepeatAuthority,
  type SupplyIssueCode
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/**
 * Phase 1M-B — prescriptions and the prescribers written on them.
 *
 * Only the dispensing roles reach any of this; the Store Service refuses a cashier. Nothing here
 * puts a patient's particulars in a URL: the list is searched by the pharmacy's own reference and
 * every other request names records by id.
 */

export async function listPrescribers(): Promise<Prescriber[]> {
  return PrescriberSchema.array().parse(await localServiceRequest("/api/v1/prescribers"));
}

export async function createPrescriber(input: PrescriberInput): Promise<Prescriber> {
  return PrescriberSchema.parse(await localServiceRequest("/api/v1/prescribers", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

export async function updatePrescriber(id: string, input: PrescriberInput): Promise<Prescriber> {
  return PrescriberSchema.parse(await localServiceRequest(`/api/v1/prescribers/${id}`, {
    method: "PUT",
    body: JSON.stringify(input)
  }));
}

export async function archivePrescriber(id: string, expectedRevision: number, reason: string): Promise<Prescriber> {
  return PrescriberSchema.parse(await localServiceRequest(`/api/v1/prescribers/${id}/archive`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, reason })
  }));
}

/** Searched by the pharmacy's own reference only — never by a patient's name. */
export async function listPrescriptions(reference?: string): Promise<PrescriptionSummary[]> {
  const query = reference?.trim() ? `?${new URLSearchParams({ reference: reference.trim() })}` : "";
  return PrescriptionSummarySchema.array().parse(await localServiceRequest(`/api/v1/prescriptions${query}`));
}

export async function getPrescription(id: string): Promise<PrescriptionDetail> {
  return PrescriptionDetailSchema.parse(await localServiceRequest(`/api/v1/prescriptions/${id}`));
}

export async function createPrescription(input: PrescriptionInput): Promise<PrescriptionDetail> {
  return PrescriptionDetailSchema.parse(await localServiceRequest("/api/v1/prescriptions", {
    method: "POST",
    body: JSON.stringify(input)
  }));
}

/** Allowed only before the first dispensing; the Store Service refuses it afterwards. */
export async function correctPrescription(id: string, input: PrescriptionInput): Promise<PrescriptionDetail> {
  return PrescriptionDetailSchema.parse(await localServiceRequest(`/api/v1/prescriptions/${id}`, {
    method: "PUT",
    body: JSON.stringify(input)
  }));
}

export async function archivePrescription(id: string, expectedRevision: number, reason: string): Promise<PrescriptionDetail> {
  return PrescriptionDetailSchema.parse(await localServiceRequest(`/api/v1/prescriptions/${id}/archive`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, reason })
  }));
}

/** A rule 65(3)(1) entry with every particular. Dispensing roles only. */
export async function getSupplyRecord(id: string): Promise<PrescriptionSupplyRecord> {
  return PrescriptionSupplyRecordSchema.parse(await localServiceRequest(`/api/v1/prescription-supply-records/${id}`));
}

/**
 * Records that a pharmacist confirmed the registered pharmacist signed the prepared entry by hand
 * and that its serial was written on the prescription. Both are manual acts, done before the
 * supply; the Store Service refuses the confirmation without either, and refuses a cashier.
 */
export async function confirmSupplyRecord(id: string): Promise<PrescriptionSupplyRecord> {
  return PrescriptionSupplyRecordSchema.parse(await localServiceRequest(`/api/v1/prescription-supply-records/${id}/confirm`, {
    method: "POST",
    body: JSON.stringify({ manualSignatureConfirmed: true, serialWrittenOnPrescription: true })
  }));
}

/** Cancels a prepared or confirmed entry before its Sale posts. Its serial is kept, never reused. */
export async function voidSupplyRecord(id: string, reason: string): Promise<PrescriptionSupplyRecord> {
  return PrescriptionSupplyRecordSchema.parse(await localServiceRequest(`/api/v1/prescription-supply-records/${id}/void`, {
    method: "POST",
    body: JSON.stringify({ reason })
  }));
}

/** The counter's words for an entry's state. None of them says the software signed anything. */
export const RECORD_STATUS_LABELS: Record<PrescriptionRecordStatus, string> = {
  prepared: "Prepared — awaiting the pharmacist's handwritten signature",
  confirmed: "Signature and serial confirmed — the sale can be posted",
  finalized: "Finalized with the posted sale",
  void: "Void — cancelled before the supply; serial not reused"
};

export const RECORD_METHOD_LABELS: Record<PrescriptionRecordMethod, string> = {
  prescription_register: "Prescription register",
  cash_or_credit_memo_book: "Cash or credit memo book"
};

/**
 * A typed quantity in the product's base unit, to exact atoms at its quantity scale, by string
 * arithmetic so no binary float ever enters a statutory quantity.
 */
export function unitsToAtoms(value: string, scale: number): number | null {
  const match = /^(\d+)(?:\.(\d+))?$/.exec(value.trim());
  if (!match) return null;
  const fraction = match[2] ?? "";
  if (fraction.length > scale) return null;
  const atoms = Number.parseInt(`${match[1]}${fraction.padEnd(scale, "0")}`, 10);
  return Number.isSafeInteger(atoms) && atoms >= 1 ? atoms : null;
}

export function atomsToUnitsText(atoms: number, scale: number): string {
  if (scale === 0) return String(atoms);
  const digits = String(Math.abs(atoms)).padStart(scale + 1, "0");
  const whole = digits.slice(0, -scale);
  const fraction = digits.slice(-scale).replace(/0+$/, "");
  return `${atoms < 0 ? "-" : ""}${whole}${fraction ? `.${fraction}` : ""}`;
}

// --- Words ----------------------------------------------------------------------------------

export const REPEAT_LABELS: Record<RepeatAuthority, string> = {
  once: "Once only (no repeat stated)",
  stated_times: "A stated number of times",
  stated_without_count: "Repeat stated, no number given"
};

export function repeatText(authority: RepeatAuthority, times: number | null, intervalDays: number | null): string {
  const base = authority === "stated_times" && times !== null ? `${times} times in all` : REPEAT_LABELS[authority];
  return intervalDays ? `${base}, at least ${intervalDays} days apart` : base;
}

/** The counter's words for each unmet requirement. Factual; nothing here claims verification. */
export const PRESCRIPTION_ISSUE_TEXT: Record<PrescriptionIssueCode, string> = {
  prescription_missing: "Needs a prescription linked to it.",
  prescription_not_found: "The linked prescription is not one of this pharmacy's.",
  prescription_archived: "The linked prescription is archived and cannot be dispensed.",
  prescription_link_not_required: "This line needs no prescription. Remove the link.",
  prescription_substitution_not_permitted: "The prescription names a different product. Another preparation cannot be supplied in its place.",
  prescription_quantity_exceeded: "More than the prescription has left.",
  prescription_repeat_not_authorised: "The prescription does not authorise dispensing it again.",
  prescription_repeat_too_soon: "The prescriber's stated interval has not yet passed.",
  prescription_dated_after_supply: "The prescription is dated after this sale.",
  prescription_date_invalid: "A date on the prescription could not be read.",
  manufacturer_not_recorded: "No manufacturer is recorded for this product, so its register entry cannot name one. An owner can add it on the product."
};

export const SUPPLY_ISSUE_TEXT: Record<SupplyIssueCode, string> = {
  supervising_pharmacist_required: "Choose the registered pharmacist supervising this supply.",
  supervising_pharmacist_invalid: "The chosen person is not a registered-pharmacist record valid on this sale's date.",
  endorsement_not_confirmed: "Confirm the pharmacy's name and address and the date have been written on the prescription.",
  prescription_record_election_unresolved: "The pharmacy's rule 65(3)(2) election is not recorded, so this supply has no register or memo book to be entered in. The owner records it in Drug Compliance.",
  prescription_memo_path_ineligible: "This pharmacy elected the cash or credit memo book, which may be used only for a drug supplied from or in its original container. Confirm that below.",
  prescription_record_not_prepared: "Everything else is in order. Prepare the statutory record: the entry gets its serial, and nothing is sold yet.",
  prescription_record_not_confirmed: "The entry is prepared. The registered pharmacist signs the printed entry by hand and its serial is written on the prescription; then a pharmacist confirms both on the entry.",
  prescription_record_stale: "The prepared entry no longer matches this sale, the election or the pharmacist. Void it and prepare a new one."
};
