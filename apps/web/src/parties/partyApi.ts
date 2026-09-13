import {
  DuplicateCandidateSchema,
  PartyAddressSchema,
  PartyDetailSchema,
  PartyRoleSchema,
  PartySchema,
  type CreatePartyRequest,
  type DuplicateCandidate,
  type Party,
  type PartyAddress,
  type PartyAddressFields,
  type PartyDetail,
  type PartyFields,
  type PartyRole,
  type PartyRoleFields
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

export type PartyStatusFilter = "active" | "archived" | "all";

export async function listParties(
  search: string,
  status: PartyStatusFilter,
  role?: string
): Promise<Party[]> {
  const query = new URLSearchParams({ status });
  if (search.trim()) query.set("search", search.trim());
  if (role) query.set("role", role);
  return PartySchema.array().parse(await localServiceRequest(`/api/v1/parties?${query}`));
}

export async function getParty(id: string): Promise<PartyDetail> {
  return PartyDetailSchema.parse(await localServiceRequest(`/api/v1/parties/${id}`));
}

export async function createParty(request: CreatePartyRequest): Promise<PartyDetail> {
  return PartyDetailSchema.parse(await localServiceRequest("/api/v1/parties", {
    method: "POST",
    body: JSON.stringify(request)
  }));
}

export async function updateParty(
  id: string,
  expectedRevision: number,
  party: PartyFields,
  reason?: string
): Promise<PartyDetail> {
  return PartyDetailSchema.parse(await localServiceRequest(`/api/v1/parties/${id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, party, reason: reason ?? null })
  }));
}

export async function changePartyLifecycle(
  id: string,
  action: "archive" | "restore",
  expectedRevision: number,
  reason: string
): Promise<PartyDetail> {
  return PartyDetailSchema.parse(await localServiceRequest(`/api/v1/parties/${id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, reason })
  }));
}

/**
 * Advisory only. The Store Service never blocks creation on a candidate — two pharmacies really can
 * share a name — so the caller decides what to do with the result.
 */
export async function findDuplicateCandidates(
  request: CreatePartyRequest
): Promise<DuplicateCandidate[]> {
  return DuplicateCandidateSchema.array().parse(
    await localServiceRequest("/api/v1/parties/duplicate-candidates", {
      method: "POST",
      body: JSON.stringify(request)
    })
  );
}

export async function addPartyRole(partyId: string, role: PartyRoleFields): Promise<PartyRole> {
  return PartyRoleSchema.parse(await localServiceRequest(`/api/v1/parties/${partyId}/roles`, {
    method: "POST",
    body: JSON.stringify(role)
  }));
}

export async function changeRoleLifecycle(
  id: string,
  action: "archive" | "restore",
  expectedRevision: number,
  reason: string
): Promise<PartyRole> {
  return PartyRoleSchema.parse(await localServiceRequest(`/api/v1/party-roles/${id}/${action}`, {
    method: "POST",
    body: JSON.stringify({ expectedRevision, reason })
  }));
}

export async function addPartyAddress(
  partyId: string,
  address: PartyAddressFields
): Promise<PartyAddress> {
  return PartyAddressSchema.parse(await localServiceRequest(`/api/v1/parties/${partyId}/addresses`, {
    method: "POST",
    body: JSON.stringify(address)
  }));
}

export async function updatePartyAddress(
  id: string,
  expectedRevision: number,
  address: PartyAddressFields,
  reason?: string
): Promise<PartyAddress> {
  return PartyAddressSchema.parse(await localServiceRequest(`/api/v1/party-addresses/${id}`, {
    method: "PUT",
    body: JSON.stringify({ expectedRevision, address, reason: reason ?? null })
  }));
}

export async function changeAddressLifecycle(
  id: string,
  action: "archive" | "restore",
  expectedRevision: number,
  reason: string
): Promise<PartyAddress> {
  return PartyAddressSchema.parse(
    await localServiceRequest(`/api/v1/party-addresses/${id}/${action}`, {
      method: "POST",
      body: JSON.stringify({ expectedRevision, reason })
    })
  );
}

/**
 * Mirrors the Store Service's normalisation so the operator sees what will be compared before the
 * request is sent. The service still normalises authoritatively; this is a preview, never a bypass.
 */
export function previewNormalizedGstin(value: string): string {
  return value.replace(/\s+/g, "").toUpperCase();
}

/** Characters 3 to 12 of a GSTIN are the holder's PAN. */
export function panInsideGstin(normalizedGstin: string): string | null {
  return normalizedGstin.length === 15 ? normalizedGstin.slice(2, 12) : null;
}
