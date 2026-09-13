# Phase 1E — Party / Supplier Identity Foundation

## Scope

One **Party** master carrying business identity, with the **Supplier** role attached, tax
registration identity (GSTIN / PAN), postal addresses, and the place-of-supply State. Archive and
restore, optimistic revisions, append-only audit, advisory duplicate candidates, and the
administration UI.

### Explicitly out of scope

| Deferred | Phase |
| --- | --- |
| Product tax classification, HSN/tax-category attachment to Products | 1F |
| Purchase inward, purchase invoices, supplier bills, goods receipt | 1G |
| Customer role behaviour: sales, receivables, loyalty | later |
| Accounting ledger, opening balances, outstanding, credit limit, ageing | later |
| Payment terms, credit days, price lists, supplier-specific rates | later |
| Multi-state GST registration for one Party | later |
| GST composition-scheme tax treatment | tax phase |
| Contacts, notes, activity history, follow-ups, any CRM surface | never in this form |

## Fundamental rules for this phase

1. **A Party is an identity, never an account.** `parties` carries no balance, no outstanding, no
   credit limit, no ageing bucket, and no amount of any kind, and none may ever be added. A future
   accounting ledger will reference `party_id` and derive every balance by summing its own postings,
   exactly as `inventory_movements` is the sole authority for quantity. This is the same rule that
   keeps `product_batches` free of a stock column.
2. **Recording a Party is not a commercial relationship.** Creating a supplier grants no credit,
   implies no agreement, and creates no transaction.
3. **Money and quantity remain absent.** Nothing in this phase stores either.
4. **The actor is the authenticated server session**, never a browser-supplied value.
5. **The Store is resolved server-side.** No endpoint accepts a store identifier from the browser.

## Design decisions

### D1 — One Party, roles attached

A pharmacy's supplier is frequently also a customer: neighbouring pharmacies buy from and sell to
each other, and a distributor may take returns. Modelling Supplier and Customer as two top-level
tables would duplicate GSTIN, PAN, and addresses for the same legal entity, and would make the
future accounting ledger ambiguous about which row is the counterparty.

So identity lives once in `parties`, and `party_roles` attaches what the Party *is to this store*.
A role carries its own `status` and `revision`, so a Party can stop being a supplier without ceasing
to exist and without losing its history.

### D2 — The role enumeration names both roles; the API accepts only `supplier`

`party_roles.role` is `CHECK (role IN ('supplier', 'customer'))`. Supplier and Customer are the
complete set of counterparty roles a pharmacy has, and both are real words in the domain today.
This follows the frozen precedent of `inventory_movements.movement_type IN ('opening_stock',
'adjustment')`, where the schema named the full closed set in Phase 1D.

Phase 1E nevertheless **rejects `customer` at the API** with `validation_failed`. What is deferred
is the Customer *workflow* — sales, receivables, loyalty — not the word. Shipping a Customer master
here would widen the phase beyond the approved scope. A test proves the rejection.

### D3 — Legal name and display name are different things

`display_name` is what the operator typed and what every list shows. `legal_name` is optional and
holds the registered name as printed on the GSTIN certificate, which is frequently longer and
different ("SHARMA MEDICALS" vs "Sharma Medical & General Stores Private Limited"). Search and
duplicate detection use `normalized_search_name`, derived from `display_name`, matching the frozen
treatment of `pharmaceutical_companies` and `brands`.

### D4 — GSTIN is one column, not a child table

A GSTIN is per *state registration*, so a legal entity operating in several states holds several.
Phase 1E stores **one** — the registration this store actually transacts with — as nullable columns
on `parties`.

A child table would be structurally more general but would force every read path through a join and
would demand a rule for *which* registration applies to a given transaction. That rule cannot be
written honestly before the purchase and tax phases exist, and guessing it would be worse than
deferring it. Multi-registration is a named non-goal; if it arrives it brings its own phase, its own
resolution rule, and its own migration.

### D5 — GSTIN validation is structural, checksummed, and cross-checked

A GSTIN is 15 characters: two-digit state code, ten-character PAN, one entity number, the letter
`Z`, and one check character.

- Normalised by removing whitespace and uppercasing; stored both as entered and normalised.
- Structure: `NN` digits, `AAAAA` letters, `NNNN` digits, `A` letter, `[1-9A-Z]`, `Z`,
  `[0-9A-Z]`.
- **Checksum** verified with the official mod-36 weighted algorithm over the first fourteen
  characters. The codebase already validates GTIN check digits, so a real checksum is in keeping
  rather than novel.
- The leading two digits **must match** the Party's place-of-supply State code. The GSTIN encodes
  the state of registration, so a mismatch is always a data-entry error. Enforced by a database
  trigger, because the State code lives in a referenced row and a `CHECK` cannot read one.
- When a GSTIN is present, the place-of-supply State is **required**, since the GSTIN supplies it.

### D6 — PAN is one column and is deliberately not unique

PAN is ten characters: `AAAAA` letters, `NNNN` digits, one letter. It is permanent and there is
exactly one per legal entity, so it belongs on `parties`. No checksum is applied: PAN's check
character algorithm is not published, and inventing one would reject valid data.

PAN is **not** given a uniqueness constraint. One legal entity holding registrations in several
states is legitimately recorded as several Parties under Phase 1E's one-GSTIN model, and those
Parties share a PAN. A shared PAN is therefore surfaced as a duplicate *candidate*, never as a
conflict.

When both are present, PAN must equal characters 3–12 of the GSTIN. That is an exact structural
identity, and violating it means one of the two was mistyped.

### D7 — Registration status distinguishes "unregistered" from "not yet recorded"

`gst_registration_status` is `'registered' | 'unregistered' | 'unknown'`.

`unregistered` is a positive assertion by the operator that this supplier holds no GSTIN.
`unknown` means nobody has captured it yet. They look the same in a nullable GSTIN column but are
not the same fact, and the difference matters to reverse-charge handling when purchase arrives.
A `CHECK` binds them: `registered` ⇔ `gstin IS NOT NULL`.

### D8 — State codes become a seeded reference master

Place of supply needs an authoritative State list. It becomes `state_codes`, administered through
the existing reference-master framework and seeded with the thirty-eight Indian GST State codes plus
Other Territory.

A hardcoded `CHECK` was rejected because the list changes — Dadra & Nagar Haveli and Daman & Diu
merged into code 26 in 2020 — and a frozen `CHECK` would need a migration each time. A reference
master with archive/restore handles that with machinery that already exists and is already tested.

### D9 — Addresses are a child table with one primary per role

`party_addresses` carries `address_role IN ('billing', 'shipping')`, one required line, optional
second line, city, State, postal code, and country. `is_primary` is constrained to at most one
active primary per (Party, role) by a partial unique index.

No contact person, no geocoding, no delivery window, no route. Those are CRM and logistics.

### D10 — Drug licence is justified and stays one field

Two nullable columns: `drug_licence_number` and `drug_licence_valid_upto`.

Justification, since the brief asked for one:

1. Indian pharmacy purchase documentation must carry the supplier's drug licence number; it is
   printed on every wholesaler invoice.
2. It is intrinsic to the Party, not to a transaction, so recording it per purchase would duplicate
   it and let copies disagree.
3. Licence expiry is a real operational check a pharmacy performs on its suppliers.

It stays a single free-text field holding the licence string exactly as printed, which is commonly
a pair ("20B-1234 / 21B-5678"). Structured licence types, categories, or multi-licence rows would be
the overbuild the brief warned against, and nothing in this phase or the next reads the field
programmatically.

### D11 — Phone and email live on the Party

Two nullable columns, `primary_phone` and `primary_email`. A contacts child table is exactly the CRM
surface that was excluded. Phone is normalised to an optional `+`, country digits, and subscriber
digits; email is lowercased and checked structurally.

### D12 — Duplicate candidates are advisory, never blocking

`POST /api/v1/parties/duplicate-candidates` mirrors the frozen Product behaviour: a score, reason
codes, an explanation, and no side effect. Signals and weights:

| Signal | Score | Why |
| --- | --- | --- |
| Normalised GSTIN match | 100 | Decisive — a GSTIN identifies one registration |
| Normalised PAN match | 60 | Same legal entity; may still be a legitimate second registration |
| Normalised display or legal name match | 25 | Common, and commonly a genuine duplicate |
| Normalised phone match | 20 | Strong in practice, weak in theory (shared landlines) |
| Normalised email match | 20 | As above |
| Same drug licence number | 40 | A licence belongs to one premises |

Creation is never blocked. Two pharmacies really can be called "Sharma Medicals".

### D13 — Uniqueness

- One active Party per normalised GSTIN: partial unique index
  `WHERE status = 'active' AND normalized_gstin IS NOT NULL`. SQLite treats NULLs as distinct, so
  the predicate is explicit rather than load-bearing.
- Display name is **not** unique.
- PAN is **not** unique — see D6.
- One active role of each kind per Party: partial unique index on `(party_id, role)`
  `WHERE status = 'active'`.
- At most one active primary address per (Party, role): partial unique index
  `WHERE status = 'active' AND is_primary = 1`.

### D14 — Archiving a Party is refused while it has active children

This follows `lifecycle_product`, which counts active packs, company roles, and composition
components and returns `archived_conflict` rather than cascading. Archiving a Party is likewise
refused while it holds an active role or an active address. Cascading would archive rows the
operator never saw and would make restore ambiguous about what to bring back, so the operator
archives the children deliberately first.

## Schema — migration `0008_party_identity.sql`

`master_change_events` is extended by the frozen rename/recreate/copy/drop pattern used by `0005`
and `0006`, adding entity types `state_code`, `party`, `party_role`, and `party_address`.

```
state_codes(id, revision, status, jurisdiction, state_code, display_name,
            created_at_utc, updated_at_utc, archived_at_utc, archive_reason)
  UNIQUE (jurisdiction, state_code)

parties(id, revision, status,
        display_name, legal_name, normalized_search_name,
        gst_registration_status, gstin, normalized_gstin,
        pan, normalized_pan,
        place_of_supply_state_id -> state_codes(id),
        primary_phone, primary_email,
        drug_licence_number, drug_licence_valid_upto,
        created_at_utc, updated_at_utc, archived_at_utc, archive_reason)

party_roles(id, revision, status, party_id -> parties(id), role,
            created_at_utc, updated_at_utc, archived_at_utc, archive_reason)

party_addresses(id, revision, status, party_id -> parties(id), address_role,
                line1, line2, city, state_id -> state_codes(id), postal_code, country_code,
                is_primary,
                created_at_utc, updated_at_utc, archived_at_utc, archive_reason)
```

Every table is `STRICT`, carries the frozen archive coherence `CHECK`, and uses the frozen UUIDv7
identifier `CHECK`.

Triggers, following `product_composition_components_integrity_*`:

- `parties_integrity_insert` / `parties_integrity_update` — the referenced State must be active, and
  when a GSTIN is present its first two characters must equal that State's code.
  `RAISE(ABORT, 'party_conflict')`.
- `party_roles_integrity_insert` / `party_roles_integrity_update` — the Party must be active.
  `RAISE(ABORT, 'party_role_conflict')`.
- `party_addresses_integrity_insert` / `party_addresses_integrity_update` — the Party must be
  active, and the referenced State, when present, must be active.
  `RAISE(ABORT, 'party_address_conflict')`.

## API surface

All reads require an authenticated session; all mutations require `owner_admin` and pass the frozen
Host/Origin mutation check.

```
GET    /api/v1/parties                            search, status, role filters
POST   /api/v1/parties                            aggregate create: party + roles + addresses
POST   /api/v1/parties/duplicate-candidates       advisory, no side effect
GET    /api/v1/parties/{id}                       detail with roles and addresses
PUT    /api/v1/parties/{id}                       expectedRevision
POST   /api/v1/parties/{id}/archive               expectedRevision + reason
POST   /api/v1/parties/{id}/restore               expectedRevision + reason
POST   /api/v1/parties/{id}/roles
PUT    /api/v1/party-roles/{id}
POST   /api/v1/party-roles/{id}/archive
POST   /api/v1/party-roles/{id}/restore
POST   /api/v1/parties/{id}/addresses
PUT    /api/v1/party-addresses/{id}
POST   /api/v1/party-addresses/{id}/archive
POST   /api/v1/party-addresses/{id}/restore
```

New typed error codes: `party_conflict`, `party_role_conflict`, `party_address_conflict`. Existing
`duplicate_conflict`, `revision_conflict`, `archived_conflict`, `not_found`, `validation_failed`,
`authorization_denied`, `service_busy` keep their frozen meanings. No raw database text ever reaches
a client.

## UI

A `Parties` section under MASTERS: list with search and status filter, an aggregate create form that
shows duplicate candidates before creating, and a detail page managing roles and addresses.
Read-only roles see the data and no mutation controls, matching Inventory and Products.

The State reference master gains a section in Reference Data using the existing generic editor.

## Test plan

**Rust** — GSTIN checksum accepts real GSTINs and rejects mistyped ones; GSTIN/State mismatch is
refused by the trigger; PAN/GSTIN cross-check; `registered` without a GSTIN is refused;
`unregistered` with a GSTIN is refused; duplicate active GSTIN conflicts; duplicate PAN is allowed;
revision conflict; archive/restore round trip; archiving a Party with an active role is refused;
`customer` role rejected; role and address uniqueness; audit rows appended with the session actor;
authorisation denied for non-admin; duplicate candidates score and order.

**Frontend** — list, create with duplicate warning, detail, role and address management, revision
conflict recovery, read-only role, and narrow-viewport layout.

**Real-service gate** — a Party created over a real socket with a real session, its GSTIN rejected
when the checksum is wrong, and its detail read back.

## Freeze criteria

Full Rust suite, frontend suite, and end-to-end suite green; no dependency added; no frozen file
altered beyond the enumerated integration points; `parties` carries no amount column; the real
customer database untouched.
