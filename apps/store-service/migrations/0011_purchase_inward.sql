-- Phase 1G GST-aware purchase inward.
--
-- The first real commercial transaction. A draft is editable and has no stock effect; posting
-- freezes the commercial and tax facts and writes immutable inventory movements, atomically.
--
-- Money is exact integer paise and tax rates exact integer basis points, per ADR-009 and ADR-016.
-- No rate, percentage, or amount is ever recomputed from current masters for a posted document:
-- every figure that decided the document is snapshotted onto it.
--
-- Sales, payment, party balance, accounting journal, GST returns, ITC, and purchase return are all
-- out of scope. Reversal is designed but deliberately not implemented: what this migration
-- guarantees is that a posted document cannot be silently edited or deleted.

-- ---------------------------------------------------------------------------------------------
-- Audit stream: a new entity type, and a new action for the posting transition.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1g0;

CREATE TABLE master_change_events (
    event_id TEXT PRIMARY KEY NOT NULL CHECK (
        length(event_id) = 36 AND substr(event_id, 15, 1) = '7'
        AND lower(substr(event_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_type TEXT NOT NULL CHECK (entity_type IN (
        'unit_of_measure', 'dosage_form', 'pharmaceutical_company', 'company_identifier',
        'brand', 'hsn_code', 'tax_category', 'tax_rate_version', 'regulatory_category',
        'product', 'product_company_role', 'product_pack', 'store_pack_policy', 'barcode',
        'ingredient', 'salt_form', 'strength_unit', 'product_composition_component',
        'product_batch',
        'state_code', 'party', 'party_role', 'party_address',
        'store_tax_identity',
        'purchase_document'
    )),
    entity_id TEXT NOT NULL CHECK (
        length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'
        AND lower(substr(entity_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    -- 'posted' is the commercial transition a document makes exactly once.
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored', 'posted')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

INSERT INTO master_change_events
SELECT * FROM master_change_events_phase1g0;
DROP TABLE master_change_events_phase1g0;

CREATE INDEX master_change_events_entity_idx
ON master_change_events(entity_type, entity_id, entity_revision);

CREATE TRIGGER master_change_events_no_update
BEFORE UPDATE ON master_change_events
BEGIN
    SELECT RAISE(ABORT, 'master_change_events_are_append_only');
END;

CREATE TRIGGER master_change_events_no_delete
BEFORE DELETE ON master_change_events
BEGIN
    SELECT RAISE(ABORT, 'master_change_events_are_append_only');
END;

-- ---------------------------------------------------------------------------------------------
-- Purchase header.
--
-- Carries both a reference to the supplier Party and a SNAPSHOT of the supplier facts that decided
-- the document. The master may legitimately change afterwards; a posted invoice may not.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE purchase_documents (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    supplier_party_id TEXT NOT NULL REFERENCES parties(id) ON DELETE RESTRICT,
    -- Exactly what the operator typed, preserved for display and legal history.
    supplier_invoice_number TEXT NOT NULL CHECK (
        length(trim(supplier_invoice_number)) BETWEEN 1 AND 64
    ),
    -- Whitespace removed and uppercased, nothing else: punctuation can be legally meaningful, so
    -- 'INV/2026/001' and 'INV-2026-001' stay distinct documents.
    normalized_supplier_invoice_number TEXT NOT NULL CHECK (
        normalized_supplier_invoice_number = upper(normalized_supplier_invoice_number)
        AND length(normalized_supplier_invoice_number) BETWEEN 1 AND 64
        AND normalized_supplier_invoice_number NOT GLOB '* *'
    ),
    invoice_date TEXT NOT NULL CHECK (invoice_date GLOB '????-??-??'),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'posted')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),

    -- Supplier snapshot, written at posting.
    supplier_display_name TEXT,
    supplier_legal_name TEXT,
    supplier_gst_registration_status TEXT CHECK (
        supplier_gst_registration_status IS NULL
        OR supplier_gst_registration_status IN ('registered', 'unregistered', 'unknown')
    ),
    supplier_normalized_gstin TEXT,
    supplier_place_of_supply_state_id TEXT REFERENCES state_codes(id) ON DELETE RESTRICT,
    -- The code is snapshotted beside the id because the code is what the treatment compared.
    supplier_state_code TEXT,

    -- Store snapshot, written at posting.
    store_gst_registration_status TEXT CHECK (
        store_gst_registration_status IS NULL
        OR store_gst_registration_status IN ('registered', 'unregistered', 'unknown')
    ),
    store_normalized_gstin TEXT,
    store_place_of_supply_state_id TEXT REFERENCES state_codes(id) ON DELETE RESTRICT,
    store_state_code TEXT,

    -- Decided at posting by comparing the two snapshotted State codes.
    tax_treatment TEXT CHECK (
        tax_treatment IS NULL OR tax_treatment IN ('intra_state', 'inter_state')
    ),

    -- Totals are the sum of the rounded line values; never an independently rounded aggregate.
    taxable_value_paise INTEGER NOT NULL DEFAULT 0 CHECK (taxable_value_paise >= 0),
    cgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (cgst_paise >= 0),
    sgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (sgst_paise >= 0),
    igst_paise INTEGER NOT NULL DEFAULT 0 CHECK (igst_paise >= 0),
    cess_paise INTEGER NOT NULL DEFAULT 0 CHECK (cess_paise >= 0),
    grand_total_paise INTEGER NOT NULL DEFAULT 0 CHECK (grand_total_paise >= 0),

    created_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    posted_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    posted_at_utc TEXT CHECK (posted_at_utc IS NULL OR posted_at_utc GLOB '????-??-??T??:??:??*Z'),
    -- Makes a retried posting safe: the same key returns the same document.
    posting_idempotency_key TEXT CHECK (
        posting_idempotency_key IS NULL OR (
            length(posting_idempotency_key) = 36 AND substr(posting_idempotency_key, 15, 1) = '7'
        )
    ),
    -- SHA-256 over the semantic payload only, so a replay carrying different facts is a conflict.
    posting_fingerprint TEXT CHECK (
        posting_fingerprint IS NULL OR length(posting_fingerprint) = 64
    ),

    -- A posted document carries its full provenance; a draft carries none of it.
    CHECK (
        (status = 'draft' AND posted_by_user_id IS NULL AND posted_at_utc IS NULL
            AND tax_treatment IS NULL AND posting_idempotency_key IS NULL)
        OR (status = 'posted' AND posted_by_user_id IS NOT NULL AND posted_at_utc IS NOT NULL
            AND tax_treatment IS NOT NULL AND posting_idempotency_key IS NOT NULL
            AND supplier_place_of_supply_state_id IS NOT NULL AND store_state_code IS NOT NULL)
    )
) STRICT;

-- One document per supplier invoice, drafts included: catching the duplicate while it is still a
-- draft is the point. Not scoped by financial year — a supplier does not reuse an invoice number,
-- and a year term would silently permit a genuine duplicate straddling April.
CREATE UNIQUE INDEX purchase_documents_supplier_invoice_uq
ON purchase_documents(supplier_party_id, normalized_supplier_invoice_number);

CREATE UNIQUE INDEX purchase_documents_idempotency_uq
ON purchase_documents(posting_idempotency_key)
WHERE posting_idempotency_key IS NOT NULL;

CREATE INDEX purchase_documents_status_idx ON purchase_documents(status, invoice_date DESC);
CREATE INDEX purchase_documents_supplier_idx ON purchase_documents(supplier_party_id);

-- ---------------------------------------------------------------------------------------------
-- Purchase line.
--
-- Commercial quantity is a count of the selected Pack and the rate is paise for ONE such Pack, so
-- taxable value is an exact integer product. Inventory quantity is derived in base atoms. Keeping
-- the two bases separate is what makes both exact (ADR-016).
-- ---------------------------------------------------------------------------------------------
CREATE TABLE purchase_lines (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    purchase_document_id TEXT NOT NULL REFERENCES purchase_documents(id) ON DELETE RESTRICT,
    line_number INTEGER NOT NULL CHECK (line_number >= 1),
    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT,

    -- Proposed batch facts held on a draft line. The Batch master row is created during posting, so
    -- an abandoned draft leaves no orphan behind.
    new_batch_number TEXT,
    new_batch_expires_on TEXT CHECK (
        new_batch_expires_on IS NULL OR new_batch_expires_on GLOB '????-??-??'
    ),
    new_batch_mrp_paise INTEGER CHECK (
        new_batch_mrp_paise IS NULL OR (new_batch_mrp_paise > 0 AND new_batch_mrp_paise <= 100000000000)
    ),

    quantity_packs INTEGER NOT NULL CHECK (quantity_packs > 0 AND quantity_packs <= 1000000),
    rate_per_pack_paise INTEGER NOT NULL CHECK (
        rate_per_pack_paise >= 0 AND rate_per_pack_paise <= 100000000000
    ),
    -- Derived by the server from the frozen Pack conversion; never supplied by a client.
    quantity_atoms INTEGER NOT NULL CHECK (
        quantity_atoms > 0 AND quantity_atoms <= 9000000000000000
    ),
    taxable_value_paise INTEGER NOT NULL CHECK (taxable_value_paise >= 0),

    -- Classification snapshot.
    hsn_code_id TEXT REFERENCES hsn_codes(id) ON DELETE RESTRICT,
    hsn_code TEXT,
    tax_category_id TEXT REFERENCES tax_categories(id) ON DELETE RESTRICT,
    tax_treatment_kind TEXT CHECK (
        tax_treatment_kind IS NULL
        OR tax_treatment_kind IN ('taxable', 'exempt', 'nil_rated', 'non_gst')
    ),
    tax_rate_version_id TEXT REFERENCES tax_rate_versions(id) ON DELETE RESTRICT,

    -- Rate snapshot in exact basis points, as resolved on the invoice date.
    cgst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (cgst_basis_points BETWEEN 0 AND 10000),
    sgst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (sgst_basis_points BETWEEN 0 AND 10000),
    igst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (igst_basis_points BETWEEN 0 AND 10000),
    cess_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (cess_basis_points BETWEEN 0 AND 10000),

    -- Computed amounts, rounded once per component per line.
    cgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (cgst_paise >= 0),
    sgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (sgst_paise >= 0),
    igst_paise INTEGER NOT NULL DEFAULT 0 CHECK (igst_paise >= 0),
    cess_paise INTEGER NOT NULL DEFAULT 0 CHECK (cess_paise >= 0),
    line_total_paise INTEGER NOT NULL DEFAULT 0 CHECK (line_total_paise >= 0),

    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),

    -- Intra-state charges CGST and SGST; inter-state charges IGST. Never both.
    CHECK (NOT (cgst_paise > 0 AND igst_paise > 0)),
    CHECK (NOT (sgst_paise > 0 AND igst_paise > 0)),
    -- A line either names an existing Batch or proposes a new one, never both.
    CHECK (batch_id IS NULL OR new_batch_number IS NULL),
    UNIQUE (purchase_document_id, line_number),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

CREATE INDEX purchase_lines_document_idx ON purchase_lines(purchase_document_id, line_number);
CREATE INDEX purchase_lines_product_idx ON purchase_lines(product_id);

-- ---------------------------------------------------------------------------------------------
-- Posted immutability, enforced by the database rather than by service discipline.
--
-- A draft stays freely editable. The single permitted transition is draft -> posted; after that the
-- row is frozen, and no posted row may be deleted. This mirrors the frozen inventory_movements
-- append-only triggers.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER purchase_documents_posted_no_update
BEFORE UPDATE ON purchase_documents
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'purchase_document_is_posted');
END;

CREATE TRIGGER purchase_documents_posted_no_delete
BEFORE DELETE ON purchase_documents
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'purchase_document_is_posted');
END;

CREATE TRIGGER purchase_lines_posted_no_update
BEFORE UPDATE ON purchase_lines
WHEN EXISTS (
    SELECT 1 FROM purchase_documents
    WHERE id = OLD.purchase_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'purchase_document_is_posted');
END;

CREATE TRIGGER purchase_lines_posted_no_delete
BEFORE DELETE ON purchase_lines
WHEN EXISTS (
    SELECT 1 FROM purchase_documents
    WHERE id = OLD.purchase_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'purchase_document_is_posted');
END;

-- ---------------------------------------------------------------------------------------------
-- Inventory ledger: add the 'purchase' movement type and a real provenance foreign key.
--
-- SQLite cannot alter a CHECK in place, so the table is rebuilt. EVERYTHING is preserved: every
-- column, the primary key, all foreign keys including the self-referencing reversal link, the
-- unique constraint, all four indexes, all three triggers, and every existing row.
--
-- 'adjustment' is deliberately NOT reused for purchases: the ledger must be able to say which
-- purchase and which line an inward came from, and it says so with a foreign key rather than a
-- parsed string.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE inventory_movements RENAME TO inventory_movements_phase1d;

CREATE TABLE inventory_movements (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT,
    movement_type TEXT NOT NULL CHECK (
        movement_type IN ('opening_stock', 'adjustment', 'purchase')
    ),
    quantity_delta_atoms INTEGER NOT NULL CHECK (
        quantity_delta_atoms <> 0
        AND quantity_delta_atoms BETWEEN -9000000000000000 AND 9000000000000000
    ),
    occurred_on TEXT NOT NULL CHECK (occurred_on GLOB '????-??-??'),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 500),
    reverses_movement_id TEXT REFERENCES inventory_movements(id) ON DELETE RESTRICT,
    -- Durable provenance: which purchase line produced this inward.
    purchase_line_id TEXT REFERENCES purchase_lines(id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL CHECK (
        length(idempotency_key) = 36 AND substr(idempotency_key, 15, 1) = '7'
        AND lower(substr(idempotency_key, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    posted_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    posted_at_utc TEXT NOT NULL CHECK (posted_at_utc GLOB '????-??-??T??:??:??*Z'),
    CHECK (movement_type <> 'opening_stock' OR quantity_delta_atoms > 0),
    CHECK (reverses_movement_id IS NULL OR movement_type = 'adjustment'),
    -- Provenance and type agree in both directions, so a purchase movement can never be anonymous
    -- and a non-purchase movement can never claim a purchase line.
    CHECK (movement_type <> 'purchase' OR purchase_line_id IS NOT NULL),
    CHECK (purchase_line_id IS NULL OR movement_type = 'purchase'),
    -- A purchase brings stock in.
    CHECK (movement_type <> 'purchase' OR quantity_delta_atoms > 0),
    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

INSERT INTO inventory_movements (
    id, store_id, product_id, product_pack_id, batch_id, movement_type, quantity_delta_atoms,
    occurred_on, reason, reverses_movement_id, idempotency_key, posted_by_user_id, posted_at_utc
)
SELECT
    id, store_id, product_id, product_pack_id, batch_id, movement_type, quantity_delta_atoms,
    occurred_on, reason, reverses_movement_id, idempotency_key, posted_by_user_id, posted_at_utc
FROM inventory_movements_phase1d;

DROP TABLE inventory_movements_phase1d;

CREATE UNIQUE INDEX inventory_movements_reversal_uq
ON inventory_movements(reverses_movement_id)
WHERE reverses_movement_id IS NOT NULL;

CREATE INDEX inventory_movements_balance_idx
ON inventory_movements(store_id, product_pack_id, batch_id);

CREATE INDEX inventory_movements_history_idx
ON inventory_movements(store_id, occurred_on, posted_at_utc);

CREATE INDEX inventory_movements_product_idx
ON inventory_movements(store_id, product_id);

CREATE INDEX inventory_movements_purchase_idx
ON inventory_movements(purchase_line_id)
WHERE purchase_line_id IS NOT NULL;

CREATE TRIGGER inventory_movements_no_update
BEFORE UPDATE ON inventory_movements
BEGIN
    SELECT RAISE(ABORT, 'inventory_movements_are_append_only');
END;

CREATE TRIGGER inventory_movements_no_delete
BEFORE DELETE ON inventory_movements
BEGIN
    SELECT RAISE(ABORT, 'inventory_movements_are_append_only');
END;

CREATE TRIGGER inventory_movements_integrity_insert
BEFORE INSERT ON inventory_movements
WHEN NOT EXISTS (
        SELECT 1 FROM product_packs pack
        JOIN products product ON product.id = pack.product_id
        WHERE pack.id = NEW.product_pack_id
          AND pack.status = 'active'
          AND product.status = 'active'
    )
 OR (NEW.batch_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM product_batches WHERE id = NEW.batch_id AND status = 'active'
    ))
BEGIN
    SELECT RAISE(ABORT, 'inventory_movement_conflict');
END;
