-- Phase 1C-C batch identity, expiry dates, and batch MRP only.
-- Stock, stock ledger, valuation, purchase, sales, pricing, and tax remain deferred.
-- Recording a batch is identity, never a stock receipt.

-- Extend the append-only audit stream without editing the earlier migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1cb;

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
        'product_batch'
    )),
    entity_id TEXT NOT NULL CHECK (
        length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'
        AND lower(substr(entity_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

INSERT INTO master_change_events
SELECT * FROM master_change_events_phase1cb;
DROP TABLE master_change_events_phase1cb;

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

-- One manufactured lot of one Product Pack.
--
-- Identity and commercial metadata only. This table deliberately carries NO quantity, balance, or
-- stock column: a batch existing is not stock existing. Every future on-hand figure must derive from
-- the inventory ledger, never from a mutable number stored here.
--
-- No store_id: a batch number is the manufacturer's code for a physical lot, and its expiry,
-- manufacturing date, and printed MRP are intrinsic to the lot rather than to whoever holds it. The
-- future ledger carries (store_id, product_pack_id, batch_id) per movement, so per-store balances
-- derive without duplicating lot identity, and one lot stays one identity to recall or trace.
CREATE TABLE product_batches (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    product_pack_id TEXT NOT NULL REFERENCES product_packs(id) ON DELETE RESTRICT,
    -- The lot string exactly as the operator saw it printed.
    batch_number TEXT NOT NULL CHECK (length(trim(batch_number)) BETWEEN 1 AND 64),
    -- What uniqueness compares: uppercased with whitespace removed.
    normalized_batch_number TEXT NOT NULL CHECK (
        normalized_batch_number = upper(trim(normalized_batch_number))
        AND length(normalized_batch_number) BETWEEN 1 AND 64
        AND normalized_batch_number NOT GLOB '*[^A-Z0-9._/-]*'
    ),
    -- Calendar-domain values: no time of day, no offset, no timezone drift.
    manufactured_on TEXT CHECK (manufactured_on IS NULL OR manufactured_on GLOB '????-??-??'),
    expires_on TEXT CHECK (expires_on IS NULL OR expires_on GLOB '????-??-??'),
    -- Money is exact integer minor units per ADR-009. Never REAL, never a decimal string.
    mrp_paise INTEGER CHECK (
        mrp_paise IS NULL OR (mrp_paise > 0 AND mrp_paise <= 100000000000)
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    CHECK (
        manufactured_on IS NULL OR expires_on IS NULL OR expires_on >= manufactured_on
    ),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

-- Manufacturers reuse lot strings across products, so batch numbers are not globally unique. The
-- boundary is one active lot per Pack.
CREATE UNIQUE INDEX product_batches_active_uq
ON product_batches(product_pack_id, normalized_batch_number)
WHERE status = 'active';

-- Soonest expiry first, undated lots last. Presentation ordering only, not a FEFO issue policy.
CREATE INDEX product_batches_pack_idx
ON product_batches(product_pack_id, status, expires_on);

CREATE TRIGGER product_batches_integrity_insert
BEFORE INSERT ON product_batches
WHEN NOT EXISTS (
    SELECT 1 FROM product_packs pack
    JOIN products product ON product.id = pack.product_id
    WHERE pack.id = NEW.product_pack_id
      AND pack.status = 'active'
      AND product.status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'product_batch_conflict');
END;

CREATE TRIGGER product_batches_integrity_update
BEFORE UPDATE OF product_pack_id, status ON product_batches
WHEN NEW.status = 'active' AND NOT EXISTS (
    SELECT 1 FROM product_packs pack
    JOIN products product ON product.id = pack.product_id
    WHERE pack.id = NEW.product_pack_id
      AND pack.status = 'active'
      AND product.status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'product_batch_conflict');
END;
