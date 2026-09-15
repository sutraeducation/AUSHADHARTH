# ADR-018: Stock custody, operations, and the end of physical custody

## Status

Accepted. Prerequisite for Phase 1J. Completes the stock-status model ADR-017 introduced, and
finishes the Adjustment posting workflow Phase 1D specified but never surfaced.

## Context

Phase 1D gave the inventory ledger one correction primitive: a signed `adjustment` with an optional
line of free text. It was enough to keep an append-only ledger honest, and the blueprint said so
plainly — *"it is not a commercial workflow and no document generates it"*.

It is not enough to run a pharmacy. A crushed strip, a shelf that counted two short, a lot that
expired while it sat there, and a carton the disposal contractor took away are four different facts
about the business. A ledger that records all four as `adjustment: -10` can satisfy an accountant
that the arithmetic balances while telling nobody what actually happened — and no later report,
valuation or audit can recover the difference from prose someone may or may not have typed.

Phase 1I then introduced stock statuses and left a specific hole. `sellable`, `quarantined` and
`non_sellable` all describe goods **in the pharmacy's custody**, and only `sellable` may be billed.
But nothing in the schema could ever reduce `non_sellable`: only `sale` and `purchase_return` may
carry a negative quantity, and both are constrained to `sellable`. Written-off stock accumulated
forever. A Write-off Register built on that would have reported goods as written off while the
ledger insisted they were still on the premises.

## Decision

### 1. Custody and saleability are different facts, and only one of them can end

```
sellable      held, available to sell, subject to every independent control
quarantined   held, unavailable, waiting for somebody to decide
non_sellable  held, permanently unavailable — written off and STILL IN THE BUILDING
```

Physical custody is `sellable + quarantined + non_sellable`. A write-off moves quantity between
those columns and changes no total. **Custody ends only when goods physically leave**, which this
phase records as a negative `stock_removal` movement against `non_sellable`.

There is deliberately **no fourth `removed` status**. Removed quantity is history, not a balance; a
status row accumulating it would be counted as stock by every future report that sums the statuses.

This distinction is the reason the interface never says "destroyed" when it means "written off".
An operator who confuses the two produces a figure that cannot be reconciled against the shelf.

### 2. Operator intent is typed, and it is an invariant rather than a label

Six kinds, each a separate screen with its own safeguards, because *"reduce this number"* is not a
thing a pharmacist ever actually means:

| Kind | What it does | Who |
|---|---|---|
| `physical_count` | Operator counts; the server computes the variance | pharmacist, owner |
| `adjustment` | Phase 1D's correction, finally reachable | owner |
| `damage` | sellable → non_sellable, or → quarantined pending assessment | pharmacist, owner |
| `expiry` | sellable → non_sellable for a lot past its date | pharmacist, owner |
| `quarantine` | sellable → quarantined | pharmacist, owner |
| `removal` | non_sellable → gone; custody ends | owner |

A line must agree with the kind of document it belongs to, enforced by a `BEFORE INSERT`/`UPDATE`
trigger and mirrored by a rule table in the domain layer. A `damage` document cannot quietly contain
an expiry write-off; a `removal` cannot take goods off the shelf. Neither layer is the only thing
standing between a mistake and the ledger.

The generic adjustment stays with the owner precisely because it is the one operation that can move
a number with no physical event behind it at all. Removal stays with the owner because it asserts
that goods have left the building.

### 3. The operator states a count; the server states the variance

A physical count sends **what was counted**, never a delta. The variance is computed inside the
posting transaction against a balance read under its `BEGIN IMMEDIATE` write lock:

```
delta = counted_atoms − balance(store, pack, batch, status)
```

A browser looking at a stale screen — or a browser that is lying — cannot decide how much stock
exists. The typed reason follows the arithmetic, not the request: a shortfall is recorded as
`physical_count_loss` whatever the caller called it.

**A zero variance is a real answer.** "I counted it and it was right" is an audit fact, so the line
is recorded and no movement is written — the ledger correctly forbids a zero movement.

### 4. Typed cause, supplementary prose

`reason_code` is a closed vocabulary: `physical_count_gain`, `physical_count_loss`, `damage`,
`breakage`, `expiry`, `theft_or_loss`, `data_correction`, `quality_hold`, `disposal`. A future
register, valuation or authorisation rule reads **that**. The note is for a human.

No future system should ever have to parse free text to learn whether stock was stolen or merely
miscounted. This is the single most important thing Phase 1J preserves for the accounting and
valuation phases that do not exist yet.

### 5. Theft reduces the stock it was taken from

Goods stolen or lost are already gone, so `theft_or_loss` can only decrease, and it decreases the
status the goods were in — it is not routed through `non_sellable` first to satisfy schema
mechanics. Writing something off says it may not be sold; theft says it is not there.

### 6. Expiry remains a POS control, independently of all of this

The counter already refuses an expired batch at line entry **and again at posting**, reading
`product_batches.expires_on` live against the business date. That is untouched. Phase 1J's expiry
operation is bookkeeping — it stops expired quantity being reported as sellable stock — and the
interface says so explicitly rather than implying the goods have been disposed of.

An `expiry` operation against a lot that has not reached its date is refused (`batch_not_expired`),
because a false record is worse than no record.

### 7. Quarantine gains a way in; the write-off stays terminal

Phase 1I's `stock_dispositions` widens by exactly one value: `from_status` becomes
`('sellable', 'quarantined')`. `non_sellable` is still absent, so opening the door for quarantine
entry does not quietly reopen the write-off Phase 1I made terminal on purpose.

## Consequences

- A Write-off Register and a Physical Removal Register can now both be built, and they will agree
  with the stock ledger.
- Every stock event carries the operator, the date, the typed cause, the note, the document and the
  line that produced it. `stock_operation_line_id` is paired both ways with movement type, so a
  stock-operation movement can never be anonymous and nothing else can borrow the provenance.
- A pharmacist gains real authority over floor work they previously could not record at all.
- The conservative rules will occasionally refuse something a pharmacy would have waved through —
  a removal of stock not yet written off, an expiry write-off a day early. That is the intended
  direction of the error.

## Non-goals

Valuation, cost layers, FIFO, weighted average, COGS, journals, GST filing, barcode resolution,
printing, inter-store transfer, reorder suggestions, and correction of a mistaken write-off all
remain future decisions. Phase 1J preserves the semantics they will need; it assigns no money.
