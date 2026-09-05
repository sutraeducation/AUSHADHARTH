import {
  ReferenceMasterResponseSchema,
  type ReferenceKind,
  type ReferenceMasterResponse
} from "@aushadharth/contracts";
import { localServiceRequest } from "../platform/localService";

export type ReferenceStatusFilter = "active" | "archived" | "all";

export async function listReferences(
  kind: ReferenceKind,
  search = "",
  status: ReferenceStatusFilter = "active",
  parentId?: string
): Promise<ReferenceMasterResponse[]> {
  const query = new URLSearchParams({ status });
  if (search.trim()) query.set("search", search.trim());
  if (parentId) query.set("parentId", parentId);
  return ReferenceMasterResponseSchema.array().parse(
    await localServiceRequest(`/api/v1/reference/${kind}?${query}`)
  );
}

export async function createReference(
  kind: ReferenceKind,
  attributes: Record<string, unknown>
): Promise<ReferenceMasterResponse> {
  return ReferenceMasterResponseSchema.parse(await localServiceRequest(`/api/v1/reference/${kind}`, {
    method: "POST",
    body: JSON.stringify({ attributes })
  }));
}

export async function updateReference(
  record: ReferenceMasterResponse,
  attributes: Record<string, unknown>
): Promise<ReferenceMasterResponse> {
  return ReferenceMasterResponseSchema.parse(await localServiceRequest(
    `/api/v1/reference/${record.kind}/${record.id}`,
    {
      method: "PUT",
      body: JSON.stringify({ expectedRevision: record.revision, attributes })
    }
  ));
}

export async function changeReferenceLifecycle(
  record: ReferenceMasterResponse,
  action: "archive" | "restore",
  reason: string
): Promise<ReferenceMasterResponse> {
  return ReferenceMasterResponseSchema.parse(await localServiceRequest(
    `/api/v1/reference/${record.kind}/${record.id}/${action}`,
    {
      method: "POST",
      body: JSON.stringify({ expectedRevision: record.revision, reason })
    }
  ));
}

export function basisPointsToPercent(value: number): string {
  const whole = Math.trunc(value / 100);
  const fractional = Math.abs(value % 100).toString().padStart(2, "0");
  return `${whole}.${fractional}`;
}

export function percentToBasisPoints(value: string): number | null {
  const normalized = value.trim();
  const match = /^(\d{1,3})(?:\.(\d{1,2}))?$/.exec(normalized);
  if (!match) return null;
  const whole = Number.parseInt(match[1], 10);
  const fractional = Number.parseInt((match[2] ?? "").padEnd(2, "0") || "0", 10);
  const result = whole * 100 + fractional;
  return result <= 10_000 ? result : null;
}
