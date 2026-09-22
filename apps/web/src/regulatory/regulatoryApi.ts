import {
  DrugComplianceSchema,
  ProductRegulatorySchema,
  RegulatoryClassificationSchema,
  StoreProfessionalSchema,
  ComplianceLicenceSchema,
  RecordElectionSchema,
  type CreateRegulatoryClassificationRequest,
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
  ndps_purview: "NDPS Act purview"
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

export const CAPACITY_LABELS: Record<ProfessionalCapacity, string> = {
  registered_pharmacist: "Registered pharmacist",
  competent_person: "Competent person"
};
