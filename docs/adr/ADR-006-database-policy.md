# ADR-006: Database transaction and migration policy

## Status
Accepted for Phase 0.

## Context
Power loss, retries, and upgrades must not leave partial business operations or
an ambiguous schema.

## Decision
Use SQLite foreign keys, WAL, a five-second busy timeout, and full synchronous
mode. Business operations use explicit transactions. Migrations are ordered,
immutable, forward-only, and applied by the service before serving requests.
Future final-sale posting is one atomic, idempotent transaction.

## Consequences
Every migration needs fresh-install, upgrade, restart, failure, and backup tests.
An application update cannot casually downgrade a migrated database.

## Non-goals
No pharmacy transaction schema or downgrade migration is defined in Phase 0.
