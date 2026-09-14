# Phase 1G — GST-Aware Purchase Inward

## 1. Objective

The first real commercial transaction: a supplier's invoice recorded as a document that posts exact
GST and exact stock, atomically, and then cannot be quietly changed.

## 2. Scope

Purchase draft lifecycle, posting, GST resolution and snapshots, exact money, batch selection or
inline creation, inventory inward through the frozen ledger, and the Purchases UI.

## 3. Exclusions

Sales and POS, customer workflow, supplier payment, party balance, accounting journal, GST return
filing, GSTR-2B, ITC claim, reverse charge, purchase return, sales return, selling price, margin,
discount, free quantity or scheme, stock valuation, FIFO, weighted average, inter-store transfer,
expiry write-off, cloud, licensing, multi-counter. **The reversal endpoint and UI are also excluded**
— see §8.

## 4. Prerequisite foundations (all frozen and verified present)

| Foundation | Phase | What 1G uses |
| --- | --- | --- |
| Store place of supply, GSTIN, registration status | 1G-0 | the Store half of tax treatment |
| Party with supplier role, GSTIN, State | 1E | the supplier half |
| Product, Pack, `base_quantity_atoms` | 1B | quantity conversion |
| Product HSN + Tax Category | 1F | classification |
| `resolve_tax_rate`, half-open periods | 1F | rate on the invoice date |
| Batch identity, `prepare_batch` | 1C-C | batch validation, reused not copied |
| `inventory_movements`, `BEGIN IMMEDIATE`, idempotency | 1D | stock inward |
| Session actor, Host/Origin mutation check | 1C-A | authorization |

## 5. Rounding

Governed entirely by **ADR-016**. Nothing in this phase re-decides it.

## 6. Document lifecycle

```
DRAFT ──post──► POSTED
  │
  └── editable, revision-controlled, NO inventory effect
                     │
                     └── immutable commercial + tax snapshots, immutable inventory
```

There is no `CANCELLED` state in this phase, because nothing can yet produce one (§8).

## 7. Posted immutability

Enforced by **database triggers**, not service discipline. `purchase_documents` and `purchase_lines`
each carry `BEFORE UPDATE` and `BEFORE DELETE` triggers that abort any change to a row whose status
is `posted`, and any deletion of one. A draft remains freely editable; the instant it posts, the row
is frozen at the storage layer. This follows the frozen precedent of
`inventory_movements_no_update` / `_no_delete`.

The one permitted transition is `draft → posted` itself, which the update trigger allows by
inspecting `OLD.status`.

## 8. Reversal architecture — designed, deliberately not implemented

A future reversal will:

- reference the original posted document by id;
- create a **new** document of a reversing kind, never mutate the original;
- post compensating `inventory_movements` using the frozen `reverses_movement_id` link;
- negate the stored amounts rather than recomputing them, so a reversal is exactly equal and
  opposite even if a rate version changed in between (ADR-016 §10);
- preserve every original snapshot and audit row.

Phase 1G implements **none** of it. What Phase 1G guarantees is that a posted mistake cannot be
silently edited or deleted — it can only be corrected by a future compensating document. The schema
reserves nothing speculative for this; the link is expressible with the columns that already exist.

## 9. Supplier snapshots

The line references `supplier_party_id` for identity, and the header **snapshots** the facts that
must not change when the Party master later does:

`supplier_display_name`, `supplier_legal_name`, `supplier_gst_registration_status`,
`supplier_normalized_gstin`, `supplier_place_of_supply_state_id`, and the State's `state_code`.

The State **code** is snapshotted alongside the id because the code is what the treatment decision
actually compared; if a State master were ever re-coded, the posted document must still show what
was compared.

Supplier address is **not** snapshotted: a purchase inward records the supplier's invoice, and this
phase prints nothing.

## 10. Store snapshots

Symmetrically: `store_gst_registration_status`, `store_normalized_gstin`,
`store_place_of_supply_state_id`, `store_state_code`. A later Store Profile edit must not rewrite
the treatment of a document already posted.

## 11. Supplier invoice number

`supplier_invoice_number` stores exactly what was entered. `normalized_supplier_invoice_number`
stores the comparison value: **whitespace removed and uppercased, nothing else.**

Punctuation is deliberately preserved. `INV/2026/001` and `INV-2026-001` are different invoice
numbers on paper and may be different documents; stripping separators would silently merge them. The
1C-C batch-number precedent normalises the same conservative way.

**Uniqueness: one document per (supplier, normalized invoice number)**, drafts included — catching
the duplicate while it is still a draft is the whole point. Enforced by a plain unique index, not a
partial one: there is no `cancelled` status in this phase to exclude, and inventing a predicate for
a state that cannot occur would be dead schema.

Financial-year scoping is **rejected**. It would be invented dependence: a supplier does not reuse
an invoice number across years in practice, and adding a year term would silently permit a genuine
duplicate that happens to straddle April. If a supplier ever does reuse one, that is a conversation
with the supplier, not a schema accommodation.

## 12. Header model — `purchase_documents`

`id`, `store_id`, `supplier_party_id`, `supplier_invoice_number`,
`normalized_supplier_invoice_number`, `invoice_date`, `status`, `revision`, the supplier snapshots
(§9), the store snapshots (§10), `tax_treatment` (`intra_state` | `inter_state`, set at posting),
the five paise totals, `created_by_user_id`, `created_at_utc`, `updated_at_utc`,
`posted_by_user_id`, `posted_at_utc`, `posting_idempotency_key`, `posting_fingerprint`.

No balance, no payment state, no accounting state.

## 13. Line model — `purchase_lines`

`id`, `purchase_document_id`, `line_number`, `product_id`, `product_pack_id`, `batch_id`,
`quantity_packs`, `rate_per_pack_paise`, `quantity_atoms`, `taxable_value_paise`, `hsn_code_id`,
`hsn_code` (snapshot), `tax_category_id`, `tax_treatment_kind` (snapshot of the category's
`tax_treatment`), `tax_rate_version_id`, the four basis-point snapshots, the four paise components,
and `line_total_paise`.

## 14–16. Product / Pack integrity, quantity, and rate basis

The line names a Product, a Pack, and optionally a Batch. The server verifies Pack belongs to
Product and Batch belongs to Pack; the frozen composite foreign keys enforce it independently.

```
quantity_packs      integer count of the selected Pack   (client supplies)
rate_per_pack_paise integer paise for ONE such Pack      (client supplies)
quantity_atoms      = quantity_packs × pack.base_quantity_atoms   (server derives)
taxable_value_paise = quantity_packs × rate_per_pack_paise        (server derives)
```

The client never supplies atoms or taxable value. The UI labels the rate with the selected Pack, so
the basis is stated, never implied.

## 17. Batch — draft holds intent, posting creates

A draft line stores **proposed** batch facts (`new_batch_number`, `new_batch_expires_on`,
`new_batch_mrp_paise`) or an existing `batch_id`. The Batch master row is created **during posting,
inside the posting transaction**.

This is the choice that prevents orphans: an abandoned draft leaves no Batch behind, because none
was ever created. Validation calls the frozen `prepare_batch` — made `pub(crate)`, the smallest
reuse boundary — so batch number normalisation, expiry ordering, and MRP bounds are not written
twice.

## 18–19. Classification and rate resolution

Each line resolves, at posting, from the Product's Tax Category and the **invoice date**, through
the frozen `resolve_tax_rate`. HSN is read independently; no HSN→rate mapping is invented.

## 20. Tax treatment

```
store_state_code == supplier_state_code  ->  intra_state
otherwise                                ->  inter_state
```

Compared server-side from persisted State identity. Intra-state charges CGST + SGST and **must not**
charge IGST; inter-state charges IGST and **must not** charge CGST or SGST. Cess applies under
either, as the rate version specifies.

## 21. Treatments and missing facts — never conflated

| Tax category treatment | Behaviour |
| --- | --- |
| `taxable` | resolve a rate version; **fail** if none applies on the invoice date |
| `exempt`, `nil_rated`, `non_gst` | zero tax, **asserted by the classification itself**; no rate version required |

Missing facts are **errors, never zero**:

| Missing | Error |
| --- | --- |
| Store place of supply | `store_tax_profile_incomplete` |
| Supplier place of supply | `supplier_tax_profile_incomplete` |
| Product Tax Category | `product_tax_classification_incomplete` |
| No rate version on the invoice date (taxable only) | `tax_rate_not_found` |

## 22. Registration status

Posting requires a **place of supply** on both sides; it does not require either side to be
GST-registered. An unregistered supplier is an ordinary commercial reality and its purchase is
recorded with the resolved treatment and rate. Registration status is snapshotted for the future
reverse-charge and ITC work that this phase excludes, and is not used to alter the computation here
— inventing reverse-charge behaviour would be guessing at tax law this phase has no mandate to
implement.

## 23–24. Money and rounding

Entirely ADR-016, implemented once in `domain::money` and called from nowhere else.

## 25–26. Inventory movement and provenance

Posting writes one `inventory_movements` row per line, `movement_type = 'purchase'`, with
`quantity_delta_atoms = +quantity_atoms` and `occurred_on = invoice_date`.

Provenance is a real foreign key: `inventory_movements.purchase_line_id` references
`purchase_lines(id)`. The ledger can therefore answer *which purchase and which line this inward came
from* without string parsing. `'adjustment'` is never reused to disguise a purchase.

No mutable stock column is added; balance stays derived.

## 27. Atomic posting

One `BEGIN IMMEDIATE` transaction, matching the frozen inventory concurrency model, covering:
draft lock and revision check, idempotency check, Store load, Supplier load and role check,
snapshots, per-line Product/Pack/Batch validation, classification and rate resolution, money
computation, Batch creation, movement insertion, totals, `posted` transition, and audit. Any failure
rolls the whole thing back: no partial document, no partial stock, no orphan Batch.

## 28. Idempotency

`posting_idempotency_key` is UUIDv7 and unique. A replay carrying the same key returns the original
posted document when the **posting fingerprint** matches, and `idempotency_conflict` when it does
not. The fingerprint is a SHA-256 over the semantic payload only — document id, supplier, invoice
number and date, and each line's product, pack, batch intent, quantity and rate — never presentation
fields.

## 29. Revision

Drafts use `expectedRevision` and bump on every write. A posted document stops behaving like
revisioned master data: the trigger refuses further change regardless of revision.

## 30. Audit

`master_change_events` gains `purchase_document`, following every previous phase. Events: `created`
on draft creation, `updated` on draft change, and `posted` on posting — which requires extending the
frozen `action` enumeration, done in the same recreate.

## 31. Authorization

Read: any authenticated session. Draft mutation and posting: `owner_admin`, behind the frozen
Host/Origin check.

## 32. API

```
GET    /api/v1/purchases                    list
POST   /api/v1/purchases                    create draft
GET    /api/v1/purchases/{id}               detail with lines
PUT    /api/v1/purchases/{id}               update draft header
POST   /api/v1/purchases/{id}/lines         add draft line
PUT    /api/v1/purchase-lines/{id}          update draft line
DELETE /api/v1/purchase-lines/{id}          remove draft line
POST   /api/v1/purchases/{id}/post          post  { expectedRevision, idempotencyKey }
```

Posting is a named command, never `PUT status = "posted"`.

## 33. UI

A **Purchases** area: list, draft editor, posted detail. At 390 px the line table reflows to stacked
cards rather than forcing a horizontal page scroll.

## 34. Errors

`purchase_not_found`, `purchase_not_draft`, `revision_conflict`, `duplicate_supplier_invoice`,
`supplier_not_eligible`, `store_tax_profile_incomplete`, `supplier_tax_profile_incomplete`,
`product_tax_classification_incomplete`, `tax_rate_not_found`, `product_pack_mismatch`,
`batch_pack_mismatch`, `batch_conflict`, `invalid_quantity`, `invalid_purchase_rate`,
`arithmetic_overflow`, `idempotency_conflict`, `posting_conflict`, plus the frozen auth and
`service_busy` codes.

**Every one gets a frontend safe message, proven by a test that reads the code list from the
contract and asserts each maps to something other than the generic fallback** — the 1E/1G-0 defect
must not recur.

## 35. Accessibility

Labelled controls, `aria-invalid` + `aria-describedby`, dialog focus trap and restore, `role="alert"`
for failures, `role="status"` for progress, keyboard-complete line entry, first-invalid focus.

## 36. Migration 0011

Creates `purchase_documents` and `purchase_lines`; rebuilds `inventory_movements` to extend
`movement_type` with `'purchase'` and add the `purchase_line_id` foreign key; extends
`master_change_events` with the `purchase_document` entity type and the `posted` action; adds the
posted-immutability triggers.

> **Integrity rule for this phase.** `sqlx::migrate!` embeds migrations at **compile time**. After
> creating or changing `0011`, force a rebuild before trusting any migration-dependent test — a
> stale test binary silently exercises the old schema and produces a false PASS. This was observed
> during design: a probe that deliberately dropped the append-only triggers appeared to pass until
> the binary was rebuilt, after which it correctly failed. **Never accept a migration result from a
> stale binary.**
>
> The rebuild of `inventory_movements` must preserve exactly: every column, the primary key, all
> foreign keys including the self-referencing `reverses_movement_id`, the unique constraints, all
> four indexes, all three triggers, and every existing row.

## 37–38. Real-service gate and browser preview

Both mandatory, both over real HTTP / real built frontend against disposable SQLite. The gate proves
the whole workflow including an idempotent replay that does not double stock. The preview includes a
**ten-line pharmacy usability assessment**.

## 39. Accounting compatibility

Posted documents carry the supplier reference, the tax component breakdown, the rate version, and
the treatment — everything a future payable or ITC workflow needs — without this phase creating any
balance, ledger, or payment.

## 40. Freeze acceptance matrix

| Criterion | Check |
| --- | --- |
| Posted header and lines immutable | database trigger + anti-vacuous proof |
| Browser cannot set tax, totals, or atoms | spoof tests |
| Rounding matches ADR-016 | every vector |
| Totals = sum of rounded lines | multi-line vector |
| Intra vs inter exclusive | treatment tests |
| Missing facts error, never zero | four error tests |
| Atomic rollback, no orphan Batch | forced-failure test |
| Idempotent replay does not double stock | gate + unit |
| Provenance FK present | schema + gate |
| No mutable stock column, no float | schema + grep |
| Migrations 0001–0010 byte-identical | `git diff` |
| Every error code has a safe message | mapping test |
| 10-line entry usable | browser preview |
| Phases 0–1G-0 regression green | full suites |
