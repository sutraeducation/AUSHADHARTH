import { useCallback, useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { DashboardSummarySchema, type DashboardSummary, type UserRole } from "@aushadharth/contracts";
import { COMPANY_NAME, PRODUCT_NAME, PRODUCT_TAGLINE } from "@aushadharth/configuration";
import { AuthProvider, useAuth } from "../auth/AuthContext";
import { LocalServiceError, localServiceRequest } from "../platform/localService";
import { useUiPreferences } from "../state/uiPreferences";
import { ReferenceDataLandingPage, ReferenceDataSectionPage } from "../reference/ReferenceData";
import { ProductCreatePage, ProductDetailPage, ProductEditPage, ProductListPage } from "../products/ProductCatalog";
import { InventoryPage } from "../inventory/Inventory";
import { PartyCreatePage, PartyDetailPage, PartyEditPage, PartyListPage } from "../parties/Parties";
import { PurchaseCreatePage, PurchaseDetailPage, PurchaseListPage } from "../purchases/Purchases";
import { SaleCreatePage, SaleDetailPage, SaleListPage } from "../sales/Sales";
import { SalePrintPage } from "../print/SalePrintPage";
import { DrugCompliancePage } from "../regulatory/DrugCompliance";
import { H1HardCopyPage, H1RegisterPage, PrescriptionCreatePage, PrescriptionDetailPage, PrescriptionEditPage, PrescriptionListPage, PrescriptionRecordPage } from "../prescriptions/Prescriptions";
import { ReturnCreatePage, ReturnDetailPage, ReturnListPage } from "../returns/Returns";
import { StoreProfilePage } from "../settings/StoreProfile";
import { DataSafetyPage, RestoreFlow } from "../settings/DataSafety";

const LOGIN_PATTERN = /^[A-Za-z0-9._-]{3,64}$/;
const UPPERCASE_PATTERN = /\p{Uppercase}/u;
const LOWERCASE_PATTERN = /\p{Lowercase}/u;
const NUMBER_PATTERN = /\p{Number}/u;

export function App() {
  return <AuthProvider><ApplicationEntry /></AuthProvider>;
}

function ApplicationEntry() {
  const auth = useAuth();
  switch (auth.state) {
    case "BOOTING": return <LaunchScreen />;
    case "LOCAL_SERVICE_UNAVAILABLE": return <ServiceUnavailable onRetry={auth.retry} />;
    case "COMPATIBILITY_ERROR": return <CompatibilityError onRetry={auth.retry} />;
    case "RESTORE_IN_PROGRESS": return <RestoreAwaitingRestart onRetry={auth.retry} />;
    case "SETUP_REQUIRED":
      return <Routes><Route path="/setup" element={<SetupScreen />} /><Route path="*" element={<Navigate to="/setup" replace />} /></Routes>;
    case "AUTH_REQUIRED":
      return <Routes><Route path="/login" element={<LoginScreen />} /><Route path="*" element={<Navigate to="/login" replace />} /></Routes>;
    case "AUTHENTICATED":
      return <Routes><Route path="/app" element={<Navigate to="/app/dashboard" replace />} /><Route path="/app/dashboard" element={<AppShell><Dashboard /></AppShell>} /><Route path="/app/products" element={<AppShell><ProductListPage /></AppShell>} /><Route path="/app/products/new" element={<AppShell><ProductCreatePage /></AppShell>} /><Route path="/app/products/:id" element={<AppShell><ProductDetailPage /></AppShell>} /><Route path="/app/products/:id/edit" element={<AppShell><ProductEditPage /></AppShell>} /><Route path="/app/parties" element={<AppShell><PartyListPage /></AppShell>} /><Route path="/app/parties/new" element={<AppShell><PartyCreatePage /></AppShell>} /><Route path="/app/parties/:id" element={<AppShell><PartyDetailPage /></AppShell>} /><Route path="/app/parties/:id/edit" element={<AppShell><PartyEditPage /></AppShell>} /><Route path="/app/purchases" element={<AppShell><PurchaseListPage /></AppShell>} /><Route path="/app/purchases/new" element={<AppShell><PurchaseCreatePage /></AppShell>} /><Route path="/app/purchases/:id" element={<AppShell><PurchaseDetailPage /></AppShell>} /><Route path="/app/sales" element={<AppShell><SaleListPage /></AppShell>} /><Route path="/app/sales/new" element={<AppShell><SaleCreatePage /></AppShell>} /><Route path="/app/sales/:id" element={<AppShell><SaleDetailPage /></AppShell>} /><Route path="/app/sales/:id/print" element={<AppShell><SalePrintPage /></AppShell>} /><Route path="/app/sales/:id/h1-register" element={<AppShell><H1HardCopyPage /></AppShell>} /><Route path="/app/h1-register" element={<AppShell><H1RegisterPage /></AppShell>} /><Route path="/app/prescriptions" element={<AppShell><PrescriptionListPage /></AppShell>} /><Route path="/app/prescriptions/new" element={<AppShell><PrescriptionCreatePage /></AppShell>} /><Route path="/app/prescriptions/:id" element={<AppShell><PrescriptionDetailPage /></AppShell>} /><Route path="/app/prescriptions/:id/edit" element={<AppShell><PrescriptionEditPage /></AppShell>} /><Route path="/app/prescription-records/:id" element={<AppShell><PrescriptionRecordPage /></AppShell>} /><Route path="/app/returns" element={<AppShell><ReturnListPage /></AppShell>} /><Route path="/app/returns/:id" element={<AppShell><ReturnDetailPage /></AppShell>} /><Route path="/app/sales/:id/return" element={<AppShell><ReturnCreatePage kind="sales_return" /></AppShell>} /><Route path="/app/purchases/:id/return" element={<AppShell><ReturnCreatePage kind="purchase_return" /></AppShell>} /><Route path="/app/inventory" element={<Navigate to="/app/inventory/stock" replace />} /><Route path="/app/inventory/:section" element={<AppShell><InventoryPage /></AppShell>} /><Route path="/app/settings/store" element={<AppShell><StoreProfilePage /></AppShell>} /><Route path="/app/settings/data-safety" element={<AppShell><DataSafetyPage /></AppShell>} /><Route path="/app/settings/drug-compliance" element={<AppShell><DrugCompliancePage /></AppShell>} /><Route path="/app/reference" element={<AppShell><ReferenceDataLandingPage /></AppShell>} /><Route path="/app/reference/:section" element={<AppShell><ReferenceDataSectionPage /></AppShell>} /><Route path="/" element={<Navigate to="/app/dashboard" replace />} /><Route path="/login" element={<Navigate to="/app/dashboard" replace />} /><Route path="/setup" element={<Navigate to="/app/dashboard" replace />} /><Route path="*" element={<AuthenticatedNotFound />} /></Routes>;
  }
}

function BrandMark({ compact = false }: { compact?: boolean }) {
  return <div className={compact ? "brand-mark brand-mark--compact" : "brand-mark"}><span className="brand-mark__symbol" aria-hidden="true">A</span><span><strong>{PRODUCT_NAME}</strong>{!compact && <small>{PRODUCT_TAGLINE}</small>}</span></div>;
}

function LaunchScreen() {
  return <main className="entry-page" aria-busy="true"><section className="launch-card" aria-labelledby="launch-title"><BrandMark /><div className="launch-spinner" aria-hidden="true" /><h1 id="launch-title">Starting AUSHADHARTH</h1><p role="status" aria-live="polite">Connecting to Local Store Service</p><small>Preparing your pharmacy workspace</small></section></main>;
}

function ServiceUnavailable({ onRetry }: { onRetry: () => Promise<unknown> }) {
  const [retrying, setRetrying] = useState(false);
  return <EntryPanel title="Local Store Service Unavailable" tone="danger"><p>The local AUSHADHARTH service must be running on this PC. Your internet connection is not required.</p><div className="status-line" role="status"><StatusDot tone="danger" /> Local service unavailable</div><Button onClick={async () => { setRetrying(true); await onRetry(); setRetrying(false); }} disabled={retrying}>{retrying ? "Checking…" : "Retry connection"}</Button></EntryPanel>;
}

/**
 * Shown once a restore has replaced the database.
 *
 * The service deliberately closed the file it had open, so every route now answers
 * `service_restoring`. Nothing is broken and nothing is lost; it simply cannot continue until it
 * is started again, and saying exactly that is more useful than any spinner.
 */
function RestoreAwaitingRestart({ onRetry }: { onRetry: () => Promise<unknown> }) {
  return <EntryPanel title="Restore Complete — Restart Required" tone="warning"><p>The backup has been restored. Close AUSHADHARTH and start the Local Store Service again to begin using the restored data.</p><p>Everyone who was signed in has been signed out.</p><div className="status-line" role="status"><StatusDot tone="warning" /> Waiting for the Local Store Service to restart</div><Button onClick={() => void onRetry()}>Check again</Button></EntryPanel>;
}

function CompatibilityError({ onRetry }: { onRetry: () => Promise<unknown> }) {
  return <EntryPanel title="Application Compatibility Problem" tone="warning"><p>This web application and the Local Store Service are different versions. Update them together before continuing.</p><div className="status-line" role="alert"><StatusDot tone="warning" /> Compatibility check failed</div><Button onClick={() => void onRetry()}>Check again</Button></EntryPanel>;
}

function EntryPanel({ title, tone, children }: { title: string; tone: "danger" | "warning"; children: ReactNode }) {
  return <main className="entry-page"><section className="entry-card entry-card--message"><BrandMark /><div className={`message-icon message-icon--${tone}`} aria-hidden="true">!</div><h1>{title}</h1>{children}<p className="company-line">A Product of {COMPANY_NAME}</p></section></main>;
}

/**
 * First run: create a new workspace, or put an existing pharmacy back.
 *
 * A brand-new pharmacy is by far the common case, so the form is what appears first — but an owner
 * whose PC has just been replaced arrives here too, with their backup in hand and nothing else, and
 * for them the only useful thing on this screen is the other door. The service decides whether that
 * door may be used by looking at whether this installation is genuinely blank; the browser only
 * offers it.
 */
function SetupScreen() {
  const [mode, setMode] = useState<"create" | "restore">("create");
  return mode === "create"
    ? <SetupCreateScreen onRestore={() => setMode("restore")} />
    : <SetupRestoreScreen onBack={() => setMode("create")} />;
}

function SetupRestoreScreen({ onBack }: { onBack: () => void }) {
  return <main className="entry-page entry-page--split"><EntryAside heading="Restore this pharmacy from a backup" text="Choose the .aushbackup file you kept. AUSHADHARTH will check it before it uses it, and will tell you exactly what it contains." /><section className="form-card" aria-labelledby="restore-setup-title"><p className="step-label">FIRST-RUN RESTORE</p><h1 id="restore-setup-title">Restore from a backup</h1><p className="form-intro">This installation has no data yet, so nothing can be lost by restoring.</p><RestoreFlow firstRun /><div className="entry-choice"><Button variant="secondary" onClick={onBack}>Create a new workspace instead</Button></div><p className="privacy-note">Your backup file never leaves this PC. It is read by the Local Store Service only.</p></section></main>;
}

function SetupCreateScreen({ onRestore }: { onRestore: () => void }) {
  const auth = useAuth();
  const [values, setValues] = useState({ store: "", owner: "", login: "", password: "", confirm: "" });
  const [showPassword, setShowPassword] = useState(false);
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const errors = validateSetup(values);
  const fieldError = (field: string, value: string) => submitted || value.length > 0 ? errors[field] : undefined;
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSubmitted(true);
    if (Object.keys(errors).length) { setError("Check the highlighted fields."); return; }
    setSubmitting(true); setError("");
    try {
      await auth.setup({ storeDisplayName: values.store, ownerDisplayName: values.owner, loginIdentifier: values.login, password: values.password });
      setValues((current) => ({ ...current, password: "", confirm: "" }));
    } catch (caught) { setError(caught instanceof LocalServiceError ? caught.message : "Setup could not be completed. Try again."); }
    finally { setSubmitting(false); }
  };
  return <main className="entry-page entry-page--split"><EntryAside heading="Set up your pharmacy workspace" text="AUSHADHARTH keeps your pharmacy data on this PC and remains available without internet." /><section className="form-card" aria-labelledby="setup-title"><p className="step-label">FIRST-RUN SETUP</p><h1 id="setup-title">Create your workspace</h1><p className="form-intro">Only the essentials for now. Registration, tax, and address details can be added later.</p><form onSubmit={submit} noValidate><TextField label="Business / Store name" autoFocus value={values.store} onChange={(store) => setValues({ ...values, store })} error={fieldError("store", values.store)} autoComplete="organization" /><TextField label="Owner / Admin name" value={values.owner} onChange={(owner) => setValues({ ...values, owner })} error={fieldError("owner", values.owner)} autoComplete="name" /><TextField label="Login ID" hint="3–64 letters, numbers, dots, underscores, or hyphens" value={values.login} onChange={(login) => setValues({ ...values, login })} error={fieldError("login", values.login)} autoComplete="username" /><PasswordField label="Password" value={values.password} onChange={(password) => setValues({ ...values, password })} visible={showPassword} onToggle={() => setShowPassword(!showPassword)} error={fieldError("password", values.password)} autoComplete="new-password" /><p className="password-rule">Use 12–128 Unicode characters and at least three of: uppercase, lowercase, number, symbol.</p><PasswordField label="Confirm password" value={values.confirm} onChange={(confirm) => setValues({ ...values, confirm })} visible={showPassword} onToggle={() => setShowPassword(!showPassword)} error={fieldError("confirm", values.confirm)} autoComplete="new-password" />{error && <FormError>{error}</FormError>}<Button type="submit" full disabled={submitting}>{submitting ? "Creating workspace…" : "Create Workspace"}</Button></form><div className="entry-choice"><Button variant="secondary" onClick={onRestore}>Restore from a backup instead</Button></div><p className="privacy-note">Your password is sent only to the Local Store Service for secure hashing. It is never saved by this browser.</p></section></main>;
}

function LoginScreen() {
  const auth = useAuth();
  const [login, setLogin] = useState(""); const [password, setPassword] = useState("");
  const [visible, setVisible] = useState(false); const [error, setError] = useState("");
  const [cooldown, setCooldown] = useState(0); const [submitting, setSubmitting] = useState(false);
  useEffect(() => { if (cooldown <= 0) return; const timer = window.setInterval(() => setCooldown((value) => Math.max(0, value - 1)), 1_000); return () => window.clearInterval(timer); }, [cooldown]);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!LOGIN_PATTERN.test(login.trim()) || !password) { setError("Enter a valid Login ID and password."); return; }
    setSubmitting(true); setError("");
    try { await auth.login({ loginIdentifier: login, password }); setPassword(""); }
    catch (caught) { if (caught instanceof LocalServiceError) { setError(caught.message); if (caught.code === "rate_limited") setCooldown(caught.retryAfterSeconds ?? 5); } else setError("The Local Store Service could not complete sign in."); setPassword(""); }
    finally { setSubmitting(false); }
  };
  return <main className="entry-page entry-page--split"><EntryAside heading={`Welcome to ${auth.status?.storeDisplayName ?? "AUSHADHARTH"}`} text="Sign in locally to continue. Internet access is not required for your pharmacy workspace." /><section className="form-card" aria-labelledby="login-title"><p className="step-label">LOCAL SIGN IN</p><h1 id="login-title">Sign in to AUSHADHARTH</h1><p className="form-intro">Use the Login ID created for this installation.</p><div className="local-ready"><StatusDot tone="success" /><span><strong>Local Service Online</strong><small>Ready for secure local sign in</small></span></div><form onSubmit={submit} noValidate><TextField label="Login ID" autoFocus value={login} onChange={setLogin} autoComplete="username" /><PasswordField label="Password" value={password} onChange={setPassword} visible={visible} onToggle={() => setVisible(!visible)} autoComplete="current-password" />{error && <FormError>{error}{cooldown > 0 && ` Try again in ${cooldown} seconds.`}</FormError>}<Button type="submit" full disabled={submitting || cooldown > 0}>{submitting ? "Signing in…" : cooldown > 0 ? `Wait ${cooldown}s` : "Sign In"}</Button></form><p className="recovery-note">Account recovery is managed locally. Contact your workspace owner if you cannot sign in.</p><p className="company-line">A Product of {COMPANY_NAME}</p></section></main>;
}

function EntryAside({ heading, text }: { heading: string; text: string }) {
  return <aside className="entry-aside"><BrandMark /><div><p className="eyebrow">LOCAL-FIRST PHARMACY OPERATIONS</p><h2>{heading}</h2><p>{text}</p><div className="entry-benefits"><span>Works without internet</span><span>Data stays on this PC</span><span>Secure local access</span></div></div><small>A Product of {COMPANY_NAME}</small></aside>;
}

function AppShell({ children }: { children: ReactNode }) {
  const auth = useAuth(); const collapsed = useUiPreferences((state) => state.sidebarCollapsed); const toggleSidebar = useUiPreferences((state) => state.toggleSidebar);
  const [menuOpen, setMenuOpen] = useState(false); const menuButton = useRef<HTMLButtonElement>(null); const user = auth.status?.user;
  /**
   * On a phone the navigation is a drawer, closed until it is asked for. It used to be a column of
   * every route standing above the page, so the first screen of every screen was a menu and the
   * work was below the fold. The same markup serves both: the sidebar is the drawer, and the
   * width decides whether it is permanent furniture or something you open.
   */
  const [navOpen, setNavOpen] = useState(false);
  const navToggle = useRef<HTMLButtonElement>(null); const navClose = useRef<HTMLButtonElement>(null);
  const location = useLocation();
  const closeNav = useCallback(() => { setNavOpen(false); navToggle.current?.focus(); }, []);
  // Choosing a destination is the end of navigating, so arriving closes the drawer.
  useEffect(() => { setNavOpen(false); }, [location.pathname]);
  // Focus follows the drawer in, and the page behind it does not scroll under the reader's thumb.
  useEffect(() => {
    if (!navOpen) return;
    navClose.current?.focus();
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => { document.body.style.overflow = previous; };
  }, [navOpen]);
  useEffect(() => {
    const close = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (navOpen) { setNavOpen(false); navToggle.current?.focus(); return; }
      if (menuOpen) { setMenuOpen(false); menuButton.current?.focus(); }
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [menuOpen, navOpen]);

  const link = ({ isActive }: { isActive: boolean }) => `nav-item ${isActive ? "nav-item--active" : ""}`;
  const dispensing = user?.role === "owner_admin" || user?.role === "pharmacist";
  return <div className={`app-layout ${collapsed ? "app-layout--collapsed" : ""} ${navOpen ? "app-layout--nav-open" : ""}`}>
    {navOpen && <button className="nav-backdrop" type="button" tabIndex={-1} aria-hidden="true" onClick={closeNav} />}
    <aside id="primary-navigation" className={`sidebar ${navOpen ? "sidebar--open" : ""}`} aria-label="Primary navigation">
      <div className="sidebar__brand">
        <BrandMark compact={collapsed} />
        <button ref={navClose} className="nav-drawer-close" type="button" onClick={closeNav} aria-label="Close navigation"><span aria-hidden="true">×</span></button>
      </div>
      <nav className="sidebar__nav">
        <NavLink to="/app/dashboard" className={link}><DashboardIcon /><span>Dashboard</span></NavLink>
        <span className="nav-group-label">MASTERS</span>
        <NavLink to="/app/products" className={link}><ProductIcon /><span>Products</span></NavLink>
        <NavLink to="/app/parties" className={link}><SupplierIcon /><span>Suppliers</span></NavLink>
        <NavLink to="/app/reference" className={link}><ReferenceIcon /><span>Reference Data</span></NavLink>
        <span className="nav-group-label">OPERATIONS</span>
        <NavLink to="/app/sales" className={link}><SaleIcon /><span>Sales</span></NavLink>
        {dispensing && <NavLink to="/app/prescriptions" className={link}><PrescriptionIcon /><span>Prescriptions</span></NavLink>}
        {dispensing && <NavLink to="/app/h1-register" className={link}><PrescriptionIcon /><span>H1 Register</span></NavLink>}
        <NavLink to="/app/purchases" className={link}><PurchaseIcon /><span>Purchases</span></NavLink>
        <NavLink to="/app/returns" className={link}><ReturnIcon /><span>Returns</span></NavLink>
        <NavLink to="/app/inventory" className={link}><InventoryIcon /><span>Inventory</span></NavLink>
        <span className="nav-group-label">CONFIGURATION</span>
        <NavLink to="/app/settings/store" className={link}><SettingsIcon /><span>Store Profile</span></NavLink>
        <NavLink to="/app/settings/drug-compliance" className={link}><SettingsIcon /><span>Drug Compliance</span></NavLink>
        <NavLink to="/app/settings/data-safety" className={link}><DataSafetyIcon /><span>Data Safety</span></NavLink>
      </nav>
      {!collapsed && <p className="sidebar__next">Accounting and GST returns arrive in a later slice.</p>}
      <button className="sidebar-toggle" type="button" onClick={toggleSidebar} aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"} aria-expanded={!collapsed}><span aria-hidden="true">{collapsed ? "›" : "‹"}</span><span>{collapsed ? "" : "Collapse"}</span></button>
    </aside>
    <div className="workspace">
      <header className="topbar">
        <button ref={navToggle} className="nav-toggle" type="button" onClick={() => setNavOpen(true)} aria-label="Open navigation" aria-expanded={navOpen} aria-controls="primary-navigation">
          <span aria-hidden="true">☰</span>
        </button>
        <div className="topbar__workspace"><span className="topbar__label">WORKSPACE</span><strong>{auth.status?.storeDisplayName ?? "AUSHADHARTH"}</strong></div>
        <div className="topbar__actions">
          <ConnectivityStatus />
          <div className="user-menu">
            <button ref={menuButton} className="user-button" type="button" onClick={() => setMenuOpen(!menuOpen)} aria-haspopup="menu" aria-expanded={menuOpen}><span className="avatar" aria-hidden="true">{initials(user?.displayName)}</span><span><strong>{user?.displayName}</strong><small>{roleLabel(user?.role)}</small></span><span aria-hidden="true">⌄</span></button>
            {menuOpen && <div className="user-popover" role="menu"><p><strong>{user?.displayName}</strong><small>{user?.loginIdentifier}</small></p><button role="menuitem" type="button" onClick={() => void auth.logout()}>Sign out</button></div>}
          </div>
        </div>
      </header>
      <main className="main-content">{children}</main>
    </div>
  </div>;
}

function ConnectivityStatus() {
  const [internetOnline, setInternetOnline] = useState(navigator.onLine);
  useEffect(() => { const online = () => setInternetOnline(true); const offline = () => setInternetOnline(false); window.addEventListener("online", online); window.addEventListener("offline", offline); return () => { window.removeEventListener("online", online); window.removeEventListener("offline", offline); }; }, []);
  return <div className="connectivity" aria-label="Connection status"><span><StatusDot tone="success" /> Local Service <strong>Online</strong></span><span className="internet-state">Internet <strong>{internetOnline ? "Online" : "Offline"}</strong></span></div>;
}

function Dashboard() {
  const auth = useAuth();
  const summary = useQuery<DashboardSummary>({ queryKey: ["dashboard", "summary"], queryFn: async () => DashboardSummarySchema.parse(await localServiceRequest("/api/v1/dashboard/summary")), retry: false });
  useEffect(() => { if (summary.error instanceof LocalServiceError && ["session_expired", "authentication_required"].includes(summary.error.code)) void auth.expireSession(); }, [summary.error]);
  return <><header className="page-header"><div><p className="eyebrow">OVERVIEW</p><h1>Dashboard</h1><p>Your local pharmacy workspace is ready.</p></div><span className="date-chip">{new Intl.DateTimeFormat("en-IN", { dateStyle: "medium" }).format(new Date())}</span></header><section className="welcome-panel"><div><span className="welcome-kicker">FOUNDATION READY</span><h2>Welcome, {auth.status?.user?.displayName}</h2><p>AUSHADHARTH is running locally. Pharmacy operational figures will appear only after their modules are enabled and real transactions exist.</p></div><div className="shield-mark" aria-hidden="true">✓</div></section>{summary.isPending ? <LoadingState /> : summary.isError ? <ErrorState onRetry={() => void summary.refetch()} /> : <section className="metric-grid" aria-label="Current catalog summary"><Metric label="Active products" value={summary.data.activeProductCount} note="Medicine and pharmacy item identities" /><Metric label="Active packs / SKUs" value={summary.data.activePackCount} note="Configured saleable presentations" /><Metric label="Local service" value="Online" note="Core operations available without internet" success /></section>}<section className="readiness-card"><div><h2>Workspace readiness</h2><p>Foundation services available for the next implementation slices.</p></div><ul><li><StatusDot tone="success" /><span><strong>Local database</strong><small>Store Service-owned SQLite foundation</small></span><b>Ready</b></li><li><StatusDot tone="success" /><span><strong>Secure access</strong><small>Local user and session protection</small></span><b>Ready</b></li><li><StatusDot tone="neutral" /><span><strong>Operational modules</strong><small>Masters, sales, purchase, and stock</small></span><b className="readiness-planned">Planned</b></li></ul></section></>;
}

function Metric({ label, value, note, success = false }: { label: string; value: number | string; note: string; success?: boolean }) { return <article className="metric-card"><span>{label}</span><strong className={success ? "metric-success" : ""}>{value}</strong><p>{note}</p></article>; }
function LoadingState() { return <div className="state-card" role="status"><div className="small-spinner" /> Loading workspace summary…</div>; }
function ErrorState({ onRetry }: { onRetry: () => void }) { return <div className="state-card state-card--error" role="alert"><span>Workspace summary is temporarily unavailable.</span><Button variant="secondary" onClick={onRetry}>Retry</Button></div>; }
function AuthenticatedNotFound() { const location = useLocation(); return <AppShell><div className="not-found"><h1>Page not found</h1><p>No application page exists at <code>{location.pathname}</code>.</p><NavLink to="/app/dashboard">Return to Dashboard</NavLink></div></AppShell>; }

interface TextFieldProps { label: string; value: string; onChange: (value: string) => void; error?: string; hint?: string; autoComplete: string; autoFocus?: boolean; }
function TextField({ label, value, onChange, error, hint, autoComplete, autoFocus }: TextFieldProps) { const id = label.toLowerCase().replace(/\W+/g, "-"); return <div className="field"><label htmlFor={id}>{label}</label><input id={id} value={value} onChange={(event) => onChange(event.target.value)} aria-invalid={Boolean(error)} aria-describedby={error || hint ? `${id}-help` : undefined} autoComplete={autoComplete} autoFocus={autoFocus} />{(error || hint) && <small id={`${id}-help`} className={error ? "field-error" : ""}>{error ?? hint}</small>}</div>; }
function PasswordField(props: Omit<TextFieldProps, "hint" | "autoFocus"> & { visible: boolean; onToggle: () => void }) { const id = props.label.toLowerCase().replace(/\W+/g, "-"); return <div className="field"><label htmlFor={id}>{props.label}</label><span className="password-input"><input id={id} type={props.visible ? "text" : "password"} value={props.value} onChange={(event) => props.onChange(event.target.value)} aria-invalid={Boolean(props.error)} aria-describedby={props.error ? `${id}-help` : undefined} autoComplete={props.autoComplete} /><button type="button" onClick={props.onToggle} aria-label={`${props.visible ? "Hide" : "Show"} ${props.label.toLowerCase()}`}>{props.visible ? "Hide" : "Show"}</button></span>{props.error && <small id={`${id}-help`} className="field-error">{props.error}</small>}</div>; }
function Button({ children, variant = "primary", full = false, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "primary" | "secondary"; full?: boolean }) { return <button className={`button button--${variant} ${full ? "button--full" : ""}`} {...props}>{children}</button>; }
function FormError({ children }: { children: ReactNode }) { return <div className="form-error" role="alert">{children}</div>; }
function StatusDot({ tone }: { tone: "success" | "danger" | "warning" | "neutral" }) { return <span className={`status-dot status-dot--${tone}`} aria-hidden="true" />; }
function DataSafetyIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3l7 3v5c0 4.5-3 8.2-7 10-4-1.8-7-5.5-7-10V6zM9 12l2 2 4-4" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function DashboardIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 4h6v6H4zm10 0h6v6h-6zM4 14h6v6H4zm10 0h6v6h-6z" /></svg>; }
function ProductIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 7h14v12H5zM8 4h8v3M9 11h6M12 8v6" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function PrescriptionIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M7 3h7l4 4v14H7zM14 3v4h4M10 12h5M10 16h5" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function ReturnIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 14L4 9l5-5M4 9h10a6 6 0 0 1 0 12H7" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function SaleIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 4h12l1 16H5zM9 8a3 3 0 0 0 6 0" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function PurchaseIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5h3l2 9h9l2-6H8M10 19a1 1 0 1 0 0-.01M18 19a1 1 0 1 0 0-.01" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function InventoryIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 8l8-4 8 4v8l-8 4-8-4zM4 8l8 4 8-4M12 12v8" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function ReferenceIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 3h14v18H5zM8 7h8M8 11h8M8 15h5" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function SettingsIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h14v14H5zM9 9h6v6H9" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function SupplierIcon() { return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 8h11v8H3zM14 11h4l3 3v2h-7zM7 19a2 2 0 1 0 0-.01M18 19a2 2 0 1 0 0-.01" fill="none" stroke="currentColor" strokeWidth="2" /></svg>; }
function initials(name?: string) { return name?.split(/\s+/).slice(0, 2).map((part) => part[0]).join("").toUpperCase() || "AU"; }
function roleLabel(role?: UserRole) { return role === "owner_admin" ? "Owner / Admin" : role === "pharmacist" ? "Pharmacist" : "Cashier"; }
function validateSetup(values: { store: string; owner: string; login: string; password: string; confirm: string }) { const errors: Record<string, string> = {}; if (!values.store.trim()) errors.store = "Store name is required."; if (!values.owner.trim()) errors.owner = "Owner name is required."; if (!LOGIN_PATTERN.test(values.login.trim())) errors.login = "Enter a valid Login ID."; const scalars = Array.from(values.password); const categories = [scalars.some((value) => UPPERCASE_PATTERN.test(value)), scalars.some((value) => LOWERCASE_PATTERN.test(value)), scalars.some((value) => NUMBER_PATTERN.test(value)), scalars.some((value) => !UPPERCASE_PATTERN.test(value) && !LOWERCASE_PATTERN.test(value) && !NUMBER_PATTERN.test(value))].filter(Boolean).length; if (scalars.length < 12 || scalars.length > 128 || categories < 3) errors.password = "Password does not meet the requirements."; if (!values.confirm) errors.confirm = "Confirm your password."; else if (values.confirm !== values.password) errors.confirm = "Passwords do not match."; return errors; }
