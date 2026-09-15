import { createContext, useContext, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AuthStatusResponseSchema,
  SessionResponseSchema,
  SystemInfoResponseSchema,
  type AuthStatusResponse,
  type LoginRequest,
  type SessionResponse,
  type SetupRequest
} from "@aushadharth/contracts";
import { LocalServiceError, localServiceRequest } from "../platform/localService";

export type StartupState =
  | "BOOTING"
  | "LOCAL_SERVICE_UNAVAILABLE"
  | "COMPATIBILITY_ERROR"
  | "SETUP_REQUIRED"
  | "AUTH_REQUIRED"
  // The database this service had open has been replaced and it is waiting to be restarted.
  // Distinct from being unavailable: nothing is wrong, and telling somebody to restart is a very
  // different instruction from telling them their service has failed.
  | "RESTORE_IN_PROGRESS"
  | "AUTHENTICATED";

interface AuthContextValue {
  state: StartupState;
  status: AuthStatusResponse | null;
  retry: () => Promise<unknown>;
  setup: (request: SetupRequest) => Promise<SessionResponse>;
  login: (request: LoginRequest) => Promise<SessionResponse>;
  logout: () => Promise<void>;
  expireSession: () => Promise<void>;
}

const AuthContext = createContext<AuthContextValue | null>(null);

const STARTUP_QUERY_KEY = ["app", "startup"] as const;

class CompatibilityFailure extends Error {}

async function fetchStartup(): Promise<AuthStatusResponse> {
  const [systemRaw, statusRaw] = await Promise.all([
    localServiceRequest("/api/v1/system/info"),
    localServiceRequest("/api/v1/auth/status")
  ]);
  try {
    const system = SystemInfoResponseSchema.parse(systemRaw);
    if (system.apiVersion !== "v1" || system.compatibility.maximumWebMajorVersion < 0) {
      throw new CompatibilityFailure();
    }
    return AuthStatusResponseSchema.parse(statusRaw);
  } catch (error) {
    if (error instanceof CompatibilityFailure || error instanceof LocalServiceError) throw error;
    throw new CompatibilityFailure("The local service is not compatible with this web application.");
  }
}

export function AuthProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const startup = useQuery({
    queryKey: STARTUP_QUERY_KEY,
    queryFn: fetchStartup,
    retry: false,
    staleTime: 15_000,
    refetchInterval: 60_000
  });

  const state: StartupState = startup.isPending
    ? "BOOTING"
    : startup.isError
      ? startup.error instanceof CompatibilityFailure
        ? "COMPATIBILITY_ERROR"
        : startup.error instanceof LocalServiceError && startup.error.code === "service_restoring"
          ? "RESTORE_IN_PROGRESS"
          : "LOCAL_SERVICE_UNAVAILABLE"
      : startup.data.setupRequired
        ? "SETUP_REQUIRED"
        : startup.data.authenticated
          ? "AUTHENTICATED"
          : "AUTH_REQUIRED";

  // Every cached query other than startup holds authenticated business or reference data belonging
  // to the session being replaced. Removing them by exclusion — rather than by an enumerated prefix
  // list that silently misses new query keys — keeps the invariant that no prior-session record can
  // be served to the next session.
  const clearSessionScopedQueries = () => {
    queryClient.removeQueries({ predicate: (query) => query.queryKey[0] !== STARTUP_QUERY_KEY[0] });
  };

  const setAuthenticated = (session: SessionResponse) => {
    clearSessionScopedQueries();
    queryClient.setQueryData<AuthStatusResponse>(STARTUP_QUERY_KEY, {
      setupRequired: false,
      authenticated: true,
      user: session.user,
      storeDisplayName: session.storeDisplayName
    });
  };

  const markSignedOut = () => {
    queryClient.setQueryData<AuthStatusResponse>(STARTUP_QUERY_KEY, (current) => ({
      setupRequired: false,
      authenticated: false,
      user: null,
      storeDisplayName: current?.storeDisplayName ?? null
    }));
    clearSessionScopedQueries();
  };

  const value: AuthContextValue = {
    state,
    status: startup.data ?? null,
    retry: () => startup.refetch(),
    setup: async (request) => {
      const session = SessionResponseSchema.parse(await localServiceRequest("/api/v1/auth/setup", {
        method: "POST",
        body: JSON.stringify(request)
      }));
      setAuthenticated(session);
      return session;
    },
    login: async (request) => {
      const session = SessionResponseSchema.parse(await localServiceRequest("/api/v1/auth/login", {
        method: "POST",
        body: JSON.stringify(request)
      }));
      setAuthenticated(session);
      return session;
    },
    logout: async () => {
      await localServiceRequest("/api/v1/auth/logout", { method: "POST", body: "{}" });
      markSignedOut();
    },
    expireSession: async () => {
      markSignedOut();
      await startup.refetch();
    }
  };

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const value = useContext(AuthContext);
  if (!value) throw new Error("useAuth must be used inside AuthProvider");
  return value;
}
