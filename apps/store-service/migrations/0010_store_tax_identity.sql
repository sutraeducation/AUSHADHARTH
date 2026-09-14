-- Phase 1G-0 store tax identity only. A Purchase prerequisite, not Purchase itself.
--
-- Tax treatment on a document is decided by comparing two places of supply. Phase 1E gave the
-- supplier one; this gives the Store its own. Without it, CGST+SGST versus IGST could only be
-- assumed or asked of the browser, and both are wrong: one silently mis-taxes every inter-state
-- purchase, the other makes the client authoritative for tax.
--
-- One registration only, exactly as Phase 1E decided for parties. A store registered in several
-- states would need a rule for which registration applies to a given document, and that rule cannot
-- be written honestly before the documents exist.

-- Extend the append-only audit stream without editing the earlier migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1f;

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
        'store_tax_identity'
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
SELECT * FROM master_change_events_phase1f;
DROP TABLE master_change_events_phase1f;

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

-- Additive and nullable, so an existing installation keeps working and the operator fills this in
-- before the first GST-aware purchase rather than during a migration.
ALTER TABLE store_identity ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
-- 'unregistered' asserts the store holds no GSTIN; 'unknown' means it has not been captured yet.
ALTER TABLE store_identity ADD COLUMN gst_registration_status TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE store_identity ADD COLUMN gstin TEXT;
ALTER TABLE store_identity ADD COLUMN normalized_gstin TEXT;
-- The Store's own place of supply. The half of the tax-treatment comparison that was missing.
ALTER TABLE store_identity ADD COLUMN place_of_supply_state_id TEXT
    REFERENCES state_codes(id) ON DELETE RESTRICT;

CREATE INDEX store_identity_state_idx ON store_identity(place_of_supply_state_id);

-- SQLite cannot add a CHECK to an existing table, so the coherence rules that `parties` states as
-- table CHECKs are enforced here by a trigger with identical semantics.
--
-- Only a State that actually CHANGES is validated against activity, matching the Phase 1F rule: the
-- endpoint writes every column on each call, so re-validating an unchanged value would trap a store
-- whose State was archived after assignment. Clearing to NULL is therefore always permitted.
CREATE TRIGGER store_identity_tax_integrity_update
BEFORE UPDATE OF gst_registration_status, gstin, normalized_gstin, place_of_supply_state_id
ON store_identity
WHEN
    -- A GSTIN is present exactly when the store is asserted to be registered.
    (NEW.gst_registration_status = 'registered'
        AND (NEW.gstin IS NULL OR NEW.normalized_gstin IS NULL))
 OR (NEW.gst_registration_status <> 'registered'
        AND (NEW.gstin IS NOT NULL OR NEW.normalized_gstin IS NOT NULL))
 OR NEW.gst_registration_status NOT IN ('registered', 'unregistered', 'unknown')
    -- A GSTIN encodes its state of registration, so the place of supply cannot be unknown.
 OR (NEW.normalized_gstin IS NOT NULL AND NEW.place_of_supply_state_id IS NULL)
    -- A newly chosen State must be active.
 OR (NEW.place_of_supply_state_id IS NOT NULL
        AND (OLD.place_of_supply_state_id IS NULL
             OR NEW.place_of_supply_state_id <> OLD.place_of_supply_state_id)
        AND NOT EXISTS (
            SELECT 1 FROM state_codes
            WHERE id = NEW.place_of_supply_state_id AND status = 'active'
        ))
    -- The GSTIN's first two characters are its State code.
 OR (NEW.normalized_gstin IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM state_codes
        WHERE id = NEW.place_of_supply_state_id
          AND jurisdiction = 'IN'
          AND state_code = substr(NEW.normalized_gstin, 1, 2)
    ))
BEGIN
    SELECT RAISE(ABORT, 'store_tax_conflict');
END;
