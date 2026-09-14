# ADR-015: Product tax classification without a stored rate

## Status
Accepted. Complements ADR-009 (value representation) and ADR-012 (authenticated reference
administration). Builds on ADR-014 (party identity), which supplies the place-of-supply context a
future document will need.

## Context

Purchase and Sale documents must know a Product's tax treatment. The frozen Phase 1A masters already
carry the pieces: `hsn_codes` classifies goods, `tax_categories` names a treatment, and
`tax_rate_versions` holds effective-dated rates as four integer basis-point components — CGST, SGST,
IGST, and cess — with half-open `[effective_from, effective_to)` periods and database-enforced
no-overlap among active versions.

What was missing was the link from a Product to those masters, and a single authoritative place to
answer "what rate applied on this date?".

## Decision

### A Product identifies its classification; it never stores a rate

`products` gains exactly two nullable columns — `hsn_code_id` and `tax_category_id` — and no rate,
percentage, or basis-point column. None may ever be added.

Tax rates change. A rate copied onto a Product is correct only until the next notification, after
which every historical document that reads it silently becomes wrong. The correct division is:

- the **Product** identifies *which* Tax Category applies;
- the **Tax Category** owns the effective-dated rate history;
- a **posted document** snapshots the figures it actually applied, and is thereafter self-describing.

This mirrors the rule that keeps `product_batches` free of a quantity column and makes
`inventory_movements` the sole authority for stock.

### Two columns, not a classification history entity

A dedicated `product_tax_classifications` table with its own lifecycle was considered and rejected.

Point-in-time truth belongs to the transaction: a posted line recording what it applied is strictly
better evidence than reconstructing what the Product was *configured* as on some date. Change
history already exists in the append-only, database-enforced `master_change_events` stream, and the
classification endpoint writes both the previous and next values into it. A history entity would
have added a second revision and archive surface, a "which row is current" rule, and a real risk of
lifecycle deadlock, for nothing those two facts do not already provide.

Because classification lives on `products`, it uses the Product's own revision and `expectedRevision`
— one concurrency surface, not two — and audits under the existing `'product'` entity type, so no
`master_change_events` migration was needed at all.

### HSN and Tax Category stay independent

The frozen masters encode no relationship between them, and this phase invents none. An HSN code is
a goods classification; a Tax Category carries rates. There is no universal HSN→rate table in the
schema, and hardcoding one would be a guess that outlives its accuracy. The two are selected
independently.

### One resolver owns effective-date logic

`domain::taxation::resolve_tax_rate(executor, tax_category_id, on_date)` is the single
implementation of the interval rule, honouring half-open `[from, to)` exactly as the frozen
migration comment and no-overlap triggers define it. Only active versions resolve. The Tax
Category's own status is deliberately not filtered, so a Product referencing a since-archived
category still resolves its rate for historical display — archiving restricts assignment, not
reading.

Phase 1G calls this function instead of re-deriving date arithmetic.

### The resolver reports components; it does not choose a treatment

All four components are returned, and nothing selects between `CGST + SGST` and `IGST`. That
depends on supplier or customer registration and place of supply — transaction context that
Phase 1E's Party and State identity supplies and Phase 1G will combine with the Product's Tax
Category and the document date. No transaction geography is stored on the Product.

### Classification is optional until a document needs it

The migration is additive and nullable; every existing Product keeps working. Requiring a
classification at Product creation would block ordinary catalog work for a document that does not
yet exist, so the requirement belongs at GST-aware posting in Phase 1G. The UI shows an honest
*incomplete* state, rendered distinctly from a load error.

### Only a changed reference is validated

The integrity triggers check a reference **that actually changes**, comparing `NEW` against `OLD`.

The rule is load-bearing rather than cosmetic. The endpoint writes both columns on every call, so
re-validating unchanged values would trap a Product holding a since-archived HSN: clearing only its
Tax Category would re-check the untouched archived reference and be refused, leaving the Product
permanently uneditable. Removing the comparison during hardening reproduced exactly that failure, so
the guard is proven, not assumed.

The service performs the same check first to give a precise message — missing and archived are
different mistakes — with the trigger as the database-level backstop.

## Consequences

Classification is an Owner/Admin mutation behind the frozen Host/Origin check, audited with the
actor taken from the validated server session. Every response echoes the date it resolved for,
making it impossible to read the rate as permanent Product metadata. No new dependency, no new
configuration surface.

`asOf` selects which historical rate is *displayed* and is never persisted, so a caller naming a
date is a feature rather than a risk. The UI therefore sends the workstation's calendar date, the
same way the Phase 1D ledger derives `occurredOn`; the service falls back to its own clock only
when no date is given. That fallback is UTC, which runs a day behind India for the first five and a
half hours of every business day — long enough to show yesterday's rate the morning a change takes
effect, which browser preview caught. Resolving an IANA business time zone server-side would need a
tzdb dependency, so the workstation date is both the smaller and the more accurate answer.

## Non-goals

No HSN→rate mapping. No permanent Product rate. No transaction geography on the Product. No
classification history entity. No change to the frozen tax masters. No Purchase, Sale, tax invoice,
GST return, input credit, or accounting surface.
