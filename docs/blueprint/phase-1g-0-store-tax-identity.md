# Phase 1G-0 — Store Tax Identity (Purchase prerequisite)

## 1. Objective

Give the Store its own authoritative GST registration and place-of-supply State, so Phase 1G can
decide `CGST + SGST` versus `IGST` from data rather than assumption.

## 2. Why this phase exists

The Phase 1G prerequisite audit found a hard gap. Tax treatment on a purchase is decided by
comparing two places of supply:

| Side | Status |
| --- | --- |
| Supplier's place of supply | **Present** — `parties.place_of_supply_state_id` (Phase 1E) |
| Store's place of supply | **Missing** — `store_identity` has only `store_id`, `display_name`, `business_time_zone`, `created_at_utc` |

Nothing else in the repository supplies it: first-run setup captures only a display name,
`/api/v1/catalog/context` returns only `storeId`, `/api/v1/auth/status` returns only
`storeDisplayName`, and no Store Settings screen exists.

The two unacceptable ways to proceed without it were both rejected:

- **Assume intra-state.** That hardcodes a tax answer and would silently mis-tax every inter-state
  purchase.
- **Let the operator choose per document.** That makes the browser authoritative for tax treatment,
  which the Phase 1G trust boundary forbids.

## 3. Scope

Four columns on the singleton `store_identity`, an owner-only API, and a Store Profile screen.

## 4. Exclusions

Store legal name, store addresses, store PAN, multi-state registration, branch or additional place
of business, composition-scheme treatment, logo or letterhead, anything printed on a document, and
every Phase 1G Purchase concern. PAN in particular is deliberately out: the gate this phase exists
to clear is *place of supply*, and PAN plays no part in deciding it.

## 5. Storage model

Columns on `store_identity`, not a new table.

`store_identity` is already a database-enforced singleton — `store_identity_single_store_insert`
refuses a second row and `store_identity_no_delete` refuses removal — so the store's registration is
one-to-one with it by construction. A separate table would add a join and a "which row is current"
question to answer a question that has exactly one answer.

One registration only, exactly as Phase 1E decided for parties: a store operating in several states
would need a resolution rule for which registration applies to a given document, and that rule
cannot be written honestly before the documents exist. Multi-state is a named non-goal.

```sql
ALTER TABLE store_identity ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE store_identity ADD COLUMN gst_registration_status TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE store_identity ADD COLUMN gstin TEXT;
ALTER TABLE store_identity ADD COLUMN normalized_gstin TEXT;
ALTER TABLE store_identity ADD COLUMN place_of_supply_state_id TEXT REFERENCES state_codes(id);
```

`revision` is included because every other mutable record in this codebase carries optimistic
concurrency and the store profile is no less deserving of it; `DEFAULT 1` keeps the existing insert
paths working untouched.

SQLite cannot add a `CHECK` to an existing table, so the coherence rules that `parties` expresses as
table `CHECK`s are enforced here by triggers instead, with identical semantics.

## 6. Validation — reused, never re-implemented

`domain::parties::normalize_gstin` is called verbatim: the same structural shape, the same official
mod-36 check digit, the same whitespace and case normalisation. `gstin_state_code` extracts the
prefix. No second GSTIN implementation is created, so the two can never drift.

The State list is the `state_codes` master seeded in Phase 1E.

## 7. Invariants

Enforced in the service for a precise message and by triggers so the guarantee does not depend on
service discipline:

1. `gst_registration_status` is `registered` exactly when a GSTIN is present. `unregistered` asserts
   the store has none; `unknown` means it has not been captured. These are different facts.
2. A GSTIN requires a place-of-supply State.
3. The GSTIN's first two characters must equal that State's code, in jurisdiction `IN`.
4. A newly assigned State must be active; a State archived after assignment stays readable, and
   clearing is always permitted — the Phase 1F rule that avoids trapping a record.

## 8. Revision, audit, authorization

Optimistic `expectedRevision`, bumped on every write. One `master_change_events` row per change with
the previous and next values and the actor from the validated server session. The frozen entity-type
enumeration gains `store_tax_identity` through the established rename/recreate migration pattern
used by `0005`, `0006`, and `0008`.

Read: any authenticated session — Phase 1G's purchase screen needs it. Write: `owner_admin` behind
the frozen Host/Origin mutation check.

## 9. API

```
GET /api/v1/store/tax-identity
PUT /api/v1/store/tax-identity   { expectedRevision, gstRegistrationStatus, gstin, placeOfSupplyStateId, reason }
```

`GET` also returns `complete`, true when a State is present — the single fact Phase 1G requires.
A store may be `unregistered` and still complete: an unregistered buyer still has a place of supply.

Typed errors only, reusing frozen codes: `validation_failed`, `not_found`, `archived_conflict`,
`revision_conflict`, `authorization_denied`, `authentication_required`, `service_busy`,
`internal_error`, plus `party_conflict`'s store equivalent `store_tax_conflict` for a
GSTIN/State disagreement.

## 10. UI

A **Store Profile** screen at `/app/settings/store`, reached from a new `CONFIGURATION` group in the
sidebar — there is no settings surface today. It shows the registration status, GSTIN, and
place-of-supply State, with an owner-only edit dialog reusing the established dialog conventions.

The screen states plainly why the State matters, because an operator who leaves it blank will later
be blocked from posting a purchase and deserves to know that in advance rather than at the till.

Required states: loading, incomplete, error, success — an error must never render as "not set".

## 11. Migration — `0010_store_tax_identity.sql`

Additive. Migrations `0001`–`0009` stay byte-identical. No table rebuild except the frozen
`master_change_events` recreate that the entity-type extension requires.

## 12. Tests

**Backend** — an unset store reads as incomplete; set a State alone; set a registered GSTIN with a
matching State; GSTIN with a mismatched State refused by service *and* independently by the
database; a failed check digit refused; `registered` without a GSTIN refused; `unregistered`
carrying a GSTIN refused; an archived State refused for a new assignment but readable once assigned;
clearing always permitted; revision conflict; audit carries previous, next, and the session actor;
read needs a session; write needs `owner_admin`; a spoofed actor changes nothing; no raw database
text in any error.

**Frontend** — incomplete, populated, error distinct from incomplete with retry, edit and save,
State selector, revision conflict and reload, read-only role, 390 px.

**Real-service gate** — over real HTTP against disposable SQLite: set the store's tax identity, read
it back, confirm a mismatched GSTIN is refused, and confirm the database refuses it directly.

**Browser preview** — the mandatory visual gate, desktop and 390 px.

## 13. Security

Loopback only. Writes are `owner_admin` behind the Host/Origin check. The actor is the server
session. No new dependency, no new configuration surface, and no change to how the database is
located.

## 14. Phase 1G compatibility

After this phase, Phase 1G resolves treatment as:

```
store.place_of_supply_state_id == supplier.place_of_supply_state_id  ->  CGST + SGST
otherwise                                                            ->  IGST
```

and refuses to post when the store's State is unset, rather than guessing.

## 15. Freeze acceptance matrix

| Criterion | Check |
| --- | --- |
| Store has an authoritative place-of-supply State | schema + API |
| GSTIN validation reused, not duplicated | single `normalize_gstin` call site for stores |
| GSTIN agrees with the State | service test + independent database test |
| Registered ⇔ GSTIN present | test |
| Archived State not assignable, still readable | test |
| Clearing never trapped | test |
| Optimistic concurrency | revision conflict test |
| Audit carries previous, next, actor | test |
| Migrations 0001–0009 byte-identical | `git diff` |
| No dependency change | manifest/lockfile diff |
| No Purchase, accounting, or ITC surface | scope grep |
| Phases 0–1F regression green | full suites |
| Browser preview accepted | visual gate |
