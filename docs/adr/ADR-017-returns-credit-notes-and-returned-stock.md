# ADR-017: Returns, credit notes, and the status of returned stock

## Status

Accepted. Prerequisite for Phase 1I. Builds on ADR-009 (money, quantity and time), ADR-012
(server-derived audit actors), ADR-015 (product tax classification) and ADR-016 (commercial and tax
rounding).

## Context

Phase 1I is the first slice in which goods travel backwards: a customer brings something back to
the counter, or the pharmacy sends something back to its supplier. Three questions have to be
settled before any of it can be written down, and none of them can be answered by analogy with the
forward-direction code already in the repository.

1. **What is the document called, and who issues it?** "Sales return equals credit note" and
   "purchase return means we issue a debit note" are both in common usage in Indian retail software.
   Encoding either as a database truth would make a legal claim this project is not entitled to
   make.
2. **Can the GST already charged be reduced?** A refund and a tax adjustment are different acts, and
   the second is bounded by statute and by facts this software does not hold.
3. **What happens to the goods?** A returned medicine is physically back in the building. Whether it
   may be sold again is a pharmacy question with a drug-law dimension, not a stock-arithmetic
   question.

Each was verified against the primary source rather than settled from memory. Where the law is
silent, this ADR says so explicitly and records the resulting rule as **project safety policy**, not
as a statutory requirement.

## What the law actually says

### Credit notes and debit notes are both issued by the supplier

Section 34 of the CGST Act, 2017:

- **s.34(1)** — where a tax invoice has been issued and the taxable value or tax charged is found to
  exceed the amount payable, *"the registered person, who has supplied such goods or services or
  both, may issue to the recipient one or more credit notes"*.
- **s.34(3)** — where the taxable value or tax charged is found to be less than the amount payable,
  *"the registered person, who has supplied such goods or services or both, shall issue to the
  recipient one or more debit notes"*.

Both instruments are issued by **the person who supplied**. A recipient never issues either one for
a supply made to it. This is decisive for the purchase-return side: when this pharmacy sends goods
back to its supplier, the pharmacy is the recipient of the original supply and therefore **does not
issue a GST debit note**. Any field, code, label or document in this system that called it one would
be wrong as a matter of law.

### The time limit on declaring a credit note

**s.34(2)**, as substituted with effect from 1 October 2025 by section 126 of the Finance (No. 7)
Act, 2025, requires the details of a credit note to be declared in the return for the month during
which it was issued, and not later than **the thirtieth day of November following the end of the
financial year in which such supply was made, or the date of furnishing of the relevant annual
return, whichever is earlier**.

The commonly repeated "September" deadline is the pre-2022 text and is superseded. This system does
not enforce the deadline — it holds no filing state — but it must never present a rule that
contradicts it.

### What a credit or debit note has to carry

**Rule 53(1A)** of the CGST Rules, 2017 requires the supplier's name, address and GSTIN; the nature
of the document; a consecutive serial number **not exceeding sixteen characters, unique for a
financial year**; the date; the recipient's details; **the serial number(s) and date(s) of the
corresponding tax invoice(s)**; the value, rate and amount of tax; and a signature. **Rule 46(b)**
imposes the same sixteen-character limit on a tax invoice serial.

### The two routes for goods a retailer sends back

**Circular No. 72/46/2018-GST** addresses precisely this case. A retailer returning goods to its
supplier has two routes:

- **(A) Fresh supply.** The return is treated as an outward supply by the retailer, who issues **an
  invoice** for it. The supplier takes credit on that invoice. This route is available where the
  retailer is registered.
- **(B) Delivery challan.** The goods go back under a **delivery challan**, and **the supplier**
  issues the credit note under s.34(1).

Which route applies is a commercial and factual matter between the two parties. It is not derivable
from anything this software holds.

### Drug law on reselling returned medicine

The Drugs and Cosmetics Rules, 1945 contain **no retail rule** governing the resale of a medicine
returned by a customer. The nearest provision is in **Schedule M**, which binds manufacturers:
*"Products returned from the market shall be destroyed unless it is certain that their quality is
satisfactory… Where any doubt arises over the quality of the product, it shall not be considered
suitable for reissue or reuse."* **Rule 110** prohibits the sale of a Schedule C substance after its
expiry date.

So: there is a clear rule for manufacturers and an explicit prohibition on selling expired Schedule C
substances, and **silence** on retail resale of customer returns.

## Decision

### 1. The vocabulary is what the parties actually did, never a GST instrument we assume

The system has exactly one internal concept, `return_document`, discriminated by `return_kind`:

| Kind | What it records | What it is not |
| --- | --- | --- |
| `sales_return` | Goods a customer brought back, and the money refunded | Not automatically a GST credit note |
| `purchase_return` | Goods sent back to a supplier | **Never** a debit note issued by us |

`debit_note` appears nowhere in the schema, the error vocabulary, the contract, or the interface, and
a test asserts its continued absence. On the purchase side the document records which of Circular
72/46/2018-GST's two routes was taken (`gst_route`: `fresh_supply` or `supplier_credit_note`), and
where the supplier issues the credit note, that note is captured as **separate append-only evidence
of what the supplier did** — never as a document of ours.

### 2. Whether GST is reduced is stated by the operator, never inferred

A posted sales return records `tax_adjustment_status`:

- `commercial_only` — the money is refunded and the GST already charged stands.
- `tax_adjustable` — the return is being treated as reducing the GST charged.

The service **validates the combination and refuses an incomplete one**; it does not choose. In
particular it does **not** conclude eligibility from the customer's registration status. A registered
customer does not make a credit note automatic, and an unregistered one does not forbid it: s.34 turns
on the supply, the invoice and the s.34(2) deadline, and on whether the recipient has reversed any
credit taken — facts this software does not hold and must not guess. Presenting a guess as an answer
would be worse than presenting nothing, because it would be acted on.

The screen says so in the operator's own words rather than hiding the choice behind a default.

### 3. The reversal reuses the original's frozen facts, and partials sum exactly

A return line copies the original line's tax snapshot — rate version, basis points, HSN, treatment —
and never re-resolves them. A rate that changed between the sale and the return is irrelevant: the
reversal must be equal and opposite to what was actually charged.

Partial returns use **cumulative-difference reversal**. For each tax component, the amount reversed by
a return of `r` atoms against an original of `Q` atoms with `R₀` already returned is

```
share(R₀ + r) − share(R₀)      where share(n) = round(original_amount × n / Q)
```

computed in `i128`, with ADR-016's half-away-from-zero tie. Because each step is a difference of two
roundings of the same function, **any sequence of partial returns sums exactly to the original** —
there is no residual paise left stranded on the last one. The line total is the sum of the reversed
components, not a sixth independent proportion of the original total.

### 4. Returned stock never becomes sellable by the act of returning it

Every inventory movement carries a `stock_status` of `sellable`, `quarantined` or `non_sellable`, and
balances are derived per status. The counter can only be offered `sellable` quantity.

A sales return may dispose goods to `quarantined` or `non_sellable` only. **`sellable` is not
expressible**: it is absent from the column's CHECK constraint, from the contract enum, and from the
interface. Stock becomes sellable again only through a separate, separately authorised **disposition
transfer** — a paired append-only movement (`−quarantined`, `+sellable`) recorded by a pharmacist with
a stated reason, never a mutation of anything.

**This is project safety policy, not a statutory requirement**, and the distinction matters. The law
surveyed above does not prohibit a pharmacy from reselling a customer return; it simply provides no
rule permitting it either. Given that silence, and given that Schedule M requires a manufacturer to
be *certain* of quality before reissue, the conservative reading is the only defensible one to encode:
a medicine that has left the pharmacy's control does not return to the shelf because a screen defaulted
it there.

Two consequences follow from Rule 110, and both are enforced:

- An **expired lot cannot be quarantined** by a return. Quarantine means "awaiting a judgement that
  might make this sellable", and for an expired lot no such judgement exists. It is written off.
- An **expired lot cannot be released** from quarantine, even if it was quarantined while still good
  and expired while waiting. Quarantine is a one-way door once the date passes.

**A write-off is terminal.** A transfer may start only from `quarantined`. Undoing a judgement that
goods must not be sold is not a side effect of the release door; it would need its own design and its
own audit story, and Phase 1I does not provide one.

### 5. Numbering

Return documents are numbered by the server at the moment of posting, in their own per-kind series
(`SR`, `PR`), from the same `document_number_series` allocator the sales side uses. The format is
`SERIES/YYMM/NNNNNN` — for example `SR/2627/000001` — which is **fourteen characters**, inside both
Rule 46(b)'s and Rule 53(1A)'s sixteen-character limit, with the series, the financial year and the
sequence all still legible. A number is allocated inside the posting transaction and is therefore
never consumed by a posting that fails.

## Consequences

- The purchase-return side can never be mistaken for a tax document of ours, because the schema does
  not contain the word.
- GST adjustability is an auditable operator statement with a recorded reason, not a derived value.
  If the pharmacy's accountant disagrees with a decision, the decision is visible and attributable.
- Quarantined and written-off quantity is real, visible and counted — but never billable. A
  pharmacist can see what is waiting on them, and a cashier can see who it is waiting for.
- The conservative stock rule will occasionally quarantine something a pharmacy would have restocked
  without thinking. That is the intended direction of the error.

## Non-goals

GST return filing, input-tax-credit reversal tracking, the s.34(2) deadline as an enforced control,
supplier reconciliation, accounting ledgers, and correction of a mistaken write-off all remain future
blueprint decisions.

## Sources

Verified against the primary texts, not secondary commentary:

- CGST Act, 2017, s.34(1), s.34(2), s.34(3) — including the s.34(2) proviso as substituted w.e.f.
  1 October 2025 by s.126 of the Finance (No. 7) Act, 2025.
- CGST Rules, 2017, Rule 46(b) and Rule 53(1A).
- CBIC Circular No. 72/46/2018-GST.
- Drugs and Cosmetics Rules, 1945 — Schedule M, and Rule 110.
