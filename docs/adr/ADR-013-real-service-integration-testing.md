# ADR-013: Real Store Service integration testing without a database-path override

## Status
Accepted. Supersedes nothing; complements ADR-002, ADR-004, and ADR-006.

## Context

Phases through 1D introduced batch identity, exact-money MRP, and an append-only
inventory ledger. The Phase 1C-A3 freeze audit required a real Store Service
integration gate before any money-plus-stock workflow, and that point is now
reached: Purchase is the next commercial domain.

Until now, confidence in the frontend↔backend seam came from HTTP-level test
doubles. Backend behaviour was already exercised against a real migrated SQLite
database through `tower::ServiceExt::oneshot`, but no test crossed a real TCP
socket into a listening service.

The obstacle was storage authority. `RuntimePaths::resolve` always resolves the
authoritative database to the user's real
`%LOCALAPPDATA%\Bizarth Technologies\AUSHADHARTH`, and `main.rs` hardcodes that
path and the loopback port. Running a test against the started binary would
therefore write to a real pharmacy's live database.

## Decision

**No database-path override is introduced.** Production `RuntimePaths`,
`validate_database_path`, and `main.rs` are unchanged.

Instead, a Cargo **integration test target** (`apps/store-service/tests/`)
assembles the real service stack in-process and binds it to a real loopback TCP
port:

- a temporary directory per test run, created by `tempfile`;
- `infrastructure::database::connect`, which applies the real migrations with the
  real pragmas;
- `api::router`, the same router `main.rs` serves;
- `axum::serve` on `127.0.0.1:0`, an OS-assigned ephemeral port;
- requests issued as raw HTTP/1.1 over `tokio::net::TcpStream`.

Cargo compiles targets under `tests/` only for `cargo test`. They are absent from
`cargo build --release`, so this adds no production surface at all — not a
disabled code path, not a feature-gated branch, but code that does not exist in a
shipped binary.

## Consequences

**Why this is safe.** There is no activation boundary to get wrong, because
production startup is not modified and cannot be redirected. The real
`%LOCALAPPDATA%` database is never opened by the harness: the path handed to
`connect` comes from `tempfile::tempdir()`, which the operating system places
under the user's temp directory and removes when the guard drops. That path is
outside the repository and outside OneDrive, so it satisfies
`validate_database_path` on its own terms rather than by bypassing it.

**No arbitrary-path feature is created.** A customer-facing database relocation
capability would need its own ADR, its own validation, its own migration and
backup story, and its own threat model. None of that is implied here. Nothing in
this decision lets an end user, an environment variable, or a browser choose
where the authoritative database lives.

**No new dependency.** `tokio`, `axum`, `sqlx`, and `tempfile` are already
present. The HTTP client is a small hand-written HTTP/1.1 writer and reader in
the test target, which is why no client crate is added. Adding one would have
been a dependency change requiring separate approval.

**CI behaviour.** The harness runs inside the ordinary
`cargo test --all-targets --all-features` invocation, needs no service to be
running, and binds an ephemeral port so parallel runs cannot collide. Each test
gets its own database, so there is no shared state and no ordering requirement.

**Cleanup.** The `TempDir` guard removes the directory and its SQLite, WAL, and
SHM files when the test ends. A crashed run leaves only an OS temp directory,
never anything under the customer data root.

**Security.** The service binds loopback only. The harness performs a real
first-run setup and a real cookie session, so authentication, the Host/Origin
mutation policy, and role checks are genuinely exercised. No password, token, or
session cookie is logged by the harness.

## What this does and does not cover

Covered: TCP, HTTP/1.1 framing, Axum routing and extractors, cookie session
authentication, the Host and content-type mutation policy, role authorisation,
real migrations, SQLite constraints and triggers, transaction behaviour, and
typed error envelopes.

Not covered: the browser runtime, the compiled web bundle, the service worker,
`RuntimePaths` resolution itself, logging initialisation, static asset serving,
and graceful shutdown — these remain exercised by their own unit tests and by the
Playwright suite against the built frontend.

## Non-goals

No customer database relocation, no configurable data root, no production
environment-variable configuration surface, and no weakening of
`validate_database_path`.
