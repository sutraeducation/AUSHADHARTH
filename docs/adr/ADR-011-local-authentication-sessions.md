# ADR-011: Local authentication and opaque cookie sessions

## Status

Accepted for Phase 1C-A.

## Context

AUSHADHARTH must authenticate users without internet access. The production web
UI and API share the Store Service origin, while the initial Windows deployment
uses loopback HTTP. Browser storage must not become a credential store.

## Decision

Use canonical local login identifiers and Argon2id password hashes with unique
salts. Password length means 12–128 Unicode scalar values; contents are hashed
exactly as entered without trimming, normalization, or case folding. Uppercase,
lowercase, numeric, and other characters are classified using Unicode-aware
scalar properties, and at least three categories are required. Successful setup
or login creates a cryptographically random 256-bit
opaque token. Only its SHA-256 hash is stored in SQLite. The raw token is sent in
an `HttpOnly`, `SameSite=Strict`, path-scoped cookie with a bounded lifetime.
Every authentication decision is made by the Store Service against the session
and active User record.

The cookie omits `Secure` on the Phase 1C-A loopback HTTP origin because browsers
would otherwise withhold it. The service adds `Secure` when the request is served
from an approved HTTPS loopback origin. Any future LAN deployment must provide
HTTPS before cookies are accepted outside this loopback model.

Authentication mutations accept JSON only, reject cross-site browser requests,
validate Host and Origin as loopback authorities, and emit no wildcard CORS
headers. Failed-login increments are serialized in SQLite, and a short bounded
cooldown protects login without permanent offline lockout. Attempt rows older
than 24 hours are opportunistically deleted during login, retaining substantially
more history than the active five-minute window. Session rows survive Store
Service restarts until expiry or revocation. Multiple concurrent sessions are
currently allowed; pruning expired and revoked sessions is deferred to future
user/session administration.

## Consequences

Passwords and raw session tokens never enter logs, audit payloads, API responses,
or browser storage. Logout revokes the server row and clears the cookie. Disabled
or archived users cannot authenticate or continue a session. User roles provide
a stable future attachment point for permission tables without changing User IDs.

## Deferred

Owner recovery, password changes, user administration, fine-grained permissions,
LAN authentication, external identity, and cloud authentication are deferred.
