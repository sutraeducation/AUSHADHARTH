-- Phase 1H-0 medicine price-control foundation. A Sales prerequisite, not Sales itself.
--
-- Phase 1H's design audit proved the repository had no way to represent a notified ceiling price, so
-- a POS could not have answered "what is the legal maximum for this medicine on this date?". This
-- migration adds that, and nothing else: no sale, no invoice number, no outward movement, no tender.
--
-- Three facts are kept deliberately separate and are never merged into one column:
--
--   * Batch MRP            a lot's printed maximum retail price, INCLUSIVE of GST (Phase 1C-C)
--   * Controlled ceiling   a formulation's notified maximum,     EXCLUSIVE of GST (here)
--   * Selling rate         what this store charges,              EXCLUSIVE of GST (Phase 1H)
--
-- `product_kind = 'medicine'` does NOT imply price control: only notified scheduled formulations are
-- controlled, and many medicines are not. Applicability is therefore an explicit assertion, never an
-- inference, and never a guess from a product's name.
--
-- The shape mirrors the proven Phase 1F pair: tax_categories + tax_rate_versions became
-- controlled_formulations + price_control_versions, with the same half-open effective periods and
-- the same database-enforced no-overlap rule.

-- Extend the append-only audit stream without editing the earlier migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1g;

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
        'controlled_formulation', 'price_control_version', 'product_price_control'
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
SELECT * FROM master_change_events_phase1g;
DROP TABLE master_change_events_phase1g;

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
-- Controlled formulations: the notified formulation a ceiling belongs to.
--
-- This is a CURATED master, not a drug database. It names what a notification names, and a Product
-- is linked to it by explicit operator assignment. Matching on display name or composition text is
-- forbidden: composition is operator-entered clinical truth of variable completeness, a notification
-- keys on formulation/dosage form/strength as notified, and a combination product may match on some
-- components and not others. Guessing there would put a legal ceiling on the wrong medicine.
--
-- `strength_text` is deliberately free text and deliberately NOT parsed. It records the notification
-- as written so a human can verify the mapping; nothing computes from it.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE controlled_formulations (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    jurisdiction TEXT NOT NULL CHECK (
        jurisdiction = upper(trim(jurisdiction)) AND length(jurisdiction) BETWEEN 2 AND 16
    ),
    formulation_code TEXT NOT NULL CHECK (
        formulation_code = upper(trim(formulation_code))
        AND length(formulation_code) BETWEEN 1 AND 64
        AND formulation_code NOT GLOB '*[^A-Z0-9._/-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 200),
    dosage_form_id TEXT REFERENCES dosage_forms(id) ON DELETE RESTRICT,
    strength_text TEXT CHECK (strength_text IS NULL OR length(trim(strength_text)) BETWEEN 1 AND 200),
    -- Borrowed from the existing regulatory_categories convention: the app enforces configured data,
    -- it does not certify that the data is legally correct or current.
    verification_state TEXT NOT NULL DEFAULT 'unverified'
        CHECK (verification_state IN ('unverified', 'verified', 'rejected')),
    source_note TEXT CHECK (source_note IS NULL OR length(trim(source_note)) BETWEEN 1 AND 500),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX controlled_formulations_active_code_uq
ON controlled_formulations(jurisdiction, formulation_code) WHERE status = 'active';

CREATE INDEX controlled_formulations_name_idx
ON controlled_formulations(status, display_name);

CREATE TRIGGER controlled_formulations_dosage_form_insert
BEFORE INSERT ON controlled_formulations
WHEN NEW.dosage_form_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM dosage_forms WHERE id = NEW.dosage_form_id AND status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'controlled_formulation_conflict');
END;

-- Only a reference that actually CHANGES is validated, exactly as migration 0009 reasoned:
-- re-validating an unchanged, since-archived reference would trap the row permanently.
CREATE TRIGGER controlled_formulations_dosage_form_update
BEFORE UPDATE OF dosage_form_id ON controlled_formulations
WHEN NEW.dosage_form_id IS NOT NULL
 AND (OLD.dosage_form_id IS NULL OR NEW.dosage_form_id <> OLD.dosage_form_id)
 AND NOT EXISTS (
    SELECT 1 FROM dosage_forms WHERE id = NEW.dosage_form_id AND status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'controlled_formulation_conflict');
END;

-- ---------------------------------------------------------------------------------------------
-- Price-control versions: the effective-dated ceiling itself.
--
-- Periods are half-open [effective_from, effective_to) exactly as tax_rate_versions are, so on the
-- day equal to effective_to the NEXT version applies, or none. A new notification is a NEW version,
-- never an edit of the old one, so changing today's ceiling cannot rewrite yesterday's meaning.
--
-- `ceiling_basis` is explicit because a notified ceiling is not always per pack, and dividing one by
-- an arbitrary pack count would invent a legal fact. `per_base_unit` is the shape DPCO notifications
-- actually take (per tablet, per ml); `per_pack` exists so a genuinely per-pack notification can be
-- recorded truthfully — the resolver reports it as incomparable rather than guessing.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE price_control_versions (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    controlled_formulation_id TEXT NOT NULL
        REFERENCES controlled_formulations(id) ON DELETE RESTRICT,
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    -- Exact integer minor units per ADR-009. No REAL, no float, anywhere in the money path.
    ceiling_price_paise INTEGER NOT NULL CHECK (
        ceiling_price_paise > 0 AND ceiling_price_paise <= 100000000000
    ),
    ceiling_basis TEXT NOT NULL CHECK (ceiling_basis IN ('per_base_unit', 'per_pack')),
    ceiling_basis_unit_id TEXT REFERENCES units_of_measure(id) ON DELETE RESTRICT,
    notification_reference TEXT CHECK (
        notification_reference IS NULL OR length(trim(notification_reference)) BETWEEN 1 AND 200
    ),
    source_note TEXT CHECK (source_note IS NULL OR length(trim(source_note)) BETWEEN 1 AND 500),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_to > effective_from),
    -- A per-base-unit ceiling is meaningless without naming the unit it is quoted in: that unit is
    -- what the resolver compares against the Product's own base unit to decide comparability.
    CHECK (ceiling_basis <> 'per_base_unit' OR ceiling_basis_unit_id IS NOT NULL),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX price_control_versions_resolve_idx
ON price_control_versions(controlled_formulation_id, status, effective_from);

CREATE TRIGGER price_control_versions_formulation_insert
BEFORE INSERT ON price_control_versions
WHEN NOT EXISTS (
    SELECT 1 FROM controlled_formulations WHERE id = NEW.controlled_formulation_id AND status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'price_control_conflict');
END;

CREATE TRIGGER price_control_versions_unit_insert
BEFORE INSERT ON price_control_versions
WHEN NEW.ceiling_basis_unit_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM units_of_measure WHERE id = NEW.ceiling_basis_unit_id AND status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'price_control_conflict');
END;

CREATE TRIGGER price_control_versions_unit_update
BEFORE UPDATE OF ceiling_basis_unit_id ON price_control_versions
WHEN NEW.ceiling_basis_unit_id IS NOT NULL
 AND (OLD.ceiling_basis_unit_id IS NULL OR NEW.ceiling_basis_unit_id <> OLD.ceiling_basis_unit_id)
 AND NOT EXISTS (
    SELECT 1 FROM units_of_measure WHERE id = NEW.ceiling_basis_unit_id AND status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'price_control_conflict');
END;

-- Two active versions for one formulation can never overlap, so "the ceiling in force on a date" is
-- never ambiguous. Structurally identical to tax_rate_versions_no_overlap_insert/update.
CREATE TRIGGER price_control_versions_no_overlap_insert
BEFORE INSERT ON price_control_versions
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM price_control_versions existing
    WHERE existing.controlled_formulation_id = NEW.controlled_formulation_id
      AND existing.status = 'active'
      AND existing.effective_from < COALESCE(NEW.effective_to, '9999-12-31')
      AND NEW.effective_from < COALESCE(existing.effective_to, '9999-12-31')
)
BEGIN
    SELECT RAISE(ABORT, 'price_control_effective_period_overlap');
END;

CREATE TRIGGER price_control_versions_no_overlap_update
BEFORE UPDATE ON price_control_versions
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM price_control_versions existing
    WHERE existing.controlled_formulation_id = NEW.controlled_formulation_id
      AND existing.id <> NEW.id
      AND existing.status = 'active'
      AND existing.effective_from < COALESCE(NEW.effective_to, '9999-12-31')
      AND NEW.effective_from < COALESCE(existing.effective_to, '9999-12-31')
)
BEGIN
    SELECT RAISE(ABORT, 'price_control_effective_period_overlap');
END;

-- ---------------------------------------------------------------------------------------------
-- Product applicability.
--
-- Additive and defaulted, so every Product that already exists keeps reading and writing normally.
--
-- 'unknown' is the default and is a REAL state, not a null hiding as one: nobody has assessed this
-- product yet. It must never be read as "not controlled" — a medicine silently appearing
-- uncontrolled because reference data is incomplete is exactly the failure this phase exists to
-- prevent. The resolver reports it as unknown, and Phase 1H snapshots it onto the posted line so the
-- gap is auditable rather than invisible.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE products ADD COLUMN price_control_status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (price_control_status IN ('unknown', 'not_applicable', 'controlled'));
ALTER TABLE products ADD COLUMN controlled_formulation_id TEXT
    REFERENCES controlled_formulations(id) ON DELETE RESTRICT;

CREATE INDEX products_price_control_idx
ON products(price_control_status, controlled_formulation_id);

-- 'controlled' and a formulation imply each other. A controlled Product with no formulation could
-- never resolve a ceiling, and a formulation on an uncontrolled Product would be a dangling claim.
CREATE TRIGGER products_price_control_insert
BEFORE INSERT ON products
WHEN (NEW.price_control_status = 'controlled' AND NEW.controlled_formulation_id IS NULL)
  OR (NEW.price_control_status <> 'controlled' AND NEW.controlled_formulation_id IS NOT NULL)
  OR (NEW.controlled_formulation_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM controlled_formulations
        WHERE id = NEW.controlled_formulation_id AND status = 'active'
     ))
BEGIN
    SELECT RAISE(ABORT, 'product_price_control_conflict');
END;

-- Only a reference that actually CHANGES is validated, for the reason migration 0009 records: a
-- Product holding a since-archived formulation must stay editable, or clearing its status would be
-- refused by a check on the reference it is trying to abandon.
CREATE TRIGGER products_price_control_update
BEFORE UPDATE OF price_control_status, controlled_formulation_id ON products
WHEN (NEW.price_control_status = 'controlled' AND NEW.controlled_formulation_id IS NULL)
  OR (NEW.price_control_status <> 'controlled' AND NEW.controlled_formulation_id IS NOT NULL)
  OR (NEW.controlled_formulation_id IS NOT NULL
      AND (OLD.controlled_formulation_id IS NULL
           OR NEW.controlled_formulation_id <> OLD.controlled_formulation_id)
      AND NOT EXISTS (
        SELECT 1 FROM controlled_formulations
        WHERE id = NEW.controlled_formulation_id AND status = 'active'
     ))
BEGIN
    SELECT RAISE(ABORT, 'product_price_control_conflict');
END;
