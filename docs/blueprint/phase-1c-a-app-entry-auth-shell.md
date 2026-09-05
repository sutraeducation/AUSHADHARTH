# Phase 1C-A: App entry, local authentication, shell, and dashboard

## Startup state machine

The UI resolves Store Service system compatibility and authentication status
before routing. Its explicit states are `BOOTING`, `LOCAL_SERVICE_UNAVAILABLE`,
`COMPATIBILITY_ERROR`, `SETUP_REQUIRED`, `AUTH_REQUIRED`, and `AUTHENTICATED`.
Internet connectivity is secondary: internet offline is not a Store Service
failure and does not block authenticated local use.

Public routes are `/`, `/setup`, and `/login`. The protected application starts
at `/app/dashboard`. Server status redirects first-run installations to Setup,
unauthenticated installations to Login, and authenticated visits to Dashboard.
Production fallback hosting continues serving `index.html` on browser refresh;
the Store Service session, never client state alone, authorizes protected data.

## First-run and User identity

Setup is permanently available only while `users` is empty. It collects Store
display name, owner display name, local Login ID, and password. `BEGIN IMMEDIATE`
serializes the final empty-state check, existing `store_identity` is reused, and
Store, owner User, and initial session commit atomically. Concurrent setup cannot
create two first owners and setup never reopens merely because an account is
disabled.

Login IDs are 3–64 ASCII letters, digits, dots, underscores, or hyphens. A
lowercase normalized form is globally unique while the entered display form is
retained. Email and mobile identity remain separate future attributes. User and
Session IDs are UUIDv7. Initial roles are `owner_admin`, `pharmacist`, and
`cashier`; future permission tables can reference the durable User and role
without rebuilding identity.

## Passwords, sessions, and login protection

Passwords are one-way hashed with Argon2id using the library's versioned encoded
format and a unique OS-random salt. Length is defined as 12–128 Unicode scalar
values, with no trimming, normalization, case folding, or other transformation
of password contents. Unicode-aware uppercase, lowercase, numeric, and other
scalar categories are used consistently by Store Service and UI, with at least
three categories required. Neither password nor hash is serialized, audited, or
logged. Unknown and incorrect accounts receive the same response and both execute
password-hash work. Login-attempt keys are SHA-256 hashes of the normalized
identifier, use a five-minute rolling window, and apply a temporary 5–60 second
exponential cooldown beginning with the fifth failure. SQLite `BEGIN IMMEDIATE`
serialization prevents parallel failures from losing increments. Success clears
only that identifier's failure record; there is no permanent automatic lockout.
Rows older than 24 hours are opportunistically deleted during login, while rows
inside the active protection window are retained.

Sessions use 256-bit OS-random opaque tokens. SQLite stores only SHA-256 token
hashes. The raw token exists only in an `HttpOnly`, `SameSite=Strict` cookie and
is replaced after every successful authentication. Multiple concurrent sessions
are currently allowed. Sessions last 12 hours, survive service restart, reject
expiry and inactive Users, and are revoked on logout. Expired/revoked record
pruning is deferred to future user/session administration. Loopback HTTP requires
omission of `Secure`; approved HTTPS requests add it, and any future LAN topology
must require HTTPS.

## Local API security

Authentication mutations require JSON. Browser requests marked cross-site are
rejected, Host and Origin must be loopback authorities, and authenticated routes
publish no wildcard CORS policy. SameSite cookies, Origin checks, and JSON-only
mutations form the Phase 1C-A CSRF boundary. The Store Service remains bound to
`127.0.0.1:47831`. API errors use stable safe codes and never expose SQL or paths.

Routes are:

- `GET /api/v1/auth/status`
- `POST /api/v1/auth/setup`
- `POST /api/v1/auth/login`
- `POST /api/v1/auth/logout`
- `GET /api/v1/auth/session`
- `GET /api/v1/dashboard/summary`

## Application shell and visual system

The desktop shell has a collapsible left navigation, Store/user/service top bar,
and main content region. Dashboard is the only active module. Semantic CSS tokens
cover typography, spacing, surfaces, borders, focus, status colors, radii, and
elevation. Controls have visible focus, semantic labels, keyboard submission,
Escape-close with focus restoration, sufficient contrast, responsive behavior,
and reduced-motion support.

Dashboard reports only truthful available facts: Store name, active Product count,
active Pack/SKU count, local-service state, current User, and foundation readiness.
It never fabricates sales, profit, stock value, expiry, or customer metrics.

## Browser storage and offline policy

Only the non-sensitive sidebar presentation preference is persisted in
`localStorage`. Passwords, password hashes, session tokens, and business masters
are excluded. The service worker precaches versioned application-shell assets
only and does not cache authentication or business API responses.

## Audit actor boundary

User identity comes only from the server-validated session. Phase 1C-A does not
retrofit frozen Phase 1A/1B mutations. Phase 1C-A2 and 1C-A3 must protect their UI
and mutation routes and populate `master_change_events.actor_id` from the resolved
server session, never from a browser-supplied actor field. Terminal identity and
the complete permission matrix remain separate follow-up decisions.

## Boundaries

Phase 1C-A2 adds UI for existing reference foundations. Phase 1C-A3 adds UI for
the existing Product/Pack catalog. Ingredient/composition integration begins only
in Phase 1C-B. Recovery, user administration, detailed permissions, business
transactions, cloud identity, licensing, and multi-counter LAN are deferred.
