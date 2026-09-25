import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../app/App";

const system = {
  status: "ok",
  apiVersion: "v1",
  applicationVersion: "0.0.0",
  compatibility: { minimumWebVersion: "0.0.0", maximumWebMajorVersion: 0 }
};
const user = { id: "01900000-0000-7000-8000-000000000001", loginIdentifier: "owner", displayName: "Store Owner", role: "owner_admin", revision: 1 };
const session = { user, storeDisplayName: "Care Pharmacy", expiresAtUtc: "2099-01-01T00:00:00Z" };

function response(body: unknown, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

function fetchFor(status: { setupRequired: boolean; authenticated: boolean }, overrides: Record<string, unknown> = {}) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (path.includes("/system/info")) return response(overrides.system ?? system);
    if (path.includes("/auth/status")) {
      const authStatus = overrides.status ?? { ...status, user: status.authenticated ? user : null, storeDisplayName: status.setupRequired ? null : "Care Pharmacy" };
      return response(typeof authStatus === "function" ? authStatus() : authStatus);
    }
    if (path.includes("/auth/setup")) return response(overrides.setup ?? session, overrides.setupStatus as number | undefined ?? 201);
    if (path.includes("/auth/login")) return response(overrides.login ?? session, overrides.loginStatus as number | undefined ?? 200);
    if (path.includes("/auth/logout")) return response(null, 204);
    if (path.includes("/dashboard/summary")) return response(overrides.dashboard ?? { storeDisplayName: "Care Pharmacy", activeProductCount: 4, activePackCount: 7 }, overrides.dashboardStatus as number | undefined ?? 200);
    throw new Error(`Unexpected request: ${path} ${init?.method ?? "GET"}`);
  });
}

function renderApp(path = "/") {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[path]}><App /></MemoryRouter></QueryClientProvider>);
}

beforeEach(() => localStorage.clear());
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("application entry state machine", () => {
  it("shows a truthful boot screen while the local service is being detected", () => {
    vi.stubGlobal("fetch", vi.fn(() => new Promise(() => undefined)));
    renderApp();
    expect(screen.getByRole("heading", { name: "Starting AUSHADHARTH" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Connecting to Local Store Service");
  });

  it("shows an actionable local-service unavailable state instead of an internet error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("connection refused")));
    renderApp();
    expect(await screen.findByRole("heading", { name: "Local Store Service Unavailable" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Retry connection" })).toBeInTheDocument();
    expect(screen.queryByText("Internet Offline")).not.toBeInTheDocument();
  });

  it("separates a compatibility problem from service availability", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: false }, { system: { ...system, apiVersion: "v2" } }));
    renderApp();
    expect(await screen.findByRole("heading", { name: "Application Compatibility Problem" })).toBeInTheDocument();
  });

  it("routes first-run installations to setup and associates empty-field errors accessibly", async () => {
    const fetchMock = fetchFor({ setupRequired: true, authenticated: false });
    vi.stubGlobal("fetch", fetchMock);
    renderApp("/app/dashboard");
    expect(await screen.findByRole("heading", { name: "Create your workspace" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create Workspace" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Check the highlighted fields");
    for (const [label, message] of [
      ["Business / Store name", "Store name is required."],
      ["Owner / Admin name", "Owner name is required."],
      ["Login ID", "Enter a valid Login ID."],
      ["Password", "Password does not meet the requirements."],
      ["Confirm password", "Confirm your password."]
    ]) {
      const input = screen.getByLabelText(label, { exact: true });
      expect(input).toHaveAttribute("aria-invalid", "true");
      const description = input.getAttribute("aria-describedby");
      expect(description).toBeTruthy();
      expect(document.getElementById(description!)).toHaveTextContent(message);
    }
    fireEvent.change(screen.getByLabelText("Business / Store name"), { target: { value: "Care Pharmacy" } });
    expect(screen.getByLabelText("Business / Store name")).toHaveAttribute("aria-invalid", "false");
    expect(screen.queryByText("Store name is required.")).not.toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("links password-confirmation mismatch to the confirmation field", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: true, authenticated: false }));
    renderApp("/setup");
    await screen.findByRole("heading", { name: "Create your workspace" });
    fireEvent.change(screen.getByLabelText("Password", { exact: true }), { target: { value: "Strong-Password-42" } });
    const confirmation = screen.getByLabelText("Confirm password", { exact: true });
    fireEvent.change(confirmation, { target: { value: "Different-Password-42" } });
    expect(confirmation).toHaveAttribute("aria-invalid", "true");
    expect(document.getElementById(confirmation.getAttribute("aria-describedby")!)).toHaveTextContent("Passwords do not match.");
  });

  it("counts non-BMP passwords as Unicode scalar values at the 11/12 boundary", async () => {
    const fetchMock = fetchFor({ setupRequired: true, authenticated: false });
    vi.stubGlobal("fetch", fetchMock);
    renderApp("/setup");
    await screen.findByRole("heading", { name: "Create your workspace" });
    fireEvent.change(screen.getByLabelText("Business / Store name"), { target: { value: "Care Pharmacy" } });
    fireEvent.change(screen.getByLabelText("Owner / Admin name"), { target: { value: "Store Owner" } });
    fireEvent.change(screen.getByRole("textbox", { name: /Login ID/ }), { target: { value: "owner" } });
    const password = screen.getByLabelText("Password", { exact: true });
    const confirmation = screen.getByLabelText("Confirm password", { exact: true });
    const elevenScalars = `Ää١${"😀".repeat(8)}`;
    const twelveScalars = `Ää١${"😀".repeat(9)}`;
    fireEvent.change(password, { target: { value: elevenScalars } });
    fireEvent.change(confirmation, { target: { value: elevenScalars } });
    fireEvent.click(screen.getByRole("button", { name: "Create Workspace" }));
    expect(password).toHaveAttribute("aria-invalid", "true");
    expect(fetchMock).toHaveBeenCalledTimes(2);
    fireEvent.change(password, { target: { value: twelveScalars } });
    fireEvent.change(confirmation, { target: { value: twelveScalars } });
    fireEvent.click(screen.getByRole("button", { name: "Create Workspace" }));
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    const setupCall = fetchMock.mock.calls.find(([input]) => String(input).includes("/auth/setup"));
    expect(JSON.parse(String(setupCall?.[1]?.body)).password).toBe(twelveScalars);
  });

  it("enforces the 128/129 Unicode scalar password boundary", async () => {
    const fetchMock = fetchFor({ setupRequired: true, authenticated: false });
    vi.stubGlobal("fetch", fetchMock);
    renderApp("/setup");
    await screen.findByRole("heading", { name: "Create your workspace" });
    fireEvent.change(screen.getByLabelText("Business / Store name"), { target: { value: "Care Pharmacy" } });
    fireEvent.change(screen.getByLabelText("Owner / Admin name"), { target: { value: "Store Owner" } });
    fireEvent.change(screen.getByRole("textbox", { name: /Login ID/ }), { target: { value: "owner" } });
    const password = screen.getByLabelText("Password", { exact: true });
    const confirmation = screen.getByLabelText("Confirm password", { exact: true });
    const oneHundredTwentyNine = `Aa1${"!".repeat(126)}`;
    const oneHundredTwentyEight = `Aa1${"!".repeat(125)}`;
    fireEvent.change(password, { target: { value: oneHundredTwentyNine } });
    fireEvent.change(confirmation, { target: { value: oneHundredTwentyNine } });
    fireEvent.click(screen.getByRole("button", { name: "Create Workspace" }));
    expect(password).toHaveAttribute("aria-invalid", "true");
    expect(fetchMock).toHaveBeenCalledTimes(2);
    fireEvent.change(password, { target: { value: oneHundredTwentyEight } });
    fireEvent.change(confirmation, { target: { value: oneHundredTwentyEight } });
    fireEvent.click(screen.getByRole("button", { name: "Create Workspace" }));
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  });

  it("supports password visibility and setup-to-dashboard flow", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: true, authenticated: false }));
    renderApp("/setup");
    await screen.findByRole("heading", { name: "Create your workspace" });
    fireEvent.change(screen.getByLabelText("Business / Store name"), { target: { value: "Care Pharmacy" } });
    fireEvent.change(screen.getByLabelText("Owner / Admin name"), { target: { value: "Store Owner" } });
    fireEvent.change(screen.getByRole("textbox", { name: /Login ID/ }), { target: { value: "owner" } });
    const password = screen.getByLabelText("Password");
    fireEvent.change(password, { target: { value: "Strong-Password-42" } });
    fireEvent.change(screen.getByLabelText("Confirm password"), { target: { value: "Strong-Password-42" } });
    expect(password).toHaveAttribute("type", "password");
    fireEvent.click(screen.getByRole("button", { name: "Show password" }));
    expect(password).toHaveAttribute("type", "text");
    fireEvent.submit(screen.getByRole("button", { name: "Create Workspace" }).closest("form")!);
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  });

  it("protects app routes and redirects authenticated refreshes to the dashboard", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: false }));
    const first = renderApp("/app/dashboard");
    expect(await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" })).toBeInTheDocument();
    first.unmount();
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: true }));
    renderApp("/app/dashboard");
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  });
});

describe("login and authenticated shell", () => {
  it("shows generic invalid credentials and clears the password", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: false }, { login: { code: "invalid_credentials", message: "raw", issues: [], retryAfterSeconds: null }, loginStatus: 401 }));
    renderApp("/login");
    await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" });
    fireEvent.change(screen.getByLabelText("Login ID"), { target: { value: "owner" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "Wrong-Password-42" } });
    fireEvent.submit(screen.getByRole("button", { name: "Sign In" }).closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("login ID or password is incorrect");
    expect(screen.getByLabelText("Password")).toHaveValue("");
  });

  it("shows a bounded cooldown without identifying the account", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: false }, { login: { code: "rate_limited", message: "raw", issues: [], retryAfterSeconds: 5 }, loginStatus: 429 }));
    renderApp("/login");
    await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" });
    fireEvent.change(screen.getByLabelText("Login ID"), { target: { value: "owner" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "Wrong-Password-42" } });
    fireEvent.submit(screen.getByRole("button", { name: "Sign In" }).closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts");
    expect(screen.getByRole("button", { name: /Wait 5s/ })).toBeDisabled();
  });

  it("signs in, shows truthful metrics, and never invents sales or stock figures", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: false }));
    renderApp("/login");
    await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" });
    fireEvent.change(screen.getByLabelText("Login ID"), { target: { value: "owner" } });
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "Strong-Password-42" } });
    fireEvent.submit(screen.getByRole("button", { name: "Sign In" }).closest("form")!);
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    expect(await screen.findByText("Active products")).toBeInTheDocument();
    expect(screen.getByText("Active packs / SKUs")).toBeInTheDocument();
    expect(screen.queryByText(/today's sales/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/stock value/i)).not.toBeInTheDocument();
  });

  it("logs out through the keyboard-accessible user menu", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: true }));
    renderApp("/app/dashboard");
    const menu = await screen.findByRole("button", { name: /Store Owner/ });
    fireEvent.click(menu);
    expect(screen.getByRole("menuitem", { name: "Sign out" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menuitem", { name: "Sign out" })).not.toBeInTheDocument();
    expect(menu).toHaveFocus();
    fireEvent.click(menu);
    fireEvent.click(screen.getByRole("menuitem", { name: "Sign out" }));
    expect(await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" })).toBeInTheDocument();
  });

  it("returns to login when the authoritative service reports an expired session", async () => {
    let statusRequests = 0;
    vi.stubGlobal("fetch", fetchFor(
      { setupRequired: false, authenticated: true },
      {
        status: () => statusRequests++ === 0
          ? { setupRequired: false, authenticated: true, user, storeDisplayName: "Care Pharmacy" }
          : { setupRequired: false, authenticated: false, user: null, storeDisplayName: "Care Pharmacy" },
        dashboard: { code: "session_expired", message: "raw", issues: [], retryAfterSeconds: null },
        dashboardStatus: 401
      }
    ));
    renderApp("/app/dashboard");
    expect(await screen.findByRole("heading", { name: "Sign in to AUSHADHARTH" })).toBeInTheDocument();
  });

  it("persists only a non-sensitive sidebar preference", async () => {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: true }));
    renderApp("/app/dashboard");
    fireEvent.click(await screen.findByRole("button", { name: "Collapse sidebar" }));
    const stored = Object.keys(localStorage).map((key) => `${key}:${localStorage.getItem(key)}`).join("|");
    expect(localStorage.getItem("aushadharth-ui-preferences")).toContain("sidebarCollapsed");
    expect(stored).not.toMatch(/password|session|token|Strong-Password/i);
  });
});

/**
 * Phase U1. On a phone the navigation used to stand above the page, so every screen opened on a
 * menu. It is a drawer now: shut on arrival, opened deliberately, and shut again the moment it has
 * done its job. These proofs are about that behaviour, which is the same in every viewport — the
 * width only decides whether the drawer is ever hidden.
 */
describe("mobile navigation drawer", () => {
  beforeEach(() => { localStorage.clear(); });

  async function signedIn() {
    vi.stubGlobal("fetch", fetchFor({ setupRequired: false, authenticated: true }));
    renderApp("/app/dashboard");
    await screen.findByRole("heading", { name: "Dashboard" });
  }

  it("starts closed, with the page itself the first thing on screen", async () => {
    await signedIn();
    const toggle = screen.getByRole("button", { name: "Open navigation" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(document.querySelector(".sidebar")).not.toHaveClass("sidebar--open");
    expect(document.querySelector(".nav-backdrop")).not.toBeInTheDocument();
    // The workspace is reachable without opening anything.
    expect(screen.getByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  });

  it("opens on request, names the current route, and closes again", async () => {
    await signedIn();
    fireEvent.click(screen.getByRole("button", { name: "Open navigation" }));
    expect(screen.getByRole("button", { name: "Open navigation" })).toHaveAttribute("aria-expanded", "true");
    expect(document.querySelector(".sidebar")).toHaveClass("sidebar--open");
    // The route the reader is on is the one marked current, so the drawer orients rather than lists.
    expect(screen.getByRole("link", { name: "Dashboard" })).toHaveClass("nav-item--active");

    fireEvent.click(screen.getByRole("button", { name: "Close navigation" }));
    expect(document.querySelector(".sidebar")).not.toHaveClass("sidebar--open");
    expect(screen.getByRole("button", { name: "Open navigation" })).toHaveFocus();
  });

  it("closes when a destination is chosen, because arriving is the end of navigating", async () => {
    await signedIn();
    fireEvent.click(screen.getByRole("button", { name: "Open navigation" }));
    expect(document.querySelector(".sidebar")).toHaveClass("sidebar--open");
    fireEvent.click(screen.getByRole("link", { name: "Purchases" }));
    await waitFor(() => expect(document.querySelector(".sidebar")).not.toHaveClass("sidebar--open"));
  });

  it("closes on Escape and on the backdrop, and returns focus to the control that opened it", async () => {
    await signedIn();
    fireEvent.click(screen.getByRole("button", { name: "Open navigation" }));
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(document.querySelector(".sidebar")).not.toHaveClass("sidebar--open"));
    expect(screen.getByRole("button", { name: "Open navigation" })).toHaveFocus();

    fireEvent.click(screen.getByRole("button", { name: "Open navigation" }));
    fireEvent.click(document.querySelector(".nav-backdrop")!);
    await waitFor(() => expect(document.querySelector(".sidebar")).not.toHaveClass("sidebar--open"));
  });

  it("keeps one navigation, not two: the drawer lists the same routes the sidebar does", async () => {
    await signedIn();
    const before = screen.getAllByRole("link").map((link) => link.getAttribute("href"));
    fireEvent.click(screen.getByRole("button", { name: "Open navigation" }));
    const after = screen.getAllByRole("link").map((link) => link.getAttribute("href"));
    expect(after).toEqual(before);
  });

  it("says the workspace is planned rather than broken", async () => {
    await signedIn();
    expect(screen.getByText("Planned")).toBeInTheDocument();
    expect(screen.queryByText("Not enabled")).not.toBeInTheDocument();
  });
});
