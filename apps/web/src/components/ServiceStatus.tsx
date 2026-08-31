import { useQuery } from "@tanstack/react-query";
import { HealthResponseSchema, type HealthResponse } from "@aushadharth/contracts";
import { fetchHealth } from "../platform/localService";

export function ServiceStatus() {
  const health = useQuery<HealthResponse>({
    queryKey: ["local-service", "health"],
    queryFn: async () => HealthResponseSchema.parse(await fetchHealth()),
    refetchInterval: 10_000,
    retry: false
  });

  const label = health.isPending
    ? "Connecting"
    : health.isSuccess
      ? "Local Service Online"
      : "Local Service Offline";

  return (
    <div className={`service-status service-status--${health.status}`} role="status" aria-live="polite">
      <span className="service-status__dot" aria-hidden="true" />
      {label}
    </div>
  );
}
