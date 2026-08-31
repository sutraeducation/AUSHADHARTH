# ADR-008: Identifier and future sync readiness

## Status
Accepted for Phase 0.

## Context
Future multi-store sync cannot safely rely on database-local integer identifiers
or retrofit durable command identity.

## Decision
Persistent domain identities use globally unique, durable UUIDv7 values generated
locally. Integer row IDs may exist only as private performance details. Future
retryable commands carry unique idempotency keys, and future sync uses a durable
transactional outbox owned by the Store Service.

## Consequences
Schema and APIs expose stable string identifiers. Conflict and record-ownership
semantics still require a dedicated sync ADR before implementation.

## Non-goals
No outbox, cloud protocol, conflict resolver, or multi-store sync exists now.
