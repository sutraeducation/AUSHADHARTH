# Phase 1C-C — Batch, Expiry, and Batch MRP Foundation

## 1. Objective

Give a Product Pack the identity of the real manufactured lots it is supplied in, so a pharmacy can record a batch number, its manufacturing and expiry dates, and the MRP printed on that lot. This is **identity and commercial metadata only**. It is not stock, and it does not move inventory.

## 2. Scope

- `product_batches` — a manufactured lot of one Product Pack, with batch number, optional manufacturing and expiry dates, and optional MRP in integer paise.
- Authenticated reads; Owner/Admin mutations; server-session audit actor; optimistic revisions; archive-not-delete.
- A **Batches** workspace inside the existing Pack management area, beside Store Policy and Barcodes.

## 3. Explicit exclusions

Not implemented, not partially started, not stubbed: live stock, stock ledger, opening-stock posting, stock valuation, purchase, GRN, purchase return, sales/POS, sales return, customers, suppliers, accounting, GST returns, tax engine, discounts, schemes, generic substitution, clinical equivalence, prescription validation, drug interactions, dosage guidance, cloud sync, licensing, LAN multi-counter.

Deliberately deferred within the batch domain itself:

- **Purchase rate and selling price.** Only the manufacturer-printed **MRP** is recorded. Cost, margin, landed cost, and price lists belong to the commercial phases.
- **Near-expiry threshold as business configuration.** See §9.
- **Batch barcodes and serial numbers.** See §14.
- **Opening stock.** Explicitly deferred to the inventory-ledger phase; recording a Batch must never be mistaken for receiving stock.

## 4. Domain terminology

| Term | Meaning |
| --- | --- |
| Batch / Lot | One manufactured lot of one Product Pack, as labelled by the manufacturer. |
| Batch number | The manufacturer's lot string, exactly as printed. |
| Manufacturing date | Calendar date the lot was manufactured. Optional. |
| Expiry date | Calendar date the lot expires. Optional. |
| Batch MRP | Maximum Retail Price printed on that lot, in integer paise. |
| Expiry state | A **derived** presentation value, never a stored column. |

## 5. Identity hierarchy

```
Brand → Product → Product Pack / SKU → Batch
                     └── Store Pack Policy
                     └── Barcodes
Product → Composition            (Phase 1C-B, unchanged)
```

**Batch attaches to Product Pack**, never directly to Product and never bypassing the Pack. A lot is always a lot *of a specific saleable presentation*. Composition stays on the Product and is never attached to, copied onto, or derived from a Batch.

## 6. Batch number uniqueness

Manufacturers reuse lot strings freely across different products, so batch numbers are **not globally unique** and must not be modelled as such.

The uniqueness boundary is **one active Batch per `(product_pack_id, normalized_batch_number)`**, expressed as a partial unique index `WHERE status = 'active'`, matching how Phase 1B scopes active SKUs and barcodes.

The same lot string on a *different* Pack is legitimate and permitted — including a different Pack of the same Product.

**Normalization and display.** Two columns are kept: `batch_number` preserves the operator's entered form for display, and `normalized_batch_number` is `upper(trim(...))` and is what uniqueness compares. This follows the frozen `sku_code` and `barcodes.normalized_value` convention — uppercase, trimmed, restricted to `A-Z 0-9 . _ / -`. Case and surrounding whitespace therefore never create a duplicate lot, while the printed form the operator saw is never lost.

## 7. Store scoping — Batch is installation-global

**This is the principal design decision of the phase and is flagged for freeze review.**

`sku_store_id` is store-scoped because a SKU is the *store's own* code. A batch number is the opposite: it is the **manufacturer's** code for a physical lot, and the facts attached to it — expiry date, manufacturing date, printed MRP — are intrinsic to that lot, not to whoever holds it. Copying SKU scoping here would be the wrong inference.

Therefore `product_batches` carries **no `store_id`**. One manufactured lot of one Pack is one Batch identity for the installation.

What *is* store-specific is **how much of that lot a store holds**, and that is stock, which this phase does not implement. The future inventory ledger will carry `(store_id, product_pack_id, batch_id, …)` on each movement row, so per-store balances derive naturally without ever duplicating batch identity. Recall and traceability also work correctly this way: one lot is one identity to trace.

## 8. Date semantics

`manufactured_on` and `expires_on` are **calendar-domain** values stored as `TEXT` constrained by `GLOB '????-??-??'`, exactly like the frozen `effective_from` / `effective_to` columns. They carry no time of day and no offset, so they are timezone-independent by construction and cannot drift with a browser locale or a server clock zone.

Shape alone is not validity, so the API additionally parses each value with `time::Date` through the existing `domain::catalog::validate_date` helper, rejecting values such as `2027-02-30` that satisfy the GLOB.

Rules:

- Both dates are **optional**. A non-expiring general pharmacy item legitimately has no expiry, and manufacturing date is frequently unknown at data-entry time.
- When both exist, `expires_on >= manufactured_on`, enforced by a database CHECK and by the API.
- **An already-expired batch is accepted.** Historical lots must be recordable; refusing them would make the catalog unable to describe reality.
- The browser never supplies authoritative dates; it submits the operator's chosen calendar date as a plain `YYYY-MM-DD` string.

## 9. Expiry semantics — derived, never stored

No `is_expired`, `expiry_status`, or similar mutable flag exists. Such a column would be wrong the day after it was written. Expiry state is derived wherever it is displayed, by comparing `expires_on` to the current calendar date as `YYYY-MM-DD` strings — a comparison that is itself timezone-independent.

Four presentation states:

| State | Condition |
| --- | --- |
| No expiry recorded | `expires_on` is null |
| Expired | `expires_on` < today |
| Expiring soon | today ≤ `expires_on` ≤ today + threshold |
| Valid | otherwise |

The **near-expiry threshold is a display aid, not clinical or legal truth**. It lives as a single named UI constant with no database, API, or business-rule authority, and making it configurable is explicitly deferred. Nothing in the Store Service consumes it, and no decision is taken on its basis.

## 10. MRP representation

MRP is money, so ADR-009 governs it: **never binary floating point**. The column is `mrp_paise INTEGER`, the integer minor unit of the rupee, matching the `*_basis_points` integer-scaling precedent already frozen in Phase 1A.

- Nullable, because a lot may be recorded before its MRP is known.
- `CHECK (mrp_paise IS NULL OR (mrp_paise > 0 AND mrp_paise <= 100000000000))`.
- The contract type is `z.number().int().positive().nullable()`; no `REAL`, `FLOAT`, or decimal string is authoritative anywhere.
- The UI accepts and displays **rupees**, converting to and from paise with exact string arithmetic — the same technique Phase 1B uses for quantity atoms and Phase 1C-B uses for strengths. `₹12.50` is `1250`, never `12.5`.
- Purchase rate and selling price are **not** added. Only the printed MRP of the lot.

## 11. Lifecycle, revision, and audit

Identical to every frozen catalog child entity.

- `revision INTEGER >= 1`; every mutation carries `expectedRevision`, and a mismatch returns `revision_conflict` carrying `expectedRevision`/`currentRevision`.
- `status IN ('active','archived')` with the paired archive CHECK; archive requires a reason, restore reruns integrity and uniqueness checks.
- Batch revisions are independent of Pack and Product revisions, matching `product_packs` and `product_composition_components`.
- Every mutation writes one `master_change_events` row with `entity_type = 'product_batch'`, added by repeating the Phase 1B/1C-B audit-stream rebuild precedent. `actor_id` comes **exclusively from the validated server session**; a browser-supplied actor is ignored.

**Archive means "no longer available for new transactions."** It does not assert that inventory disappeared and it never deletes the row. `ON DELETE RESTRICT` guarantees a Batch referenced by a future ledger can never be physically removed, so historical traceability survives.

## 12. Pack relationship and Product archive parity

`product_batches.product_pack_id` references `product_packs(id) ON DELETE RESTRICT`. A Batch may only be created against an **active** Pack of an **active** Product, enforced by a trigger mirroring `product_packs_integrity_*`.

Phase 1B refuses to archive a Product while active children exist, and Phase 1C-B extended that count to composition. Pack archival is the parallel case here: archiving a Pack while it still has active Batches would orphan live lots, so `archive_pack` gains the same guard. Product archive already transitively requires its Packs to be archived first, so no further change is needed there.

## 13. Future inventory compatibility

The schema is shaped so a future ledger references `store_id`, `product_pack_id`, and `batch_id` without any identity rebuild, and so purchase receipt, sale issue, sales return, purchase return, stock adjustment, damage, expiry write-off, transfer, and opening stock can all be added as movement rows later.

**No movement type, no balance, and no quantity column is implemented now.** The Batch row deliberately contains no `quantity_on_hand`, `current_stock`, `stock_balance`, `inward_quantity`, or `outward_quantity`. A mutable stock number on an identity row is the anti-pattern this phase exists to avoid; every future balance must be derived from the ledger.

## 14. Barcode boundary

Barcode identity remains **Pack-level and unchanged**. A batch number is not a barcode, and no Batch barcode, GTIN batch extension, or serial number is introduced. Product barcode, Pack barcode, batch number, and future serial number stay four distinct concepts. Introducing a batch-level barcode would require its own justified design decision and is out of scope here.

## 15. API contract

Following the frozen Pack-child conventions exactly (`/packs/{id}/barcodes`, `/barcodes/{id}/archive`):

```
GET    /api/v1/packs/{packId}/batches            → batches for the Pack, ordered
POST   /api/v1/packs/{packId}/batches            → create
GET    /api/v1/batches/{batchId}                 → one batch
PUT    /api/v1/batches/{batchId}                 → update (expectedRevision)
POST   /api/v1/batches/{batchId}/archive         → (expectedRevision, reason)
POST   /api/v1/batches/{batchId}/restore         → (expectedRevision, reason)
```

Ordering is `ORDER BY expires_on IS NULL, expires_on, normalized_batch_number` — soonest expiry first, undated lots last, then a deterministic tie-break. This is presentation ordering only and is **not** a FEFO issue policy; FEFO belongs to the sales phase.

Errors reuse the frozen `CatalogErrorResponse` envelope. A **duplicate active lot on a Pack maps to the existing `duplicate_conflict`**, exactly as a duplicate SKU does — inventing a parallel code for the same class of violation would be inconsistent. One new typed code, `batch_conflict`, is reserved for Pack/Product state violations raised by the integrity trigger. No raw SQLite, sqlx, or Rust text ever reaches the client.

## 16. Authorization

Unchanged from the frozen pattern and not extended. Reads require a valid local session and are available to `owner_admin`, `pharmacist`, and `cashier`. Every mutation requires `validate_mutation_request` (Host/Origin) plus an authenticated `owner_admin`. **No pharmacist stock permission is invented** — the permissions model has three roles and this phase adds none.

## 17. UI and UX

Batches appear inside the existing Pack management workspace, after Store Policy and Barcodes:

```
Product Detail → Packs & SKUs → [Manage] → Store Pack Policy | Barcodes
                                         → Batches
```

No separate batch application. The Batches table shows Batch number, Manufactured, Expiry, Expiry state, MRP, Status, Actions — and **no stock quantity column and no purchase rate column**.

The editor is a top-level portal dialog matching the frozen `CatalogDialog`: batch number, manufacturing date, expiry date, and MRP in rupees. A non-editable preview shows the exact paise value that will be stored, so the operator can see that `₹12.50` becomes `1250`.

## 18. Error and state model

Loading, empty, error, and success remain four distinct states for the batch list. A failed query is never rendered as "No batches recorded" and never as a permanent "Loading…"; it carries a safe message and its own retry that refetches the failed query. Mutation failures map through the frozen safe-message table. A revision conflict shows the stale-record message with a **real** Reload latest that invalidates the authoritative query, awaits the refetch, repopulates the editor, and leaves Cancel as a separate control. Archive and restore always require explicit intent and a reason.

## 19. Accessibility

Unchanged from the frozen model and verified by regression: document-level portal dialogs, unique `useId` title ids, `aria-modal`, one active dialog, Escape close, Tab and Shift+Tab wrap, exact launcher focus restoration, first-invalid-field focus, visible focus, labelled controls, semantic tables with per-cell `data-label`, no horizontal page overflow at 390 px, and honoured reduced motion.

## 20. Migration plan

One new migration, `0006_batch_identity.sql`. Migrations `0001`–`0005` remain byte-identical. It extends the audit enumeration with `product_batch` via the established rebuild precedent, then creates `product_batches` with its CHECKs, partial unique index, ordering index, and integrity triggers. Nothing is seeded — batch data is the pharmacy's own.

Restart safety is asserted as in previous phases: connect, migrate, close, reconnect, confirm schema stability. The scope-guard test is extended to assert `product_batches` now exists while `stock`, `stock_ledger`, and `prices` still do not, and a new assertion confirms `product_batches` carries **no stock-shaped column**.

## 21. Security

No new authentication or session behaviour and no new trust boundary. No secret, token, or password is logged or returned. Browser-supplied actor identifiers carry no authority. Batch data is local business data that never leaves the machine.

## 22. Tests

**Backend** — create, read, list ordering, update, archive, restore; duplicate normalized lot on the same Pack rejected; the same lot string on a *different* Pack accepted; case and whitespace treated as duplicates; display form preserved; invalid calendar dates rejected; `expires_on < manufactured_on` rejected; an already-expired historical batch accepted; both dates absent accepted; MRP stored and returned as exact paise; non-positive MRP rejected; revision conflict; spoofed-actor resistance; all three roles against every endpoint; unauthenticated rejection; batch against an archived Pack rejected; Pack archive blocked while an active Batch exists; safe typed errors with no raw database text; rollback leaving no orphans.

**Frontend** — empty, loading, error and retry; create; edit; archive and restore; date entry and validation; rupee↔paise exactness; derived expiry state for all four cases; revision-conflict reload proving the retry carries the new revision; read-only pharmacist and cashier; keyboard focus and dialog trap; per-cell labels.

**End-to-end** — a real-browser Owner journey from Product to Pack to Batch: create, edit, archive, restore; a read-only role seeing batches without mutation controls; and a 390 px layout check. **No test asserts any stock quantity**, because none exists.

## 23. Future compatibility

Durable UUIDv7 batch identity, Pack ownership, and installation-global lots leave the future ledger, recall/traceability, FEFO issue policy, batch-wise valuation, and expiry write-off all implementable without an identity migration. None of that behaviour is implemented, implied, or claimed here.

## 24. Non-goals

Recording a Batch is **not** receiving stock. This phase asserts nothing about quantity held, availability for sale, price charged, or dispensing suitability. Expiry state is a display aid derived from a date the pharmacy entered, and carries no clinical or regulatory authority.

## 25. Freeze acceptance matrix

| # | Item | Accept when |
| --- | --- | --- |
| 1 | Migration 0006 only; 0001–0005 byte-identical | blob hashes |
| 2 | Audit enumeration extended via established precedent | migration reviewed |
| 3 | Batch attaches to Pack, never Product or composition | schema + tests |
| 4 | Batch carries no `store_id`; rationale documented | schema + §7 |
| 5 | Uniqueness is per Pack, not global | constraint test |
| 6 | Same lot string on another Pack accepted | test |
| 7 | Case/whitespace duplicates rejected; display form preserved | test |
| 8 | Dates are calendar-only and really validated | constraint + API test |
| 9 | `expires_on >= manufactured_on` enforced | constraint test |
| 10 | Expired historical batch accepted | test |
| 11 | No stored expiry flag; state derived | schema review |
| 12 | MRP exact integer paise; no float anywhere | schema + tests |
| 13 | **No stock-shaped column on the Batch row** | schema assertion test |
| 14 | No batch barcode or serial number | schema review |
| 15 | Reads authenticated; mutations Owner/Admin only | role tests |
| 16 | Audit actor from server session only | spoofed-actor test |
| 17 | Archive never deletes; restore revalidates | tests |
| 18 | Pack archive blocked while active Batch exists | test |
| 19 | Four distinct query states with working retry | frontend tests |
| 20 | Revision-conflict reload refetches and repopulates | frontend test |
| 21 | Accessibility and 390 px regressions green | frontend + E2E |
| 22 | No deferred domain implemented | scope-guard test |
| 23 | Full suite zero failures | verification run |
