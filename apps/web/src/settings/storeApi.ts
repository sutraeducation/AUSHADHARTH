import {
  StoreTaxIdentitySchema,
  type GstRegistrationStatus,
  type StoreTaxIdentity
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/** The Store's own tax identity. Phase 1G compares its State with the supplier's. */
export async function getStoreTaxIdentity(): Promise<StoreTaxIdentity> {
  return StoreTaxIdentitySchema.parse(await localServiceRequest("/api/v1/store/tax-identity"));
}

export async function updateStoreTaxIdentity(
  expectedRevision: number,
  fields: {
    gstRegistrationStatus: GstRegistrationStatus;
    gstin: string | null;
    placeOfSupplyStateId: string | null;
  },
  reason?: string
): Promise<StoreTaxIdentity> {
  return StoreTaxIdentitySchema.parse(await localServiceRequest("/api/v1/store/tax-identity", {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, ...fields, reason: reason ?? null })
  }));
}
