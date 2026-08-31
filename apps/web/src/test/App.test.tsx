import { render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../app/App";

function renderApp() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter><App /></MemoryRouter>
    </QueryClientProvider>
  );
}

afterEach(() => vi.restoreAllMocks());

describe("foundation shell", () => {
  it("shows the online state when the local service responds", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ status: "ok", apiVersion: "v1", applicationVersion: "0.0.0" })
    }));
    renderApp();
    expect(screen.getByText("Connecting")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("Local Service Online")).toBeInTheDocument());
  });

  it("shows the offline state when the local service cannot be reached", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("offline")));
    renderApp();
    await waitFor(() => expect(screen.getByText("Local Service Offline")).toBeInTheDocument());
  });
});
