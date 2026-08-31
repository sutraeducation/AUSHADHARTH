# AUSHADHARTH

**Pharmacy. Inventory. Accounts.**

A product of BIZARTH TECHNOLOGIES PRIVATE LIMITED.

AUSHADHARTH is a local-first pharmacy management application. The Phase 0
foundation consists of an installable React PWA and a loopback-only Rust Store
Service that exclusively owns the local SQLite database. Core operations must
not depend on internet access, and customer business data stays on the
customer's PC by default.

## Foundation commands

Prerequisites: Node.js 24+, pnpm 11.19.0 through Corepack, and a current stable
Rust toolchain.

```sh
pnpm install
pnpm dev
pnpm typecheck
pnpm test
pnpm build
cargo test --manifest-path apps/store-service/Cargo.toml
cargo run --manifest-path apps/store-service/Cargo.toml
```

The Store Service listens on `127.0.0.1:47831` by default. Runtime database,
logs, backups, attachments, configuration, and keys are resolved outside this
repository. See `docs/blueprint/phase-0-foundation.md` before development.

Phase 0 deliberately contains no pharmacy masters, billing, inventory,
accounting, tax, prescription, cloud-sync, licensing, or multi-counter
implementation.
