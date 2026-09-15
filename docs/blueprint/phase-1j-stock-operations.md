# Phase 1J — Stock Operations, Physical Count, Damage, Expiry, Quarantine and Removal

**Status: implementation authority.** Written against the repository at `c677813`
(EOL corrective), on the Phase 1I returns foundation at `f7c75f3`. Every claim below was checked
against the repository, not against earlier blueprints.

The custody model and the reasoning behind §§3, 5, 7 and 9 are recorded once in **ADR-018** and are
not restated here.

---

## 1. Objective

Give a pharmacy the operations it actually performs on its own stock — counting a shelf, writing off
damage, classifying an expired lot, holding something back, recording that goods left the building —
with the cause typed rather than written in prose, and without the browser ever deciding how much
stock exists.

## 2. Scope

One `stock_operations` document, Draft → Posted, in six typed kinds, with lines that name the exact
inventory identity and the typed reason; server-computed count variance under the posting write
lock; the `stock_removal` movement that ends physical custody; quarantine entry through the proven
Phase 1I disposition mechanism; an Inventory workspace with one workflow per operator intent; and
the Phase 1D Adjustment posting UI, carried into this phase.

## 3. Phase 1D completion boundary — stated, not hidden

Phase 1D §24 promised `└── Post Opening Stock / Adjustment   (Owner/Admin)`.

| Element | Phase 1D | Phase 1J |
|---|---|---|
| Backend `adjustment`, either sign | delivered | unchanged |
| Non-negative enforcement, idempotency, `BEGIN IMMEDIATE` | delivered | unchanged |
| Reversal linkage and double-reversal refusal | delivered | unchanged |
| **Adjustment UI** | **never shipped** | **shipped here** |
| Typed reason | absent | added |

**This is PHASE 1D COMPLETION CARRIED INTO PHASE 1J.** The capability is not newly invented; the
backend has accepted an adjustment since Phase 1D and its tests still pass unchanged. What was
missing was any way for an operator to reach it — the shipped dialog hard-coded `opening_stock`
while its own text said *"Correct it later with an adjustment"*, pointing at a workflow that did not
exist. Phase 1D's history is not rewritten; this blueprint records the distinction.

`POST /api/v1/inventory/movements` is retained for opening stock and as the legacy primitive its
own tests still exercise. It is not a second operator workflow: the Inventory screens route every
adjustment through `stock_operations`.

## 4. Operation kinds and reasons

Kinds: `physical_count`, `adjustment`, `damage`, `expiry`, `quarantine`, `removal`.
Reasons: `physical_count_gain`, `physical_count_loss`, `damage`, `breakage`, `expiry`,
`theft_or_loss`, `data_correction`, `quality_hold`, `disposal`.

Which reasons, directions and statuses each kind admits is a rule table in
`domain::stock_operations`, mirrored by the `stock_operation_lines_kind_insert` / `_update`
triggers. Both were attacked independently during hardening, with the trigger removed, to prove the
CHECK underneath it refuses on its own.

## 5. Physical count

The operator supplies a **counted quantity** for one Product / Pack / Batch / status. The server
computes `delta = counted − balance` inside the posting transaction. The browser may preview the
variance through `GET …/quote`, which writes nothing and which posting ignores entirely.

Zero variance is valid and recorded; no movement is written, because the ledger forbids a zero
movement and the count line is itself the durable record that the shelf was checked.

Counting the same Product/Pack/Batch/status twice on one document is refused
(`duplicate_count_line`): it is a contradiction, not extra information.

## 6. Multi-line and atomicity

A count carries as many lines as the shelf needs. One business date, one actor, one posting
transaction, one idempotency boundary. Lines touching the same lot see each other's applied deltas,
so the second is not checked against stock the first already spent. A failure on any line leaves
nothing behind — no movement, no disposition, no `applied_delta_atoms`, and the document stays a
draft.

## 7. Damage, expiry, quarantine

All three are status transfers through Phase 1I's `stock_dispositions`, which gains
`from_status = 'sellable'`, a typed `reason_code`, and a link to the operation line. Damage offers
both honest outcomes — write off, or hold pending assessment. Expiry offers only the write-off,
because no later assessment could make an expired lot sellable. Quarantine and any
`data_correction` demand a stated note; a hold that says nothing is an unexplained stock movement.

## 8. Removal

`stock_removal`: negative only, `non_sellable` only, typed reason required, operation-line
provenance required, non-negative balance enforced under the write lock. No route to sellable, and
no mutation of the write-off that preceded it.

## 9. Concurrency

Every stock-decreasing and status-decreasing operation takes `BEGIN IMMEDIATE` before reading any
balance it depends on, and balance checks are per Store / Pack / Batch / **status**. Races proved:
two removals for the last atoms, two damage postings for the last of a lot, and two lines of one
document against one identity.

## 10. Authorization

| Action | owner_admin | pharmacist | cashier |
|---|---|---|---|
| View stock, ledger and operations | ✓ | ✓ | ✓ |
| Physical count, damage, expiry, quarantine | ✓ | ✓ | ✗ |
| Adjustment, removal | ✓ | ✗ | ✗ |

The actor comes from the authenticated server session. A cashier mutates no stock by any route —
proved over real HTTP for all six kinds.

## 11. Interface

Inventory gains a **Stock Operations** section beside Stock Overview and the Stock Ledger, and one
button per intent. Stock Overview shows sellable, quarantined, not-sellable and the physical custody
total, so nobody reads a write-off as goods having left. A draft nobody posted can be discarded
from the dialog or from the list — a gap the purchase, sale and return documents still carry.

## 12. Migration

`0015_stock_operations.sql`. Rebuilds `master_change_events`, `stock_dispositions` and
`inventory_movements` with the frozen table-rebuild pattern, reproducing every column, CHECK, FK,
index and trigger verbatim before adding anything. The movement table is renamed out of the way
first and dropped before the old disposition table, because SQLite rewrites REFERENCES clauses on
rename and the old disposition table would otherwise be undroppable.

Proved by schema diff against a database built to 0014: **nothing removed, no column lost**, 14
objects added, three tables gained exactly the intended columns.

## 13. Future compatibility and formal freeze criteria

**Extends cleanly later:** Stock Adjustment, Physical Count Variance, Damage, Expiry, Quarantine,
Write-off and Physical Removal registers (typed queries, no text parsing) · valuation and COGS
(consume `reason_code`) · correction of a mistaken write-off (its own design, deliberately absent) ·
barcode-assisted counting (deferred with the POS barcode gap it belongs to).

**Freeze criteria:** blueprint and ADR approved · migration proven fresh and populated with forced
recompilation · domain and API tests · real-service gate extension · contracts · Inventory UI ·
frontend tests · E2E · real browser acceptance at desktop and ~390px with a stated verdict · every
error code mapped to a safe message and proven by the source-derived mapper test · all hardening
proofs restored byte-identically · full green regression · exact scope reconciliation · clean 0/0.
