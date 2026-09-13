-- Phase 1D inventory movement ledger and opening stock only.
-- Purchase, sales, valuation, cost, pricing, and transfers remain deferred.
--
-- The ledger is the sole authority for quantity. No mutable balance column exists on products,
-- product_packs, or product_batches, and none may ever be added: every on-hand figure is derived
-- by summing signed movements.

-- A composite foreign key needs a uniquely indexed parent key. product_packs already declares
-- UNIQUE (id, product_id) from Phase 1B; product_batches gains the matching pair so a movement can
-- never reference a batch belonging to a different pack.
CREATE UNIQUE INDEX product_batches_id_pack_uq
ON product_batches(id, product_pack_id);

CREATE TABLE inventory_movements (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    -- Quantity is always owned by a Store. Batch identity deliberately is not.
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL,
    product_pack_id TEXT NOT NULL,
    batch_id TEXT,
    movement_type TEXT NOT NULL CHECK (movement_type IN ('opening_stock', 'adjustment')),
    -- Exact signed integer in the Product's base unit at the Product's quantity_scale. Counting
    -- packs would make a fractional issue unrepresentable, so packs are context, not the unit.
    quantity_delta_atoms INTEGER NOT NULL CHECK (
        quantity_delta_atoms <> 0
        AND quantity_delta_atoms BETWEEN -9000000000000000 AND 9000000000000000
    ),
    -- Business calendar date: no time of day, no offset, no timezone drift.
    occurred_on TEXT NOT NULL CHECK (occurred_on GLOB '????-??-??'),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 500),
    reverses_movement_id TEXT REFERENCES inventory_movements(id) ON DELETE RESTRICT,
    -- Makes a retried posting safe: the same key returns the same movement instead of duplicating.
    idempotency_key TEXT NOT NULL CHECK (
        length(idempotency_key) = 36 AND substr(idempotency_key, 15, 1) = '7'
        AND lower(substr(idempotency_key, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    -- The actor is the validated server session user, never a browser-supplied value.
    posted_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    posted_at_utc TEXT NOT NULL CHECK (posted_at_utc GLOB '????-??-??T??:??:??*Z'),
    -- Opening stock establishes a starting quantity and is therefore always positive; an adjustment
    -- is the signed correction mechanism that makes an append-only ledger administrable.
    CHECK (movement_type <> 'opening_stock' OR quantity_delta_atoms > 0),
    CHECK (reverses_movement_id IS NULL OR movement_type = 'adjustment'),
    UNIQUE (idempotency_key),
    FOREIGN KEY (product_pack_id, product_id) REFERENCES product_packs(id, product_id),
    FOREIGN KEY (batch_id, product_pack_id) REFERENCES product_batches(id, product_pack_id)
) STRICT;

-- One movement may be reversed at most once.
CREATE UNIQUE INDEX inventory_movements_reversal_uq
ON inventory_movements(reverses_movement_id)
WHERE reverses_movement_id IS NOT NULL;

-- The grain a balance is evaluated and enforced at.
CREATE INDEX inventory_movements_balance_idx
ON inventory_movements(store_id, product_pack_id, batch_id);

CREATE INDEX inventory_movements_history_idx
ON inventory_movements(store_id, occurred_on, posted_at_utc);

CREATE INDEX inventory_movements_product_idx
ON inventory_movements(store_id, product_id);

-- A posted movement is history. Correction is a further movement, never an edit, so immutability is
-- enforced by the database rather than by service discipline.
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

-- Stock may only be posted against an active Product, Pack, and Batch. An expired batch is still
-- acceptable: expiry is a date, not a lock, and a write-off is a deliberate future movement.
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
