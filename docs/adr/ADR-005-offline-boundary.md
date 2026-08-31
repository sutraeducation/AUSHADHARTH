# ADR-005: Offline core-operation boundary

## Status
Accepted for Phase 0.

## Context
A pharmacy cannot stop core work when internet access fails.

## Decision
Application startup, local authentication when introduced, masters, purchases,
sales, returns, inventory, accounts, reports needed for daily operations,
printing, backup, and restore must operate without internet. Static UI assets
are local and precached; the service worker never stores authoritative data.

## Consequences
No core workflow may synchronously depend on cloud identity, licensing, CDN,
telemetry, or sync. Offline behavior must be tested deliberately.

## Non-goals
External messaging, regulatory gateways, remote backup, and cross-store views may
require internet and are outside Phase 0.
