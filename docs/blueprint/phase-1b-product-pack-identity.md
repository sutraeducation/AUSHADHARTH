# Phase 1B: Product and Pack/SKU Identity

## Scope and identity

The catalog hierarchy is `Brand -> Product/Formulation -> Product Pack/SKU -> Batch`.
This slice implements Product and Pack/SKU only. A Product is one formulation-level
identity, while a Pack is one purchasable or saleable presentation. UUIDv7 values are
the only durable domain identifiers; labels, SKU codes, and barcodes are attributes.

The minimal Product kinds are `medicine`, `device`, and `general_pharmacy_item`.
Brand is optional. A medicine requires a dosage form. Product stores temporary
formulation, route, and release descriptors for display and import readiness, but
none is a substitute for normalized composition. Identity-defining corrections must
eventually create a new Product once transaction references exist.

## Quantity atoms

All quantities are signed 64-bit integer atoms. `quantity_scale` is bounded from 0
through 6 and cannot exceed the selected base unit's `allowed_scale`. A discrete base
unit requires scale 0. At scale `s`, one human base unit is `10^s` atoms. This gives
bounded decimal precision without floating-point quantities.

The maximum accepted direct pack quantity is 9,000,000,000,000,000 atoms, leaving
headroom for checked arithmetic. Products do not contain an editable stock quantity.

## Pack conversion and containment

`product_packs.base_quantity_atoms` is the authoritative direct conversion to the
Product's base unit. An individual base-unit presentation must contain exactly one
human base unit in atoms. Container units may represent larger presentations.

Optional `contained_pack_id` and integer `contained_pack_count` describe packaging.
They never replace direct conversion. The database rejects zero or negative values,
self-containment, cross-Product containment, inactive contained packs, cycles, and
any nested conversion that does not equal direct base atoms. Non-integral containment
counts cannot be stored in SQLite STRICT integer columns.

Pack conversion is identity-defining, but editable reference metadata is not posted
commercial history. A barcode or Store policy therefore does not lock conversion.
Before accepting a correction, the service confirms that every active policy remains
valid for the new quantity. Barcode and policy rows retain the same Pack UUID.

An active parent Pack is a structural dependency. Phase 1B rejects conversion changes
to its contained Pack until the dependent Pack structure can be updated consistently;
it does not describe that rejection as permanent historical immutability. The
`is_pack_conversion_locked(pack_id)` policy is currently false because no posted
commercial tables exist. Stock, opening, purchase, sale, return, and transfer slices
must extend this hook with actual posted-reference checks. Once such history exists,
conversion correction requires a new Pack identity.

## SKU scope

SKU is optional and never an ID. A nonblank SKU is normalized to uppercase ASCII and
may contain letters, digits, dot, underscore, slash, and hyphen. It is unique among
active Packs within `sku_store_id`. This store-local scope matches the one-store local
database and avoids assuming that independently operated stores share SKU namespaces.
Blank input normalizes to no SKU.

## Product companies

A Product may have multiple company assignments with roles `manufacturer`,
`marketer`, `brand_owner`, and `importer`. Optional periods use `[effective_from,
effective_to)`. Only an equivalent active Product/company/role/period assignment is
hard-duplicate; company names remain soft signals.

## Store pack policy

Policy is scoped by Store and Pack and carries purchase/sale enablement, whole-pack
purchase, fractional-sale permission, minimum sale increment atoms, and default flags.
A default must be enabled for its operation. Only one active purchase default and one
active sale default are allowed per Product/Store. Minimum increment is positive and
cannot exceed the Pack quantity. Fractional sale is rejected for scale-0 Products.
Pricing is not part of policy.

## Barcodes

Barcodes target Pack/SKU only. Namespace and scope are explicit. `global` has no Store;
`store` requires a Store, and the `internal` namespace must be store-scoped. GTIN input
removes spaces/hyphens and validates GTIN-8/12/13/14 check digits. Other namespaces
normalize to uppercase restricted ASCII without assuming one symbology.

An active barcode is unique by namespace/value globally or by Store/namespace/value
for local scope. A Pack may have many barcodes. Archive preserves lookup history;
restore reruns uniqueness and active-Pack guards. Barcode reassignment is deliberately
absent because it would silently move identity.

## Duplicate candidates and search

Duplicate detection is advisory. Deterministic reason codes and weights use only
available data: normalized display name, Brand, dosage form, descriptive formulation,
route/release, company overlap, Pack conversion, and barcode. Barcode conflicts remain
hard constraints. Candidate results do not claim clinical or generic equivalence.
Composition-based matching is deferred.

Local search uses indexed SQLite columns and joins across Product name, Brand, company,
SKU, and barcode. No internet, external engine, or premature FTS dependency is used.

## Lifecycle, audit, and atomicity

Product, role, Pack, policy, and barcode records use Phase 1A active/archive metadata,
optimistic `expectedRevision`, typed conflicts, and no public delete. Archived entities
remain readable. Restore reruns database conflicts and parent/reference integrity.
Every successful mutation appends a `master_change_events` row in the same transaction.

Aggregate Product creation can include company roles, Packs, Pack containment,
policies, and barcodes. Each Pack has a request-local `clientKey`; a parent uses
`containedPackClientKey` to reference another Pack in the same request. These keys are
trimmed, case-normalized request correlation values only and are never stored as domain
identity. The Store Service validates the complete graph, generates UUIDv7 Pack IDs,
resolves references, and inserts children before parents. All records and audit events
commit in one SQLite transaction. Missing or duplicate keys, self-reference, cycles,
conversion mismatches, invalid policies, invalid barcodes, or database conflicts leave
no Product or child rows behind. Independent Pack creation remains supported using
durable Pack IDs.

## Explicitly deferred

Composition does not exist yet.

Batch does not exist yet.

Current stock does not exist yet.

Also deferred are ingredients, generic/substitute matching, expiry, batch pricing,
stock ledger, rack/location, purchases, sales, suppliers, customers, accounts, GST
returns, prescription workflows, Excel import, cloud sync, licensing, and multi-counter
networking.
