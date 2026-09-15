-- Phase 1I returns foundation.
--
-- The correction mechanism for immutable posted commercial documents. A posted Sale or Purchase is
-- never mutated; it is corrected by a separate compensating document that references it.
--
-- Three concepts are kept permanently separate, because conflating them would encode a legal lie:
--
--   1. COMMERCIAL RETURN  - what quantity and value of the original document is being reversed.
--   2. GST EVIDENCE       - what tax document, if any, the law attaches to that return.
--   3. STOCK DISPOSITION  - what physical inventory state the returned goods enter.
--
-- On the SALES side our Store is the original supplier, so under CGST section 34(1) it is *we* who
-- may issue a credit note when "the goods supplied are returned by the recipient".
--
-- On the PURCHASE side we are the recipient. Section 34(3) gives the debit note to "the registered
-- person, who has supplied" — the supplier — so a recipient never issues a statutory GST debit note.
-- Circular 72/46/2018-GST gives the returning retailer two routes instead: treat the return as a
-- fresh supply and issue an invoice, or return under a delivery challan and receive the supplier's
-- credit note. Our internal document is therefore a Purchase Return carrying an explicit GST route,
-- and it is NEVER called a debit note anywhere in this schema.
--
-- Excluded on purpose: unlinked returns, exchange, replacement, discount correction, schemes,
-- accounting journal, payable/receivable settlement, GST return filing, automatic refunds,
-- inter-State return logistics, stock valuation, and write-offs unrelated to a commercial return.

-- Extend the append-only audit stream without editing the earlier migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1h;

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
        'purchase_document',
        'controlled_formulation', 'price_control_version', 'product_price_control',
        'sale_document',
        'return_document', 'stock_disposition', 'supplier_credit_note_evidence'
    )),
    entity_id TEXT NOT NULL CHECK (
        length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'
        AND lower(substr(entity_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored', 'posted')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

INSERT INTO master_change_events
SELECT * FROM master_change_events_phase1h;
DROP TABLE master_change_events_phase1h;

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
-- Document numbering: two new kinds on the frozen Phase 1H spine.
--
-- Rule 46(b) caps a tax invoice serial at sixteen characters and Rule 53(1A)(c) caps a credit or
-- debit note at the same, so every series here renders through the one formatter corrected in Phase
-- 1H-C1: `SR/2627/000001` and `PR/2627/000001` are fourteen characters.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE document_number_series RENAME TO document_number_series_phase1h;

CREATE TABLE document_number_series (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    document_kind TEXT NOT NULL CHECK (
        document_kind IN ('sale', 'sales_return', 'purchase_return')
    ),
    series_code TEXT NOT NULL CHECK (
        series_code = upper(trim(series_code)) AND length(series_code) BETWEEN 1 AND 16
        AND series_code NOT GLOB '*[^A-Z0-9]*'
    ),
    financial_year TEXT NOT NULL CHECK (
        length(financial_year) = 7 AND substr(financial_year, 5, 1) = '-'
        AND financial_year NOT GLOB '*[^0-9-]*'
    ),
    next_value INTEGER NOT NULL CHECK (next_value >= 1),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    UNIQUE (store_id, document_kind, series_code, financial_year)
) STRICT;

INSERT INTO document_number_series
SELECT * FROM document_number_series_phase1h;
DROP TABLE document_number_series_phase1h;

-- ---------------------------------------------------------------------------------------------
-- Return documents.
--
-- One spine for both kinds, because everything that must never diverge is shared: linkage to an
-- original, cumulative returnability, proportional reversal from immutable snapshots, numbering,
-- posting, idempotency and immutability. What differs between the kinds is legal, and every legal
-- difference is an explicit column guarded by a kind-conditional CHECK — never hidden behind an
-- abstraction.
--
-- The original document is referenced through two real foreign keys rather than one polymorphic
-- text column, so referential integrity is enforced by the database for both kinds.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE return_documents (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    return_kind TEXT NOT NULL CHECK (return_kind IN ('sales_return', 'purchase_return')),

    -- Linked returns only: exactly one original, of the kind this return corrects.
    original_sale_document_id TEXT REFERENCES sale_documents(id) ON DELETE RESTRICT,
    original_purchase_document_id TEXT REFERENCES purchase_documents(id) ON DELETE RESTRICT,

    business_date TEXT NOT NULL CHECK (business_date GLOB '????-??-??'),
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'posted')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),

    -- Allocated at posting, never before.
    series_code TEXT,
    financial_year TEXT,
    sequence_value INTEGER CHECK (sequence_value IS NULL OR sequence_value >= 1),
    document_number TEXT CHECK (document_number IS NULL OR length(document_number) <= 16),

    -- Snapshot of the original document, so a posted return renders forever without reading it.
    original_document_number TEXT,
    original_document_date TEXT CHECK (
        original_document_date IS NULL OR original_document_date GLOB '????-??-??'
    ),

    -- Store snapshot, written at posting.
    store_gst_registration_status TEXT,
    store_normalized_gstin TEXT,
    store_place_of_supply_state_id TEXT,
    store_state_code TEXT,

    -- Counterparty snapshot: the customer on a sales return, the supplier on a purchase return.
    counterparty_party_id TEXT REFERENCES parties(id) ON DELETE RESTRICT,
    counterparty_display_name TEXT,
    counterparty_gst_registration_status TEXT,
    counterparty_normalized_gstin TEXT,
    counterparty_state_code TEXT,

    tax_treatment TEXT CHECK (
        tax_treatment IS NULL OR tax_treatment IN ('intra_state', 'inter_state')
    ),

    -- SALES SIDE ONLY. Whether the credit note this return evidences may reduce output tax
    -- liability, or is commercial only. The service validates the combination; it does not decide
    -- the law. See docs/adr/ADR-017 for why this is recorded rather than computed.
    tax_adjustment_status TEXT CHECK (
        tax_adjustment_status IS NULL
        OR tax_adjustment_status IN ('tax_adjustable', 'commercial_only')
    ),
    tax_adjustment_reason TEXT CHECK (
        tax_adjustment_reason IS NULL OR length(trim(tax_adjustment_reason)) BETWEEN 1 AND 500
    ),

    -- PURCHASE SIDE ONLY. Circular 72/46/2018-GST route. This document is NOT a debit note.
    gst_route TEXT CHECK (
        gst_route IS NULL OR gst_route IN ('fresh_supply', 'supplier_credit_note')
    ),

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

    posting_idempotency_key TEXT CHECK (
        posting_idempotency_key IS NULL OR (
            length(posting_idempotency_key) = 36 AND substr(posting_idempotency_key, 15, 1) = '7'
            AND lower(substr(posting_idempotency_key, 20, 1)) IN ('8', '9', 'a', 'b')
        )
    ),
    posting_fingerprint TEXT CHECK (
        posting_fingerprint IS NULL OR length(posting_fingerprint) = 64
    ),

    -- Exactly one original, matching the kind.
    CHECK (
        (return_kind = 'sales_return'
            AND original_sale_document_id IS NOT NULL
            AND original_purchase_document_id IS NULL)
        OR (return_kind = 'purchase_return'
            AND original_purchase_document_id IS NOT NULL
            AND original_sale_document_id IS NULL)
    ),
    -- The GST columns belong to their own side and cannot cross over.
    CHECK (return_kind = 'purchase_return' OR gst_route IS NULL),
    CHECK (return_kind = 'sales_return' OR tax_adjustment_status IS NULL),
    CHECK (tax_adjustment_reason IS NULL OR tax_adjustment_status IS NOT NULL),

    -- A draft has decided nothing; a posted return has decided all of it.
    CHECK (
        (status = 'draft' AND document_number IS NULL AND sequence_value IS NULL
            AND posted_at_utc IS NULL AND posted_by_user_id IS NULL
            AND tax_treatment IS NULL AND posting_idempotency_key IS NULL
            AND tax_adjustment_status IS NULL AND gst_route IS NULL)
        OR (status = 'posted' AND document_number IS NOT NULL AND sequence_value IS NOT NULL
            AND series_code IS NOT NULL AND financial_year IS NOT NULL
            AND posted_at_utc IS NOT NULL AND posted_by_user_id IS NOT NULL
            AND tax_treatment IS NOT NULL AND posting_idempotency_key IS NOT NULL
            AND original_document_number IS NOT NULL
            AND (return_kind <> 'sales_return' OR tax_adjustment_status IS NOT NULL)
            AND (return_kind <> 'purchase_return' OR gst_route IS NOT NULL))
    )
) STRICT;

CREATE UNIQUE INDEX return_documents_number_uq
ON return_documents(store_id, document_number)
WHERE document_number IS NOT NULL;

CREATE UNIQUE INDEX return_documents_sequence_uq
ON return_documents(store_id, series_code, financial_year, sequence_value)
WHERE sequence_value IS NOT NULL;

CREATE UNIQUE INDEX return_documents_idempotency_uq
ON return_documents(posting_idempotency_key)
WHERE posting_idempotency_key IS NOT NULL;

CREATE INDEX return_documents_listing_idx
ON return_documents(store_id, return_kind, status, business_date);

CREATE INDEX return_documents_original_sale_idx
ON return_documents(original_sale_document_id)
WHERE original_sale_document_id IS NOT NULL;

CREATE INDEX return_documents_original_purchase_idx
ON return_documents(original_purchase_document_id)
WHERE original_purchase_document_id IS NOT NULL;

-- ---------------------------------------------------------------------------------------------
-- Return lines.
--
-- Every line links to the exact original line it reverses. The commercial and tax facts are copied
-- from that original line's frozen snapshot and are never re-resolved from the mutable masters: a
-- reversal must be equal and opposite even if a rate version, a price or a product name changed in
-- between.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE return_lines (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    return_document_id TEXT NOT NULL REFERENCES return_documents(id) ON DELETE RESTRICT,
    line_number INTEGER NOT NULL CHECK (line_number >= 1),

    -- Exactly one original line, matching the document's kind (enforced by trigger below, because
    -- a CHECK cannot read the parent row).
    original_sale_line_id TEXT REFERENCES sale_lines(id) ON DELETE RESTRICT,
    original_purchase_line_id TEXT REFERENCES purchase_lines(id) ON DELETE RESTRICT,

    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,

    quantity_basis TEXT NOT NULL CHECK (quantity_basis IN ('pack', 'base_unit')),
    quantity_packs INTEGER CHECK (quantity_packs IS NULL OR quantity_packs >= 1),
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms >= 1),

    -- SALES SIDE ONLY. Where the returned goods physically go. `sellable` is deliberately absent:
    -- a commercial return may never create sellable stock, because Indian drug law provides no rule
    -- permitting a retailer to resell a medicine returned by a customer without assessment. Release
    -- to sellable is a separate, separately-authorised disposition transfer.
    disposition TEXT CHECK (
        disposition IS NULL OR disposition IN ('quarantined', 'non_sellable')
    ),

    -- Identity snapshots, copied from the original line.
    product_display_name TEXT,
    pack_display_label TEXT,
    base_unit_label TEXT,
    batch_number TEXT,
    batch_expires_on TEXT,

    -- Tax snapshots, copied from the original line. Never resolved afresh.
    hsn_code_id TEXT,
    hsn_code TEXT,
    tax_category_id TEXT,
    tax_treatment_kind TEXT CHECK (
        tax_treatment_kind IS NULL
        OR tax_treatment_kind IN ('taxable', 'exempt', 'nil_rated', 'non_gst')
    ),
    tax_rate_version_id TEXT,
    cgst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (cgst_basis_points BETWEEN 0 AND 10000),
    sgst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (sgst_basis_points BETWEEN 0 AND 10000),
    igst_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (igst_basis_points BETWEEN 0 AND 10000),
    cess_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (cess_basis_points BETWEEN 0 AND 10000),

    taxable_value_paise INTEGER NOT NULL DEFAULT 0 CHECK (taxable_value_paise >= 0),
    cgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (cgst_paise >= 0),
    sgst_paise INTEGER NOT NULL DEFAULT 0 CHECK (sgst_paise >= 0),
    igst_paise INTEGER NOT NULL DEFAULT 0 CHECK (igst_paise >= 0),
    cess_paise INTEGER NOT NULL DEFAULT 0 CHECK (cess_paise >= 0),
    line_total_paise INTEGER NOT NULL DEFAULT 0 CHECK (line_total_paise >= 0),

    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),

    CHECK (
        (quantity_basis = 'pack' AND quantity_packs IS NOT NULL)
        OR (quantity_basis = 'base_unit' AND quantity_packs IS NULL)
    ),
    -- Exactly one original line reference.
    CHECK (
        (original_sale_line_id IS NOT NULL AND original_purchase_line_id IS NULL)
        OR (original_purchase_line_id IS NOT NULL AND original_sale_line_id IS NULL)
    ),
    UNIQUE (return_document_id, line_number),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

CREATE INDEX return_lines_document_idx ON return_lines(return_document_id, line_number);

-- The returnability query reads these constantly: every posted return against one original line.
CREATE INDEX return_lines_original_sale_idx
ON return_lines(original_sale_line_id)
WHERE original_sale_line_id IS NOT NULL;

CREATE INDEX return_lines_original_purchase_idx
ON return_lines(original_purchase_line_id)
WHERE original_purchase_line_id IS NOT NULL;

-- A line's original must belong to the same kind as its document, and a sales return line must
-- state a disposition while a purchase return line must not.
CREATE TRIGGER return_lines_kind_insert
BEFORE INSERT ON return_lines
WHEN NOT EXISTS (
    SELECT 1 FROM return_documents document
    WHERE document.id = NEW.return_document_id
      AND (
          (document.return_kind = 'sales_return'
              AND NEW.original_sale_line_id IS NOT NULL
              AND NEW.disposition IS NOT NULL)
          OR (document.return_kind = 'purchase_return'
              AND NEW.original_purchase_line_id IS NOT NULL
              AND NEW.disposition IS NULL)
      )
)
BEGIN
    SELECT RAISE(ABORT, 'return_line_kind_conflict');
END;

CREATE TRIGGER return_lines_kind_update
BEFORE UPDATE OF original_sale_line_id, original_purchase_line_id, disposition, return_document_id
ON return_lines
WHEN NOT EXISTS (
    SELECT 1 FROM return_documents document
    WHERE document.id = NEW.return_document_id
      AND (
          (document.return_kind = 'sales_return'
              AND NEW.original_sale_line_id IS NOT NULL
              AND NEW.disposition IS NOT NULL)
          OR (document.return_kind = 'purchase_return'
              AND NEW.original_purchase_line_id IS NOT NULL
              AND NEW.disposition IS NULL)
      )
)
BEGIN
    SELECT RAISE(ABORT, 'return_line_kind_conflict');
END;

-- ---------------------------------------------------------------------------------------------
-- Posted immutability. A posted return is a document that left the store, exactly like a posted
-- sale or purchase.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER return_documents_posted_no_update
BEFORE UPDATE ON return_documents
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'return_document_is_posted');
END;

CREATE TRIGGER return_documents_posted_no_delete
BEFORE DELETE ON return_documents
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'return_document_is_posted');
END;

CREATE TRIGGER return_lines_posted_no_update
BEFORE UPDATE ON return_lines
WHEN EXISTS (
    SELECT 1 FROM return_documents
    WHERE id = OLD.return_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'return_document_is_posted');
END;

CREATE TRIGGER return_lines_posted_no_delete
BEFORE DELETE ON return_lines
WHEN EXISTS (
    SELECT 1 FROM return_documents
    WHERE id = OLD.return_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'return_document_is_posted');
END;

-- A return may only be raised against a POSTED original. A draft original has issued nothing and
-- moved no stock, so there is nothing to reverse.
CREATE TRIGGER return_documents_original_posted_insert
BEFORE INSERT ON return_documents
WHEN (NEW.original_sale_document_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM sale_documents WHERE id = NEW.original_sale_document_id AND status = 'posted'
     ))
  OR (NEW.original_purchase_document_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM purchase_documents
        WHERE id = NEW.original_purchase_document_id AND status = 'posted'
     ))
BEGIN
    SELECT RAISE(ABORT, 'original_document_not_posted');
END;

-- ---------------------------------------------------------------------------------------------
-- Stock disposition transfers.
--
-- Releasing quarantined stock to sellable is not a mutation and not an adjustment: it is a pair of
-- append-only movements that move quantity between statuses within one batch, leaving the physical
-- total unchanged. This header gives that pair one identity, one authoriser and one reason.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE stock_dispositions (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    quantity_atoms INTEGER NOT NULL CHECK (
        quantity_atoms >= 1 AND quantity_atoms <= 9000000000000000
    ),
    -- Quarantine is the only place a transfer starts: stock written off stays written off, and
    -- correcting that judgement is a deliberate act with its own design rather than a reuse of
    -- the release door.
    from_status TEXT NOT NULL CHECK (from_status = 'quarantined'),
    to_status TEXT NOT NULL CHECK (to_status IN ('sellable', 'non_sellable')),
    reason TEXT NOT NULL CHECK (length(trim(reason)) BETWEEN 1 AND 500),
    occurred_on TEXT NOT NULL CHECK (occurred_on GLOB '????-??-??'),
    authorised_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    idempotency_key TEXT NOT NULL CHECK (
        length(idempotency_key) = 36 AND substr(idempotency_key, 15, 1) = '7'
        AND lower(substr(idempotency_key, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    -- A transfer must actually move the goods somewhere else.
    CHECK (from_status <> to_status),
    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

CREATE INDEX stock_dispositions_batch_idx
ON stock_dispositions(store_id, product_pack_id, batch_id);

CREATE TRIGGER stock_dispositions_no_update
BEFORE UPDATE ON stock_dispositions
BEGIN
    SELECT RAISE(ABORT, 'stock_dispositions_are_append_only');
END;

CREATE TRIGGER stock_dispositions_no_delete
BEFORE DELETE ON stock_dispositions
BEGIN
    SELECT RAISE(ABORT, 'stock_dispositions_are_append_only');
END;

-- ---------------------------------------------------------------------------------------------
-- Supplier credit-note evidence.
--
-- Under Circular 72/46/2018-GST route B the supplier issues the credit note, which reaches us later
-- — often days later. Capturing it must never require mutating the posted purchase return, so it is
-- its own append-only evidence row.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE supplier_credit_note_evidence (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    return_document_id TEXT NOT NULL REFERENCES return_documents(id) ON DELETE RESTRICT,
    credit_note_number TEXT NOT NULL CHECK (
        length(trim(credit_note_number)) BETWEEN 1 AND 64
    ),
    normalized_credit_note_number TEXT NOT NULL CHECK (
        normalized_credit_note_number = upper(trim(normalized_credit_note_number))
    ),
    credit_note_date TEXT NOT NULL CHECK (credit_note_date GLOB '????-??-??'),
    credit_note_amount_paise INTEGER NOT NULL CHECK (
        credit_note_amount_paise >= 0 AND credit_note_amount_paise <= 100000000000
    ),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    recorded_at_utc TEXT NOT NULL CHECK (recorded_at_utc GLOB '????-??-??T??:??:??*Z'),
    UNIQUE (return_document_id, normalized_credit_note_number)
) STRICT;

CREATE INDEX supplier_credit_note_evidence_return_idx
ON supplier_credit_note_evidence(return_document_id);

CREATE TRIGGER supplier_credit_note_evidence_no_update
BEFORE UPDATE ON supplier_credit_note_evidence
BEGIN
    SELECT RAISE(ABORT, 'supplier_credit_note_evidence_is_append_only');
END;

CREATE TRIGGER supplier_credit_note_evidence_no_delete
BEFORE DELETE ON supplier_credit_note_evidence
BEGIN
    SELECT RAISE(ABORT, 'supplier_credit_note_evidence_is_append_only');
END;

-- Evidence belongs only to a posted purchase return that elected the supplier-credit-note route.
CREATE TRIGGER supplier_credit_note_evidence_route_insert
BEFORE INSERT ON supplier_credit_note_evidence
WHEN NOT EXISTS (
    SELECT 1 FROM return_documents
    WHERE id = NEW.return_document_id
      AND return_kind = 'purchase_return'
      AND status = 'posted'
      AND gst_route = 'supplier_credit_note'
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_credit_note_route_conflict');
END;

-- ---------------------------------------------------------------------------------------------
-- Inventory ledger: add the return movement types, return provenance, and the stock-status
-- dimension.
--
-- SQLite cannot alter a CHECK in place, so the table is rebuilt. EVERYTHING from the Phase 1D
-- ledger and its Phase 1G and Phase 1H extensions is reproduced verbatim below — every column,
-- constraint, foreign key, index and trigger — plus the deliberate additions. The frozen integrity
-- tests are re-run after a FORCED recompilation to prove the rebuild erased nothing.
--
-- `stock_status` is the dimension that lets quarantined stock exist without being sellable. It is
-- NOT a mutable balance: every movement carries the status its quantity belongs to, and a balance
-- is still derived by summing movements — now summed per status. Existing rows backfill to
-- `sellable`, which is what they have always meant.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE inventory_movements RENAME TO inventory_movements_phase1h;

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
        movement_type IN (
            'opening_stock', 'adjustment', 'purchase', 'sale',
            'sales_return', 'purchase_return', 'disposition_transfer'
        )
    ),
    -- Which stock status this quantity belongs to. POS availability reads 'sellable' only.
    stock_status TEXT NOT NULL DEFAULT 'sellable' CHECK (
        stock_status IN ('sellable', 'quarantined', 'non_sellable')
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
    -- Durable provenance: which sale line produced this outward.
    sale_line_id TEXT REFERENCES sale_lines(id) ON DELETE RESTRICT,
    -- Durable provenance: which return line produced this compensating movement.
    return_line_id TEXT REFERENCES return_lines(id) ON DELETE RESTRICT,
    -- Durable provenance: which disposition transfer produced this status movement.
    stock_disposition_id TEXT REFERENCES stock_dispositions(id) ON DELETE RESTRICT,
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
    -- The same pairing for a sale, which takes stock out.
    CHECK (movement_type <> 'sale' OR sale_line_id IS NOT NULL),
    CHECK (sale_line_id IS NULL OR movement_type = 'sale'),
    CHECK (movement_type <> 'sale' OR quantity_delta_atoms < 0),
    -- A sale always leaves a specific lot, and always leaves sellable stock.
    CHECK (movement_type <> 'sale' OR batch_id IS NOT NULL),
    CHECK (movement_type <> 'sale' OR stock_status = 'sellable'),
    -- The same pairing strength for both return kinds.
    CHECK (movement_type NOT IN ('sales_return', 'purchase_return') OR return_line_id IS NOT NULL),
    CHECK (return_line_id IS NULL OR movement_type IN ('sales_return', 'purchase_return')),
    -- A sales return brings goods back in; a purchase return sends them out.
    CHECK (movement_type <> 'sales_return' OR quantity_delta_atoms > 0),
    CHECK (movement_type <> 'purchase_return' OR quantity_delta_atoms < 0),
    -- A return always concerns a specific lot, because the original line named one.
    CHECK (movement_type NOT IN ('sales_return', 'purchase_return') OR batch_id IS NOT NULL),
    -- Goods coming back from a customer never arrive sellable.
    CHECK (movement_type <> 'sales_return' OR stock_status IN ('quarantined', 'non_sellable')),
    -- Stock leaving for a supplier leaves stock that was sellable.
    CHECK (movement_type <> 'purchase_return' OR stock_status = 'sellable'),
    -- The same pairing for a status transfer, which always names its own header and its lot.
    CHECK (movement_type <> 'disposition_transfer' OR stock_disposition_id IS NOT NULL),
    CHECK (stock_disposition_id IS NULL OR movement_type = 'disposition_transfer'),
    CHECK (movement_type <> 'disposition_transfer' OR batch_id IS NOT NULL),
    -- Everything the store bought, opened with, or corrected by hand is sellable stock.
    CHECK (
        movement_type NOT IN ('opening_stock', 'purchase') OR stock_status = 'sellable'
    ),
    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

-- Every existing movement is sellable stock, which is what it has always meant.
INSERT INTO inventory_movements (
    id, store_id, product_id, product_pack_id, batch_id, movement_type, stock_status,
    quantity_delta_atoms, occurred_on, reason, reverses_movement_id, purchase_line_id,
    sale_line_id, idempotency_key, posted_by_user_id, posted_at_utc
)
SELECT
    id, store_id, product_id, product_pack_id, batch_id, movement_type, 'sellable',
    quantity_delta_atoms, occurred_on, reason, reverses_movement_id, purchase_line_id,
    sale_line_id, idempotency_key, posted_by_user_id, posted_at_utc
FROM inventory_movements_phase1h;

DROP TABLE inventory_movements_phase1h;

CREATE UNIQUE INDEX inventory_movements_reversal_uq
ON inventory_movements(reverses_movement_id)
WHERE reverses_movement_id IS NOT NULL;

-- The grain a balance is evaluated and enforced at, now including the status it belongs to.
CREATE INDEX inventory_movements_balance_idx
ON inventory_movements(store_id, product_pack_id, batch_id, stock_status);

CREATE INDEX inventory_movements_history_idx
ON inventory_movements(store_id, occurred_on, posted_at_utc);

CREATE INDEX inventory_movements_product_idx
ON inventory_movements(store_id, product_id);

CREATE INDEX inventory_movements_purchase_idx
ON inventory_movements(purchase_line_id)
WHERE purchase_line_id IS NOT NULL;

CREATE INDEX inventory_movements_sale_idx
ON inventory_movements(sale_line_id)
WHERE sale_line_id IS NOT NULL;

CREATE INDEX inventory_movements_return_idx
ON inventory_movements(return_line_id)
WHERE return_line_id IS NOT NULL;

CREATE INDEX inventory_movements_disposition_idx
ON inventory_movements(stock_disposition_id)
WHERE stock_disposition_id IS NOT NULL;

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
