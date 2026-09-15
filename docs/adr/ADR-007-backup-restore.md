# ADR-007: Backup and restore foundation

## Status
Accepted direction; implemented by [ADR-019](ADR-019-backup-and-restore-implementation.md), which
supersedes the encryption requirement below for local backups only. Retained unchanged as the record
of the original direction.

## Context
The customer owns locally stored business data and must recover from disk,
machine, or update failure.

## Decision
Future backups use SQLite's online backup API to create a consistent snapshot,
then package it with a versioned manifest, schema/application versions,
attachments inventory, and checksums in an `.aushbackup` container. Portable
backups use authenticated encryption and must be restore-verified.

## Consequences
Retention, destinations, encryption recovery, free-space checks, atomic file
replacement, and restore drills must be designed before business data ships.

## Non-goals
Phase 0 does not create backups, retention jobs, cloud backup, or encryption keys.
