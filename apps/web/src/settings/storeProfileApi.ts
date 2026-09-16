import {
  StoreProfileSchema,
  type StoreProfile
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

/**
 * The Store's legal identity, address and pharmacy licences.
 *
 * One read for the whole profile, several narrow writes. The read exists so the screen never has to
 * assemble who the pharmacy is from four requests and hope they agree; the writes stay narrow so
 * correcting a phone number cannot collide with somebody editing the GSTIN, each carrying its own
 * revision.
 */

const PROFILE = "/api/v1/store/profile";
const ADDRESS = "/api/v1/store/address";
const LICENCES = "/api/v1/store/licences";

export interface StoreIdentityInput {
  expectedRevision: number;
  displayName: string;
  legalName: string | null;
  primaryPhone: string | null;
  primaryEmail: string | null;
  reason?: string;
}

export interface StoreAddressInput {
  /** Absent the first time an address is recorded; present and matched afterwards. */
  expectedRevision?: number;
  line1: string;
  line2: string | null;
  city: string | null;
  stateId: string | null;
  postalCode: string | null;
  reason?: string;
}

export interface StoreLicenceInput {
  expectedRevision?: number;
  licenceType: string;
  licenceNumber: string;
  issuingAuthority: string | null;
  validFrom: string | null;
  validUpto: string | null;
  reason?: string;
}

export async function getStoreProfile(): Promise<StoreProfile> {
  return StoreProfileSchema.parse(await localServiceRequest(PROFILE));
}

/** Every write returns the whole profile, so the screen never has to re-read to stay in step. */
export async function updateStoreIdentity(input: StoreIdentityInput): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(PROFILE, { method: "PUT", body: JSON.stringify(input) })
  );
}

export async function updateStoreAddress(input: StoreAddressInput): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(ADDRESS, { method: "PUT", body: JSON.stringify(input) })
  );
}

export async function createStoreLicence(input: StoreLicenceInput): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(LICENCES, { method: "POST", body: JSON.stringify(input) })
  );
}

export async function updateStoreLicence(
  id: string,
  input: StoreLicenceInput
): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(`${LICENCES}/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify(input)
    })
  );
}

export async function archiveStoreLicence(
  id: string,
  expectedRevision: number,
  reason: string
): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(`${LICENCES}/${encodeURIComponent(id)}/archive`, {
      method: "POST",
      body: JSON.stringify({ expectedRevision, reason })
    })
  );
}

export async function restoreStoreLicence(
  id: string,
  expectedRevision: number
): Promise<StoreProfile> {
  return StoreProfileSchema.parse(
    await localServiceRequest(`${LICENCES}/${encodeURIComponent(id)}/restore`, {
      method: "POST",
      body: JSON.stringify({ expectedRevision })
    })
  );
}
