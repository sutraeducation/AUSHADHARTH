# ADR-010: Security and privacy baseline

## Status
Accepted for Phase 0.

## Context
Pharmacy systems contain commercially and personally sensitive information and
run on customer-controlled Windows PCs.

## Decision
Default to loopback-only networking and least privilege. Do not log secrets,
patient/customer data, or routine medicine-sale details. Keep keys separate from
data and protect future local keys with Windows facilities plus a documented
recovery mechanism. Validate input at API boundaries and expose no filesystem
paths or business data through system endpoints.

## Consequences
Threat modeling, Windows ACLs, signed releases, dependency review, audit events,
session security, encryption-at-rest evaluation, and secure support workflows are
required before production.

## Non-goals
Authentication, authorization, SQLCipher, key provisioning, LAN TLS, telemetry,
and remote support are not implemented in Phase 0.
