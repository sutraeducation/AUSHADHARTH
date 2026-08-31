# ADR-004: Runtime data locations

## Status
Accepted for Phase 0.

## Context
Runtime business data must survive application upgrades and must not be mixed
with source code or unsafe file-sync locations.

## Decision
Resolve database, logs, backups, attachments, and configuration/key directories
through Windows local application-data conventions. Keep categories separate.
Reject database paths inside the repository or a path visibly under OneDrive.

## Consequences
Source checkouts and application packages contain no customer data. Installers,
ACLs, support tooling, and restore procedures must use the resolved locations.

## Non-goals
OneDrive is not a backup destination. Phase 0 does not implement custom runtime
path overrides or roaming profiles.
