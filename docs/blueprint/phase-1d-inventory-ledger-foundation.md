# Phase 1D — Inventory Ledger and Opening Stock Foundation

## 1. Objective

Establish the authoritative, append-only quantity foundation every future pharmacy operation will post against: an immutable inventory movement ledger, Store-scoped, with exact integer quantities, opening-stock posting, and derived balances. No commercial document is implemented.

**Phase identifier.** The repository assigns no identifier past `1C-C`; its blueprints call this "the inventory-ledger phase". Following the established `phase-<n><letter>` naming, and because this is the first *operational transaction* domain rather than another master-identity slice, it is **Phase 1D**.

## 2. Scope

- `inventory_movements` — an append-only, Store-scoped ledger of exact base-unit quantity deltas against a Product Pack and optional Batch.
- Two movement types: `opening_stock` and `adjustment` (the latter is what makes correction possible; see §11).
- Idempotent posting, non-negative balance enforcement, reversal linkage.
- Derived balances computed from the ledger — **no stored balance anywhere**.
- An **Inventory** area: Stock Overview, Stock Ledger, and an Opening Stock / Adjustment posting workflow.

## 3. Explicit exclusions

Not implemented, not stubbed, not partially started: Purchase Invoice, Purchase Order, GRN, purchase return, supplier ledger, supplier master, POS, Sales Invoice, sales return, customers, accounting journal, GST posting, stock valuation, FIFO or weighted-average cost, margin, selling-price engine, discount engine, schemes/free quantity, expiry write-off workflow, store transfer workflow, generic substitution, clinical equivalence, prescription validation, cloud sync, licensing, LAN multi-counter.

Deferred within the inventory domain itself:

- **Valuation and cost.** This phase is quantity-only. Opening stock records how much, never how much it was worth. Attaching a rate to a movement row is deliberately refused here.
- **Materialised balance cache.** Balances are computed from the ledger on demand. A rebuildable cache may be added later only if measurement justifies it; introducing one now would risk exactly the mutable-stock authority this phase exists to prevent.
- **Source/document linkage.** See §19.
- **Configurable negative-stock policy.** See §15.

## 4. Domain terminology

| Term | Meaning |
| --- | --- |
| Movement | One immutable, signed quantity change posted to the ledger. |
| Opening stock | The movement type that establishes a starting quantity. |
| Adjustment | A signed correction movement, including a reversal. |
| Atoms | Exact integer quantity in the **Product's base unit**, at the Product's `quantity_scale`. |
| Balance | `SUM(quantity_delta_atoms)`, always derived, never stored. |
| Idempotency key | A client-generated UUIDv7 making a posting retry-safe. |

## 5. Inventory identity hierarchy

```
Store ── owns ──► Inventory Movement
                        │
                        ├─► Product Pack  (commercial context, required)
                        │      └─► Product (quantity authority: base unit + scale)
                        └─► Batch         (traceability context, optional)
```

Batch identity itself stays installation-global and Pack-owned, exactly as frozen in Phase 1C-C. **Store ownership lives on the movement, not on the Batch** — which is precisely why that decision was correct: one manufacturer lot held by two Stores is one Batch identity with two sets of movements.

## 6. Store scoping

Every movement is Store-scoped: `store_id` is `NOT NULL` and references `store_identity(store_id)`. There is no such thing as a Store-less quantity.

The Store is resolved **server-side** from the installation's store identity, exactly as `/api/v1/catalog/context` already does. A browser-supplied store identifier is never accepted as authority, and the request body carries no `storeId` at all — removing the possibility rather than validating it away.

## 7. Pack / Batch consistency — enforced by the database

A movement carries `product_pack_id` and an optional `batch_id`. It must be impossible to record a Batch belonging to a different Pack. Backend validation alone is explicitly insufficient, so composite foreign keys carry the guarantee:

```sql
FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id)
FOREIGN KEY (batch_id, product_pack_id)   REFERENCES product_batches(id, product_pack_id)
```

`product_packs` already declares `UNIQUE (id, product_id)` — frozen in Phase 1B and evidently placed for exactly this purpose. `product_batches` needs the matching `UNIQUE (id, product_pack_id)`, which this migration adds as a **new unique index on the existing table**: additive, no rewrite of frozen schema.

Because SQLite skips composite foreign keys when any referencing column is `NULL`, a batchless movement is naturally permitted while a mismatched batch is impossible.

The service **also** checks the relationship explicitly before posting, so the operator receives the precise `batch_pack_mismatch` error rather than a generic integrity failure. The two are not redundant: the service check exists for message quality, the composite foreign key exists for correctness, and only the latter is trusted.

Behaviour by case:

| Case | Behaviour |
| --- | --- |
| Pack with no Batch | Permitted. `batch_id` is `NULL`. |
| Medicine Pack | Batch is **encouraged in the UI, not required by schema**. Requiring it would be an invented business rule, and the frozen model likewise does not require composition on a medicine. |
| General item | Batchless posting is normal. |
| Archived Pack or Product | Posting rejected by trigger. |
| Archived Batch | Posting rejected by trigger. Existing movements are untouched history. |
| Expired Batch | **Accepted.** Expiry is a date, not a lock; historical opening stock for an expired lot is legitimate, and write-off is a future movement, not a side effect. |

## 8. Quantity authority — base-unit atoms

**Authoritative quantity is `quantity_delta_atoms`, an exact signed integer in the Product's base unit at the Product's `quantity_scale`.** Pack and Batch are commercial and traceability context, not the unit of account.

This follows from the frozen model rather than being imposed on it: `store_pack_policies.minimum_sale_increment_atoms` and `fractional_sale_allowed` are *already* expressed in atoms, and `product_packs.base_quantity_atoms` already defines a Pack as a quantity of base units. Counting packs would make fractional issue unrepresentable.

Worked through:

> A Pack is a strip of 10 tablets. Opening stock of 5 strips posts `+50` atoms against that Pack. Selling 3 tablets later posts `−3`, leaving `47`. The 47 remain attributable to the original Pack and Batch for MRP and traceability, even though no whole strip corresponds to them.

Bounds and rules:

- `quantity_delta_atoms <> 0` — a zero movement records nothing and is rejected.
- Magnitude bounded by the frozen `MAX_BASE_QUANTITY_ATOMS`, applied to the absolute value so overflow is impossible across accumulation.
- Signed: `opening_stock` must be positive; `adjustment` may be either sign.
- No floating point at any layer — entry, transport, storage, or aggregation.

## 9. Quantity conversion

The operator may think in packs; the ledger thinks in atoms. Conversion is exact integer arithmetic: `packs × pack.base_quantity_atoms`, or a loose base-unit quantity parsed with the frozen `quantityToAtoms` against the Product's `quantity_scale`.

The posting form always displays the resulting authoritative atom quantity **before** posting, so an operator can never be surprised by a silent reinterpretation. The API accepts only atoms; pack-count arithmetic is a UI convenience that never reaches the wire.

## 10. Movement model

```
inventory_movements
  id                     uuidv7, primary key
  store_id               → store_identity(store_id)          NOT NULL
  product_id             ─┐ composite FK to product_packs    NOT NULL
  product_pack_id        ─┘                                  NOT NULL
  batch_id               ─── composite FK to product_batches  NULL
  movement_type          'opening_stock' | 'adjustment'
  quantity_delta_atoms   signed non-zero bounded integer
  occurred_on            business calendar date, YYYY-MM-DD
  reason                 optional text
  reverses_movement_id   → inventory_movements(id)            NULL
  idempotency_key        uuidv7, UNIQUE
  posted_by_user_id      → users(id)                          NOT NULL
  posted_at_utc          technical timestamp
```

`occurred_on` is a **business calendar date**, timezone-independent by construction and consistent with the frozen `effective_from` / `expires_on` convention. `posted_at_utc` is the separate technical instant, matching ADR-009's separation of business date from technical timestamp.

## 11. Movement types

A closed `CHECK` enumeration containing exactly what this phase implements: `opening_stock` and `adjustment`. Future types — purchase receipt, sale issue, returns, damage, expiry write-off, transfer — are **not** pre-declared, because a value the service cannot handle is worse than a migration. The repository has extended a `CHECK` enumeration three times already (migrations 0003, 0005, 0006) with a proven rebuild pattern; that is the documented path.

**Why `adjustment` is required now, not scope creep.** Movements are immutable, so a mis-posted opening stock can only be corrected by posting a compensating movement. Without `adjustment` the ledger would be administrable only by fabricating a second, semantically false `opening_stock`. Adjustment is the correction mechanism that makes append-only honest — it is not a commercial workflow and no document generates it.

## 12. Opening stock semantics

Opening stock is a ledger posting like any other. **No opening quantity is ever seeded onto a Pack or Batch row.**

**Multiple opening-stock postings are permitted.** A pharmacy enters opening stock product by product over days; a blanket "only one ever" rule would be wrong, and a per-Pack rule would break legitimate multi-batch openings. What must be prevented is *accidental duplication*, and that is handled precisely by the idempotency key (§16) rather than by a coarse uniqueness rule that would also block legitimate entry.

`opening_stock` requires a positive quantity. Correcting one is an `adjustment`, optionally linked by `reverses_movement_id`.

## 13. Append-only guarantees

Posted movements are immutable, enforced by the database rather than by service discipline — `BEFORE UPDATE` and `BEFORE DELETE` triggers raise `ABORT`, exactly as `master_change_events` has since Phase 1A. No API exposes update or delete for a movement. This realises ADR-009's standing rule that postings "are append-only and corrected through reversals".

## 14. Reversal and correction

`reverses_movement_id` links a correcting `adjustment` to the movement it reverses, with `ON DELETE RESTRICT` (which can never fire, since deletion is impossible) and a partial unique index so **one movement can be reversed at most once**. A reversal must be an `adjustment`; `opening_stock` may not reverse anything.

A posted movement is never silently edited to fix a quantity. History is preserved in full, and the corrected position is the sum.

## 15. Negative-stock policy

**Prohibited at posting time in this phase.** A posting whose resulting balance for `(store, pack, batch)` would be negative is rejected with a typed `insufficient_stock` error.

Chosen deliberately: this foundation has no POS that might legitimately need an override, accounting reliability is better served by refusing impossible states, and an offline-first single-writer database can enforce it correctly (§16). Relaxing it later is a configuration decision that belongs with the phase that needs it, and is explicitly deferred — the default is and stays *prohibit*.

Balance is evaluated at the `(store_id, product_pack_id, batch_id)` grain, because that is the grain a physical lot is held and issued at.

## 16. Concurrency model

Non-negative enforcement is a read-check-write, so it must be atomic. The posting transaction opens with **`BEGIN IMMEDIATE`**, taking SQLite's write lock *before* reading the current balance — the same technique the frozen `record_failure` and `setup` paths already use.

Because SQLite permits a single writer, a second concurrent posting blocks on the write lock, and on acquiring it re-reads a balance that already includes the first posting. There is no stale-read window and no lost update. Lock waits are bounded by the frozen five-second `BUSY_TIMEOUT` and surface as the typed `service_busy`, with no blind retry and no partial posting.

Idempotency closes the other half: a retried posting carries the same `idempotency_key`, and the unique constraint means the retry returns the **already-posted movement** rather than double-posting. A replay is a success, not an error.

A replayed key returns the stored movement **unconditionally**, even if the retried body differs. That is the deliberately safe direction: a client bug can then fail to post something, which is visible and recoverable, whereas re-posting would silently duplicate stock, which is neither.

## 17. Derived balance model

```sql
SELECT SUM(quantity_delta_atoms) FROM inventory_movements
WHERE store_id = ? AND product_pack_id = ? AND batch_id IS ?
```

Grouped presentation:

- **Batch balance** — `(store, pack, batch)`. The finest grain; what a physical lot holds.
- **Pack balance** — `(store, pack)`, summing its batches and any batchless movements.
- **Product balance** — `(store, product)`. Meaningful *because* quantity is in base-unit atoms: summing a Pack of 10 and a Pack of 15 yields a correct tablet total, which counting packs could never do.

Pack and Batch granularity is always retained alongside the aggregate, so traceability and per-Pack MRP context are never lost to a roll-up.

## 18. Batch balance model

A Batch carries identity, dates, and MRP — and **never a quantity**. Its available stock is derived from the ledger. An expired Batch may hold a positive balance; nothing is written off merely because a date passed. A future expiry write-off is a movement, posted deliberately.

## 19. Source and reference model

Document linkage (`source_type`, `source_id`) is **deferred**, not stubbed. There is no document to reference yet, and an unvalidated free-text source column is exactly the weak structure to avoid. The migration path is the repository's proven `CHECK`-rebuild pattern — used in 0003, 0005, and 0006 — which adds the columns and their constrained enumeration atomically when the first document type exists. Movement identity, Store scope, Pack/Batch context, and the idempotency key are all already in place, so no identity rebuild will be required.

## 20. Authorization

Reads require a valid local session and are available to `owner_admin`, `pharmacist`, and `cashier`. **Posting requires `owner_admin`** plus `validate_mutation_request` (Host/Origin), matching every frozen mutation. Opening stock is a sensitive mutation, and no pharmacist or cashier stock-adjustment right is invented here — the role model has three roles and this phase adds none.

## 21. Audit actor

`posted_by_user_id` is taken **exclusively from the validated server session** and is `NOT NULL` with a foreign key to `users`. A browser-supplied actor is ignored.

**Movements are not written to `master_change_events`.** That stream is the master-data change log; an inventory movement is operational transaction history, and the immutable movement row — carrying actor, timestamp, business date, reason, and reversal linkage — *is* its own audit record. Forcing operational postings into a master-only event model would be semantically wrong, so the two streams stay distinct by design.

## 22. Revision model

**Posted movements carry no revision and no `expectedRevision`.** Optimistic concurrency exists to stop two editors overwriting one mutable record; a movement is immutable, so there is nothing to overwrite and the concept would be cargo-cult. Safety on the posting path comes from idempotency and the atomic balance check instead. Correction is a new movement, never an edit.

## 23. API design

```
GET  /api/v1/inventory/stock?productId=&packId=   → derived balances, Store-scoped
GET  /api/v1/inventory/movements?packId=&batchId= → ledger, newest first
POST /api/v1/inventory/movements                  → post a movement
```

The posting body carries `idempotencyKey`, `movementType`, `productPackId`, optional `batchId`, `quantityDeltaAtoms`, `occurredOn`, optional `reason`, and optional `reversesMovementId`. It carries **no `storeId`** — the Store is server-resolved.

A replayed `idempotencyKey` returns `200` with the existing movement; a first posting returns `201`.

Typed error codes, deliberately few: `validation_failed`, `not_found`, `archived_conflict`, `batch_pack_mismatch`, `insufficient_stock`, `authentication_required`, `session_expired`, `authorization_denied`, `service_busy`, `internal_error`. No raw SQLite, sqlx, or Rust text reaches the client.

## 24. UI design

A new top-level **Inventory** destination, beside Masters:

```
Inventory
  ├── Stock Overview   balances by Product → Pack → Batch, Store-scoped
  └── Stock Ledger     movement history, newest first
        └── Post Opening Stock / Adjustment   (Owner/Admin)
```

The posting dialog selects Product, then Pack, then Batch where applicable, takes a quantity in packs or base units, and **always shows the resulting authoritative atom quantity before posting**. Batch selection shows batch number, expiry, MRP, and derived expiry state; an expired batch remains selectable because historical opening stock is legitimate. Batch creation is not hidden inside this flow — batches are created in the existing Pack workspace, which stays the single authority.

No valuation column, no cost, no price.

## 25. Error, empty, loading, and success states

Four distinct states for every inventory query — stock overview, ledger, and each selector. A failed query is never rendered as an empty balance and never as a permanent "Loading…"; each carries a safe message and its own retry that refetches the failed query. Posting failures map to the typed codes above with safe text. `insufficient_stock` states plainly what is available.

## 26. Accessibility

Unchanged from the frozen model: portal dialogs with unique `useId` titles, `aria-modal`, one active dialog, Escape close, Tab and Shift+Tab wrap, launcher focus restoration, first-invalid-field focus, visible focus, labelled controls, semantic tables with per-cell `data-label`, no horizontal page overflow at 390 px, honoured reduced motion.

## 27. Migration design

One new migration, `0007_inventory_ledger.sql`. Migrations `0001`–`0006` remain byte-identical. It adds the `product_batches(id, product_pack_id)` unique index needed for the composite foreign key, creates `inventory_movements` with its constraints, composite foreign keys, indexes, and append-only triggers. Nothing is seeded.

Restart safety is asserted as in every prior phase. The scope-guard test gains `inventory_movements` to the expected list while continuing to assert that `prices`, `purchases`, and `sales` do not exist — and a new assertion confirms **no stock-shaped column exists on `products`, `product_packs`, or `product_batches`**.

## 28. Tests

**Backend** — valid opening stock; exact atom arithmetic; multi-movement aggregation; pack/batch match accepted; **mismatched batch rejected by the database, not merely by the service**; batchless posting; zero quantity rejected; negative opening stock rejected; adjustment of either sign; non-negative enforcement; `insufficient_stock` typed; idempotent replay returning the same movement without double-posting; reversal linkage and double-reversal rejection; UPDATE and DELETE both rejected by trigger; archived Pack, Product, and Batch rejected; expired Batch accepted; Store scoping; all three roles against every endpoint; unauthenticated rejection; server actor with spoofed-actor resistance; rollback leaving no orphans; safe errors with no raw database text.

**Frontend** — Inventory navigation; overview empty, populated, loading, error, retry; posting form with Product → Pack → Batch selection; exact quantity preview for both pack and base-unit entry; validation; successful posting updating the derived balance; ledger rendering; read-only roles seeing no posting control; keyboard and first-invalid focus; per-cell labels; logout cache isolation for inventory queries.

**End-to-end** — real Chromium: open Inventory, post opening stock, verify the derived balance and the ledger row, a batch-aware posting, a read-only user unable to post, and 390 px layout.

**Contract-shape tests** — the frontend double's responses are parsed against the shared Zod schemas, so a double that drifts from the contract fails the suite.

## 29. Real-Rust integration gate — decision

The Phase 1C-A3 audit recommended a real browser + real Store Service gate before money, stock, or batch semantics. Batch has landed and inventory is now, so the question is answered explicitly rather than deferred by silence.

**Decision: not in this phase, and the blocking reason is structural, not effort.**

`RuntimePaths::resolve` has **no environment or CLI override**: it always resolves the authoritative database to the user's real `%LOCALAPPDATA%\Bizarth Technologies\AUSHADHARTH`, and `validate_database_path` *actively rejects* any repository-local path. A real-service E2E run would therefore either write to the developer's genuine pharmacy data, or require adding a database-redirect configuration surface to frozen Phase 0 platform code — a new way to point the authoritative database elsewhere, which carries its own security weight and warrants its own ADR rather than being improvised inside an inventory phase.

What already mitigates the gap: the 64 backend tests drive the **real axum router against a real migrated SQLite database**, so constraints, composite foreign keys, triggers, transactions, and typed errors are genuinely integration-tested. The untested seam is only the frontend↔backend HTTP contract, and this phase narrows it with the contract-shape tests in §28.

**Prerequisite for a future gate**, stated concretely: an ADR and a Phase-0-scoped change introducing an explicit, clearly-bounded runtime-path override for test execution, after which a Playwright `webServer` entry can launch the real service against a disposable database.

## 30. Future Purchase / POS compatibility

A purchase receipt or sale issue becomes a movement row with a new `movement_type` and the deferred source linkage from §19. Store scope, Pack and Batch context, exact atoms, idempotency, append-only semantics, and reversal are all already in place, so no identity or quantity rebuild will be needed. FEFO issue policy, valuation, and pricing remain future decisions that this schema permits but does not presume.

## 31. Multi-store compatibility

Movements are Store-scoped from the first row, and Batch identity is deliberately not, so a second Store is additive: new movements against the same Batch identity, with balances naturally separating by `store_id`. No migration of existing rows would be required.

## 32. Non-goals

This phase asserts nothing about value, cost, price, or profitability, and implements no purchase or sale. A balance is a derived quantity statement about what the ledger says a Store holds — not a valuation, not an availability promise, and not a clinical or regulatory claim.

## 33. Freeze acceptance matrix

| # | Item | Accept when |
| --- | --- | --- |
| 1 | Migration 0007 only; 0001–0006 byte-identical | blob hashes |
| 2 | **No mutable stock column on products, packs, or batches** | schema assertion test |
| 3 | Quantity authority is base-unit atoms; no float anywhere | schema + tests |
| 4 | Zero-quantity movement rejected | constraint test |
| 5 | Store scope required; never browser-supplied | schema + API review |
| 6 | Batch/Pack mismatch rejected **by the database** | test bypassing the service |
| 7 | Archived Pack/Product/Batch rejected; expired Batch accepted | tests |
| 8 | Movements immutable — UPDATE and DELETE rejected by trigger | tests |
| 9 | Reversal linked; double reversal rejected | constraint test |
| 10 | Idempotent replay returns the same movement | test |
| 11 | Non-negative balance enforced atomically under `BEGIN IMMEDIATE` | test |
| 12 | Derived balances correct across multiple movements and batches | tests |
| 13 | Batch carries no quantity | schema review |
| 14 | Reads authenticated; posting Owner/Admin only | role tests |
| 15 | Actor from server session only | spoofed-actor test |
| 16 | No `master_change_events` row for movements; rationale documented | §21 |
| 17 | Four distinct query states with working retry | frontend tests |
| 18 | Accessibility and 390 px regressions green | frontend + E2E |
| 19 | Integration-gate question answered explicitly | §29 |
| 20 | No commercial-domain leakage | scope-guard test |
| 21 | Full suite zero failures; phases 0–1C-C green | verification run |
