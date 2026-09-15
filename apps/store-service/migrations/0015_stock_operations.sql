-- ---------------------------------------------------------------------------------------------
-- Phase 1J — Stock operations.
--
-- Phase 1D gave the ledger a generic `adjustment`: a signed correction with an optional line of
-- free text. It was enough to keep an append-only ledger honest, and it is not enough to run a
-- pharmacy. A crushed strip, a shelf that counted two short, a lot that expired on the shelf and a
-- carton the disposal contractor took away are four different facts about the business, and a
-- system that records all four as "adjustment: -10" cannot later tell anyone which happened.
--
-- This migration adds the document that says which. Operator intent is a typed `operation_kind`,
-- the inventory cause is a typed `reason_code`, and the free text stays what it always should have
-- been: supplementary detail for a human, never the thing a report has to parse.
--
-- THE CUSTODY MODEL, stated once because everything below depends on it.
--
--   sellable      goods held, available to sell, subject to every independent control — a Batch
--                 past its expiry is refused by the POS whatever this table says.
--   quarantined   goods held, unavailable, waiting for somebody to decide.
--   non_sellable  goods held, permanently unavailable. Written off from saleable inventory and
--                 STILL IN THE BUILDING.
--
-- Physical custody is therefore sellable + quarantined + non_sellable, and a write-off moves
-- quantity between those columns without changing the total. Custody ends only when goods
-- physically leave, which Phase 1J records as a negative `stock_removal` movement against
-- `non_sellable`. There is deliberately no fourth `removed` status: removed quantity is history,
-- not a balance, and a status row that accumulated it would be counted as stock by every future
-- report that sums the statuses.
--
-- Phase 1I left `non_sellable` unreachable in the outward direction — nothing in the schema could
-- reduce it — so a write-off register would have contradicted the ledger it was built on. That is
-- the specific hole `stock_removal` closes.
--
-- Excluded on purpose: valuation, cost layers, FIFO, weighted average, COGS, journals, GST
-- filing, barcode resolution, printing, stock transfer between stores, reorder suggestions.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- master_change_events — rebuilt to admit the new entity types.
-- Reproduced verbatim from 0014 apart from the two added values.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1i;

DROP TRIGGER IF EXISTS master_change_events_no_update;
DROP TRIGGER IF EXISTS master_change_events_no_delete;
DROP INDEX IF EXISTS master_change_events_entity_idx;

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
        'return_document', 'stock_disposition', 'supplier_credit_note_evidence',
        'stock_operation'
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
SELECT * FROM master_change_events_phase1i;

DROP TABLE master_change_events_phase1i;

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
-- Stock operations — the document.
--
-- Draft → Posted, the same one-way spine the purchase, sale and return documents already use. A
-- physical count is the reason this is a document rather than a bare movement: counting a shelf
-- produces many lines that share one date, one counter and one decision, and either all of them
-- are true together or the count has to be done again.
--
-- No statutory number series. A stock operation is an internal record, not a tax document — the
-- purchase side already demonstrates a document that carries no store-issued serial.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE stock_operations (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,

    -- What the operator set out to do. Each kind is a separate screen with its own safeguards,
    -- because "reduce this number" is not a thing a pharmacist ever actually means.
    operation_kind TEXT NOT NULL CHECK (operation_kind IN (
        'physical_count',   -- counted the shelf; the server works out the variance
        'adjustment',       -- Phase 1D's generic correction, finally reachable from a screen
        'damage',           -- goods spoiled in custody
        'expiry',           -- a lot that reached its expiry date on the shelf
        'quarantine',       -- held back pending a decision
        'removal'           -- physical custody ended
    )),

    business_date TEXT NOT NULL CHECK (business_date GLOB '????-??-??'),
    status TEXT NOT NULL CHECK (status IN ('draft', 'posted')),
    revision INTEGER NOT NULL CHECK (revision >= 1),

    -- Document-level human detail. Never the thing a report reads.
    note TEXT CHECK (note IS NULL OR length(trim(note)) BETWEEN 1 AND 500),

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

    -- Draft and posted are different states, not one row with a flag. A draft holds none of the
    -- posting facts and a posted document holds all of them.
    CHECK (
        (status = 'draft' AND posted_by_user_id IS NULL AND posted_at_utc IS NULL
            AND posting_idempotency_key IS NULL)
        OR (status = 'posted' AND posted_by_user_id IS NOT NULL AND posted_at_utc IS NOT NULL
            AND posting_idempotency_key IS NOT NULL)
    )
) STRICT;

CREATE UNIQUE INDEX stock_operations_idempotency_uq
ON stock_operations(posting_idempotency_key)
WHERE posting_idempotency_key IS NOT NULL;

CREATE INDEX stock_operations_listing_idx
ON stock_operations(store_id, operation_kind, status, business_date);

CREATE TRIGGER stock_operations_posted_no_update
BEFORE UPDATE ON stock_operations
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_is_posted');
END;

CREATE TRIGGER stock_operations_posted_no_delete
BEFORE DELETE ON stock_operations
WHEN OLD.status = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_is_posted');
END;

-- ---------------------------------------------------------------------------------------------
-- Stock operation lines.
--
-- `stock_status` is the status the line acts ON; `target_stock_status` is where the quantity goes
-- when the line is a transfer, and NULL when it is not. A count carries what was counted and,
-- after posting, the variance the server worked out — never a delta the browser sent.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE stock_operation_lines (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    stock_operation_id TEXT NOT NULL REFERENCES stock_operations(id) ON DELETE RESTRICT,
    line_number INTEGER NOT NULL CHECK (line_number >= 1),

    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT,

    stock_status TEXT NOT NULL CHECK (
        stock_status IN ('sellable', 'quarantined', 'non_sellable')
    ),
    target_stock_status TEXT CHECK (
        target_stock_status IS NULL
        OR target_stock_status IN ('sellable', 'quarantined', 'non_sellable')
    ),

    -- Which way the quantity moves. A transfer states both ends; everything else states one.
    direction TEXT NOT NULL CHECK (direction IN ('count', 'increase', 'decrease', 'transfer')),

    -- The inventory fact. Future reports, valuation and authorisation read THIS, never the note.
    reason_code TEXT NOT NULL CHECK (reason_code IN (
        'physical_count_gain',
        'physical_count_loss',
        'damage',
        'breakage',
        'expiry',
        'theft_or_loss',
        'data_correction',
        'quality_hold',
        'disposal'
    )),

    -- Physical count only: what the operator actually counted. Zero is a real answer.
    counted_atoms INTEGER CHECK (
        counted_atoms IS NULL
        OR (counted_atoms >= 0 AND counted_atoms <= 9000000000000000)
    ),
    -- Everything except a count: the quantity asked for.
    quantity_atoms INTEGER CHECK (
        quantity_atoms IS NULL
        OR (quantity_atoms >= 1 AND quantity_atoms <= 9000000000000000)
    ),
    -- How the operator expressed it, so the screen can show back what they typed.
    quantity_basis TEXT NOT NULL CHECK (quantity_basis IN ('pack', 'base_unit')),
    quantity_packs INTEGER CHECK (quantity_packs IS NULL OR quantity_packs >= 1),

    -- Written at posting: the signed effect the server computed and applied. NULL while draft.
    applied_delta_atoms INTEGER CHECK (
        applied_delta_atoms IS NULL
        OR applied_delta_atoms BETWEEN -9000000000000000 AND 9000000000000000
    ),

    note TEXT CHECK (note IS NULL OR length(trim(note)) BETWEEN 1 AND 500),

    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),

    -- A count states what was counted and nothing else; everything else states a quantity.
    CHECK (
        (direction = 'count' AND counted_atoms IS NOT NULL AND quantity_atoms IS NULL)
        OR (direction <> 'count' AND quantity_atoms IS NOT NULL AND counted_atoms IS NULL)
    ),
    -- A transfer names its destination, and it must actually be somewhere else.
    CHECK (
        (direction = 'transfer' AND target_stock_status IS NOT NULL
            AND target_stock_status <> stock_status)
        OR (direction <> 'transfer' AND target_stock_status IS NULL)
    ),
    CHECK (
        (quantity_basis = 'pack' AND quantity_packs IS NOT NULL)
        OR (quantity_basis = 'base_unit' AND quantity_packs IS NULL)
    ),
    UNIQUE (stock_operation_id, line_number),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

CREATE INDEX stock_operation_lines_document_idx
ON stock_operation_lines(stock_operation_id, line_number);

CREATE INDEX stock_operation_lines_identity_idx
ON stock_operation_lines(product_pack_id, batch_id, stock_status);

-- A line must agree with the kind of document it belongs to. A CHECK cannot read the parent row,
-- so this is a trigger — the same shape Phase 1I uses for return lines.
--
-- This is where operator intent stops being a label and becomes an invariant: a `damage` document
-- cannot quietly contain an expiry write-off, and a `removal` cannot take goods from the shelf.
CREATE TRIGGER stock_operation_lines_kind_insert
BEFORE INSERT ON stock_operation_lines
WHEN (
    SELECT CASE (SELECT operation_kind FROM stock_operations WHERE id = NEW.stock_operation_id)
        WHEN 'physical_count' THEN
            NEW.direction <> 'count'
            OR NEW.reason_code NOT IN ('physical_count_gain', 'physical_count_loss')
        WHEN 'adjustment' THEN
            NEW.direction NOT IN ('increase', 'decrease')
            OR NEW.reason_code NOT IN ('theft_or_loss', 'data_correction')
            -- Goods taken or lost are already gone, so the quantity can only fall.
            OR (NEW.reason_code = 'theft_or_loss' AND NEW.direction <> 'decrease')
        WHEN 'damage' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code NOT IN ('damage', 'breakage')
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status NOT IN ('quarantined', 'non_sellable')
        WHEN 'expiry' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code <> 'expiry'
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status <> 'non_sellable'
        WHEN 'quarantine' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code NOT IN ('quality_hold', 'damage')
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status <> 'quarantined'
        WHEN 'removal' THEN
            NEW.direction <> 'decrease'
            OR NEW.reason_code <> 'disposal'
            -- Custody can only end for goods already written off. Removing sellable stock would
            -- be a sale nobody recorded, and removing quarantined stock would pre-empt the
            -- decision quarantine exists to wait for.
            OR NEW.stock_status <> 'non_sellable'
        ELSE 1
    END
)
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_line_conflict');
END;

CREATE TRIGGER stock_operation_lines_kind_update
BEFORE UPDATE ON stock_operation_lines
WHEN (
    SELECT CASE (SELECT operation_kind FROM stock_operations WHERE id = NEW.stock_operation_id)
        WHEN 'physical_count' THEN
            NEW.direction <> 'count'
            OR NEW.reason_code NOT IN ('physical_count_gain', 'physical_count_loss')
        WHEN 'adjustment' THEN
            NEW.direction NOT IN ('increase', 'decrease')
            OR NEW.reason_code NOT IN ('theft_or_loss', 'data_correction')
            OR (NEW.reason_code = 'theft_or_loss' AND NEW.direction <> 'decrease')
        WHEN 'damage' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code NOT IN ('damage', 'breakage')
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status NOT IN ('quarantined', 'non_sellable')
        WHEN 'expiry' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code <> 'expiry'
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status <> 'non_sellable'
        WHEN 'quarantine' THEN
            NEW.direction <> 'transfer'
            OR NEW.reason_code NOT IN ('quality_hold', 'damage')
            OR NEW.stock_status <> 'sellable'
            OR NEW.target_stock_status <> 'quarantined'
        WHEN 'removal' THEN
            NEW.direction <> 'decrease'
            OR NEW.reason_code <> 'disposal'
            OR NEW.stock_status <> 'non_sellable'
        ELSE 1
    END
)
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_line_conflict');
END;

-- A posted document's lines are as fixed as the document.
CREATE TRIGGER stock_operation_lines_posted_no_update
BEFORE UPDATE ON stock_operation_lines
WHEN (
    SELECT status FROM stock_operations WHERE id = OLD.stock_operation_id
) = 'posted'
 AND OLD.applied_delta_atoms IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_is_posted');
END;

CREATE TRIGGER stock_operation_lines_posted_no_delete
BEFORE DELETE ON stock_operation_lines
WHEN (
    SELECT status FROM stock_operations WHERE id = OLD.stock_operation_id
) = 'posted'
BEGIN
    SELECT RAISE(ABORT, 'stock_operation_is_posted');
END;

-- ---------------------------------------------------------------------------------------------
-- stock_dispositions — rebuilt so quarantine has a way IN as well as a way out.
--
-- Phase 1I built this for one job: releasing or writing off quarantined goods returned by a
-- customer. Phase 1J needs the same paired-movement mechanism to put goods INTO quarantine, so
-- `from_status` widens by exactly one value and gains a typed reason.
--
-- `non_sellable` is deliberately still absent from `from_status`. Phase 1I made a write-off
-- terminal on purpose, and widening the door for quarantine entry must not quietly reopen it.
-- ---------------------------------------------------------------------------------------------
-- The movement table is renamed out of the way first. It carries the only foreign key into
-- stock_dispositions, and SQLite rewrites REFERENCES clauses on rename: leaving it in place
-- would make the old disposition table undroppable, because live rows would still point at it.
ALTER TABLE inventory_movements RENAME TO inventory_movements_phase1i;

DROP TRIGGER IF EXISTS inventory_movements_no_update;
DROP TRIGGER IF EXISTS inventory_movements_no_delete;
DROP TRIGGER IF EXISTS inventory_movements_integrity_insert;
DROP INDEX IF EXISTS inventory_movements_reversal_uq;
DROP INDEX IF EXISTS inventory_movements_balance_idx;
DROP INDEX IF EXISTS inventory_movements_history_idx;
DROP INDEX IF EXISTS inventory_movements_product_idx;
DROP INDEX IF EXISTS inventory_movements_purchase_idx;
DROP INDEX IF EXISTS inventory_movements_sale_idx;
DROP INDEX IF EXISTS inventory_movements_return_idx;
DROP INDEX IF EXISTS inventory_movements_disposition_idx;

ALTER TABLE stock_dispositions RENAME TO stock_dispositions_phase1i;

DROP TRIGGER IF EXISTS stock_dispositions_no_update;
DROP TRIGGER IF EXISTS stock_dispositions_no_delete;
DROP INDEX IF EXISTS stock_dispositions_batch_idx;

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
    -- Quarantine may now be entered as well as left. Stock written off stays written off:
    -- correcting that judgement is a deliberate act with its own design, not a reuse of this door.
    from_status TEXT NOT NULL CHECK (from_status IN ('sellable', 'quarantined')),
    to_status TEXT NOT NULL CHECK (to_status IN ('sellable', 'quarantined', 'non_sellable')),
    -- The typed cause, so a Quarantine Register never has to parse the reason text.
    reason_code TEXT CHECK (reason_code IS NULL OR reason_code IN (
        'damage', 'breakage', 'expiry', 'quality_hold', 'data_correction', 'disposal'
    )),
    reason TEXT NOT NULL CHECK (length(trim(reason)) BETWEEN 1 AND 500),
    occurred_on TEXT NOT NULL CHECK (occurred_on GLOB '????-??-??'),
    authorised_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    idempotency_key TEXT NOT NULL CHECK (
        length(idempotency_key) = 36 AND substr(idempotency_key, 15, 1) = '7'
        AND lower(substr(idempotency_key, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    -- Set when a Phase 1J stock operation produced this transfer; NULL for a Phase 1I release.
    stock_operation_line_id TEXT REFERENCES stock_operation_lines(id) ON DELETE RESTRICT,
    -- A transfer must actually move the goods somewhere else.
    CHECK (from_status <> to_status),
    -- Leaving the shelf is only ever a hold or a write-off; becoming sellable again is only ever
    -- something quarantined goods do.
    CHECK (from_status <> 'sellable' OR to_status IN ('quarantined', 'non_sellable')),
    CHECK (to_status <> 'sellable' OR from_status = 'quarantined'),
    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

INSERT INTO stock_dispositions (
    id, store_id, product_id, product_pack_id, batch_id, quantity_atoms, from_status, to_status,
    reason_code, reason, occurred_on, authorised_by_user_id, created_at_utc, idempotency_key,
    stock_operation_line_id
)
SELECT
    id, store_id, product_id, product_pack_id, batch_id, quantity_atoms, from_status, to_status,
    NULL, reason, occurred_on, authorised_by_user_id, created_at_utc, idempotency_key,
    NULL
FROM stock_dispositions_phase1i;

-- Deliberately NOT dropped yet: inventory_movements_phase1i still references it.

CREATE INDEX stock_dispositions_batch_idx
ON stock_dispositions(store_id, product_pack_id, batch_id);

CREATE INDEX stock_dispositions_operation_idx
ON stock_dispositions(stock_operation_line_id)
WHERE stock_operation_line_id IS NOT NULL;

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
-- inventory_movements — rebuilt for two new movement types and one new provenance column.
--
-- Everything from 0014 is reproduced verbatim before anything is added. The additions are:
--   * `stock_count`   a physical count variance, either sign, at the status counted
--   * `stock_removal` physical custody ending, negative, non_sellable only
--   * `stock_operation_line_id` provenance, paired both ways like every other provenance column
-- ---------------------------------------------------------------------------------------------
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
            'sales_return', 'purchase_return', 'disposition_transfer',
            'stock_count', 'stock_removal'
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
    -- Durable provenance: which stock operation line produced this movement.
    stock_operation_line_id TEXT REFERENCES stock_operation_lines(id) ON DELETE RESTRICT,
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

    -- ---- Phase 1J ----------------------------------------------------------------------------
    -- A stock-operation movement can never be anonymous, and nothing else may claim one. The
    -- movement types that carry it are exactly those a stock operation can produce: a count
    -- variance, a removal, a status transfer, and the Phase 1D adjustment now reachable from a
    -- screen.
    CHECK (
        movement_type NOT IN ('stock_count', 'stock_removal')
        OR stock_operation_line_id IS NOT NULL
    ),
    CHECK (
        stock_operation_line_id IS NULL
        OR movement_type IN (
            'stock_count', 'stock_removal', 'disposition_transfer', 'adjustment'
        )
    ),
    -- Custody ends only for goods already written off, and only ever downwards.
    CHECK (movement_type <> 'stock_removal' OR quantity_delta_atoms < 0),
    CHECK (movement_type <> 'stock_removal' OR stock_status = 'non_sellable'),
    CHECK (movement_type <> 'stock_removal' OR batch_id IS NOT NULL),
    -- A count is counted against a specific lot, or against the batchless balance of a pack.
    CHECK (movement_type <> 'stock_count' OR reverses_movement_id IS NULL),

    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

-- Every existing movement carries forward unchanged, with no stock operation behind it.
INSERT INTO inventory_movements (
    id, store_id, product_id, product_pack_id, batch_id, movement_type, stock_status,
    quantity_delta_atoms, occurred_on, reason, reverses_movement_id, purchase_line_id,
    sale_line_id, return_line_id, stock_disposition_id, stock_operation_line_id,
    idempotency_key, posted_by_user_id, posted_at_utc
)
SELECT
    id, store_id, product_id, product_pack_id, batch_id, movement_type, stock_status,
    quantity_delta_atoms, occurred_on, reason, reverses_movement_id, purchase_line_id,
    sale_line_id, return_line_id, stock_disposition_id, NULL,
    idempotency_key, posted_by_user_id, posted_at_utc
FROM inventory_movements_phase1i;

DROP TABLE inventory_movements_phase1i;

-- Nothing references the old disposition table any more, so it can finally go.
DROP TABLE stock_dispositions_phase1i;

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

CREATE INDEX inventory_movements_operation_idx
ON inventory_movements(stock_operation_line_id)
WHERE stock_operation_line_id IS NOT NULL;

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
