-- ---------------------------------------------------------------------------------------------
-- Backup and restore.
--
-- Three narrow additions. Everything else a backup needs already exists, because the SQLite file
-- has been the whole pharmacy since Phase 0 and nothing outside it is authoritative.
--
--   1. A product fingerprint in the file header, so a restore can tell an AUSHADHARTH database
--      from an ARTHBIZ one, from a random SQLite file, and from something crafted to look like us.
--   2. `restore_provenance`, the permanent record of a restore, written into the database that was
--      restored — because the operational journal that tracked the swap lives outside this file and
--      is deleted when the swap completes.
--   3. Two audit entity types, so a backup and a restore are business events rather than log lines.
--
-- Deliberately NOT here:
--   * the restore journal — it records the replacement of this very file and cannot live inside it;
--   * backup-age reminder state — that is a property of one installation, not of the business, and
--     restoring it would let a six-month-old backup claim on a new machine that it was taken
--     yesterday.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- Product identity.
--
-- SQLite keeps a 32-bit application id at offset 68 of the file header, which is exactly the
-- question a restore has to answer first: is this ours? 0x41555348 is ASCII "AUSH".
--
-- THIS CONSTANT MUST NEVER CHANGE. Every backup taken after this migration carries it, and a
-- restore that rejected the old value would strand the customer's own files.
--
-- Databases created before this migration carry 0. That is not a defect and must not be treated as
-- one: they are identified by the migration checksum chain instead, which is just as much ours and
-- rather harder to forge than four bytes. Restore therefore accepts 0 or 0x41555348, and refuses
-- any other non-zero value.
-- ---------------------------------------------------------------------------------------------
PRAGMA application_id = 1096110920;

-- ---------------------------------------------------------------------------------------------
-- Restore provenance.
--
-- Append-only. One row per successful restore, written after the restored database has been
-- reopened and validated — never before, because a row describing a restore that then failed would
-- be a lie that outlived the operation.
--
-- The lineage columns are the reason this table exists. A restore preserves the Store exactly and
-- issues a NEW installation identity, so `source_installation_id` → `new_installation_id` is the
-- only durable evidence of which machine the data came from. Future licensing reads that chain; it
-- does not read a policy, because Backup and Restore deliberately encode none.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE restore_provenance (
    restore_id TEXT PRIMARY KEY NOT NULL CHECK (
        length(restore_id) = 36 AND substr(restore_id, 15, 1) = '7'
        AND lower(substr(restore_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    restored_at_utc TEXT NOT NULL CHECK (restored_at_utc GLOB '????-??-??T??:??:??*Z'),

    -- Null on a first-run restore, where there is genuinely nobody signed in: the users arrive with
    -- the backup. A fabricated actor would be worse than an honest absence.
    initiated_by_user_id TEXT,
    initiated_by_login TEXT,

    source_backup_created_at_utc TEXT NOT NULL CHECK (
        source_backup_created_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    source_store_id TEXT NOT NULL,
    source_installation_id TEXT NOT NULL,
    new_installation_id TEXT NOT NULL,

    -- Lower-case hex SHA-256 of the snapshot bytes. Corruption detection, not authentication.
    source_database_sha256 TEXT NOT NULL CHECK (
        length(source_database_sha256) = 64 AND source_database_sha256 = lower(source_database_sha256)
        AND source_database_sha256 NOT GLOB '*[^a-f0-9]*'
    ),
    -- Null on a first-run restore, because there was no database to replace.
    pre_restore_database_sha256 TEXT CHECK (
        pre_restore_database_sha256 IS NULL OR (
            length(pre_restore_database_sha256) = 64
            AND pre_restore_database_sha256 = lower(pre_restore_database_sha256)
            AND pre_restore_database_sha256 NOT GLOB '*[^a-f0-9]*'
        )
    ),

    backup_format_version INTEGER NOT NULL CHECK (backup_format_version >= 1),
    source_schema_version INTEGER NOT NULL CHECK (source_schema_version >= 1),
    migrated_to_schema_version INTEGER NOT NULL CHECK (migrated_to_schema_version >= 1),

    -- A restore can only ever move a database forward.
    CHECK (migrated_to_schema_version >= source_schema_version)
) STRICT;

CREATE INDEX restore_provenance_history_idx ON restore_provenance(restored_at_utc);

CREATE TRIGGER restore_provenance_no_update
BEFORE UPDATE ON restore_provenance
BEGIN
    SELECT RAISE(ABORT, 'restore_provenance_is_append_only');
END;

CREATE TRIGGER restore_provenance_no_delete
BEFORE DELETE ON restore_provenance
BEGIN
    SELECT RAISE(ABORT, 'restore_provenance_is_append_only');
END;

-- ---------------------------------------------------------------------------------------------
-- master_change_events — rebuilt to admit two more entity types.
--
-- Reproduced verbatim from 0015 apart from the two added values. `action` already carries both
-- words this phase needs: a backup is 'created' and a restore is 'restored'.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1j;

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
        'stock_operation',
        'backup', 'restore_operation'
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
SELECT * FROM master_change_events_phase1j;

DROP TABLE master_change_events_phase1j;

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
