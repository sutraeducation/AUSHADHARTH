# ADR-003: Single-PC and future LAN multi-counter model

## Status
Accepted for Phase 0.

## Context
The first release targets one Windows PC, while future stores may use multiple
counters.

## Decision
Phase 0 binds the service to loopback only. A future explicit LAN mode will make
one designated store server authoritative and connect authenticated counters to
its API. The API boundary must remain suitable for that transition.

## Consequences
Single-PC exposure is minimal. LAN discovery, TLS, authentication, authorization,
firewall rules, failover, and server replacement require later decisions.

## Non-goals
LAN listeners, peer-to-peer databases, and multi-counter coordination are not
implemented now.
