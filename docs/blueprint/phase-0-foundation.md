# Phase 0 foundation blueprint

## Product

**AUSHADHARTH — Pharmacy. Inventory. Accounts.**

A product of BIZARTH TECHNOLOGIES PRIVATE LIMITED.

## What is built

- A pnpm workspace with pinned package-manager and Node requirements.
- A minimal React, TypeScript, and Vite application shell using React Router,
  TanStack Query, Zustand, React Hook Form, and Zod as the frozen frontend set.
- An installable PWA manifest and an explicit Workbox service worker that
  precaches only versioned application-shell assets.
- A Rust/Tokio/Axum Store Service foundation with versioned health and system
  information endpoints and loopback-only binding.
- Same-origin production UI hosting by the Store Service and a Vite development
  proxy to the loopback API.
- Store Service-owned SQLite via SQLx, a first migration, safe connection pragmas,
  migration restart tests, and runtime-path guards.
- Structured local logging outside the repository with privacy constraints.
- Versioned frontend validation contracts for health and compatibility responses.
- Architecture decisions and foundation test seams.

## What is intentionally not built

There are no medicine, customer, supplier, sales, purchase, inventory, accounting,
GST, prescription, cloud-sync, licensing, multi-counter networking, dashboard,
WhatsApp, or email features. Authentication, authorization, encryption at rest,
backup execution, restore execution, installers, and production update handling
also require later designs and are not implied by the foundation.

## Single-PC topology

```text
Installed browser PWA
        │ same-origin HTTP on loopback
        ▼
Rust Store Service (127.0.0.1 only)
        │ exclusive local connection
        ▼
SQLite under Windows local application data
```

The service hosts the compiled web application. Internet access is not required
to load the production UI, call core APIs, or use the local database.

## Future multi-counter topology

One explicitly designated store PC will run the authoritative Store Service and
SQLite database. Authenticated counters will use the service API over a protected
LAN connection. No client will open SQLite through SMB. LAN discovery, TLS,
sessions, permissions, firewall management, failover, and server replacement are
separate blueprint work and are not implemented here.

## Local-data ownership and offline guarantee

Customer business data remains on the customer's PC by default. Core startup,
masters, purchasing, billing, returns, inventory, accounts, essential reports,
printing, backup, and restore must eventually work without internet. External
gateways and future cloud features must fail independently without blocking local
committed work.

The service worker is an application-shell availability tool only. It is not a
business database and does not cache API responses.

## Database ownership and runtime storage

Only the Store Service owns SQLite. Database, logs, backups, attachments, and
configuration/keys resolve to separate Windows local application-data
directories outside the repository. Repository and obvious OneDrive database
locations are rejected. Connection pragmas are documented separately.

## Backup direction

Future backups use SQLite's online backup API and a versioned `.aushbackup`
container with a manifest, checksums, attachment inventory, and authenticated
encryption. Restore must validate compatibility and integrity before atomically
replacing operational data. A live database file copy is not a backup.

## Future cloud boundary

Cloud capability will be optional and asynchronous. The Store Service—not the
browser—will own a durable outbox, retries, cloud credentials, and conflict
handling. Local globally unique identifiers and idempotency keys preserve a path
to sync without implementing it prematurely.

## Next blueprint decision before pharmacy masters

Freeze the canonical medicine-and-pack identity model together with units and
conversion, batch/expiry identity, MRP and tax classification boundaries, stock
ledger invariants, quantity precision, duplicate detection, and lifecycle rules.
No Medicine Master code should begin until that model and its migration/API
contract have been reviewed alongside the atomic inventory ledger design.
