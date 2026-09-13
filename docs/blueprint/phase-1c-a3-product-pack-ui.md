# Phase 1C-A3 — Product and Pack Catalog UI

## Scope and navigation

Phase 1C-A3 exposes the frozen Phase 1B Product/Pack domain through the authenticated application shell. **Products** is a real destination under **Masters**, before **Reference Data**, at:

- `/app/products` — searchable, status-filtered catalog
- `/app/products/new` — atomic Product and initial Pack creation
- `/app/products/:id` — Product detail workspace
- `/app/products/:id/edit` — active Product editing

The feature deliberately uses “Product Catalog”, not “Medicine Master”: ingredients, composition, batches, stock, pricing, expiry, and clinical behavior are not present.

## Product list and detail

The list queries Store Service and shows Product name, kind, Brand, Dosage Form, Base Unit, and lifecycle status. It includes loading, empty, no-result, error/retry, active/archived/all filtering, responsive table labels, and explicit View/Edit actions. Pack counts are omitted because the frozen list projection does not provide them efficiently.

The detail page is the durable workspace. Overview presents only current Product identity. Company Roles and Packs & SKUs are inline sections. Selecting **Manage** on a Pack opens its Store Pack Policy and Barcode workspace. Composition, batches, stock, price, and expiry tabs are absent.

Archived Products remain addressable by ID. Only active Products expose editing and child creation.

## Create and edit fields

Product identity contains the frozen fields: kind, optional Brand, Dosage Form, Base Unit, quantity precision, display name, and optional formulation/route/release display descriptors. Reference choices come from server-filtered Phase 1A APIs rather than hard-coded options.

Medicine requires a Dosage Form. Medical Device and General Pharmacy Item follow the backend’s optional rule. A Base Unit is always required. Selecting a discrete Base Unit fixes precision to zero and describes it as “Whole units only”. Continuous Units allow precision 0–6. Decimal text is converted to integer atoms with string arithmetic; binary floating-point quantities are never submitted.

New Products require an initial Pack. Optional initial company role, scoped SKU, Store policy, and Barcode are sent through the frozen aggregate-create request using a request-local Pack key. Store Service generates all durable UUIDv7 IDs and owns atomic rollback.

## Duplicate candidate warning

Creation first calls the advisory duplicate-candidate endpoint. Matches appear as “Possible similar product already exists”, with candidate links and friendly reason labels. The warning explicitly disclaims generic, clinical, and substitution equivalence. An owner may continue with a legitimate record. Hard identity, SKU, and Barcode conflicts remain blocking backend decisions.

## Company roles

The detail workspace supports Manufacturer, Marketer, Brand Owner, and Importer assignments with a searchable Company selector and optional effective dates. Multiple Companies and roles are allowed. Add, edit, archive, and restore use revision-aware endpoints; the UI never implies exactly one manufacturer.

## Packs, conversions, and SKU

Pack rows show label, direct Base Unit quantity, containment, optional SKU, and lifecycle. Add/edit uses the authoritative direct `base_quantity_atoms`. Optional containment selects another active Pack belonging to the same Product and displays the calculated expected Base Unit quantity. The UI rejects non-positive quantities and conversion mismatch; Store Service remains authoritative for self/cyclic/cross-Product containment and active-parent conflicts.

Example: a Strip directly containing 15 Tablets and a Box containing 10 such Strips must store 150 Tablets as the Box direct conversion. A scoped SKU is optional and is not domain identity. Barcode or policy presence does not create a UI-side conversion lock; `conversion_conflict` reports only the safe backend reason.

A SKU is Store-scoped in every Pack editor, not only in aggregate create: `skuStoreId` accompanies a non-blank `skuCode` and both are cleared together when the SKU is blank, matching the frozen Phase 1B rule that the pair is all-or-nothing. The Store identifier always comes from the authenticated `/api/v1/catalog/context` response; the browser never invents or persists a Store identity, and Store Service plus the SQLite foreign key remain authoritative. A Pack carrying a SKU cannot be submitted while that context is unavailable — the failure is reported with a retry rather than silently dropped.

## Store Pack Policy

Each Pack may have a Store-scoped policy for purchase/sale enablement, whole-pack purchase, fractional sale, minimum sale increment, and default purchase/sale status. Default checks require their corresponding enabled checks. Fractional sale is disabled for precision-zero Products. Minimum increments are displayed in user quantities and converted to integer atoms. Policy add/edit and archive/restore retain revisions and history.

## Barcodes

A Pack can have multiple GTIN, internal, or Code 128 identifiers. The editor captures namespace, value, optional symbology, and global/store scope. Internal identifiers are forced to current-store scope. Values are immutable after assignment; correction means archive and add a replacement. Barcode list/add/archive/restore use the Pack relationship. Batch Barcodes are out of scope.

## Authorization and audit actor

All Product Catalog reads require a valid local session. Owner/Admin, Pharmacist, and Cashier may read. Only Owner/Admin may create, edit, archive, restore, or manage roles, Packs, policies, and Barcodes. Client-side action visibility is supplementary: Store Service enforces every endpoint.

Phase 1B Catalog handlers now use the existing Phase 1C-A authentication boundary. Mutation actor identity is derived exclusively from the validated server session; any browser-supplied actor value is ignored. `master_change_events.actor_id` records that session user for Product, role, Pack, policy, and Barcode changes. This extends the already-approved local-session decision and does not require a new ADR.

## Revision, lifecycle, and errors

Mutations send expected revisions. A stale response says “This record was changed after you opened it” and offers Reload latest/Cancel; no automatic overwrite occurs. Reload latest is a real reload for Product, company role, Pack, Store Pack Policy, and every lifecycle action: it invalidates the authoritative query, awaits the refetch, and repopulates the open editor from the latest record, so the next save carries the reloaded revision. Cancel remains a separate control, and closing a dialog is never treated as a reload. Archive is never delete, requires a reason for Product/role/Pack flows, and restore reruns backend integrity checks.

The client safely maps validation, duplicate, revision, not-found, archived, conversion, Barcode, default-Pack, authentication, authorization, service-busy, and internal failures. Raw SQLite, Rust, and backend messages are not displayed. Authentication expiry returns control to the established login flow.

Loading, empty, error, and success stay four distinct states everywhere, including the Store Pack Policy panel, the Barcode list, reference-name lookups, and the Catalog context. A failed query never renders as a valid empty business state such as “No store policy configured” or “No barcodes assigned”, and never remains a permanent “Loading…”; each failure carries a safe message and its own retry.

Signing out or losing a session clears every cached authenticated query except the startup probe, by exclusion rather than by an enumerated prefix list. Establishing a session does the same before the new session's data is stored, so no business or reference record from a previous session can reach the next one.

## Accessibility and responsive behavior

The feature uses headings, breadcrumbs, labelled controls, semantic tables, text-bearing status indicators, field-level errors, focus-first-invalid behavior, and visible actions. Editors are top-level portal dialogs with unique labels, focus trapping in both Tab and Shift+Tab directions, Escape close, and exact launcher focus restoration. No nested dialog architecture is introduced. Wide tables scroll inside their own container and keep per-cell labels, so a narrow viewport never forces the page itself to scroll horizontally; forms and detail grids adapt while remaining desktop-first for common 1366 px Windows screens. Reduced-motion preferences are honored by the shared shell styles.

## Deferred work

Ingredients, salts/composition, generic/substitute search, batches, expiry, batch MRP, stock and stock ledger, purchasing, sales/POS, customers, suppliers, accounting, GST returns, prescriptions, Excel import, cloud sync, licensing, and LAN multi-counter behavior remain explicitly deferred.
