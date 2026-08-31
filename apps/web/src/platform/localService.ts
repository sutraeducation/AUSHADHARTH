const LOCAL_SERVICE_ORIGIN = import.meta.env.VITE_LOCAL_SERVICE_ORIGIN ?? "";

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
