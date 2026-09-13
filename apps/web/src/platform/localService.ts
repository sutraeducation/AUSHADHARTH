const LOCAL_SERVICE_ORIGIN = import.meta.env.VITE_LOCAL_SERVICE_ORIGIN ?? "";

export class LocalServiceError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly retryAfterSeconds?: number,
    public readonly issues: ReadonlyArray<{ field: string; message: string }> = [],
    public readonly expectedRevision: number | null = null,
    public readonly currentRevision: number | null = null
  ) {
    super(message);
  }
}

export async function fetchHealth(): Promise<unknown> {
  const response = await fetch(`${LOCAL_SERVICE_ORIGIN}/api/v1/health`, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(3_000)
  });

  if (!response.ok) {
    throw new Error(`Local service health request failed with ${response.status}`);
  }

  return response.json();
}

export async function localServiceRequest(
  path: string,
  init: RequestInit = {}
): Promise<unknown> {
  const headers = new Headers(init.headers);
  headers.set("Accept", "application/json");
  if (init.body) headers.set("Content-Type", "application/json");

  const response = await fetch(`${LOCAL_SERVICE_ORIGIN}${path}`, {
    ...init,
    headers,
    credentials: "same-origin",
    signal: init.signal ?? AbortSignal.timeout(5_000)
  });
  if (response.status === 204) return null;

  const body = await response.json().catch(() => null) as {
    code?: string;
    message?: string;
    retryAfterSeconds?: number;
    issues?: Array<{ field: string; message: string }>;
    expectedRevision?: number | null;
    currentRevision?: number | null;
  } | null;
  if (!response.ok) {
    throw new LocalServiceError(
      response.status,
      body?.code ?? "internal_error",
      safeErrorMessage(body?.code),
      body?.retryAfterSeconds,
      body?.issues ?? [],
      body?.expectedRevision ?? null,
      body?.currentRevision ?? null
    );
  }
  return body;
}

function safeErrorMessage(code?: string): string {
  switch (code) {
    case "invalid_credentials": return "The login ID or password is incorrect.";
    case "rate_limited": return "Too many attempts. Wait briefly and try again.";
    case "setup_unavailable": return "Setup was already completed. Continue to sign in.";
    case "session_expired": return "Your session expired. Sign in again.";
    case "authentication_required": return "Sign in to continue.";
    case "authorization_denied": return "Your role does not permit this operation.";
    case "service_busy": return "The local service is busy. Try again shortly.";
    case "duplicate_conflict": return "A conflicting active record already exists.";
    case "revision_conflict": return "This record was changed after you opened it.";
    case "not_found": return "This record no longer exists.";
    case "archived_conflict": return "This action conflicts with the record's archive status.";
    case "effective_date_overlap": return "This effective period overlaps an existing active rate.";
    case "conversion_conflict": return "This Pack conversion conflicts with its containment or an active parent Pack.";
    case "barcode_conflict": return "This barcode is already assigned within the same scope.";
    case "default_pack_conflict": return "A default Pack must be enabled, and only one default is allowed for each purpose.";
    case "validation_failed": return "Check the highlighted information and try again.";
    default: return "AUSHADHARTH could not complete that request. Try again.";
  }
}
