# ADR-012: Authenticated reference administration

## Status

Accepted for Phase 1C-A2.

## Decision

Store Service requires a valid local session for all Phase 1A reference reads. Reference mutations additionally require the `owner_admin` role. Pharmacist and cashier roles are read-only until a reviewed permissions model exists.

Mutation audit events derive `actor_id` from the validated server-side session. Browser request bodies cannot select or override the actor. Existing Phase 1A normalization, uniqueness, optimistic revision, archive/restore, and transaction behavior are unchanged.

Reference list requests may use `parentId` only for company identifiers and tax-rate versions, allowing parent screens to request bounded server-filtered data.

## Consequences

User-facing reference APIs are no longer anonymously callable. Internal tests and tools must authenticate deliberately. Client-side action visibility is usability only; Store Service authorization remains authoritative. A broader permission engine and noninteractive integration credentials remain deferred.
