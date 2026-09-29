import {
  DrugComplianceSchema,
  ProductRegulatorySchema,
  RegulatoryClassificationSchema,
  StoreProfessionalSchema,
  ComplianceLicenceSchema,
  ScheduleXRegisterSchema,
  ScheduleXRegisterEntrySchema,
  ScheduleXPrescriptionAnnotationSchema,
  ScheduleXPrescriptionAnnotationsSchema,
  type ConfirmScheduleXEntryRequest,
  type ScheduleXEntryKind,
  type ScheduleXEntryStatus,
  type ScheduleXRegister,
  type RecordScheduleXPrescriptionAnnotationRequest,
  type ScheduleXPrescriptionAnnotations,
  type VoidScheduleXEntryRequest,
  LicenceDrugCoverageSchema,
  RecordElectionSchema,
  type CloseLicenceDrugCoverageRequest,
  type CreateLicenceDrugCoverageRequest,
  type CreateRegulatoryClassificationRequest,
  type Form20fAuthorityState,
  type Form20fGap,
  type LicenceLegalStatus,
  type LicenceValidityBasis,
  type UpdateLicenceAuthorityRequest,
  type DrugCompliance,
  type LicenceForm,
  type ProductRegulatory,
  type ProfessionalCapacity,
  type RecordElectionKind,
  type RecordElectionMethod,
  type RegulatoryAnswer,
  type RegulatoryClassification,
  type RegulatoryGate,
  type RegulatoryScheme
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/**
 * Phase 1M-A — the store's regulatory record.
 *
 * Every write here is an owner's assertion, and the Store Service records who made it. The browser
 * never decides what a product is: it shows what was recorded and resolved, and sends what the owner
 * typed. Nothing is inferred from a product's name, HSN or dosage form, here or anywhere.
 */

export async function getProductRegulatory(productId: string, asOf?: string): Promise<ProductRegulatory> {
  const query = asOf ? `?asOf=${encodeURIComponent(asOf)}` : "";
  return ProductRegulatorySchema.parse(
    await localServiceRequest(`/api/v1/products/${productId}/regulatory${query}`)
  );
}

export async function createClassification(
  productId: string,
  input: CreateRegulatoryClassificationRequest
): Promise<RegulatoryClassification> {
  return RegulatoryClassificationSchema.parse(
    await localServiceRequest(`/api/v1/products/${productId}/regulatory/classifications`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

/** A change in the law: the old finding keeps answering for every date it covered. */
export async function closeClassification(
  productId: string,
  classificationId: string,
  input: { expectedRevision: number; effectiveTo: string; reason: string }
): Promise<RegulatoryClassification> {
  return RegulatoryClassificationSchema.parse(
    await localServiceRequest(
      `/api/v1/products/${productId}/regulatory/classifications/${classificationId}/close`,
      { method: "POST", body: JSON.stringify(input) }
    )
  );
}

/** A finding entered in error: it stops answering for any date at all. */
export async function archiveClassification(
  productId: string,
  classificationId: string,
  input: { expectedRevision: number; reason: string }
): Promise<RegulatoryClassification> {
  return RegulatoryClassificationSchema.parse(
    await localServiceRequest(
      `/api/v1/products/${productId}/regulatory/classifications/${classificationId}/archive`,
      { method: "POST", body: JSON.stringify(input) }
    )
  );
}

export async function getDrugCompliance(): Promise<DrugCompliance> {
  return DrugComplianceSchema.parse(await localServiceRequest("/api/v1/store/drug-compliance"));
}

export interface ProfessionalInput {
  expectedRevision?: number;
  fullName: string;
  capacity: ProfessionalCapacity;
  registrationNumber: string | null;
  registeringAuthority: string | null;
  validFrom: string | null;
  validUpto: string | null;
  linkedUserId: string | null;
  reason?: string;
}

/**
 * Phase 1M-B. The professionals on record, for choosing who supervised a supply. A non-owner is
 * given names and capacities only; registration numbers stay with the owner.
 */
export async function listProfessionals() {
  return StoreProfessionalSchema.array().parse(await localServiceRequest("/api/v1/store/professionals"));
}

export async function createProfessional(input: ProfessionalInput) {
  return StoreProfessionalSchema.parse(
    await localServiceRequest("/api/v1/store/professionals", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export async function updateProfessional(id: string, input: ProfessionalInput) {
  return StoreProfessionalSchema.parse(
    await localServiceRequest(`/api/v1/store/professionals/${id}`, {
      method: "PUT",
      body: JSON.stringify(input)
    })
  );
}

export async function archiveProfessional(id: string, input: { expectedRevision: number; reason: string }) {
  return StoreProfessionalSchema.parse(
    await localServiceRequest(`/api/v1/store/professionals/${id}/archive`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export interface ComplianceLicenceInput {
  licenceForm: LicenceForm;
  licenceNumber: string;
  issuingAuthority: string | null;
  validFrom: string | null;
  validUpto: string | null;
  displayLicenceId: string | null;
  reason?: string;
}

export async function createComplianceLicence(input: ComplianceLicenceInput) {
  return ComplianceLicenceSchema.parse(
    await localServiceRequest("/api/v1/store/compliance-licences", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export async function archiveComplianceLicence(id: string, input: { expectedRevision: number; reason: string }) {
  await localServiceRequest(`/api/v1/store/compliance-licences/${id}/archive`, {
    method: "POST",
    body: JSON.stringify(input)
  });
}

/**
 * Phase 1M-D1-B. What the owner says about a licence's standing, kept apart from the licence's
 * own particulars: a suspension changes none of the numbers on the certificate.
 */
export async function updateLicenceAuthority(id: string, input: UpdateLicenceAuthorityRequest) {
  return ComplianceLicenceSchema.parse(
    await localServiceRequest(`/api/v1/store/compliance-licences/${id}/authority`, {
      method: "PUT",
      body: JSON.stringify(input)
    })
  );
}

/** One drug written onto one Form 20F, from a date. Item 2 of the Form is "Names of drugs". */
export async function createDrugCoverage(input: CreateLicenceDrugCoverageRequest) {
  return LicenceDrugCoverageSchema.parse(
    await localServiceRequest("/api/v1/store/licence-drug-coverage", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

/** The drug struck off the licence: the row keeps answering for the days it governed. */
export async function closeDrugCoverage(id: string, input: CloseLicenceDrugCoverageRequest) {
  return LicenceDrugCoverageSchema.parse(
    await localServiceRequest(`/api/v1/store/licence-drug-coverage/${id}/close`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

/** Coverage entered in error: it stops answering for any date at all. */
export async function archiveDrugCoverage(id: string, input: { expectedRevision: number; reason: string }) {
  await localServiceRequest(`/api/v1/store/licence-drug-coverage/${id}/archive`, {
    method: "POST",
    body: JSON.stringify(input)
  });
}

export interface RecordElectionInput {
  election: RecordElectionKind;
  method: RecordElectionMethod;
  effectiveFrom: string;
  effectiveTo: string | null;
  evidenceReference: string | null;
  reason?: string;
}

export async function createRecordElection(input: RecordElectionInput) {
  return RecordElectionSchema.parse(
    await localServiceRequest("/api/v1/store/record-elections", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

// --- Words ----------------------------------------------------------------------------------

export const SCHEME_LABELS: Record<RegulatoryScheme, string> = {
  schedule_h: "Schedule H",
  schedule_h1: "Schedule H1",
  schedule_x: "Schedule X",
  schedule_c: "Schedule C",
  schedule_c1: "Schedule C(1)",
  ndps_purview: "NDPS Act purview",
  punjab_restricted_supply: "Punjab restricted supply (State notification)"
};

export const ANSWER_LABELS: Record<RegulatoryAnswer, string> = {
  applies: "Applies",
  does_not_apply: "Does not apply",
  unknown: "Unknown — not recorded"
};

export const GATE_LABELS: Record<RegulatoryGate, string> = {
  clear: "Sellable",
  prescription_required: "Sellable on a prescription",
  unresolved: "Classification unresolved",
  workflow_unavailable: "Regulated sale — workflow not yet available"
};

export const LICENCE_FORM_LABELS: Record<LicenceForm, string> = {
  form_20: "Form 20 — retail, drugs other than Schedules C, C(1) and X",
  form_20a: "Form 20A — restricted retail",
  form_20b: "Form 20B — wholesale",
  form_20f: "Form 20F — retail, Schedule X",
  form_20g: "Form 20G — wholesale, Schedule X",
  form_21: "Form 21 — retail, Schedules C and C(1)",
  form_21a: "Form 21A — restricted retail, Schedule C(1)",
  form_21b: "Form 21B — wholesale, Schedules C and C(1)"
};

export const ELECTION_LABELS: Record<RecordElectionKind, string> = {
  rule_65_3_prescription_supply: "Rule 65(3)(2) — supply on prescription",
  rule_65_4_non_prescription_schedule_c: "Rule 65(4)(2) — Schedule C supply without prescription"
};

export const METHOD_LABELS: Record<RecordElectionMethod, string> = {
  prescription_register: "Prescription register",
  register: "Register",
  cash_or_credit_memo_book: "Cash or credit memo book"
};

/** Each election offers only the alternatives its own sub-rule names. */
export const METHODS_FOR: Record<RecordElectionKind, RecordElectionMethod[]> = {
  rule_65_3_prescription_supply: ["prescription_register", "cash_or_credit_memo_book"],
  rule_65_4_non_prescription_schedule_c: ["register", "cash_or_credit_memo_book"]
};

/**
 * Phase 1M-D1-B. `unknown` is spelled out rather than shown as a blank, because a blank on a
 * compliance screen reads as "nothing to do here" — which is the opposite of what it means.
 */
export const LEGAL_STATUS_LABELS: Record<LicenceLegalStatus, string> = {
  in_force: "In force",
  suspended: "Suspended",
  cancelled: "Cancelled",
  unknown: "Not recorded"
};

export const VALIDITY_BASIS_LABELS: Record<LicenceValidityBasis, string> = {
  perpetual: "Perpetual — no expiry",
  fixed_term: "Fixed term",
  unknown: "Not recorded"
};

export const AUTHORITY_STATE_LABELS: Record<Form20fAuthorityState, string> = {
  established: "Form 20F covers this drug",
  not_established: "Form 20F does not cover this drug",
  unresolved: "Form 20F authority not established"
};

/** What to do next, for each way the answer can fall short. "No" is not an instruction. */
export const AUTHORITY_GAP_LABELS: Record<Form20fGap, string> = {
  no_licence_recorded: "No Form 20F is recorded for this pharmacy.",
  licence_status_unknown: "A Form 20F is on file, but nobody has recorded whether it is in force.",
  licence_suspended: "The Form 20F is recorded as suspended.",
  licence_cancelled: "The Form 20F is recorded as cancelled.",
  licence_not_in_force_on_date: "The Form 20F's term does not cover this date.",
  validity_basis_unknown: "Nobody has recorded whether the Form 20F runs perpetually or to a date.",
  product_not_covered: "The Form 20F is in force, and this drug is not among the drugs recorded on it for this date."
};

export const CAPACITY_LABELS: Record<ProfessionalCapacity, string> = {
  registered_pharmacist: "Registered pharmacist",
  competent_person: "Competent person"
};

// --- Phase 1M-D2: the Schedule X working record -----------------------------------------------

/**
 * Rule 65(21)(a) requires a bound, serially page numbered register. This reads the WORKING RECORD
 * that helps a person write it — never the register itself, and never a page number.
 */
export async function getScheduleXRegister(): Promise<ScheduleXRegister> {
  return ScheduleXRegisterSchema.parse(
    await localServiceRequest("/api/v1/store/schedule-x/register")
  );
}

/** Records that both physical acts of rule 65(21) were done, and who is named for them. */
export async function confirmScheduleXEntry(id: string, input: ConfirmScheduleXEntryRequest) {
  return ScheduleXRegisterEntrySchema.parse(
    await localServiceRequest(`/api/v1/store/schedule-x/register/${id}/confirm`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

/** Closes the working entry. After this it never changes again. */
export async function finalizeScheduleXEntry(id: string) {
  return ScheduleXRegisterEntrySchema.parse(
    await localServiceRequest(`/api/v1/store/schedule-x/register/${id}/finalize`, {
      method: "POST",
      body: JSON.stringify({})
    })
  );
}

/** Withdraws an entry prepared in error. The row and its reference are kept, never reissued. */
export async function voidScheduleXEntry(id: string, input: VoidScheduleXEntryRequest) {
  return ScheduleXRegisterEntrySchema.parse(
    await localServiceRequest(`/api/v1/store/schedule-x/register/${id}/void`, {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}

export const SCHEDULE_X_STATUS_LABELS: Record<ScheduleXEntryStatus, string> = {
  prepared: "Not yet written in the register",
  confirmed: "Written and authenticated",
  finalized: "Closed",
  void: "Withdrawn"
};

export const SCHEDULE_X_ENTRY_KIND_LABELS: Record<ScheduleXEntryKind, string> = {
  receipt: "Received",
  supply: "Supplied"
};

/** A particular the store never recorded says so, rather than borrowing today's master value. */
export const NOT_RECORDED = "Not recorded";

// --- Phase 1M-D3-A: rule 65(11)(c), the note written on the physical prescription --------------

/**
 * The dispensing occasions still waiting for the physical note, and the confirmations already
 * recorded — with the seller particulars a confirmation would freeze right now.
 */
export async function getScheduleXPrescriptionAnnotations(): Promise<ScheduleXPrescriptionAnnotations> {
  return ScheduleXPrescriptionAnnotationsSchema.parse(
    await localServiceRequest("/api/v1/store/schedule-x/prescription-annotations")
  );
}

/**
 * Records that a person wrote the seller name, the seller address and the dispensing date on the
 * physical prescription, above the prescriber's signature.
 *
 * AUSHADHARTH does not write on the prescription and does not sign it. This stores the statement
 * that somebody did.
 */
export async function recordScheduleXPrescriptionAnnotation(
  input: RecordScheduleXPrescriptionAnnotationRequest
) {
  return ScheduleXPrescriptionAnnotationSchema.parse(
    await localServiceRequest("/api/v1/store/schedule-x/prescription-annotations", {
      method: "POST",
      body: JSON.stringify(input)
    })
  );
}
