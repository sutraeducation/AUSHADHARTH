# ADR-001: Local-first topology

## Status
Accepted for Phase 0.

## Context
Core pharmacy work must continue without internet and business data should stay
on the customer's PC by default.

## Decision
Use a React PWA served locally in production by a Rust Store Service. The service
and its SQLite database are authoritative. Internet and cloud components are not
on the critical path for core operations.

## Consequences
Installations remain usable during internet outages. Windows packaging, service
lifecycle, local recovery, and safe updates become product responsibilities.

## Non-goals
Cloud sync, cloud hosting, licensing, and remote administration are not Phase 0.
