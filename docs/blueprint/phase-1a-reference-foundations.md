# Phase 1A reference foundations

## Scope

Phase 1A adds reference data needed before product identity can be implemented:

- units of measure;
- dosage forms;
- pharmaceutical companies and verified identifiers;
- brands;
- HSN codes;
- tax categories and effective-dated rates;
- regulatory category reference metadata;
- append-only master change events.

Product, pack/SKU, ingredient/composition, batch, barcode, rack/location, stock,
sales, purchases, accounts, prescriptions, import, and regulatory enforcement
remain deferred.

## Tables introduced

Migration `0002_reference_foundations.sql` creates:

1. `units_of_measure`
2. `dosage_forms`
3. `pharmaceutical_companies`
4. `company_identifiers`
5. `brands`
6. `hsn_codes`
7. `tax_categories`
8. `tax_rate_versions`
9. `regulatory_categories`
10. `master_change_events`

All durable IDs are UUIDv7-compatible text values. SQLite row IDs are not API or
domain identifiers. The database may contain no store only during initial setup;
after configuration, insert/delete guards preserve exactly one locally
authoritative store using the existing `store_identity` foundation.

## Lifecycle and revision convention

Mutable reference rows begin with revision `1` and status `active`. Every
successful create, update, archive, or restore records an append-only event;
update/archive/restore increment revision exactly once in the same transaction.

Updates include `expectedRevision`. A mismatch returns `revision_conflict` with
expected and current revisions. Archived rows remain readable but reject ordinary
updates. Archive requires a reason and UTC timestamp. Restore clears archive
metadata and reruns database uniqueness constraints. Physical deletion is not an
application operation.

## Audit events

`master_change_events` records a UUIDv7 event, entity type and ID, resulting
revision, action, UTC time, optional reason, payload schema version, and JSON
change payload. Actor and terminal IDs remain nullable until their respective
foundations exist. Database triggers reject event update and deletion. No API is
provided for mutating audit rows.

Audit payloads contain reference changes only. They must not contain credentials,
patient/customer data, or future transaction detail.

## Normalization

- Canonical codes and identifier namespaces are trimmed lowercase ASCII using
  letters, digits, dot, underscore, and hyphen.
- Company and brand search names are derived by trimming, collapsing whitespace,
  and lowercasing display names.
- HSN values remove whitespace and use uppercase ASCII letters/digits.
- Jurisdiction codes use uppercase ASCII.
- Company identifier values are trimmed uppercase within their declared
  namespace. No namespace-specific legal meaning is inferred.
- Database constraints remain the final integrity boundary.

Company and brand display names are deliberately not unique. Verified active
company identifiers are unique by namespace and normalized value. HSN and other
canonical reference codes use their documented scoped uniqueness.

## Tax rate representation

Tax rates use integer basis points:

```text
100 basis points = 1.00%
500 basis points = 5.00%
10000 basis points = 100.00%
```

No tax column uses `REAL` or floating point. CGST, SGST, IGST, and a reserved
percentage cess field are independently constrained from 0 through 10000. No
jurisdiction-specific equality between components is assumed in this reference
slice.

Rate periods are half-open: `[effective_from, effective_to)`. A null end is
open-ended. Therefore a rate ending on `2027-01-01` and another beginning on
`2027-01-01` are adjacent and valid. Active periods for one tax category may not
overlap. New versions are inserted; historical economics must never be rewritten.

## Seeds

Only 15 generic units are seeded: tablet, capsule, piece, strip, box, bottle,
vial, ampoule, tube, sachet, ml, litre, mg, gram, and kilogram. Their identifiers
are stable UUIDv7-compatible constants. No dosage form, company, brand, HSN, tax,
or regulatory conclusion is seeded without an approved source.

## API

Versioned routes use `/api/v1/reference/{kind}` with list/search, get, create,
replace-style update, archive, and restore operations. Supported kinds are
`units`, `dosage-forms`, `companies`, `company-identifiers`, `brands`,
`hsn-codes`, `tax-categories`, `tax-rate-versions`, and
`regulatory-categories`.

Errors are mapped to stable transport codes: validation, duplicate, revision,
not-found, archived-state, effective-date overlap, and internal error. SQLite
messages and filesystem details are never returned.

## Deferred product hierarchy

The frozen future hierarchy remains:

```text
Brand → Product/Formulation → Product Pack/SKU → Batch
```

Phase 1A implements only Brand. It adds no product identity, conversions, batch
quantity, stock balance, pricing default, or product tax assignment.
