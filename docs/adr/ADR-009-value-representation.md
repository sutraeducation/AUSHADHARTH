# ADR-009: Money, quantity, and time representation

## Status
Accepted foundation rule.

## Context
Floating-point rounding and ambiguous local timestamps can corrupt financial and
stock records.

## Decision
Never use binary floating point for money. Store money as documented integer
minor units or fixed-scale decimal values. Quantities use an explicitly bounded
fixed scale. Technical timestamps are UTC ISO-8601 values. Business documents
also retain business date and IANA time-zone context. Future accounting postings
are append-only and corrected through reversals.

## Consequences
Each field requires units, scale, rounding mode, and time semantics. APIs must not
silently coerce these values to JavaScript `number` when precision can be lost.

## Non-goals
Tax rounding, medicine pack conversion, valuation, and accounting ledgers remain
future blueprint decisions.
