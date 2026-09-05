# Phase 1C-A2 — Reference Data UI

## Boundary

This slice exposes only the reference masters already implemented in Phase 1A. Product/Pack, medicine, composition, batch, stock, purchasing, billing, parties, accounting, compliance workflows, import, cloud, licensing, and multi-counter networking remain deferred.

## Navigation and routes

The authenticated App Shell contains a **Masters → Reference Data** destination. The landing route is `/app/reference`; focused routes are `/app/reference/units`, `/dosage-forms`, `/companies`, `/brands`, `/hsn`, `/tax`, and `/regulatory`. Company identifiers and tax-rate versions are managed in the context of their parent records rather than as sidebar destinations.

All routes are reached only after the startup/session state machine reports `AUTHENTICATED`. Refresh and unknown-route behavior continue to use the existing application fallback and guards.

## Temporary role policy

- `owner_admin`: read and create/update/archive/restore.
- `pharmacist`: read-only.
- `cashier`: read-only.

The browser uses the role returned by the validated session only to hide or disable actions. Store Service independently enforces the policy: every reference request requires a valid session and every mutation requires `owner_admin`. This is deliberately conservative and is not a general permissions engine.

## Server-derived audit actor

Reference mutation handlers resolve the current user from the opaque session cookie. `master_change_events.actor_id` is populated from that server-side result. Request bodies do not carry an authoritative actor; an extra browser-supplied `actorId` has no effect. Phase 1A validation, revision, uniqueness, archive, and restore semantics remain unchanged.

## List and form patterns

Reference screens share semantic tables, text search, lifecycle filters, loading/empty/error states, retries, status text, and keyboard-reachable actions. Simple masters use focused dialogs. Company identifiers and tax rates use wider parent-detail dialogs. This is a shared UI pattern, not a metadata-driven form engine; entity-specific fields and conversions remain explicit.

Forms provide labels, required indicators, helper text, inline client and structured server validation, loading state, Cancel, keyboard submit, Escape close, focus containment/restoration, and revision-conflict feedback. A conflict never silently overwrites: the user is told the record changed and can close/reload the latest list.

## Lifecycle behavior

Archive is never presented as deletion. It requires a reason and retains history. Restore supplies a reason and lets the backend rerun current uniqueness, reference, and effective-period protections. Active records are the default view; archived and combined filters are explicit.

## Tax percentage convention

SQLite and Store Service remain authoritative in integer basis points. UI input accepts a decimal percentage string with at most two fractional digits and converts it using string components: `5.00` becomes `500`. Submitted values never rely on binary floating-point arithmetic. Display converts basis points back to a fixed two-decimal string.

Tax-rate versions are not edited in place. Users create a new version, or archive/restore an existing version. Effective periods are shown as half-open `[from, to)` intervals: the end date is excluded and may be the start of an adjacent version.

## Accessibility and responsive behavior

Tables retain native table semantics, headers, status text, labeled actions, and a horizontal-scroll boundary at narrow widths. Dialogs are rendered as document-level portals and each title receives a stable, instance-unique identifier. When a company-identifier or tax-rate editor is opened over its parent details, the parent remains mounted but is inert and hidden from the accessibility tree. Only the active child owns Tab, Shift+Tab, and Escape; closing it restores focus to the exact launching control before the parent trap resumes. Simple dialogs use the same focus lifecycle without a child layer.

Errors use alert/live regions, and invalid submission moves focus to the first invalid control. Focus styles and reduced-motion behavior follow the application tokens. Forms collapse to one column on narrow laptops/tablets while desktop density remains primary.

## Authentication hardening used by this slice

All request-path Argon2id hashing and verification—including first-run setup and dummy verification for unknown users—crosses a Tokio `spawn_blocking` boundary. Join failures are reported through the existing safe internal failure response; credential failures remain deliberately indistinguishable and no password material is logged or returned.

Short authentication write transactions are serialized inside Store Service while SQLite remains the authoritative source of attempt, cooldown, and session state. A success clears attempt state only after verification; a failure performs its atomic increment when it reaches the serialized write boundary, so mixed success/failure results follow database transaction order. A separate identifier never shares attempt state. SQLite `BUSY`/`LOCKED` conditions that survive the bounded critical section map to the typed `service_busy` response with a one-second retry hint, never to invalid credentials or a raw database error. Transactions are not blindly replayed, avoiding duplicate sessions and audit effects.

## Deferred

Phase 1C-A3/1C-B may add deeper App Shell decomposition and Product/Pack UI. It must reuse the session-derived authorization/audit boundary and must not weaken the frozen reference-master rules.
