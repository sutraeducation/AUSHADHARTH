# ADR-016: Commercial and tax rounding

## Status
Accepted. Completes the deferral ADR-009 recorded in its non-goals: *"Tax rounding, medicine pack
conversion, valuation, and accounting ledgers remain future blueprint decisions."* Prerequisite for
Phase 1G.

## Context

Phase 1G computes money for the first time. Nothing in the repository defines how: there is no
rounding rule in any ADR and no rounding implementation in the service or the frontend. Writing
purchase arithmetic without settling this first would bake an accidental rule into the first
document the system ever posts, and every later reconciliation would inherit it.

The frozen foundations give three fixed points:

- **Money is exact integer paise** (ADR-009). No binary floating point, ever.
- **Tax rates are exact integer basis points**, `100 = 1.00%`, range `0..=10000`
  (`tax_rate_versions`, Phase 1A).
- **The project refuses ambiguity rather than rounding silently.** The frozen `rupeesToPaise`
  rejects input with more than two decimals instead of rounding it.

## Decision

### 1. There is exactly one rounding point in the system

The arithmetic is arranged so that only a single operation can produce a remainder:

```
taxable_value_paise = quantity_packs × rate_per_pack_paise          -- exact, integer × integer
inventory_atoms     = quantity_packs × pack.base_quantity_atoms     -- exact, frozen conversion
component_tax_paise = round(taxable_value_paise × basis_points / 10_000)   -- the only rounding
```

This is the reason Phase 1G defines purchase quantity as an **integer count of the selected Pack**
(see §3). A fractional quantity or a per-base-unit rate would introduce a second rounding point in
the extension step, and every additional rounding point is another place for two systems to
disagree by a paise. One is the minimum the mathematics allows, and it is what this ADR fixes.

### 2. Tie behaviour: half away from zero

```
round(n / d) = (2·|n| mod d ≥ d) ? ⌈|n|/d⌉ : ⌊|n|/d⌋   , carrying the sign of n
```

A fraction of exactly one half rounds away from zero: `0.5 → 1`, `1.5 → 2`, `−0.5 → −1`.

**Why this direction.** The only rounding rule Indian GST law states is section 170 of the CGST Act,
which rounds *the amount of tax, interest, penalty, fine or any other sum payable* to the nearest
rupee, rounding up when the part is fifty paise or more and ignoring it when less. That is a
half-up rule.

Being precise about what that does and does not settle:

- Section 170 governs rounding **to the nearest rupee**, for **sums payable** — a return and
  payment concern.
- It does **not** mandate rounding at the paise level on an invoice line, and no statute this
  project can cite does.

So the paise-level tie-break is a **project decision**, adopted because it points the same way as
the only statutory rounding direction in the domain. It is documented here rather than left
implicit, and it is not presented as a legal requirement.

Banker's rounding (half to even) was rejected: it is statistically attractive but points a
different way from section 170 on exactly the values a reviewer would check by hand, and it would
surprise anyone reconciling against a supplier's invoice.

### 3. Units, fixed explicitly

| Quantity | Unit | Type |
| --- | --- | --- |
| Money | paise | integer |
| Tax rate | basis points, `100 = 1.00%` | integer |
| Inventory quantity | base-unit atoms at the Product's `quantity_scale` | integer |
| Purchase quantity | count of the selected Product Pack | integer |
| **Purchase rate** | **paise per one selected Pack** | integer |

The purchase rate basis is **per Pack**, never per base unit, because that is how a supplier's
invoice quotes it: *5 strips @ ₹42.50* means ₹42.50 for one strip. The API field is named
`ratePerPackPaise` and the UI labels it with the selected Pack, so the basis is never implied.

Stock quantity and price quantity are kept separate on purpose: the same line is *5 packs* to the
supplier and *50 base atoms* to the ledger, and the server derives the second from the first.

### 4. Where rounding happens: per line, per component

Each tax component is rounded independently, on each line:

```
cgst_paise = round(taxable × cgst_basis_points / 10_000)
sgst_paise = round(taxable × sgst_basis_points / 10_000)
igst_paise = round(taxable × igst_basis_points / 10_000)
cess_paise = round(taxable × cess_basis_points / 10_000)
```

Only the components belonging to the resolved treatment are charged; the others are zero (Phase 1G
blueprint §16).

```
line_total_paise = taxable_value_paise + charged components
```

### 5. Document totals are the sum of rounded lines — never re-rounded

```
document.taxable_value_paise = Σ line.taxable_value_paise
document.cgst_paise          = Σ line.cgst_paise
document.sgst_paise          = Σ line.sgst_paise
document.igst_paise          = Σ line.igst_paise
document.cess_paise          = Σ line.cess_paise
document.grand_total_paise   = Σ line.line_total_paise
```

A document total is **never** computed by applying a rate to an aggregate. This makes the
reconciliation question vacuous by construction: there is no second, independently rounded figure
that could disagree with the sum of its parts, so the printed total always equals what the lines
add up to. Any system that rounds an aggregate separately has to define a correction line; this one
does not.

### 6. No rupee-level rounding in Phase 1G

Section 170's rounding to the nearest rupee applies to sums payable. A purchase record stores exact
paise, and nothing in Phase 1G computes a sum payable. Introducing a rupee round-off here would be
inventing statute where none applies to this document.

A supplier's printed invoice often carries its own round-off line, so a recorded grand total may
differ from the supplier's printed total by a few paise. That is a real and expected difference
between two independently computed documents, not an error in either. Recording the supplier's own
round-off figure is a named non-goal of Phase 1G; the schema does not preclude adding it later.

### 7. Arithmetic must not overflow, and must not use floating point

All computation is `i64` integer arithmetic in Rust. The widest intermediate is
`taxable_value_paise × basis_points`. With `basis_points ≤ 10_000` and taxable value bounded by the
frozen `MAX_MRP_PAISE` scale of `100_000_000_000`, the product reaches `10^15`, well inside `i64`'s
`≈9.22 × 10^18`. Inputs are bounded on entry so the product cannot be constructed out of range, and
a checked multiplication refuses rather than wrapping.

`f32` and `f64` appear nowhere in the money path; a test asserts their absence across the service.

### 8. JavaScript is display-only

The browser never computes an authoritative amount. It may format paise for display using exact
integer string arithmetic, exactly as the frozen `paiseToRupees` already does, and it may show a
preview computed the same way — but the server recomputes every figure at posting and stores its
own result. Any amount arriving in a request body is ignored, not trusted; a test proves a spoofed
total changes nothing.

### 9. Persisted snapshots

A posted line stores the computed figures **and** the inputs that produced them: the tax rate
version, its four basis-point components, the resolved treatment, the taxable value, each component
amount, and the line total. Re-deriving a historical document from current masters is therefore
never necessary and never done.

### 10. Corrections

Rounding is deterministic, so recomputing the same inputs yields the same result. A reversal
negates stored amounts rather than recomputing them, which keeps a reversal exactly equal and
opposite to what was posted even if a rate version changes in between. Sign handling is defined in
§2.

## Test vectors

Verified arithmetically and asserted in `domain::money`.

| # | Case | Taxable (paise) | Basis points | Exact | Rounded |
| --- | --- | --- | --- | --- | --- |
| 1 | Exact, no remainder | 10 000 | 600 | 600 | **600** |
| 2 | Below half | 1 | 250 | 0.025 | **0** |
| 3 | Exactly half | 1 | 5 000 | 0.5 | **1** |
| 4 | Exactly half, odd quotient | 3 | 5 000 | 1.5 | **2** |
| 5 | Above half | 1 | 7 500 | 0.75 | **1** |
| 6 | Odd basis points | 12 345 | 250 | 308.625 | **309** |
| 7 | High value | 123 456 789 | 900 | 11 111 111.01 | **11 111 111** |
| 8 | Low value | 7 | 250 | 0.175 | **0** |
| 9 | Nil rate | 50 000 | 0 | 0 | **0** |
| 10 | Negative (reversal) | −1 | 5 000 | −0.5 | **−1** |

Multi-line, where per-line rounding is observable:

| Line | Taxable | bp | Line tax |
| --- | --- | --- | --- |
| A | 1 | 5 000 | 1 |
| B | 1 | 5 000 | 1 |
| **Σ** | **2** | 5 000 | **2** |

Rounding the aggregate instead would give `round(2 × 5000 / 10000) = 1`. The document total is
**2**, because §5 sums the rounded lines. This case is asserted directly, because it is precisely
where a careless implementation diverges.

## Consequences

One rounding point, one tie direction, totals that always equal the sum of their parts, and no
floating point anywhere near money. A reviewer can verify any line by hand.

## Non-goals

Rupee-level round-off on a purchase, the supplier's own round-off line, valuation, costing, selling
price, margin, discount, currency other than INR, and every accounting posting.
