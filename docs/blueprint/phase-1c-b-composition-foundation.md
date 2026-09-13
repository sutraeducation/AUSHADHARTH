# Phase 1C-B — Ingredient, Salt Form, and Product Composition Foundation

## 1. Objective

Give Medicine Products a structured, exact, auditable **business identity** for what they contain, so that a pharmacy can tell two similar medicines apart and record a manufacturer's stated composition. This is catalog identity data only. It is not a clinical system.

## 2. Scope

- `ingredients` reference master — the active moiety / base substance (Paracetamol, Amoxicillin, Diclofenac).
- `salt_forms` reference master — the chemical form modifier (Sodium, Potassium, Hydrochloride, Trihydrate).
- `strength_units` reference master — units in which a pharmaceutical strength is expressed (mcg, mg, g, mL, IU, mEq, tablet).
- `product_composition_components` — a Medicine Product's ordered composition, each component carrying an exact strength.
- Authenticated read APIs; Owner/Admin mutation APIs; server-session audit actor; optimistic revisions; archive-not-delete.
- Product detail **Composition** section with add/edit/archive/restore, plus the three new masters in Reference Data.

## 3. Explicit exclusions

Not in this phase, and not partially started: generic substitution, therapeutic or generic equivalence, drug interactions, clinical warnings, dosage recommendation, prescription validation, Batch, Expiry, Batch MRP, Stock, inventory ledger, Purchase, POS/Sales, Customers, Suppliers, Accounting, GST returns, Excel import, cloud sync, licensing, LAN multi-counter.

Also deliberately deferred within the composition domain itself:

- **QS / q.s. components.** Every component in this phase carries a numeric strength. A "quantity sufficient" excipient has no numeric strength and would weaken the strength CHECK constraints. Deferred as a distinct modelling decision.
- **Excipient capture as a workflow.** `component_role` distinguishes `active` from `inactive` so the column need not be added later, but the UI and validation are built for active composition.
- **Required composition.** A Medicine Product may exist with no composition. Enforcing composition would invalidate every Product created in Phase 1B/1C-A3.
- **Unit conversion** between `units_of_measure` and `strength_units` (see §8).

## 4. Domain terminology

| Term | Meaning |
| --- | --- |
| Ingredient | The active moiety, independent of salt. "Diclofenac". |
| Salt Form | The chemical form modifier applied to an Ingredient. "Sodium". |
| Component | One Ingredient (+ optional Salt Form) with a strength, belonging to one Product. |
| Composition | The ordered set of a Product's components. |
| Strength | An exact numerator, optionally per an exact denominator. |
| Single-ingredient medicine | Exactly one active component. |
| Combination medicine | Two or more active components. |

## 5. Identity model — three levels, not collapsed

```
Ingredient  (active moiety)
   └── optional Salt Form  (chemical form)
          └── Product Composition Component  (Product ↔ Ingredient + strength)
                    └── belongs to Product / Formulation
```

**Ingredient and Salt Form are deliberately separate entities.** "Diclofenac Sodium" and "Diclofenac Potassium" share the moiety *Diclofenac*. Collapsing them into one `ingredients` row per salt would make them unrelated strings and permanently destroy the grouping a future (deferred) generic feature would need. Keeping the salt as a separate optional reference preserves that grouping **without implementing or implying any substitution or equivalence today**.

Salt Form is optional: "Paracetamol 500 mg" has no salt form.

Worked examples, expressed in this model:

| Medicine | Components |
| --- | --- |
| Paracetamol 500 mg | Paracetamol, no salt, 500 mg |
| Amoxicillin 500 mg | Amoxicillin, no salt (or Trihydrate), 500 mg |
| Amoxicillin + Clavulanic Acid | two components, ordered |
| Diclofenac Sodium 50 mg | Diclofenac + Sodium, 50 mg |
| Pantoprazole Sodium 40 mg | Pantoprazole + Sodium, 40 mg |
| Azithromycin 200 mg / 5 mL | Azithromycin, 200 mg per 5 mL |
| Insulin 100 IU / mL | Insulin, 100 IU per 1 mL |
| Clotrimazole 1% w/w cream | Clotrimazole, 1 g per 100 g, presented as percentage |

No medical master data is seeded from these examples. They are modelling checks only.

## 6. Schema proposal — migration `0005_composition_foundation.sql`

Follows every Phase 1A/1B convention: `STRICT` tables, UUIDv7 `id` CHECK, `revision >= 1`, `status IN ('active','archived')` with the paired archive CHECK, `*_at_utc` GLOB checks, partial unique indexes on active rows, and `BEFORE INSERT/UPDATE` triggers raising typed codes.

**Audit stream extension.** `master_change_events.entity_type` is a CHECK enumeration. Phase 1B extended it using the SQLite rebuild pattern under the comment *"Extend the append-only audit stream without editing the Phase 1A migration"*. Phase 1C-B repeats that exact precedent to add `ingredient`, `salt_form`, `strength_unit`, and `product_composition_component`. This is the repository's established convention, not a redesign of frozen schema.

```
ingredients
  id, revision, status,
  canonical_code  (lower, [a-z0-9._-], unique)
  display_name
  normalized_search_name
  description            (nullable)
  timestamps + archive pair

salt_forms
  id, revision, status,
  canonical_code (unique), display_name, normalized_search_name,
  timestamps + archive pair

strength_units
  id, revision, status,
  canonical_code (unique), display_name,
  dimension IN ('mass','volume','count','activity','substance_equivalent'),
  allowed_scale INTEGER 0..6,
  timestamps + archive pair

product_composition_components
  id, revision, status,
  product_id            → products(id) ON DELETE RESTRICT
  ingredient_id         → ingredients(id) ON DELETE RESTRICT
  salt_form_id          → salt_forms(id) ON DELETE RESTRICT   (nullable)
  component_role        IN ('active','inactive')
  display_order         INTEGER NOT NULL >= 0
  strength_presentation IN ('absolute','percentage')
  strength_numerator_atoms   INTEGER NOT NULL > 0
  strength_numerator_scale   INTEGER NOT NULL 0..6
  strength_numerator_unit_id → strength_units(id)
  strength_denominator_atoms   INTEGER NULL > 0
  strength_denominator_scale   INTEGER NULL 0..6
  strength_denominator_unit_id → strength_units(id) NULL
  timestamps + archive pair
```

Constraints on the component table:

- Denominator triple is all-NULL or all-NOT-NULL (one CHECK) — no half-specified concentration.
- `strength_presentation = 'percentage'` requires **`strength_denominator_scale = 0 AND strength_denominator_atoms = 100`**. Pinning the scale as well as the value keeps the CHECK exact and unambiguous; SQLite has no `power()` with which to accept the equivalent `1000 @ scale 1`.
- Numerator scale must not exceed the numerator unit's `allowed_scale` (trigger, mirroring `products_unit_compatibility_*`); same for the denominator.
- Referenced Product, Ingredient, Salt Form, and Strength Units must be `active` when a component is active (trigger, mirroring `products_active_references_*`).
- Referenced Product must have `product_kind = 'medicine'` (trigger, typed code `composition_product_kind_conflict`).
- Partial unique index: one active component per Product/Ingredient/Salt. Because SQLite treats `NULL`s as distinct in a unique index — which would silently allow the same no-salt Ingredient twice, the most common case — the index is expressed over **`(product_id, ingredient_id, COALESCE(salt_form_id, ''))` `WHERE status = 'active'`**.

Indexes: `(product_id, status, display_order)`, `(ingredient_id)`.

**Product archive parity.** Phase 1B refuses to archive a Product while it has active `product_packs` or `product_company_roles`. Active composition components must join that same count, so `lifecycle_product` is extended; otherwise a Product could be archived while still carrying active composition, contradicting the frozen rule and the dialog text the operator is shown.

## 7. Strength representation

Exact integer atoms with an explicit scale and unit, consistent with ADR-009 and with the frozen Pack model. **Binary floating point is never authoritative and is never submitted.**

A value is `atoms / 10^scale` of `unit`. `500 mg` is `atoms=500, scale=0, unit=mg`. `0.125 mg` is `atoms=125, scale=3, unit=mg`.

- **Per dosage unit** — denominator `NULL`. The strength is per **one unit of the Product's own `base_unit_id`**, which Phase 1B already makes mandatory. "Paracetamol 500 mg" on a tablet Product is unambiguous.
- **Concentration** — denominator present. `200 mg / 5 mL`, `100 IU / 1 mL`, `1 g / 100 mL`.
- **Percentage** — stored exactly as a ratio with denominator 100 and `strength_presentation='percentage'`, so `1% w/w` is stored as `1 g / 100 g` and *displayed* as `1% w/w`. Percentage is therefore a **presentation of an exact ratio, not a unit**, which avoids inventing a dimensionless unit and keeps arithmetic exact. The `w/w`, `w/v`, `v/v` suffix is **derived** from the numerator and denominator unit dimensions (mass/mass, mass/volume, volume/volume) rather than stored, so it can never disagree with the data. Numerator and denominator dimensions are therefore deliberately *not* required to match.

Component scale is **independent of `products.quantity_scale`**. Product quantity scale governs how a Pack's inventory quantity may be subdivided; strength scale governs how precisely a manufacturer states a dose. They are different semantic domains and are deliberately not shared.

## 8. Unit strategy — a separate `strength_units` master

**This is the principal architectural decision of the phase and is flagged for freeze review.**

The frozen Phase 1A `units_of_measure` table cannot express pharmaceutical strength:

- Its `dimension` CHECK allows only `count`, `container`, `volume`, `mass`. International Units (biological activity) and milliequivalents fit none of them.
- `is_discrete` and `allowed_scale` encode **inventory subdivision** semantics that are meaningless for a strength.
- It is foreign-keyed from `products.base_unit_id` and `product_packs.container_unit_id`. Adding an `iu` row there would make "IU" selectable as a Pack container unit.
- `mcg` is not seeded and would be needed.

Extending it would require rebuilding a frozen Phase 1A table **and** contaminating Pack/inventory unit semantics — which Phase 1C-B's own constraints forbid. Therefore Phase 1C-B introduces a **separate `strength_units` master** and leaves `units_of_measure` byte-identical.

Accepted cost, documented: `mg` and `ml` exist in both masters, in two different roles. Conversion or reconciliation between the two is explicitly **deferred**; nothing in this phase converts a strength into an inventory quantity.

Treatment of the required unit kinds:

| Kind | Dimension | Seeded |
| --- | --- | --- |
| mass | `mass` | mcg, mg, g |
| volume | `volume` | mL, litre |
| count / unit-dose | `count` | tablet, capsule, drop, actuation |
| international unit | `activity` | IU |
| equivalent | `substance_equivalent` | mEq |
| percentage | — | not a unit; see §7 |

All three new masters reuse the frozen Phase 1A reference framework (`MasterKind` → `path/table/entity_type/json_expression/search_expression` + `validate_attributes`), inheriting its CRUD, search, auth, audit, revision, and archive behaviour unchanged.

## 9. Product relationship

- Composition attaches to **Product / Formulation identity** — never to Pack/SKU, never to a future Batch. This matches Phase 1B, where a Pack is a saleable presentation of one Product and carries no clinical meaning.
- One Product → zero or many components.
- Components are **child entities with their own `revision` and `status`**, exactly like `product_company_roles` and `product_packs`. A composition mutation therefore **does not** bump `products.revision`; Product revision continues to govern Product identity fields only. This is the frozen Phase 1B pattern, not a new one.
- Ordering is `ORDER BY display_order, id` — deterministic even when two components share an order value. `display_order` is intentionally **not** unique so that reordering never requires a transient constraint violation.
- Only `medicine` Products may carry composition; Device and General Pharmacy Item are rejected at the database and the API.
- Archive preserves the component and its history; restore reruns the integrity and uniqueness checks.

## 10. API contract

Reference masters reuse the existing generic Phase 1A endpoints, with three new kinds — `ingredients`, `salt-forms`, `strength-units`:

```
GET    /api/v1/reference/{kind}?search=&status=
POST   /api/v1/reference/{kind}
PUT    /api/v1/reference/{kind}/{id}
POST   /api/v1/reference/{kind}/{id}/archive | /restore
```

Composition is Product-scoped and new:

```
GET    /api/v1/products/{id}/composition                    → ordered components
POST   /api/v1/products/{id}/composition                    → create component
PUT    /api/v1/composition-components/{id}                  → update (expectedRevision)
POST   /api/v1/composition-components/{id}/archive|restore  → lifecycle (expectedRevision, reason)
```

`GET /api/v1/products/{id}` gains a `composition` array so the detail page loads in one request, mirroring how `companyRoles` and `packs` are already embedded. In contracts the field is `z.array(...).default([])`, following the existing `AggregatePackSchema.barcodes` precedent, so the addition is additive and no existing parse can break.

On create, `displayOrder` may be omitted; the Store Service then assigns `COALESCE(MAX(display_order), -1) + 1` within the Product, so a client never has to compute ordering to add a component.

Error codes reuse the frozen `CatalogErrorResponse` envelope. One new typed code is added: `composition_conflict` for product-kind and duplicate-component violations. No raw SQLite, sqlx, or Rust text is ever returned.

## 11. Authorization

Unchanged from the frozen pattern. Reads require a valid local session and are available to `owner_admin`, `pharmacist`, and `cashier`. Every mutation requires `validate_mutation_request` (Host/Origin) plus an authenticated `owner_admin`. Client-side action visibility remains supplementary; the Store Service enforces every endpoint.

## 12. Audit

Every component and master mutation writes one `master_change_events` row with `entity_type` in the extended enumeration, the entity revision, the action, the optional reason, and the JSON payload. `actor_id` is taken **exclusively from the validated server session**; any browser-supplied actor value is ignored, as proven by the existing spoofed-actor test which this phase extends to composition.

## 13. Revision model

Optimistic concurrency on every mutation. `PUT` and lifecycle requests carry `expectedRevision`; a mismatch returns `revision_conflict` with `expectedRevision`/`currentRevision`. The UI shows the safe stale-record message with a real **Reload latest** that invalidates the authoritative query, awaits the refetch, and repopulates the editor — the pattern frozen in Phase 1C-A3.

## 14. Archive semantics

Archive is never delete. Archiving a component requires a reason and preserves the row and its audit history. Restore reruns integrity and uniqueness checks, so a restore that would duplicate an active `(product, ingredient, salt form)` is rejected.

Archiving an **Ingredient or Salt Form that is still referenced remains permitted**, matching the frozen Phase 1A/1B semantic asserted by `archiving_preserves_a_master_referenced_by_another_master`: a master is archived, never cascaded and never blocked, existing references are preserved as history, and `ON DELETE RESTRICT` prevents actual deletion. What the integrity triggers do enforce is the forward direction — a component may not be **created or restored** against an archived Ingredient, Salt Form, or Strength Unit.

## 15. Duplicate-candidate boundary

The existing duplicate-candidate endpoint is **deliberately unchanged**. It evaluates a `CreateProductRequest` *before* the Product exists, and composition is added after creation, so composition cannot participate without changing the frozen aggregate-create contract. Extending it is therefore deferred for a structural reason, not an oversight.

The existing advisory wording stands and is not weakened: candidate matches remain business-identity advisory only and explicitly do **not** imply generic, therapeutic, clinical, or substitution equivalence. Nothing in this phase asserts that two Products with identical composition are interchangeable.

## 16. UX

The Product detail page gains a **Composition** section for Medicine Products, between Company Roles and Packs & SKUs. Device and General Pharmacy Item Products do not show it.

- Empty state: "No composition recorded." with an Add Component action for Owner/Admin on active Products.
- The component editor is a top-level portal dialog matching the frozen `CatalogDialog`: searchable Ingredient picker, optional Salt Form picker, active/inactive role, strength numerator (value + unit), optional "per" denominator (value + unit), and a percentage toggle that constrains the denominator to 100.
- A live, non-editable preview renders the canonical composition string as it will be displayed ("Azithromycin 200 mg / 5 mL"), so the operator can see exactly what was captured.
- Components render in a semantic table with per-cell labels: Ingredient, Salt, Strength, Role, Status, Actions — with Edit and Archive/Restore per row.
- Combination medicines simply list multiple rows in `display_order`; an Order field on each component controls sequence.
- Canonical display text is **generated**, never free-typed. There is no user-entered composition string in this phase, so display can never disagree with the stored data.
- Reference Data gains Ingredients, Salt Forms, and Strength Units sections, reusing the frozen generic master UI.

## 17. Accessibility

Unchanged from the frozen Phase 1C-A3 model and verified by regression: document-level portal dialogs, unique `useId` title ids, `aria-modal`, exactly one active dialog, Escape close, Tab and Shift+Tab wrap, exact launcher focus restoration, first-invalid-field focus, visible focus, labelled controls, semantic tables with per-cell `data-label`, no horizontal page overflow at 390 px, and honoured reduced-motion.

## 18. Error and state model

Loading, empty, error, and success remain four distinct states for the composition list, the Ingredient picker, the Salt Form picker, and the Strength Unit picker. A failed query is never rendered as a valid empty state and never as a permanent "Loading…"; each failure carries a safe message and its own retry that refetches the failed query. Session expiry returns control to the login flow and clears every session-scoped cache.

## 19. Migration plan

One new migration, `0005_composition_foundation.sql`. Migrations `0001`–`0004` remain byte-identical. The migration extends the audit enumeration via the Phase 1B rebuild precedent, creates the three masters plus the component table with their triggers and indexes, and seeds `salt_forms` and `strength_units` with a practical starting set. `ingredients` is seeded **empty** — ingredient data is the pharmacy's own and is not invented here.

Restart safety is asserted the same way Phase 1B asserts it: connect, migrate, close, reconnect, and confirm the schema and seeded row counts are stable.

The Phase 1B scope-guard test that asserts `ingredients` and `compositions` tables do **not** exist must be updated to reflect that composition now legitimately exists, while continuing to assert that `batches`, `stock`, `stock_ledger`, and `prices` do not.

## 20. Testing plan

**Backend** — master CRUD, duplicate canonical codes, archive-blocked-while-referenced, component create/update/archive/restore, combination products, deterministic ordering, exact strengths across mg / mcg / IU / mEq / mg-per-5-mL / percentage, rejection of half-specified denominators, rejection of percentage without denominator 100, scale exceeding a unit's `allowed_scale`, non-medicine rejection, duplicate active `(product, ingredient, salt)` rejection, FK integrity, revision conflicts, spoofed-actor resistance, all three roles against every endpoint, aggregate rollback leaving no orphans, and safe typed errors with no raw database text.

**Frontend** — empty/loading/error/retry, add, edit, multiple components, combination rendering and ordering, exact strength conversion without floating point, percentage constraint, validation and first-invalid focus, revision-conflict reload proving the retry carries the new revision, archive/restore, read-only pharmacist and cashier, Tab/Shift+Tab trap, and per-cell labels.

**End-to-end** — a real-browser Owner journey creating a Medicine Product, adding a single-ingredient composition, extending it to a combination, editing a strength, and archiving a component; a read-only role seeing composition without mutation controls; and a narrow-viewport check.

Tests must prove the invariants above. No test exists to raise a count, and no frontend double may accept a payload the Store Service would reject.

## 21. Security considerations

No new authentication or session behaviour. No secret, token, or password is logged or returned. Browser-supplied actor and store identifiers carry no authority. Composition data is local business data and never leaves the machine. The new masters add no new trust boundary; they reuse the frozen authenticated reference endpoints.

## 22. Future compatibility

The model is shaped so that deferred work remains possible without another identity migration: the Ingredient/Salt split preserves active-moiety grouping for a future generic feature; `component_role` is present for excipients; `strength_presentation` preserves operator intent; exact numerator/denominator supports a future unit-conversion service; and the component table is Product-scoped so a future Batch never inherits composition by accident. **None of that future behaviour is implemented, implied, or claimed here.**

## 23. Non-goals

This phase makes no clinical claim of any kind. It does not assert that two Products with the same composition are equivalent, interchangeable, or substitutable. It performs no interaction, contraindication, allergy, dosage, or prescription checking. Composition is manufacturer-stated catalog identity data entered by the pharmacy, and the interface says so.

## 24. Freeze acceptance matrix

| # | Item | Accept when |
| --- | --- | --- |
| 1 | Migration 0005 only; 0001–0004 byte-identical | verified by blob hash |
| 2 | Audit enumeration extended via Phase 1B precedent | migration reviewed |
| 3 | Ingredient and Salt Form not collapsed | schema + tests |
| 4 | `units_of_measure` untouched | blob hash |
| 5 | Strength exact, no authoritative binary float | backend + frontend tests |
| 6 | Denominator all-or-nothing | constraint test |
| 7 | Percentage stored as exact ratio with denominator 100 | constraint test |
| 8 | Composition attaches to Product only | schema + tests |
| 9 | Medicine-only composition | trigger + API test |
| 10 | Deterministic ordering | test |
| 11 | Component revisions independent of Product revision | test |
| 12 | Reads authenticated; mutations Owner/Admin only | role tests |
| 13 | Audit actor from server session only | spoofed-actor test |
| 14 | Archive never deletes; restore revalidates | tests |
| 14b | Product archive blocked while composition is active | test |
| 14c | Same Ingredient with no Salt cannot be added twice | constraint test |
| 15 | Duplicate-candidate wording unchanged and advisory | source review |
| 16 | Four distinct query states with working retry | frontend tests |
| 17 | Revision-conflict reload refetches and repopulates | frontend test |
| 18 | Accessibility and responsive regressions green | frontend + E2E |
| 19 | No deferred domain implemented | scope-guard test |
| 20 | Full suite zero failures | verification run |
