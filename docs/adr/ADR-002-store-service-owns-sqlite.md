# ADR-002: Store Service owns SQLite

## Status
Accepted for Phase 0.

## Context
Billing, inventory, and accounting require consistent relational transactions
and recoverable local storage.

## Decision
Only the Store Service may open the SQLite database. UI clients use a versioned
API. SQLite files must never be opened through SMB or another network share.

## Consequences
One process can enforce pragmas, migrations, authorization, transactions, and
backup consistency. All data access must be represented by service operations.

## Non-goals
Browser-side replicas, direct SQL access by clients, and shared-file databases
are excluded.
