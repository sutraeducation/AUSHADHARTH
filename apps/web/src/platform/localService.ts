const LOCAL_SERVICE_ORIGIN = import.meta.env.VITE_LOCAL_SERVICE_ORIGIN ?? "";

export class LocalServiceError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly retryAfterSeconds?: number
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
  } | null;
  if (!response.ok) {
    throw new LocalServiceError(
      response.status,
      body?.code ?? "internal_error",
      safeErrorMessage(body?.code),
      body?.retryAfterSeconds
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
    case "validation_failed": return "Check the highlighted information and try again.";
    default: return "AUSHADHARTH could not complete that request. Try again.";
  }
}
