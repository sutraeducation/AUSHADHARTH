# Phase 1H — Sales / POS Foundation

**Status: implementation authority.** Written against the repository at `394c9759` (Phase 1G) and
reconciled against `70d718c9` (Phase 1H-0 medicine price control). Every claim below was checked
against the repository, not against earlier blueprints.

---

## 1. Objective

Record the store's first **outward** commercial transaction: an over-the-counter sale that moves real
stock out, charges real GST, issues a real invoice number and takes real tender — without weakening
the inventory ledger, batch integrity, money precision, GST history, local-first operation, audit
identity, or the Purchase foundation.

## 2. Scope

One canonical **Sale** document, Draft → Posted, **delivered to the customer at the Store**, with or
without an identified customer; outward GST resolved from the product's Tax Category on the sale's
business date; exact integer money on an explicit quantity/rate basis; hard MRP and controlled-price
ceilings; one
negative inventory movement per line with durable provenance; a store-issued invoice number allocated
atomically at posting; single-tender payment evidence.

## 3. Exclusions

Home delivery · interstate dispatch · ship-to address · bill-to/ship-to · delivery challan ·
e-commerce dispatch · **any outward sale where goods are delivered anywhere other than the Store** ·
sales return · void/cancel after post · credit note · reversal endpoint · credit sale and customer
receivable · discount of any kind · scheme/free quantity · margin or pricing engine · stock valuation
· accounting journal · cash or bank ledger · payment settlement · GSTR-1 or any return filing · ITC
computation · loyalty · FEFO/FIFO automatic batch selection · printing engine · scanner device
integration.

## 4. Dependencies (verified present at `394c9759`)

| Needed | Exists as | Reusable unchanged? |
|---|---|---|
| Product / Pack / base-unit atoms | `products.quantity_scale`, `product_packs.base_quantity_atoms` | yes |
| Sale permission per pack | `store_pack_policies.sale_enabled` | yes |
| Sale granularity / pack-break control | `minimum_sale_increment_atoms` | yes — **designed for this** |
| Sub-base-unit permission | `fractional_sale_allowed` | yes — see §12.1, which corrects an earlier misreading |
| POS pack default | `default_sale_pack` | yes |
| Batch identity, expiry, MRP | `product_batches.batch_number / expires_on / mrp_paise` | yes |
| Tax classification | `products.hsn_code_id`, `products.tax_category_id` | yes |
| Rate by date | `domain::taxation::resolve_tax_rate(category, on_date)` | yes — deliberately treatment-agnostic |
| Store place of supply | `store_identity` (Phase 1G-0) | yes |
| Party + customer role | `party_roles.role CHECK (role IN ('supplier','customer'))` | **schema already admits `customer`** |
| Money, checked arithmetic, rounding | `domain::money` + ADR-016 | yes — see §18 |
| Ledger, append-only, non-negative stock | `inventory_movements` + `BEGIN IMMEDIATE` balance read | yes |
| Barcode lookup | `GET /api/v1/barcodes/resolve` | yes — **no second barcode model** |
| Business date | `apps/web/src/platform/businessDate.ts` | yes |
| Real-service gate | `tests/real_service_gate.rs` | yes |

| Medicine price control | `price_control_status`, `resolve_price_ceiling`, `compare_basis` (Phase 1H-0) | yes — see §15.4 |

**Absent from the foundation, and therefore designed here:** document numbering (§7). It is the only
mechanism Phase 1H must build from nothing.

## 5. Customer / walk-in model

**Both, and a walk-in is not a Party.**

- `sale_documents.customer_party_id` is **nullable**. NULL means walk-in. No placeholder Party, no
  shared "Cash Customer" master row — inventing one would pollute the party master and make "how many
  customers do we have" meaningless.
- Non-NULL requires an **active `customer` role**, mirroring the supplier-eligibility rule Phase 1G
  enforces for purchases.
- A walk-in may carry a free-text `customer_name_snapshot` on the document. That is invoice text,
  never identity, and never creates a master record.
- Enabling the role: `party_roles` already permits `'customer'`; only
  `domain::parties::SUPPORTED_PARTY_ROLES` (today `[&str; 1] = ["supplier"]`) refuses it, with a
  comment saying it "arrives with the sales phase". **No migration to `party_roles` is required.**
- The frozen test `domain::parties::only_the_supplier_role_is_serviceable_in_this_phase` asserts
  `!SUPPORTED_PARTY_ROLES.contains("customer")`. Phase 1H **deliberately rewrites that test** to
  assert the new two-role set. Recorded here so it is an explicit phase decision and can never be
  mistaken for an accidental loosening.
- **Supplier validation is not weakened.** Party-role eligibility stays a closed set; purchase
  posting continues to require an active *supplier* role; sale posting requires an active *customer*
  role. Enabling one role must not make the other optional, and the design audit checks this.

## 6. Sale lifecycle

```
DRAFT    editable · revision-controlled · NO stock effect · NO invoice number · NO tax snapshot
  │  post  (a named command, never a status assignment)
POSTED   immutable · numbered · snapshotted · stock out · tender frozen
```

Posted sales are frozen by `BEFORE UPDATE/DELETE` triggers exactly as `purchase_documents` is. There
is no void, no cancel, no status dropdown, no credit note and no reversal endpoint in Phase 1H.
Correction is a future compensating document (§36), which posted-immutability is what makes possible.

## 7. Document / invoice number — frozen rules

A supplier invoice number is *data the supplier gave us*; a sale invoice number is *ours to issue*,
and issuing it wrongly is a compliance failure. All seven attributes are frozen here:

1. **Sequence scope:** `(store_id, series_code, financial_year)`.
2. **Store scope:** every series belongs to exactly one `store_id`; the store is resolved from the
   installation, never from the browser.
3. **Numbering period:** Indian financial year, 1 April – 31 March, derived **from the sale's business
   date** (§8), never from the clock: `month >= 4 → "YYYY-(YY+1)"`, else `"(YYYY-1)-YY"`. Stored on
   the document as a literal so it can never be recomputed differently later.
4. **Prefix and statutory length:** a `series_code` per store (default `INV`), stored on the series
   row and snapshotted onto the document. Rendered form `{prefix}/{compact financial year}/{zero-padded
   sequence}`, e.g. `INV/2627/000148` — **fifteen characters**.

   **Rule 46(b) of the CGST Rules caps a tax invoice serial at sixteen characters**, and Rule 53(1A)(c)
   caps a credit or debit note at the same. The form first shipped in Phase 1H, `INV/2026-27/000148`,
   was eighteen and therefore non-conforming; Phase 1H-C1 corrected it before any real invoice was
   issued. The **stored** financial year is unchanged and is still the full business fact `2026-27` on
   both the series row and the document — only the rendered serial is compact, because the full form
   does not fit. `domain::sales::document_serial` is the single renderer and **refuses** rather than
   issues a serial that would breach the limit: an invoice number is permanent once issued, and a
   non-conforming one cannot be put right by re-issuing it.
5. **Reset behaviour:** the sequence restarts at 1 for each new `(store, series, financial_year)`
   scope — a new scope row is created on first use inside the posting transaction.
6. **Uniqueness:** a unique index on `(store_id, series_code, financial_year, sequence_value)` and a
   second on the rendered `document_number`. The database, not the service, is the authority.
7. **Idempotent replay:** a replay of the same posting key returns the original document and
   allocates **nothing** — no new sequence value, no new rendered number.

**Allocation mechanism.** A `document_number_series` row per scope holds `next_value`. Allocation is
`UPDATE document_number_series SET next_value = next_value + 1 WHERE ...` followed by a `SELECT` of
that row, **both inside the same `BEGIN IMMEDIATE` transaction as the posting**. Never
`MAX(number)+1`. `RETURNING` is **not** assumed — it appears nowhere in the codebase today, so the
design does not depend on it; the update-then-select pair is equally atomic under the write lock the
posting already holds.

**Gaps.** Because allocation happens inside the posting transaction, a refused posting rolls the
counter back with everything else: **a failed posting consumes no number**, and the series has no
gaps. The browser never generates, suggests, reserves or displays a provisional number.

## 8. Business date

The Phase 1F UTC defect must not recur. The display default comes from `businessToday()` (workstation
local calendar date). The sale's business date is an explicit `YYYY-MM-DD` the operator may change;
the **server validates it and uses it** for tax-rate resolution (§17), financial-year derivation (§7),
expiry comparison (§11) and the movement's `occurred_on`. The server never derives the business date
from `date('now')` in UTC.

## 9. Product / Pack

A line names a Product and one of its Packs. The Pack must have an active `store_pack_policies` row
with `sale_enabled = 1`. `default_sale_pack` seeds the POS default. A pack belonging to another
product is refused, exactly as in Purchase.

## 10. Batch

A line references a real `product_batches` row belonging to the selected Pack. Phase 1H requires the
operator to **choose explicitly** — no FEFO/FIFO automation, which is its own design. The chooser
shows batch number, **expiry**, **available atoms** and **MRP**, ordered soonest-expiry-first as a
presentation aid only, never as an automatic selection.

## 11. Expired batch — hard block

`expires_on < business_date` → typed refusal `batch_expired`. Not a warning, not click-through. The
draft is left intact.

Attribution, stated precisely and without inventing statutory claims:

- **Law/regulation:** sale of an expired drug is prohibited under the Drugs and Cosmetics Act, 1940.
  This is the substantive reason for medicines.
- **Project safety policy:** applying the same hard block to **every** batch-bearing product
  regardless of `product_kind`, and refusing rather than warning, is *our* decision. It is stricter
  than a general-goods POS requires, and we prefer that.

**Near-expiry is display only** in Phase 1H: the expiry date is shown prominently in the chooser and
on the line. No threshold, no blocking, no configurable window — a near-expiry policy needs its own
settings and its own design.

## 12. Quantity authority

The server derives `quantity_atoms` and every downstream figure. The browser never supplies atoms,
taxable value, tax components or totals. Availability, the movement delta and the ledger all use the
server-derived atoms.

## 13. Dual-basis sale quantity — APPROVED

A sale line carries an explicit, stored **`quantity_basis`**. There is no implicit basis and no
generic "quantity" anywhere in the model or the UI.

### Basis A — `pack`

| | |
|---|---|
| quantity authority | integer selected-Pack count |
| rate authority | integer paise **per selected Pack** |
| taxable value | `quantity_packs × selling_rate_per_pack_paise` |
| inventory atoms | `quantity_packs × base_quantity_atoms` |
| permitted when | active policy with `sale_enabled = 1` |

### Basis B — `base_unit`

| | |
|---|---|
| quantity authority | integer **base-unit atoms** |
| rate authority | integer paise **per base atom** |
| taxable value | `quantity_atoms × selling_rate_per_atom_paise` |
| inventory atoms | the entered atoms |
| permitted when | `sale_enabled = 1` **and** `quantity_atoms % minimum_sale_increment_atoms = 0`; `fractional_sale_allowed = 1` is required **only** when the quantity is not aligned to one whole base unit (§12.1) |

Every one of these is exact **checked** integer multiplication, reusing `domain::money`'s existing
checked helpers and its `quantity_atoms` guard.

**Decimal pack quantities are impossible by construction.** There is no `0.3 strip`; selling three
tablets from a strip of ten is `basis = base_unit, quantity_atoms = 3`, and only where the frozen Pack
policy permits it. A pharmacy that never breaks a strip sets its increment to a whole strip (§12.1).

### 12.1 Breaking a Pack and selling a fraction of a base unit are different things

An earlier draft of this section said Basis B is "permitted when `fractional_sale_allowed = 1`". That
was a misreading of the frozen Phase 1B model, and taken literally it would have made the commonest
transaction in an Indian pharmacy impossible. The corrected rule is below; the old wording appears
nowhere else in this document.

**Why the old rule could not work.** `0002_reference_foundations.sql` seeds Tablet, Capsule and Piece
with `is_discrete = 1, allowed_scale = 0`. `products_unit_compatibility_insert` then pins any Product
on a discrete base unit to `quantity_scale = 0`, and `store_pack_policies_quantity_insert` raises
`pack_policy_conflict` unless `quantity_scale > 0 OR fractional_sale_allowed = 0`. A tablet can
therefore **never** carry `fractional_sale_allowed = 1` — the Product-catalog UI already hard-codes
this (`ProductCatalog.tsx`: `fractionalSaleAllowed: product.quantityScale > 0 && values.fractional`).
Gating loose sale on that flag would have made "three tablets from a strip of ten" unreachable for
every tablet and capsule in the catalog.

**The two concepts, separated.**

| concept | example | governed by |
|---|---|---|
| breaking a Pack | strip of 10 → sell 3 tablets | `minimum_sale_increment_atoms` |
| a fraction of a base unit | 0.5 ml of a syrup at `quantity_scale = 1` | `fractional_sale_allowed` |

Three tablets are **three whole base units**, not a fraction of one.

**The authoritative rule.** With `atoms_per_base_unit = 10 ^ quantity_scale`, a Basis B line is
permitted when all of:

1. `sale_enabled = 1` on an active Store Pack Policy;
2. `quantity_atoms > 0` (and within `MAX_SALE_QUANTITY_ATOMS`);
3. `quantity_atoms % minimum_sale_increment_atoms = 0`;
4. **only if** `quantity_atoms % atoms_per_base_unit <> 0`, then `fractional_sale_allowed = 1`.

The granularity is evaluated before the permission, so a store cannot be talked past its own
increment by holding the other flag.

**Whole-Pack-only sale is expressible, and always was.** A store that never breaks a strip sets
`minimum_sale_increment_atoms = base_quantity_atoms`; three tablets are then refused with
`quantity_increment_violation` while ten are accepted. This is an explicit configuration rather than
a side effect of a permission that means something else.

**Nothing in Phase 1B changes.** `fractional_sale_allowed` keeps its frozen meaning (genuine
sub-base-unit precision), `minimum_sale_increment_atoms` keeps its frozen meaning (operational sale
granularity), and `base_quantity_atoms` keeps its frozen meaning (Pack containment). The correction
is to how Sales *reads* those fields, not to what they mean. The Phase 1B triggers are untouched.

### Input bounds (new constants, mirroring Phase 1G)

```
MAX_SALE_QUANTITY_PACKS     = 1_000_000                (reuses money::MAX_QUANTITY_PACKS)
MAX_SALE_QUANTITY_ATOMS     = 9_000_000_000_000_000    (the ledger's own CHECK bound)
MAX_SALE_RATE_PAISE         = 100_000_000_000          (reuses money::MAX_RATE_PER_PACK_PAISE)
MAX_SALE_LINE_TAXABLE_PAISE = 1_000_000_000_000_000
```

**Why the atom bound is the ledger's bound and not something tighter.** `products.quantity_scale`
may be up to 6, so atoms are not always whole units: at scale 6 a single base unit *is* 1 000 000
atoms. A flat "one million atoms" cap would therefore limit a scale-6 product to **one unit per
line**, which would be a bug invented by the bound rather than a real constraint. The atom quantity
is bounded by the same `±9 × 10¹⁵` the inventory ledger already enforces, and nothing tighter.

The line is instead bounded where it actually matters — on money. `taxable_value_paise` is computed
with **checked** `i64` multiplication and must then satisfy `MAX_SALE_LINE_TAXABLE_PAISE`; an
overflow or a breach is a typed refusal, never a wrap and never a silent truncation.

## 14. Commercial rate basis

**The rate is always expressed in the same basis as the quantity, and the two are multiplied
exactly. No division ever occurs in the commercial money path.**

**Why a per-pack rate with an atom quantity was rejected.** It forces
`atoms × rate_per_pack ÷ base_quantity_atoms`. A strip of 15 at ₹95.50 gives 9550 ÷ 15 = 636.67 paise
per tablet — not an integer. Apportioning would either silently lose paise or introduce a **second**
rounding point, and ADR-016 permits exactly one.

**Why deriving the rate from MRP was rejected.** The same division, plus it conflates a statutory
ceiling with a commercial price (§16).

**Consequence, accepted openly:** to sell loose units the operator enters a per-unit price. The POS
may *display* an informational pro-rata reference (§20) but the stored authoritative rate is always
the integer paise the operator confirmed.

## 15. MRP ceiling — HARD BLOCK

Selling above the printed maximum retail price is prohibited under the Legal Metrology (Packaged
Commodities) Rules, 2011. Phase 1H enforces this as a **refusal**, never a warning.

The rule is: **the line's selling amount must not exceed the pro-rata share of the batch's pack MRP.**

### 15.0 The tax basis — corrected

**MRP is a GST-inclusive printed price** (Legal Metrology); the sale line's **selling rate is
GST-exclusive** (it is the taxable value, with tax added on top). Comparing the two directly is
comparing unlike quantities.

An earlier draft of this section compared `rate_per_pack_paise ≤ batch_mrp_paise`. That is **wrong
and unsafe**: with MRP ₹95.50 including 12% GST and a selling rate of ₹95.50, the customer pays
95.50 + 11.46 = **₹106.96, which is ₹11.46 above the printed MRP — while passing the check.** That is
exactly the breach this rule exists to prevent.

The comparison is therefore made against the **GST-inclusive line total**, which is already computed
and stored, so no new division and no new rounding is introduced and ADR-016 is untouched.

### 15.1 Basis A (`pack`)

```
line_total_paise  ≤  batch_mrp_paise × quantity_packs
```

### 15.2 Basis B (`base_unit`) — exact cross-product

The pro-rata share of the pack MRP for the atoms sold, without ever forming a per-atom MRP:

```
line_total_paise × base_quantity_atoms  ≤  batch_mrp_paise × quantity_atoms
```

The quantity does **not** cancel here, because GST rounding happens once per line rather than per
unit, so `line_total` is not exactly proportional to the atoms sold. The cross-product form is
therefore retained — and it is still exact, with no division and no invented per-atom MRP.

Worked example: pack of 15, pack MRP 9550 paise, selling 3 tablets at 12% GST. A rate of 568
paise/tablet gives a line total of `1704 + 204 = 1908`, and `1908 × 15 = 28 620 ≤ 9550 × 3 = 28 650`
— it passes with 30 paise to spare. At 569 the line total is `1707 + 204 = 1911`, and
`1911 × 15 = 28 665 > 28 650` — refused. A rounded "MRP per tablet" would have mis-stated this
ceiling in either direction, which is precisely why early rounding is forbidden here.

### 15.3 Overflow audit, before choosing the integer width

Schema bounds (frozen): `base_quantity_atoms ≤ 9 × 10¹⁵`, `mrp_paise ≤ 10¹¹`, ledger atoms
`≤ 9 × 10¹⁵`. Phase 1H input bounds (§13): `rate ≤ 10¹¹`, `packs ≤ 10⁶`, `atoms ≤ 9 × 10¹⁵`,
`taxable ≤ 10¹⁵`.

| expression | worst case | fits `i64` (9.22×10¹⁸)? | how it is handled |
|---|---|---|---|
| §15.1 comparison `rate ≤ mrp` | 10¹¹ | yes | plain `i64`, no multiplication at all |
| §15.2 `rate × base_quantity_atoms` | 10¹¹ × 9×10¹⁵ = **9×10²⁶** | **no** — and these are *valid* inputs | **widened to `i128`** (margin ≈ 1.9×10¹¹) |
| taxable, basis A | 10⁶ × 10¹¹ = 10¹⁷ | yes | `checked_mul`, then `≤ MAX_SALE_LINE_TAXABLE_PAISE` |
| taxable, basis B | 9×10¹⁵ × 10¹¹ = 9×10²⁶ | **no** for extreme inputs | `checked_mul` refuses before any bound test; the taxable cap then applies |
| `packs × base_quantity_atoms` | 10⁶ × 9×10¹⁵ = 9×10²¹ | **no** | already guarded by the frozen `money::quantity_atoms` checked helper plus the ledger CHECK |
| document total | ≤ 10¹⁵ × lines | yes for realistic bills | `money::sum_lines` already uses `checked_add` |

**Decision: the §15.2 comparison is evaluated in `i128`**, which Rust provides natively, because its
worst case exceeds `i64` for *schema-legal* inputs — it is not an extreme-input concern that checked
arithmetic could simply refuse. The operands are widened from `i64` and the multiplication is still
`checked_mul`, so a future bound change cannot silently wrap.

**Everything else stays `i64` with checked operations**, where an overflow is a typed refusal rather
than a wrap. The uncancelled cross-product the audit started from would have reached
`9×10¹⁵ × 10¹¹ × 9×10¹⁵ = 8.1×10⁴²`, which **exceeds `i128` as well** — the cancellation in §15.2 is
therefore not merely tidier, it is what makes the comparison representable at all.

### 15.4 Medicine price control — ENFORCED, on the Phase 1H-0 foundation

An earlier draft of this section recorded that no ceiling-price foundation existed. **Phase 1H-0
(`70d718c9`) built it**, so Phase 1H enforces the controlled ceiling as well as the MRP ceiling.
The names below are the committed ones, not proposals:

| concern | committed surface |
|---|---|
| applicability | `products.price_control_status` ∈ `unknown` / `not_applicable` / `controlled` |
| mapping | `products.controlled_formulation_id` — explicit assignment, never inferred |
| resolution | `domain::price_control::resolve_price_ceiling(executor, formulation_id, on_date)` |
| comparability | `compare_basis(&ceiling, product_base_unit_id) -> Comparability` |
| pack-basis check | `per_pack_rate_within_ceiling(rate, scale, base_quantity_atoms, ceiling)` |
| atom-basis check | `per_atom_rate_within_ceiling(rate, scale, ceiling)` |

Both checks are exact `i128` comparisons that multiply rather than divide, so no rounded per-unit
ceiling is ever invented.

**Posting rules.** When `price_control_status = 'controlled'`:

- no ceiling resolves on the business date → refuse (`price_control_unresolved`);
- a ceiling resolves but `compare_basis` is not `Comparable` → refuse (`price_control_incomparable`);
- the selling rate exceeds the ceiling → refuse (`selling_rate_above_ceiling`).

**Never silently fall back to the MRP rule.** When the status is `unknown`, the sale proceeds under
the MRP rule alone and the status is **snapshotted onto the posted line**, so the gap is auditable
rather than invisible (§28).

**Tax basis, restated because it is the trap.** A DPCO ceiling is **exclusive** of GST and is
therefore compared against the line's **taxable value**; a Batch MRP is **inclusive** and is compared
against the line's **GST-inclusive total** (§15.0–15.2). The two ceilings are never compared with
each other — both are enforced, which yields the stricter maximum without placing them on one axis.

**The application enforces the reference data that has been configured.** It does not certify that
manually entered NPPA/DPCO data is complete or current, and Phase 1H performs no online lookup: a
sale posts with no internet.

### 15.5 When MRP is unknown

`product_batches.mrp_paise` is nullable. When it is NULL there is **no known ceiling**, so no ceiling
check is possible. Phase 1H does not invent one and does not refuse the sale; the POS marks the line
"no MRP recorded for this batch" so the operator sees exactly why no ceiling applied. Inventing a
ceiling from another batch or from a sibling pack would be fabricating a legal fact.

### 15.6 Refusal behaviour

A breach returns a typed, safe error (`selling_rate_above_mrp`) whose message says the selling amount
exceeds the permitted MRP ceiling for the batch. The service **never** silently adjusts the
operator's rate, never rounds it down, never posts anyway. The draft is left exactly as it was.

## 16. Selling rate vs MRP — domain distinction

Selling Rate is **commercial input**. MRP is **reference and legal maximum**. They are different
fields with different authority and are never the same value by construction. The selling rate is
**not auto-filled from MRP**; a convenience default may be evaluated in a later phase but must not
blur the distinction. No margin engine, no derived pricing.

## 17. Outward GST treatment — counter sale only

**Phase 1H supports only over-the-counter / store-delivery Sales for which delivery is completed at
the Store. Delivery and shipping outward supplies are outside the Phase 1H transaction model.**

For the supported transaction, the goods are handed to the customer at the store, so the supply does
not involve movement of goods by either party; under IGST Act s.10(1)(c) the place of supply is the
location of the goods at the time of delivery — **the Store**. Therefore, for the supported Phase 1H
transaction:

- place of supply = Store location;
- treatment for a taxable line = **intra-State CGST + SGST**;
- the customer's State is **not required**, which removes the "unknown customer State" problem
  rather than guessing around it;
- the store's place of supply **is** required — posting without it is refused with the existing
  `store_tax_profile_incomplete`.

**This is a scope statement, not a universal claim.** It is *not* true that all retail sales are
always intra-State. It is true that *this* transaction model only covers sales delivered at the
store, and that for those the place of supply is the store.

An identified customer's GSTIN and State **are recorded historically** on the document (so a B2B
buyer has the document they need), but recording them **does not convert a store-counter sale into an
inter-State delivery workflow** and does not change the treatment.

**A future transaction that involves movement or delivery to another place must not be forced through
this Sale model with a false intra-State treatment.** It belongs to a later delivery / inter-State
Sales extension with its own place-of-supply resolution, its own document type and its own blueprint.

Rate components come from `resolve_tax_rate(product.tax_category_id, business_date)`. Missing
classification and missing rate on the date are **distinct typed refusals** and are never treated as
0%, exactly as Phase 1G established. Exempt, nil-rated and non-GST are named, never shown as 0%.

## 18. Rounding — ADR-016 reused, verified not assumed

- quantity is an integer in its basis; rate is integer paise in the same basis (§13/§14) → taxable
  value is an exact integer product with **no division**;
- no discount (§19) → no apportionment of any kind;
- tax components: `round(taxable × basis_points ÷ 10 000)`, half away from zero — **one** rounding
  point per component, and it is the *only* division in the money path;
- document totals = the sum of already-rounded line values — **no** second aggregate rounding;
- **no rupee round-off line** in Phase 1H.

The two conditions ADR-016 warned would break its assumptions — discounts, and fractional quantities
with an apportioned price — are both avoided by §13, §14 and §19.

**The §15 MRP ceiling comparison is a validation rule, not a second commercial-money rounding point.**
It produces no stored value, changes no amount, and performs no division or rounding whatsoever.

## 19. Discount — excluded

**No line discount. No document discount. No coupon. No scheme discount.** A discount changes
taxable-value semantics (line vs document level, pre- vs post-tax, proportional apportionment across
lines) and re-opens ADR-016's rounding analysis. It requires a separate commercial-pricing
architecture. Recording the sale exactly without discount is strictly more useful than recording it
approximately with one.

## 20. Basis and MRP presentation — UX rules

The operator must always know what `₹X` means. Generic "Quantity" and "Rate" labels are **forbidden**
wherever the basis could be misread.

| | Basis A — `pack` | Basis B — `base_unit` |
|---|---|---|
| quantity label | `Quantity (Strip)` — the pack's own unit label | `Quantity (Tablet)` — the base unit's label |
| rate label | `Selling Rate / Strip` | `Selling Rate / Tablet` |
| MRP shown | `Batch MRP / Strip` | `Batch MRP / Strip` (the pack MRP, unchanged) |

Labels are built from the real unit names (`product_packs.display_label`, the base
`units_of_measure`), never hardcoded to "Strip"/"Tablet".

In Basis B the POS may additionally show an **informational** pro-rata reference derived from pack
MRP, clearly marked as indicative. It is **never stored**, never treated as price authority, and
never auto-filled into the selling rate. The Batch MRP per pack remains the authoritative reference
fact, and §15.2 remains the authoritative ceiling test.

## 21. Non-negative stock, aggregation and concurrency

Frozen Phase 1D policy is preserved with **no opt-out and no configuration toggle**.

Inside the posting transaction, after `BEGIN IMMEDIATE`:

1. **Aggregate first.** Sum the requested atoms per `(store_id, product_pack_id, batch_id)` across
   **all** lines of the sale. Two individually-valid lines on the same batch must never jointly drive
   the balance negative.
2. For each aggregated stock identity, read the balance under the write lock:
   `COALESCE(SUM(quantity_delta_atoms),0) FROM inventory_movements WHERE store_id=? AND product_pack_id=? AND batch_id IS ?`
3. Refuse with `insufficient_stock` (carrying `availableAtoms`) if `available − requested < 0`.

Because the write lock is taken before any balance is read, two counters cannot both pass the check
for the same last strip; the loser is refused, never queued and applied afterwards.

## 22. Atomic posting

One `BEGIN IMMEDIATE` transaction covers, in order: final draft validation → revision check →
idempotency/replay decision → store snapshot → customer snapshot → per-line pack/policy/basis
validation → expiry check (§11) → MRP ceiling check (§15) → aggregated availability check (§21) → tax
resolution → money computation → line snapshots → **invoice number allocation (§7)** → negative
inventory movements → tender rows → status transition → audit event.

Any failure rolls back **everything**, including the allocated number, every materialised row and
every movement.

## 23. Idempotency

Same key + same logical payload → the original posted document, no second sequence value, no second
movement, no second tender row. Same key on different facts → typed `idempotency_conflict`. A unique
index on `posting_idempotency_key` enforces it in the database.

Two Phase 1G lessons carried forward explicitly:

- the error mapper must match the offending index's **columns**, because SQLite reports columns, not
  index names;
- the posting fingerprint must be computed **only over fields posting does not rewrite** — hashing
  anything posting mutates makes a successful retry look like a conflict.

## 24. Tender / payment evidence

`sale_tenders` child table: `sale_id`, `method` ∈ `cash | card | upi`, `amount_paise`, optional
`reference_text`. **Phase 1H validates exactly one row per sale** — single tender.

The repository offers no evidence that split tender is essential to a first counter foundation, and
the minimum safe scope is preferred. The table is nevertheless a **child table from the start**, so
split tender later becomes a validation change rather than migration surgery.

These are **operational evidence only**: no cash ledger, no bank ledger, no settlement, no
receivable, no accounting posting. The tender total must equal the invoice total — part payment would
be credit, which is excluded (§25).

## 25. Credit sale — excluded

**No credit sale. No customer receivable. No outstanding balance. No ageing. No collection
workflow.** Each of these requires a receivable foundation and an accounting boundary that Phase 1H
deliberately does not open.

## 26. Inventory movement

- New movement type **`sale`**, added to the `movement_type` CHECK by the same table-rebuild pattern
  migration 0011 used — every existing index and trigger reproduced verbatim, proven by re-running
  the frozen Phase 1D integrity tests after a **forced recompilation**.
- New provenance column `sale_line_id TEXT REFERENCES sale_lines(id) ON DELETE RESTRICT`, with the
  mirrored CHECK pair: a `sale` movement **must** carry one, and no other movement type **may**.
- `quantity_delta_atoms` is **negative**.
- Append-only triggers, the reversal unique index and the non-negative invariant all continue to
  apply untouched. **No mutable `current_stock` column is introduced anywhere.**

## 27. Authorization

Posting a sale is counter work: **`cashier`, `pharmacist` and `owner_admin`** may create, edit and
post a sale — unlike Purchase, which is `owner_admin` only. Reading requires a session. Every mutation
keeps the frozen Host/Origin protection. The actor comes from the authenticated server session; the
browser can never supply it, and the store is resolved from the installation.

## 28. API (shape only)

```
GET    /api/v1/sales                       ?status=draft|posted|all&customerPartyId=&from=&to=
POST   /api/v1/sales                       {customerPartyId?, customerName?, businessDate}
GET    /api/v1/sales/{id}
PUT    /api/v1/sales/{id}                  {expectedRevision, ...header}
POST   /api/v1/sales/{id}/lines            {expectedRevision, productId, productPackId, batchId,
                                            quantityBasis, quantityPacks|quantityAtoms, ratePaise}
PUT    /api/v1/sale-lines/{id}
DELETE /api/v1/sale-lines/{id}             {expectedRevision}
POST   /api/v1/sales/{id}/post             {expectedRevision, idempotencyKey,
                                            tenders:[{method, amountPaise, reference?}]}
GET    /api/v1/sales/{id}/quote            what this draft would come to — see below
GET    /api/v1/packs/{id}/sellable-batches batch, expiry, available atoms, MRP — for the chooser
```

**`/quote` was added during implementation.** §29 requires the tender due to be permanently visible,
and that cannot be satisfied without it: the tax is resolved from the rate in force on the business
date, which only the Store Service knows, and a browser that added the tax up itself would be a second
implementation of the money path — the exact defect §15.0 exists to prevent.

The endpoint reuses `resolve_and_compute`, the very function posting uses, so a quote and the invoice
that follows it cannot disagree. It allocates no number, writes no snapshot and moves no stock, and a
line that posting would refuse is refused here too with the same typed code — so an above-MRP price is
found while the customer is still at the counter rather than after the cash is in the drawer.

**Live display names on a draft line.** The four identity snapshots (`productDisplayName`,
`packDisplayLabel`, `baseUnitLabel`, `batchNumber`) are NULL until posting freezes them, which is
correct: a snapshot is what was true when the invoice was issued, and a draft has issued nothing. But
the counter still has to read its own bill while building it. `GET /api/v1/sales/{id}` therefore also
returns `currentProductDisplayName`, `currentPackDisplayLabel`, `currentBaseUnitLabel` and
`currentBatchNumber`, joined live from the catalogue and written nowhere. The browser preview found
this: before it, a draft bill showed a raw UUID for the item and the word "Chosen" for the batch.

## 29. POS workflow

The hard requirement: **materially faster than Purchase entry, keyboard-first, no modal per line.**

```
[ scan or search ] → pack (defaulted) → batch → qty → rate → Enter
        ↑                                                      │
        └───────────── focus returns to search ────────────────┘
```

- A single always-focused search/scan field. A scanned barcode ends in Enter, and that Enter takes the
  match rather than submitting an incomplete line — before it was handled, a scan produced "Find the
  product being sold" for a product the operator had just found.
- Lines are entered **inline in the table**, not in a dialog. The Phase 1G dialog suits an admin form;
  it does not suit a counter.
- A Product whose only active Pack is unambiguous selects that Pack itself; with several the operator
  still chooses, because guessing which presentation a customer asked for would be inventing an
  answer. The batch chooser is one keystroke and is stock-aware, and **nothing about a lot is ever
  auto-selected**: which lot leaves the shelf is a real decision with an expiry attached to it.
- Basis-correct labels at all times (§20).
- Running total, line count and tender due permanently visible.
- Tender and post at the end, one confirmation, with the synchronous double-submit guard Phase 1G
  proved necessary.
- Acceptance target, measured in a real browser as Phase 1G's ten-line run was: **a ten-line bill
  entered with no mouse.**

## 30. Barcode integration point

Reuse `GET /api/v1/barcodes/resolve` exactly as it stands — it already resolves
`(namespace, value, scope, store)` to a pack, honouring store-scoped and global barcodes. **No second
barcode identity system.** A scan resolves the **Pack**; **batch selection remains a separate
stock-aware step** whenever more than one batch is available, because a barcode identifies a pack,
not a lot. Device configuration and scanner-specific UX are deferred without changing this
integration point.

## 31. Snapshots on a posted sale

**Store:** legal/display name, GSTIN, registration status, State code.
**Customer (identified):** party id, display name, GSTIN, registration status, State code — so a later
rename or deregistration cannot rewrite the invoice. **Walk-in:** all NULL plus optional free text.
**Document:** invoice number and its parts, series code, financial year, business date, treatment,
taxable value, CGST/SGST/cess, grand total, posted-by user, posted-at.
**Line:** product id **and display name**, pack id **and label**, base-unit label, batch id, **batch
number**, expiry, **MRP at sale**, HSN code id and **literal code**, tax category id, treatment kind,
tax rate version id, all four basis-point components, **`quantity_basis`**, quantity in that basis,
derived atoms, rate in that basis, taxable value, each tax component, line total.

Product, pack and unit **display names are snapshotted** — Purchase resolves those live, which is fine
for an internal document but not for an outward invoice that must be reprintable years later exactly
as issued.

## 32. Real-service tests

Extend `tests/real_service_gate.rs` over real HTTP: seed → sell a whole pack → sell loose units →
refuse a decimal pack quantity → refuse an increment violation on a pack whose increment is a whole
strip → refuse a sub-base-unit quantity on a measurable pack that forbids it → refuse an expired batch → refuse a rate above the MRP ceiling on both bases →
refuse overselling, including two lines on one batch that only fail when aggregated → post → verify
the invoice number, financial year, snapshots, exact tax, negative atoms, provenance and derived
balance → replay the key and prove no second number → reuse the key on another sale → force a
mid-write failure and prove no number, no movement and no tender survive.

## 33. Browser usability acceptance

Real built frontend + real router + disposable database, never the customer database. Ten-line bill
entered through the UI **with no mouse**; both bases exercised with correct labels; MRP ceiling and
expiry refusals seen; every GST case visible; 390px and keyboard passes; teardown deletes the harness
and the disposable database, and nothing from the preview is ever tracked.

## 34. Hardening plan

Mutation proofs, one at a time, each restored byte-identically and checksum-verified: posted-sale
header/line immutability and delete protection · invoice-number allocation atomicity, no-gap
behaviour and replay non-allocation · MRP ceiling (both bases, including the early-rounding trap of
§15.2) · increment and fractional-permission enforcement · expired-batch block · tax spoof resistance
· atom authority · store/actor authority · rounding direction · idempotency replay · atomic rollback
with a genuine mid-write failure · aggregated availability · customer/store/product snapshot
independence · `sale_line_id` provenance CHECK pair · non-negative stock · and the frozen Phase 1D
append-only guarantee re-proven after the movement-type rebuild, with forced recompilation.

## 35. Migration strategy

One migration `0012_sales_outward.sql`: `sale_documents`, `sale_lines`, `sale_tenders`,
`document_number_series`; rebuild `inventory_movements` to add `'sale'` and `sale_line_id`,
reproducing every existing object verbatim; extend `master_change_events` entity and action domains.
`sqlx::migrate!` embeds migrations at **compile time** — force recompilation before trusting any
migration-dependent result.

## 36. Accounting boundary

Excluded: journal, cash ledger, bank ledger, receivable, settlement, GSTR-1, ITC. Preserved so they
can be added later **without touching a posted sale**: immutable line-level HSN, tax category,
treatment kind, rate version, basis points, taxable value and each tax component; immutable tender
rows; immutable document number and financial year; append-only audit events.

## 37. Return / reversal boundary

Not implemented. The architecture must never require mutating an original sale: a future Sales Return
is a **separate compensating document** referencing the original sale and its lines, producing its own
**positive** inventory movements and its own number from its own series. Posted-sale
immutability is what makes that possible, which is why it is enforced from day one.

> **Implemented in Phase 1I**, which supersedes this paragraph in one respect: the words above call
> the return's number a *credit-note* number. Under CGST s.34 a credit note is issued by the person
> who supplied, and whether a sales return is one at all is a question this software does not decide.
> See ADR-017 and `docs/blueprint/phase-1i-returns-foundation.md`.

## 38. Future compatibility and formal freeze criteria

**Extends cleanly later:** split tender (validation change only, §24) · discount (new line columns and
an ADR-016 revision) · delivery / inter-State sales (a new place-of-supply resolution and document
type, §17) · DPCO ceiling prices (a new master, §15.4) · FEFO suggestion (a chooser default, not a
schema change) · credit sale (needs a receivable foundation first) · e-invoice/IRN (needs the snapshot
set in §31, which is why it is complete now).

**Freeze criteria:** blueprint and design audit approved · migration with forced-recompilation proof ·
backend with unit tests · real-service gate extension · contracts · POS frontend · frontend tests ·
E2E · real browser acceptance including the no-mouse ten-line bill · 390px · keyboard and
accessibility · every error code mapped to a safe message and proven by the source-derived mapper test
· all hardening proofs restored byte-identically · full green regression · exact scope reconciliation
· clean 0/0.
