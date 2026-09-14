# Phase 1F — Product Tax Classification Foundation

## 1. Objective

Connect Product identity to the already-frozen Indian tax reference masters, so a future Purchase or
Sale document can resolve a Product's tax treatment from authoritative, effective-dated data instead
of embedding temporary tax metadata on the Product.

Phase 1F establishes:

```
Product ──► HSN classification        (hsn_codes)
Product ──► Tax Category              (tax_categories)
                  │
                  └──► Tax Rate Versions, resolved by date
```

## 2. Scope

- Two nullable classification references on `products`.
- A focused, authenticated classification endpoint with optimistic concurrency and audit.
- A reusable domain resolver: Tax Category + date → applicable Tax Rate Version.
- A Product Detail "Tax Classification" section with real searchable reference selectors.
- Migration, contracts, tests, and a real-service gate extension.

## 3. Exclusions

Purchase bill or invoice posting, inventory movement from purchase, supplier payable, accounting
journal, sales or POS, tax invoice generation, CGST/SGST/IGST *document* calculation, GST return
filing, ITC, reverse charge, stock valuation, selling price, margin, discount or scheme engine,
Customer workflow, cloud, licensing, LAN multi-counter.

## 4. Existing tax-master analysis

Read from the repository, not assumed:

| Table | Fields that matter |
| --- | --- |
| `hsn_codes` | `jurisdiction`, `hsn_code`, `description`; `UNIQUE (jurisdiction, hsn_code)` |
| `tax_categories` | `jurisdiction`, `category_code`, `display_name`, `tax_treatment IN ('taxable','exempt','nil_rated','non_gst')` |
| `tax_rate_versions` | `tax_category_id`, `effective_from`, `effective_to`, and four integer components: `cgst_basis_points`, `sgst_basis_points`, `igst_basis_points`, `cess_basis_points`, each `NOT NULL` and `BETWEEN 0 AND 10000` |

Frozen semantics confirmed in source:

- **Basis points, integers only.** `100 = 1.00%`. A frozen test asserts `typeof(cgst_basis_points) = 'integer'` and rejects `2.5`.
- **Half-open periods.** The migration comment states `[effective_from, effective_to)`; `effective_to IS NULL` is open-ended; `CHECK (effective_to IS NULL OR effective_to > effective_from)`.
- **No overlap** among *active* versions of one category, enforced by
  `tax_rate_versions_no_overlap_insert` / `_update`.
- **Immutable versions.** `reference_masters::update` rejects `TaxRateVersion` outright: archive and
  create a new effective version.

**Sufficiency for Phase 1G: yes.** All four GST components are present, exact, and effective-dated.
No redesign of the frozen tax model is required or attempted by this phase.

**HSN and Tax Category are unrelated in the schema.** There is no foreign key, join table, or any
other encoded relationship between `hsn_codes` and `tax_categories`. Phase 1F **preserves that
fact**: it does not invent a universal HSN→rate mapping, and it does not derive one from the other.
An HSN code classifies goods; a Tax Category carries the effective-dated rates. They are selected
independently.

## 5. Product → HSN model

Zero-or-one, nullable, `REFERENCES hsn_codes(id) ON DELETE RESTRICT`.

- **Not mandatory for any `product_kind` in this phase.** The frozen schema does show that a
  kind-conditional requirement is expressible — `CHECK (product_kind <> 'medicine' OR dosage_form_id
  IS NOT NULL)` — but applying one now would invalidate every Product that already exists. The
  requirement belongs where it is actually needed: at GST-aware transaction posting in Phase 1G.
- **Classification may change.** Reclassification is a real event; the column is updatable.
- **History** is carried by the append-only `master_change_events` stream (old and new values, actor,
  timestamp) and, in future, by posted document lines that snapshot the tax facts they used.
- **An archived HSN stays readable** on a Product that already references it; it is not selectable
  for a new assignment.

## 6. Product → Tax Category model

Identical shape: zero-or-one, nullable, `REFERENCES tax_categories(id) ON DELETE RESTRICT`, optional
at this phase, changeable, archived references readable but not newly assignable.

**No tax rate is copied onto the Product.** The Product identifies its category; the rate is always
resolved from `tax_rate_versions` by date.

## 7. Storage-model decision

**Option A — two nullable columns on `products`. Chosen.**

**Option B — a dedicated `product_tax_classifications` history entity. Rejected.**

Reasoning:

1. **Point-in-time truth belongs to the transaction, not the master.** A posted Purchase or Sale line
   will snapshot the category, the resolved rate version, and its basis points. That records what
   was *actually applied*, which is what an audit or a return needs — strictly better than
   reconstructing what the Product was *configured* as on some date.
2. **Change history already exists.** `master_change_events` is append-only and database-enforced
   (`no_update` / `no_delete` triggers). A classification change writes an event carrying both the
   previous and the next classification, which answers which Product, from what, to what, by whom,
   and when.
3. **A history table would add a second concurrency and lifecycle surface** — its own revision, its
   own archive semantics, and a "which row is current" resolution rule — for no gain the two points
   above do not already provide, and with a real risk of the lifecycle deadlock Stage 16 warns about.
4. **Query ergonomics.** Every Product read already selects a fixed column list; two more columns
   cost nothing, while a child entity would put a join on the catalog's hottest path.

The decision is recorded in ADR-015.

## 8. Historical-tax strategy

- The Product **never** stores a rate, a percentage, or a basis-point value. No such column is
  created, and a test asserts none exists.
- The Product stores only *which* Tax Category applies.
- The rate for a given date is resolved from `tax_rate_versions` at read time.
- Phase 1G snapshots the resolved figures onto the posted line, after which the document is
  self-describing and immune to later rate changes.

## 9. Tax Rate Version resolution

New domain function, the single place effective-date logic lives:

```
resolve_tax_rate(executor, tax_category_id, on_date) -> Option<ResolvedTaxRate>
```

```sql
SELECT id, effective_from, effective_to,
       cgst_basis_points, sgst_basis_points, igst_basis_points, cess_basis_points
FROM tax_rate_versions
WHERE tax_category_id = ?1
  AND status = 'active'
  AND effective_from <= ?2
  AND (effective_to IS NULL OR effective_to > ?2)
```

- Half-open `[from, to)` exactly as the frozen comment and the no-overlap triggers define it. On the
  day equal to `effective_to`, the *next* version applies, or none.
- Only `status = 'active'` versions resolve; an archived version is history, not a current rate.
- At most one row can match, because the frozen triggers forbid overlapping active periods.
- Returns exact integers. No floating point anywhere in the path.
- The Tax Category's own `status` does **not** filter resolution: a Product that already references a
  since-archived category must still resolve its rate for historical display. Assignment is what is
  restricted, not reading.

Phase 1G calls this function rather than re-implementing date logic.

## 10. Geography boundary

The resolver returns **all four components** and deliberately does **not** choose between
`CGST + SGST` and `IGST`. That choice depends on transaction context — supplier or customer
registration and place of supply — which Phase 1E now supplies via Party and State, and which
Phase 1G will combine with the Product's Tax Category, the document date, and this resolver.

No transaction geography is stored on the Product. A test asserts the resolver exposes components
without selecting a treatment.

## 11. Completeness rules

- A Product may exist with no classification; every existing Product continues to read and write
  normally. The migration is purely additive and nullable.
- New Product creation does **not** require classification. The requirement begins at GST-aware
  transaction posting in Phase 1G, which is the first point where an unclassified Product actually
  causes harm. Enforcing it earlier would block ordinary catalog work for a document that does not
  yet exist.
- The UI surfaces an honest **incomplete** state, clearly distinguished from a load error.

## 12. Lifecycle

- Assigning an **archived** HSN or Tax Category is refused — in the service for a precise message,
  and by a database trigger so the guarantee does not depend on service discipline.
- A Product that already references a since-archived master keeps displaying it, resolved to its
  real label rather than a raw id.
- Classification is **intrinsic Product metadata**, not a child row. It therefore does **not** block
  Product archive, and it archives and restores with the Product.
- The integrity trigger is `BEFORE UPDATE OF hsn_code_id, tax_category_id`, so it fires only when
  those columns are actually in the SET clause. Ordinary Product edits, archive, and restore never
  name them and can never be deadlocked by an archived reference.
- **The trigger validates only a reference that actually changes.** The rule is "a newly assigned
  reference must be active", not "every non-NULL reference must be active".

  This distinction is load-bearing. The classification endpoint writes both columns on every call,
  so a rule that re-validated unchanged values would trap a Product holding a since-archived HSN:
  clearing only its Tax Category would re-check the untouched archived HSN and be refused, leaving
  the Product permanently uneditable. Comparing against `OLD` removes the trap and is exactly the
  documented semantic — an archived reference stays where it was assigned, but cannot be newly
  chosen.
- Clearing a classification to NULL is therefore always permitted, including when the current
  reference has since been archived.

## 13. Revision

Classification lives on `products`, so it uses the **Product's own revision**. The endpoint requires
`expectedRevision` and bumps `products.revision`. No second concurrency surface is introduced, and a
stale write is refused with `revision_conflict` carrying both revisions.

## 14. Audit

One `master_change_events` row per change, `entity_type = 'product'`, `action = 'updated'`.

`'product'` is already in the frozen entity-type enumeration, so **no `master_change_events`
rename/recreate migration is needed** — a meaningful simplification over Phases 1C-B, 1C-C, and 1E.

The payload carries the previous and next `hsnCodeId` and `taxCategoryId`, plus a marker that this
was a tax-classification change. The actor is the authenticated server session user. A
browser-supplied actor is never trusted.

## 15. Authorization

- Read: any authenticated session.
- Mutate: `owner_admin` only, behind the frozen Host/Origin mutation check.

## 16. API

```
GET /api/v1/products/{id}/tax-classification[?asOf=YYYY-MM-DD]
PUT /api/v1/products/{id}/tax-classification
```

`GET` returns the assigned `hsnCodeId` and `taxCategoryId`, a `complete` flag, the `asOf` date
actually used (defaulting to the service's current date), and `applicableRate` — the resolved
version's id, period, and four integer components, or `null` when nothing applies.

`PUT` takes `expectedRevision`, `hsnCodeId`, `taxCategoryId` (either may be null to clear), and an
optional `reason`.

`hsnCodeId` and `taxCategoryId` are also added to the existing Product list and detail responses,
since they are now Product columns.

Typed errors only, reusing frozen codes where they exist: `validation_failed`, `not_found`,
`archived_conflict`, `revision_conflict`, `authorization_denied`, `authentication_required`,
`service_busy`, `internal_error`. No raw SQL or Rust detail reaches a client.

## 17. Product UI

A **Tax Classification** section on Product Detail showing HSN, Tax Category, and — only when it can
be stated truthfully — the rate applicable on a given date.

The rate is labelled as **effective-date-derived**, with the date shown, and is never presented as
Product metadata. There is no rate selector: a rate belongs to the Tax Category's version history,
not to the Product.

Owner/Admin edits through a focused dialog. Pharmacist and Cashier see the same data read-only.

Required states: **loading**, **incomplete**, **error**, **success** — and an error must never
render as "not classified".

## 18. Product create / edit UX

**Option A: Product identity creation stays unchanged; classification is managed in Product Detail.**

Enlarging the create aggregate would add branches to the duplicate-candidate flow and the atomic
create transaction for no workflow gain, given classification is optional at this phase and Phase 1G
enforces completeness at posting.

## 19. Reference selectors

Real, searchable `hsn-codes` and `tax-categories` reference APIs — no hardcoded lists. Selectors
offer only active records. A historically assigned archived reference is still resolved to its
label for display, never left as a raw UUID. Loading, error with retry, and no-results states are all
handled.

## 20. Error / state model

Load failure, empty classification, and a refused mutation are three distinct, separately rendered
states. Retry is offered where a retry can help. Backend messages are mapped through the frozen
`localService` message table; a test asserts no raw backend text is displayed.

## 21. Accessibility

Labelled controls, `aria-invalid` plus `aria-describedby` on errors, dialog focus trap and restore
via the existing `CatalogDialog`, `role="alert"` for failures, `role="status"` for loading, and a
table that scrolls inside its own container at 390 px without the page scrolling horizontally.

## 22. Migration — `0009_product_tax_classification.sql`

Additive only. Migrations `0001`–`0008` remain byte-identical.

```sql
ALTER TABLE products ADD COLUMN hsn_code_id TEXT REFERENCES hsn_codes(id) ON DELETE RESTRICT;
ALTER TABLE products ADD COLUMN tax_category_id TEXT REFERENCES tax_categories(id) ON DELETE RESTRICT;

CREATE INDEX products_tax_category_idx ON products(tax_category_id);

CREATE TRIGGER products_tax_classification_insert BEFORE INSERT ON products ...
CREATE TRIGGER products_tax_classification_update
BEFORE UPDATE OF hsn_code_id, tax_category_id ON products ...
```

Each trigger raises `RAISE(ABORT, 'product_tax_conflict')` when a non-NULL reference is not active.
No Product rebuild, no `master_change_events` recreation, no rate column, and no NOT NULL added to an
existing table.

## 23. Duplicate candidates

**Unchanged.** An HSN code is a broad goods classification that thousands of unrelated products
legitimately share, so it would add noise rather than signal, and a Tax Category even more so.
Neither establishes business identity, and neither implies therapeutic or generic equivalence.

## 24. Tests

**Domain resolver** — before the first version; exactly `effective_from`; inside an interval;
exactly `effective_to` (the next version applies, per half-open semantics); after a closed final
version; inside an open-ended final version; archived version excluded; archived category still
resolves; malformed date rejected; components returned as exact integers; no treatment chosen.

**Backend** — unclassified Product reads normally; assign HSN alone, Tax Category alone, both;
change; clear; archived HSN refused; archived Tax Category refused; the database refuses each
independently of the service; historically assigned archived reference still readable; revision
conflict; Product revision bumped; audit row with previous and next values and the session actor;
authentication required; `owner_admin` required; actor spoof ignored; no rate column on `products`;
Product archive not blocked by classification; ordinary Product edit unaffected by an archived
assigned reference.

**Frontend** — incomplete state; populated state; error distinct from incomplete with retry; HSN and
Tax Category selectors; save; clear; revision conflict and reload; archived reference label;
read-only role; 390 px layout; logout cache isolation.

## 25. Real-service integration

Extend the existing gate over real HTTP against real migrated SQLite: setup, create a Product,
create HSN / Tax Category / a dated rate version, assign the classification, read it back, confirm
the resolver returns the right version for dates either side of a boundary, and confirm the database
itself refuses an archived reference. The harness is not weakened and never touches the customer
database.

## 26. Security

Loopback only. Mutations are `owner_admin` behind the Host/Origin check. The audit actor comes from
the validated server session. No new dependency. No new configuration surface.

## 27. Phase 1G compatibility

1G obtains: a Product's Tax Category, an authoritative date-based resolver returning four exact
integer components, and Party/State context from 1E. It combines them to choose intra- versus
inter-state treatment and snapshots the result onto the posted line. Nothing in 1F constrains that
choice or pre-empts it.

## 28. Non-goals

No HSN→rate mapping. No permanent Product rate. No transaction geography on the Product. No history
entity. No change to the frozen tax masters. No Purchase, Sale, GST return, or accounting surface.

## 29. Freeze acceptance matrix

| Criterion | Check |
| --- | --- |
| No rate/percentage column on `products` | schema assertion, unit + gate |
| No floating point in the tax path | `f32`/`f64` grep across the service |
| Half-open `[from, to)` honoured | resolver boundary tests |
| Archived reference not assignable | service test + independent database test |
| Archived reference still readable | test |
| No lifecycle deadlock | Product edit/archive/restore with an archived assigned reference |
| Product revision + `expectedRevision` | revision conflict test |
| Audit carries previous, next, actor | test |
| Migrations 0001–0008 byte-identical | `git diff` |
| No dependency change | manifest/lockfile diff |
| Phases 0–1E regression green | full suites |
| No Purchase/Sale/GST-return/accounting surface | scope grep |
