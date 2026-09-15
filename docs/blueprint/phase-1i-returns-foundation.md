# Phase 1I — Returns / Credit-Note / Purchase-Return Foundation

**Status: implementation authority.** Written against the repository at `0470aef8` (Phase 1H) and
reconciled against `e7b6dab5` (Phase 1H-C1, compliant invoice serial). Every claim below was checked
against the repository and, where it touches law, against the primary text — not against earlier
blueprints and not from memory.

The legal reasoning behind §§5, 9 and 11 is recorded once in **ADR-017** and is not restated here.

---

## 1. Objective

Record goods travelling **backwards** — a customer returning to the counter, or the pharmacy sending
stock back to a supplier — without mutating any original document, without inventing a tax position
the software is not entitled to hold, and without ever putting a returned medicine back on the shelf
by default.

## 2. Scope

One canonical **return document**, Draft → Posted, discriminated by `return_kind`
(`sales_return` | `purchase_return`); linked to exactly one posted original document and, line by
line, to exactly one original line; cumulative-difference value and tax reversal that sums exactly
across any number of partials; an operator-stated GST position rather than a derived one; a stock
status dimension on every movement with quarantine by default on the sales side; an authorised
disposition transfer as the only route back to sellable; supplier credit-note evidence as append-only
record of what the supplier did; server-allocated per-kind document numbers inside the posting
transaction.

## 3. Exclusions

Unlinked (free-standing) returns · returns against a draft document · partial-line price
renegotiation · replacement/exchange as a single document · GST return filing · ITC reversal
tracking · the s.34(2) deadline as an enforced control · supplier reconciliation · accounting
journal · cash/bank ledger · refund settlement instruments (the money is recorded, not moved) ·
correction of a mistaken write-off · destruction/disposal records and Form registers · recall
handling · expiry-return-to-supplier schemes · credit-note printing engine.

## 4. Dependencies (verified present at `e7b6dab5`)

| Needed | Exists as | Reusable unchanged? |
|---|---|---|
| Posted-document immutability | `sale_documents` / `purchase_documents` posted triggers | yes — this is what makes a compensating document possible |
| Frozen line snapshots | `sale_lines` / `purchase_lines` tax and identity columns | yes — §7 reads them, never re-resolves |
| Document numbering | `document_number_series` + allocator (Phase 1H §7) | **extended**, not replaced — §12 |
| Compliant serial format | `domain::sales::document_serial` (Phase 1H-C1) | yes — reused verbatim for `SR` / `PR` |
| Ledger, append-only, non-negative stock | `inventory_movements` + `BEGIN IMMEDIATE` balance read | **extended** with a status dimension — §9 |
| Money and rounding | `domain::money` + ADR-016 | yes — §8 adds one derived rule, no new rounding point |
| Audit actor from session | `require_*` + `master_change_events` (ADR-012) | yes |
| Batch identity and expiry | `product_batches.expires_on` | yes — read **live**, see §11.3 |
| Real-service gate | `tests/real_service_gate.rs` | yes |

**Absent from the foundation, and therefore designed here:** the stock status dimension (§9), the
disposition transfer (§10), and the return document itself.

## 5. Terminology — the rule that governs every name in this phase

A sales return is **not** automatically a GST credit note, and a purchase return is **never** a debit
note issued by this pharmacy. Under CGST s.34 both instruments are issued by *the person who
supplied*; a recipient issues neither. See ADR-017.

Accordingly:

- The internal concept is `return_document`, discriminated by `return_kind`.
- The token `debit_note` appears nowhere in the schema, the error vocabulary, the contract, or the
  interface. A test asserts its continued absence and will fail if anyone adds it.
- On the purchase side the document records which route of Circular 72/46/2018-GST was taken
  (`gst_route`: `fresh_supply` | `supplier_credit_note`).
- Where the supplier issues the credit note, it is captured as **separate append-only evidence** of
  what the supplier did — never as a document of ours, and never editable into the posted return.

## 6. Linked returns only

A return may be opened only against a **posted** original document, and every line names exactly one
original line. There is no free-standing return in this phase.

This is a deliberate narrowing. An unlinked return has no tax history to reverse, no original rate to
reuse and no quantity to bound against, so it would have to invent all three. A trigger
(`return_documents_original_posted_insert`) enforces the posted-original rule at the database, not
only in the handler.

## 7. The shared spine

Both kinds are one table, one lifecycle and one posting path. The differences are expressed as
column-level constraints rather than as two parallel implementations:

| | `sales_return` | `purchase_return` |
|---|---|---|
| Original | `original_sale_document_id` | `original_purchase_document_id` |
| Stock effect | movements **in**, `quantity_delta_atoms > 0` | movements **out**, `quantity_delta_atoms < 0` |
| Landing status | `quarantined` or `non_sellable` | draws on `sellable` |
| Disposition | required | must be absent |
| GST field | `tax_adjustment_status` required | `gst_route` required |
| Posting authority | pharmacist | owner |

Every row in that table is a CHECK constraint or a trigger, so the two kinds cannot leak into each
other even by direct SQL.

## 8. Value and tax reversal — cumulative difference

A return line copies the original line's tax snapshot (rate version, basis points, HSN, treatment)
and **never re-resolves it**. A rate that changed between the sale and the return is irrelevant: the
reversal must be equal and opposite to what was charged.

For each component, a return of `r` atoms against an original of `Q` atoms with `R₀` already returned
reverses

```
share(R₀ + r) − share(R₀)      where share(n) = round(original_amount × n / Q)
```

in `i128`, with ADR-016's half-away-from-zero tie. Each step is a difference of two roundings of the
same monotone function, so **any sequence of partials sums exactly to the original** — no residual
paise is stranded on the last return. The line total is the **sum of the reversed components**, not a
sixth independent proportion, which is what keeps the components and the total consistent.

This adds no new rounding point: `share` is the same single rounding ADR-016 already fixes.

## 9. Stock status — a dimension on the movement, not a second ledger

Every `inventory_movements` row carries `stock_status ∈ {sellable, quarantined, non_sellable}`, and
balances are **derived per status**. There is no quarantine table, no location dimension, and still
no stored balance anywhere.

The counter reads `sellable` only. Every balance derivation in the sales and inventory paths filters
on it, which is what makes quarantine mean something rather than being a label.

Structural rules, enforced by CHECK constraints on the movement:

- a `sales_return` movement must be positive and must land in `quarantined` or `non_sellable`;
- a `purchase_return` movement must be negative and may draw only on `sellable`;
- a `sale`, `purchase` or `opening_stock` movement is `sellable`;
- a return movement must name its `return_line_id`, and only a return movement may carry one;
- a `disposition_transfer` must name its `stock_disposition_id`, and only a transfer may carry one.

## 10. Quarantine release — an append-only transfer, never a mutation

Releasing quarantined goods writes a `stock_dispositions` row and **two opposite movements within
the one lot** (`−quarantined`, `+sellable`). Nothing is edited; the physical total is unchanged; the
ledger keeps both the quarantine and the decision that ended it, with the authorising user and a
stated reason.

`from_status` may only be `quarantined`: **a write-off is terminal**. Undoing a judgement that goods
must not be sold is not a side effect of the release door.

The decision is made on the Inventory · Stock Overview screen, on the quarantined row itself. A
cashier sees `Waiting on a pharmacist` rather than a button that would only be refused.

## 11. Expiry

### 11.1 An expired lot cannot be quarantined

Quarantine means "awaiting a judgement that might make this sellable". For a lot past its expiry no
such judgement exists (Rule 110), so a return of an expired lot is written off outright. The screen
states this instead of offering a choice that would be refused.

### 11.2 An expired lot cannot be released

A lot quarantined while still good, which expires while waiting, stays quarantined until it is
written off. The release path re-checks expiry against the later of the recorded date and today, so a
back-dated release cannot step around an expiry that has since passed.

### 11.3 Expiry is read live; everything else is read from the snapshot

The returnable-lines view reports the lot's expiry **as it stands today**, not the value frozen onto
the sale line. Everything else on that view — names, rates, values — comes from the snapshot, because
those must reverse what was charged. Expiry is the exception because it decides whether the goods
could ever go back on the shelf, and a screen showing a stale date would offer quarantine for a
medicine that can never be sold again.

## 12. Numbering

Returns use the existing `document_number_series` allocator with `document_kind` extended to
`sales_return` and `purchase_return`, and their own series codes (`SR`, `PR`). The format is Phase
1H-C1's `SERIES/YYMM/NNNNNN` — `SR/2627/000001`, fourteen characters, inside Rule 46(b)'s and Rule
53(1A)'s sixteen-character limit.

A number is allocated **inside** the posting transaction, so a posting that fails consumes none.

## 13. Anti-over-return

Two checks, and both are needed for different reasons.

- **At line entry** — a friendly refusal that counts posted returns against the original line *and
  the lines already on this draft*. Without the second half a draft accumulates silently and only
  objects at the till, which is the worst moment to find out.
- **Inside the posting transaction** — the authority. Under `BEGIN IMMEDIATE`, the whole document is
  aggregated per original line and re-checked, so two drafts competing for the last strip cannot both
  have it.

The posting check is proved non-vacuously by writing an over-quantity line straight into the table,
behind the handler's back, and showing the posting still refuses.

## 14. GST position — stated, never inferred

See ADR-017 §2. The service validates the combination and refuses an incomplete one; it does not
conclude eligibility from the customer's registration status. The screen presents the choice in the
operator's words and says plainly that the software does not decide it.

## 15. Lifecycle, idempotency and immutability

Draft → Posted, one way. A posted return is closed to every editing route (`return_not_draft`) and to
direct SQL (`return_document_is_posted` triggers on both the document and its lines).

Posting is idempotent on `posting_idempotency_key`: a replay returns the same document and moves no
stock again; the same key carrying different facts is refused with `idempotency_conflict`.

## 16. Authorization

| Action | Who |
|---|---|
| Open, edit, add and remove draft lines | any authenticated counter user |
| Post a **sales** return | pharmacist or owner |
| Release or write off quarantined stock | pharmacist or owner |
| Post a **purchase** return | owner |
| Record supplier credit-note evidence | owner |

The actor is taken from the authenticated server session (ADR-012); nothing is read from the browser.

## 17. Migration

`0014_returns_foundation.sql`, applied with the frozen table-rebuild pattern
(`RENAME TO …_phase1h` → `CREATE` → `INSERT … SELECT` → `DROP`) for `master_change_events`,
`document_number_series` and `inventory_movements`, reproducing every index and trigger verbatim.
Existing movements backfill to `sellable`.

`sqlx::migrate!` embeds migrations at **compile time**, so recompilation must be forced before any
migration-dependent result is trusted.

## 18. Interface

- **Returns** list, and a return detail that renders a posted return forever from its own snapshots.
- **Return Items** from a posted sale or purchase: what is coming back, what is already back, what
  can still be returned, and where it goes.
- **Inventory · Stock Overview** gains a status column and the quarantine decision.
- **Stock Ledger** names every movement's origin in the operator's words rather than leaving a blank
  Reason column beside stock that a document moved.

Every service error code is mapped to a safe message and proven by a source-derived mapper test, so a
code added to the service without a message fails the build rather than reaching the counter as
"AUSHADHARTH could not complete that request".

## 19. Hardening

Two adversaries, both run against the real binary and a real populated database:

- **The database as adversary** — direct SQL with the service out of the way, each attack cloning a
  real row and changing exactly one thing, bracketed by a SHA-256 of the database file.
- **The service as adversary** — HTTP against the running binary, each refusal bracketed by a digest
  of everything a posting could move, so a refusal that still moved stock or consumed a number would
  be visible even though the status said no.

## 20. Future compatibility and formal freeze criteria

**Extends cleanly later:** replacement/exchange (a second document referencing the same return) ·
destruction registers (a consumer of `non_sellable` balances) · ITC reversal tracking (a consumer of
`tax_adjustment_status`) · supplier reconciliation (a consumer of the credit-note evidence) ·
correction of a mistaken write-off (needs its own design and audit story, deliberately absent) ·
GSTR-1 (needs the snapshot set §7 already completes).

**Freeze criteria:** blueprint and design audit approved · migration with forced-recompilation proof ·
backend with unit tests · real-service gate extension · contracts · frontend · frontend tests · E2E ·
real browser acceptance with a stated verdict · every error code mapped to a safe message and proven
by the source-derived mapper test · all hardening proofs restored byte-identically · full green
regression · exact scope reconciliation · clean 0/0.
