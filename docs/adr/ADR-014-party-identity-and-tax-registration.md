# ADR-014: Party identity, roles, and tax registration

## Status
Accepted. Complements ADR-008 (identifiers), ADR-009 (value representation), and ADR-012
(authenticated reference administration).

## Context

Purchase inward needs a supplier, and a supplier needs an identity: a name, a tax registration, a
place of supply, addresses, and a drug licence. Phase 1E establishes that identity on its own, ahead
of the Product tax classification (1F) and Purchase (1G) phases that will use it.

Two questions had to be settled before any of it could be written down: what a Party *is* relative
to an accounting ledger, and how a supplier relates to a future customer.

## Decision

### A Party is an identity, never an account

`parties` carries no balance, outstanding amount, credit limit, ageing bucket, or amount of any
kind, and none may ever be added. A future accounting ledger will reference `party_id` and derive
every figure by summing its own postings.

This is the same rule that keeps `product_batches` free of a quantity column and makes
`inventory_movements` the sole authority for stock. The failure it prevents is identical: a stored
figure that drifts from the postings it is supposed to summarise, with no way to tell which is
right. Tests assert the absence of such a column at the schema level, in the unit suite and again
in the real-service gate.

### One Party, with roles attached

Identity lives once in `parties`; `party_roles` records what the Party is *to this store*. A
neighbouring pharmacy is routinely both supplier and customer, and modelling those as two top-level
tables would duplicate GSTIN, PAN, and addresses for one legal entity and leave the accounting
ledger ambiguous about which row is the counterparty.

`party_roles.role` is `CHECK (role IN ('supplier', 'customer'))` — the complete set of counterparty
roles a pharmacy has, named in the schema the way Phase 1D named `'adjustment'` alongside
`'opening_stock'`. The Phase 1E **API rejects `customer`**: what is deferred is the Customer
workflow — sales, receivables, loyalty — not the word. A role carries its own status and revision,
so a Party can stop being a supplier without ceasing to exist.

### One GSTIN per Party, verified arithmetically

A GSTIN is per state registration, so an entity operating in several states holds several. Phase 1E
stores **one** — the registration this store transacts with — as columns on `parties`.

A child table would be structurally more general but would demand a rule for *which* registration
applies to a given transaction. That rule cannot be written honestly before purchase and tax exist,
and guessing it is worse than deferring it. Multi-registration is a named non-goal.

The GSTIN is validated three ways:

1. **Structure** — two digits of state code, a ten-character PAN, an entity number, the literal
   `Z`, and a check character.
2. **Checksum** — the official mod-36 weighted algorithm over the first fourteen characters,
   verified in development against genuine published GSTINs and exercised in tests by numbers that
   are well-shaped and wrong. A mistyped GSTIN that merely looks right is worse than one that is
   obviously wrong: it reaches a purchase document looking authoritative.
3. **Agreement with the place of supply** — the leading two digits must equal the referenced
   State's code. Enforced by a database trigger, because the code lives in a referenced row and a
   `CHECK` cannot read one.

PAN is stored alongside and, when both are present, must equal characters 3–12 of the GSTIN. PAN is
deliberately **not** unique: one entity registered in several states is recorded as several Parties
under the one-GSTIN model, and those Parties legitimately share a PAN. A shared PAN surfaces as a
duplicate candidate, never as a conflict.

### Registration status distinguishes two different facts

`gst_registration_status` is `registered | unregistered | unknown`. `unregistered` is a positive
assertion that this supplier holds no GSTIN; `unknown` means nobody has captured it yet. A nullable
GSTIN column alone cannot tell them apart, and the difference matters to reverse-charge handling
when purchase arrives. A `CHECK` binds the two: `registered` if and only if a GSTIN is present.

### States are a seeded reference master

Place of supply needs an authoritative list, so `state_codes` joins the existing reference-master
framework, seeded with the Indian GST State codes. A hardcoded `CHECK` was rejected because the list
changes — Dadra and Nagar Haveli and Daman and Diu merged into code 26 in 2020 — and superseded
codes stay seeded so a supplier's historic GSTIN still validates.

### Drug licence is justified, and stays one field

Two nullable columns, `drug_licence_number` and `drug_licence_valid_upto`. Indian pharmacy purchase
documentation must carry the supplier's drug licence number, it is intrinsic to the Party rather
than to a transaction, and its expiry is a real operational check. It stays a single free-text field
holding the string exactly as printed — commonly a pair such as `20B-1234 / 21B-5678`. Structured
licence types or multi-licence rows would be overbuild: nothing reads the field programmatically.

### Archiving is refused, never cascaded

Archiving a Party while it holds an active role or address returns `archived_conflict`, following
`lifecycle_product`. Cascading would archive rows the operator never saw and would leave restore
ambiguous about what to bring back.

## Consequences

Party administration is Owner/Admin only, passes the frozen Host/Origin mutation check, and records
an append-only audit event per write with the actor taken from the authenticated server session.
Duplicate candidates are advisory and never block creation: two pharmacies really can be called
"Sharma Medicals".

New typed error codes — `party_conflict`, `party_role_conflict`, `party_address_conflict` — carry no
database text, and a test asserts that none of the party error bodies leak any.

## Non-goals

No accounting ledger, opening balance, credit limit, or payment terms. No purchase, sales, or price
list. No multi-state GST registration for one Party. No GST composition-scheme tax treatment, which
changes tax computation that does not yet exist. No contacts, notes, or activity history: a supplier
master is not a CRM.
