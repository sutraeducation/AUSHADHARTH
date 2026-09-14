# Phase 1H-0 — Medicine Price-Control Foundation

**Prerequisite to Phase 1H Sales/POS.** Written against the repository at `394c9759`. Every claim
below was checked against that commit.

---

## 1. Objective

Give AUSHADHARTH the minimum reliable domain foundation to represent **effective-dated medicine
ceiling-price controls**, so a future POS can answer:

> Does this medicine have an applicable controlled ceiling price on the sale date, and what legal
> maximum must the sale engine respect?

## 2. Scope

A curated **controlled formulation** identity; an effective-dated, overlap-free **price-control
version** carrying a ceiling in integer paise on an explicit unit basis; an explicit per-product
**applicability state**; a date-based **resolver**; reference-data UI; authorization; tests.

## 3. Exclusions

A complete external drug database · automatic matching of products to notifications · internet or
online NPPA lookup · bulk import of notifications · scheduled-vs-non-scheduled legal determination by
the application · GST-inclusive/exclusive conversion of ceilings · Sales tables, POS screens, sale
invoice numbering, sale movements or tender · any claim that the application independently certifies
legal correctness.

## 4. Regulatory / domain distinction — stated precisely

Three distinct facts, deliberately never merged:

| Fact | What it is | Where it lives | Tax basis |
|---|---|---|---|
| **Batch MRP** | the printed maximum retail price of a specific lot | `product_batches.mrp_paise` (Phase 1C-C) | **inclusive** of GST (Legal Metrology) |
| **Controlled ceiling** | a notified maximum price for a formulation | Phase 1H-0, here | **exclusive** of GST (DPCO: MRP = ceiling + GST as applicable) |
| **Selling rate** | what this store charges | Phase 1H sale line | **exclusive** of GST |

- **Law/regulation:** `product_kind = 'medicine'` does **not** imply price control. Only scheduled
  formulations notified under the DPCO are controlled; many medicines are not.
- **Project policy:** the application **enforces the reference data that has been configured**. It
  does not certify that the configured data is legally correct or current. Manually entered
  notification facts are traceable (§16) but not independently verified.

## 5. Formulation identity — HARD GATE, resolved

**Question:** is the existing Product + Composition model enough to associate a notification safely?

**Finding.** `product_composition_components` is genuinely rich — `ingredient_id`, `salt_form_id`,
`component_role`, `strength_presentation` (absolute/percentage), numerator atoms + scale + unit, and
optional denominator atoms + scale + unit for concentrations. Combined with `products.dosage_form_id`
it can *describe* a formulation precisely.

**But it is not sufficient to *match* a notification automatically**, and Phase 1H-0 will not try:

- composition is operator-entered clinical truth, of variable completeness;
- a notification keys on formulation + dosage form + strength *as notified*, whose wording and
  rounding need not coincide with our structured strengths;
- a combination product may match a notification on some components and not others;
- matching on `display_name` text is explicitly forbidden.

**Decision.** `controlled_formulations` is a **curated reference master** naming the notified
formulation. The link from a Product to a controlled formulation is an **explicit operator
assignment**, exactly as Phase 1F made HSN and Tax Category explicit assignments rather than
inferences. Composition remains clinical truth; price control is a separate commercial/legal
assignment. No text matching, no inference, no heuristic — anywhere.

This is the smallest identity that works and stops well short of a drug database.

## 6. Applicability state

A new `products.price_control_status`, CHECK-constrained to three explicit values:

| value | meaning |
|---|---|
| `unknown` | nobody has assessed this product yet — **the default for every existing row** |
| `not_applicable` | positively asserted: this product is not under price control |
| `controlled` | under price control; **requires** `controlled_formulation_id` |

A medicine can therefore truthfully be `not_applicable`. A product is never silently uncontrolled
because reference data is incomplete: `unknown` is a distinct, visible, resolvable state that the
resolver reports as such, and it is **snapshotted onto a posted sale line** so the gap is auditable
rather than invisible. Database triggers enforce the pairing (`controlled` ⟺ formulation present),
mirroring the frozen `products_tax_classification_insert/update` triggers from migration 0009.

## 7. Ceiling-price authority

Two new reference masters, mirroring the proven `tax_categories` + `tax_rate_versions` pair:

**`controlled_formulations`** — jurisdiction, `formulation_code` (canonical, uppercase),
`display_name`, optional `dosage_form_id`, `strength_text` (descriptive, deliberately *not* parsed),
`verification_state` (`unverified|verified|rejected`, borrowed from the existing
`regulatory_categories` convention), `source_note`, plus the standard revision/status/archive/audit
columns.

**`price_control_versions`** — `controlled_formulation_id`, `effective_from`, nullable
`effective_to`, `ceiling_price_paise` (INTEGER, > 0, bounded), `ceiling_basis`, nullable
`ceiling_basis_unit_id` → `units_of_measure`, `notification_reference`, `source_note`, plus
revision/status/archive/audit.

Integer paise only. No REAL, no float, anywhere. The ceiling is **not** assumed to be per pack (§8).

## 8. Unit / basis model — the critical part

`ceiling_basis` is explicit:

| basis | meaning | comparable in Phase 1H-0? |
|---|---|---|
| `per_base_unit` | the ceiling applies to **one base unit** of the product (per tablet, per ml) — the shape DPCO notifications actually take | **yes**, when `ceiling_basis_unit_id` equals the product's `base_unit_id` |
| `per_pack` | the ceiling applies to a pack presentation | **no** — recorded truthfully, resolver reports `incomparable` |

**Comparability rule:** a `per_base_unit` ceiling is comparable **only if its unit is identically the
product's `base_unit_id`**. No unit conversion is attempted — converting mg to ml, or tablets to
grams, is not something this foundation can do safely. Different unit → `incomparable`, never a
guess.

`per_pack` exists so a genuinely per-pack notification can be **recorded without being mangled**. The
resolver refuses to compare it rather than dividing by an arbitrary pack count.

### The scale trap, and the exact arithmetic

`products.quantity_scale` may be 0–6, so **one base unit is `10^scale` atoms**, not one atom. A
ceiling quoted per base unit must therefore never be divided into a per-atom figure. Cross-multiply
instead:

```
selling basis            exact comparison                                           domain
base_unit (per atom)     rate_per_atom × 10^scale            ≤ ceiling_paise         i128
pack      (per pack)     rate_per_pack × 10^scale            ≤ ceiling_paise × base_quantity_atoms   i128
```

No division, no early rounding, no invented per-atom ceiling. `10^scale ≤ 10⁶`,
`rate ≤ 10¹¹`, `ceiling ≤ 10¹¹`, `base_quantity_atoms ≤ 9×10¹⁵`, so the largest operand product is
`10¹¹ × 9×10¹⁵ = 9×10²⁶` — beyond `i64` (9.22×10¹⁸) for schema-legal inputs, so the comparison is
evaluated in **`i128`** with `checked_mul`, consistent with the Phase 1H MRP ceiling.

## 9. Effective periods

Half-open `[effective_from, effective_to)`, identical to `tax_rate_versions`: on the day equal to
`effective_to` the **next** version applies, or none. `effective_to > effective_from` enforced by
CHECK. Only `status = 'active'` versions resolve; the formulation's own status is deliberately not
filtered, so a product referencing a since-archived formulation still resolves history — exactly the
rule `resolve_tax_rate` documents.

## 10. Overlap prevention

`BEFORE INSERT` and `BEFORE UPDATE` triggers on `price_control_versions`, structurally identical to
`tax_rate_versions_no_overlap_insert/update`, raising a typed
`price_control_effective_period_overlap`. Two active versions for one formulation can never overlap,
so "the applicable ceiling on a date" is never ambiguous. Database-enforced, not service-enforced.

## 11. Resolver

One function, mirroring `domain::taxation::resolve_tax_rate`:

```rust
resolve_price_ceiling(executor, controlled_formulation_id, on_date)
    -> Result<Option<ResolvedPriceCeiling>, PriceControlError>
```

`ResolvedPriceCeiling` carries the version id, effective period, `ceiling_price_paise`,
`ceiling_basis`, `ceiling_basis_unit_id`, and `notification_reference`. Like `resolve_tax_rate` it
resolves **by date only** and makes no comparison decision of its own.

## 12. Distinction from Batch MRP

Restated because conflating them is the central risk: **Batch MRP is a lot-level, GST-inclusive
printed price; a controlled ceiling is a formulation-level, GST-exclusive notified maximum.** Neither
is derived from the other, neither is copied into the other, and the two are never stored in the same
column.

## 13. Future Sales comparison contract — and a correction to Phase 1H

Phase 1H-0 defines the interface; Phase 1H enforces it.

```rust
struct RetailPriceConstraints {
    mrp_paise: Option<i64>,              // lot-level, GST-INCLUSIVE
    ceiling: Option<ResolvedPriceCeiling>,  // formulation-level, GST-EXCLUSIVE
    price_control_status: PriceControlStatus,
    comparable: Comparability,
}

enum Outcome {
    MrpOnly,            // not price-controlled; only the MRP rule applies
    CeilingOnly,        // controlled; no MRP recorded on the lot
    BothApplicable,     // both apply — both are enforced (see below)
    ControlledUnresolved, // status = controlled but no ceiling resolves on the date  -> REFUSE
    Incomparable,       // a ceiling exists but its basis cannot be compared          -> REFUSE
}
```

**The stricter maximum is obtained by enforcing both constraints, never by comparing them.** This is
the key insight: because MRP and ceiling live on *different tax bases*, `min(mrp, ceiling)` is
meaningless and is explicitly forbidden. Requiring both checks to pass yields exactly the stricter
effective maximum without ever placing the two numbers on the same axis.

Each constraint is applied on its own natural basis:

```
DPCO ceiling  (GST-exclusive)  compare against the line's TAXABLE VALUE / rate
Batch MRP     (GST-inclusive)  compare against the line's GST-INCLUSIVE LINE TOTAL
```

### Correction required to the approved Phase 1H §15

The approved Phase 1H rule compares a **tax-exclusive selling rate** against a **tax-inclusive MRP**:

```
rate_per_pack_paise ≤ batch_mrp_paise          -- WRONG
```

With MRP ₹95.50 (inclusive of 12% GST) and a selling rate of ₹95.50 exclusive, the customer pays
95.50 + 11.46 = **₹106.96 — ₹11.46 above the printed MRP — while passing the check.** That is
precisely the breach Decision 2 exists to prevent.

**Corrected rule**, comparing like with like and still exact:

```
basis pack       line_total_paise                        ≤ mrp_paise × quantity_packs
basis base_unit  line_total_paise × base_quantity_atoms  ≤ mrp_paise × quantity_atoms
```

`line_total_paise` is the already-computed GST-inclusive line total, so the comparison introduces no
new division and no new rounding — ADR-016 is untouched. The quantity no longer cancels (tax rounding
is per line, not per unit), so the cross-product form is retained. Worst case
`10¹⁵ × 9×10¹⁵ = 9×10³⁰` → **`i128`**.

The Phase 1H blueprint is updated in the same change so the two documents cannot disagree.

## 14. Fractional-sale compatibility

The approved Phase 1H quantity model is preserved unchanged: pack basis = integer packs × paise per
pack; base-unit basis = integer atoms × paise per atom. **ADR-016 is not modified.** Both ceiling
checks are *validation* rules producing no stored value, performing no division and no rounding, so
neither adds a money rounding point. The controlled-ceiling comparison is only attempted after the
basis has been proven compatible (§8); otherwise the outcome is `Incomparable` and posting is refused.

## 15. History

Changing today's ceiling never rewrites yesterday's meaning:

- a new ceiling is a **new version** with its own effective period, never an edit of the old one;
- superseding is expressed by setting the previous version's `effective_to`, which the overlap
  triggers police;
- archiving is lifecycle, not deletion — archive-not-delete, as everywhere else in the project;
- a posted sale (Phase 1H) snapshots the resolved `price_control_version_id`, the ceiling, its basis
  and the applicability status onto the line, so the document remains legible after every later
  change.

## 16. Audit and source traceability

`notification_reference` (the order/notification identifier), `effective_from`, `source_note`, and
`verification_state` are retained on every version, plus the standard `created_at_utc` /
`updated_at_utc` and an append-only `master_change_events` row per change with the server-derived
actor. The application enforces configured data; it does not claim to certify it (§4).

## 17. Lifecycle

Standard project conventions: durable UUIDv7 ids, integer `revision` with optimistic concurrency,
`status` ∈ `active|archived`, archive requires a reason, restore is available, and every mutation
writes a `master_change_events` row. Two new entity types (`controlled_formulation`,
`price_control_version`) and one new product-level action are added to the audit domain.

## 18. Authorization

`owner_admin` manages price-control reference data and product assignment. `pharmacist` and `cashier`
may read it where the Product UI exposes it. The actor comes from the authenticated server session;
the browser is never authoritative for applicability, ceiling or comparability. No endpoint allows a
client to override a resolved authority.

## 19. APIs

Two new reference-master kinds, reusing the existing generic machinery (list, detail, create, update,
archive, restore, and the Reference Data UI) exactly as `tax-categories` and `tax-rate-versions` do:

```
/api/v1/reference/controlled-formulations
/api/v1/reference/price-control-versions
```

Plus, mirroring Phase 1F's product tax classification:

```
GET /api/v1/products/{id}/price-control?asOf=YYYY-MM-DD
PUT /api/v1/products/{id}/price-control   {expectedRevision, priceControlStatus, controlledFormulationId?}
```

The GET returns the assignment, the resolved ceiling for the date, and an explicit comparability
verdict. It is a **read** of server-resolved authority; nothing a client sends can override it.

## 20. UI

- **Reference Data:** two new sections for controlled formulations and their ceiling versions, with
  effective periods and superseded history visible, following the existing section pattern.
- **Product detail:** a *Price Control* panel beside the existing Tax Classification panel, showing
  applicability, the mapped formulation, the effective ceiling with its basis and date, the
  notification reference, and an honest state when the product is `unknown` or the ceiling is
  incomparable.
- Read-only roles see the panel without controls. **No POS or Sales screen exists in Phase 1H-0.**

## 21. Migration

`0012_medicine_price_control.sql`: create `controlled_formulations` and `price_control_versions` with
their CHECKs, indexes and overlap triggers; `ALTER TABLE products ADD COLUMN price_control_status`
(defaulting to `'unknown'`) and `ADD COLUMN controlled_formulation_id`; pairing triggers; extend the
`master_change_events` entity-type domain. `sqlx::migrate!` embeds migrations at **compile time** —
force recompilation before trusting any migration-dependent result.

## 22. Tests

Backend unit tests for: paise integer and positive ceiling; valid effective dates; overlap refused on
insert and on update; half-open boundary resolution on both edges; invalid formulation reference
refused; invalid/mismatched basis unit reported incomparable; archive and restore behaviour; the
historical resolver returning the version in force on a past date; the `controlled` ⟺ formulation
pairing triggers; and an explicit unresolved-controlled outcome.

## 23. Real-service gate

Extend `tests/real_service_gate.rs` over real HTTP: create a formulation, add two adjoining ceiling
versions, prove the half-open boundary across the wire, prove overlap refusal, assign a product,
resolve on three dates (before, between, after), prove an incomparable basis is reported rather than
guessed, and prove a `controlled` product with no applicable version resolves to
`ControlledUnresolved`.

## 24. Browser acceptance

Real built frontend, real router, disposable database — never the customer database. Verify natural
navigation, list and detail, create and update, effective periods, superseded history, product
linkage, read-only role, error states, 390px and keyboard/accessibility. No technically-green but
confusing UI is accepted.

## 25. Hardening

Anti-vacuous mutation proofs, one at a time, each restored byte-identically and checksum-verified:
overlapping effective periods · half-open boundary direction · applicability pairing trigger ·
archived/historical resolution · wrong formulation mapping · wrong/mismatched unit basis ·
unresolved controlled price · history immutability under a later ceiling change · browser authority
spoof · audit actor authority.

## 26. Exclusions restated

No Sale tables, no invoice numbering, no sale inventory movement, no tender, no POS screen. No
automatic notification import, no online lookup, no legal certification, no GST-basis conversion
between MRP and ceiling.

## 27. Future update / import strategy

Notifications change periodically. The versioned model already absorbs that: a revision is a new
`price_control_version` with a new effective period. A future bulk-import phase can populate the same
tables through the same validation and the same overlap triggers — import is a data-entry channel,
never a second authority, and must never bypass the resolver or the triggers.

## 28. Phase 1H integration contract

When Sales is implemented it must:

1. read `price_control_status` and, when `controlled`, resolve the ceiling on the **sale business
   date**;
2. refuse posting on `ControlledUnresolved` or `Incomparable`, with typed safe errors;
3. enforce the GST-**exclusive** ceiling against the taxable value, and the GST-**inclusive** MRP
   against the line total (§13) — **both**, never `min()`;
4. snapshot the applicability status, formulation id, resolved version id, ceiling and basis onto the
   posted line;
5. record `unknown` applicability on the line rather than treating it as uncontrolled silently.

**Policy decision for `unknown` (flagged, needs confirmation):** refusing every sale of an unassessed
medicine would make the application unusable on day one, since every existing product defaults to
`unknown`. Phase 1H therefore proceeds under the MRP rule alone and **records the `unknown` status on
the posted line**, which is auditable and surfaced in the UI — not silent. Blocking instead would be
a one-line policy change.

## 29. Freeze criteria

Blueprint and design audit · migration with forced-recompilation proof · backend with unit tests ·
real-service gate extension · contracts · UI · frontend tests · E2E · real browser acceptance · 390px
· keyboard/accessibility · every error code mapped to a safe message · all hardening proofs restored
byte-identically · full green regression · exact scope reconciliation · clean 0/0.
