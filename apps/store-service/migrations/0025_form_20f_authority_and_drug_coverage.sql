-- Phase 1M-D1-B — Form 20F store authority and per-product drug coverage.
--
-- Rule 61(3) of the Drugs Rules, 1945 issues the retail Schedule X licence in Form 20F, and the
-- Form itself names the drugs it licenses: item 2 of the Form is "Names of drugs". A pharmacy that
-- holds a Form 20F is therefore not licensed for Schedule X in general — it is licensed for the
-- drugs written on its own licence. Form 20F also runs perpetually unless suspended or cancelled,
-- with compliance assessed periodically, so "no expiry date" is a fact about the licence and not a
-- gap in the record.
--
-- Phase 1M-A recorded the licence itself: its form, number, issuing authority and any dates. It
-- recorded no legal STATE (in force, suspended, cancelled) and no way to tell a perpetual licence
-- from one whose expiry nobody captured, and it recorded no drug coverage at all. This migration
-- adds exactly those three facts and nothing else.
--
-- What it does NOT do: it grants no authority to sell anything. Schedule X remains refused by the
-- gate of 0021/0022/0023 and by `WORKFLOW_PENDING_SCHEMES`, before and after this migration. A
-- perfect Form 20F with perfect coverage still cannot post a Schedule X sale; the register, the
-- supply workflow and the enforcement that would use this authority are later phases.
--
-- Nothing is backfilled. Every licence recorded before today becomes `unknown` on both new axes,
-- because nobody was ever asked, and no store receives a single coverage row. A legacy Form 20F is
-- therefore not established authority until the owner records what their licence actually says.
-- ---------------------------------------------------------------------------------------------

-- 1. The licence's legal state, as the owner records it from the licence and any later order.
--    'unknown' is the honest default and never resolves as authority.
ALTER TABLE store_compliance_licences ADD COLUMN legal_status TEXT NOT NULL DEFAULT 'unknown'
    CHECK (legal_status IN ('in_force', 'suspended', 'cancelled', 'unknown'));

-- 2. Whether the licence runs perpetually or to a date. Phase 1M-A's `valid_upto` was nullable with
--    two meanings — "runs forever" and "nobody wrote it down" — which is exactly the ambiguity a
--    fail-closed gate must not inherit.
ALTER TABLE store_compliance_licences ADD COLUMN validity_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (validity_basis IN ('perpetual', 'fixed_term', 'unknown'));

-- The two-column agreement cannot be a table CHECK: SQLite cannot add one to an existing table, and
-- rebuilding `store_compliance_licences` would rewrite rows this phase has no business touching.
-- Triggers say the same thing and leave every existing row where it is.
--
-- A legacy row is `unknown`/`unknown` and may keep whatever `valid_upto` it was given, so the
-- guard speaks only about the two asserted bases.
CREATE TRIGGER store_compliance_licences_validity_basis_insert
BEFORE INSERT ON store_compliance_licences
WHEN (NEW.validity_basis = 'fixed_term' AND NEW.valid_upto IS NULL)
  OR (NEW.validity_basis = 'perpetual' AND NEW.valid_upto IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'licence_validity_basis_incoherent');
END;

CREATE TRIGGER store_compliance_licences_validity_basis_update
BEFORE UPDATE ON store_compliance_licences
WHEN (NEW.validity_basis = 'fixed_term' AND NEW.valid_upto IS NULL)
  OR (NEW.validity_basis = 'perpetual' AND NEW.valid_upto IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'licence_validity_basis_incoherent');
END;

-- ---------------------------------------------------------------------------------------------
-- 3. The drugs a Form 20F actually names.
--
-- Effective-dated exactly as Phase 1M-A's findings are: a coverage row is true from a date, and is
-- ended by recording an end date rather than by deletion, so what the licence covered last year
-- stays answerable next year. The owner records each row against the authority they read it from.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_licence_drug_coverage (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    -- The licence this coverage is written on. A coverage row without its licence is not a fact.
    licence_id TEXT NOT NULL REFERENCES store_compliance_licences(id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    -- Where the owner read this from: the licence itself, an endorsement, an order. Never inferred.
    source_citation TEXT NOT NULL CHECK (length(trim(source_citation)) BETWEEN 1 AND 300),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 300),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX store_licence_drug_coverage_lookup_idx
ON store_licence_drug_coverage(store_id, product_id, status, effective_from);

CREATE INDEX store_licence_drug_coverage_licence_idx
ON store_licence_drug_coverage(licence_id, status);

-- ---------------------------------------------------------------------------------------------
-- 4. What a coverage row must agree with.
--
-- Retail Schedule X authority is Form 20F and nothing else: Form 20G is the WHOLESALE licence of
-- rule 61(3) and cannot stand in for it, and a Form 20 or 21 is a different licence entirely. The
-- licence, the coverage and the product must also belong to the store whose licence it is.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER store_licence_drug_coverage_coherent_insert
BEFORE INSERT ON store_licence_drug_coverage
WHEN NOT EXISTS (
    SELECT 1 FROM store_compliance_licences licence
    WHERE licence.id = NEW.licence_id
      AND licence.licence_form = 'form_20f'
      AND licence.store_id = NEW.store_id
      AND licence.status = 'active'
)
   OR NOT EXISTS (
    SELECT 1 FROM store_identity store WHERE store.store_id = NEW.store_id
)
   OR NOT EXISTS (
    SELECT 1 FROM products product WHERE product.id = NEW.product_id AND product.status = 'active'
)
BEGIN
    SELECT RAISE(ABORT, 'licence_drug_coverage_incoherent');
END;

CREATE TRIGGER store_licence_drug_coverage_coherent_update
BEFORE UPDATE ON store_licence_drug_coverage
WHEN NEW.status = 'active' AND (
    NOT EXISTS (
        SELECT 1 FROM store_compliance_licences licence
        WHERE licence.id = NEW.licence_id
          AND licence.licence_form = 'form_20f'
          AND licence.store_id = NEW.store_id
          AND licence.status = 'active'
    )
    OR NEW.product_id <> OLD.product_id
    OR NEW.licence_id <> OLD.licence_id
    OR NEW.store_id <> OLD.store_id
    OR NEW.effective_from <> OLD.effective_from
)
BEGIN
    SELECT RAISE(ABORT, 'licence_drug_coverage_incoherent');
END;

-- One answer per product per moment. Two active rows covering the same product over the same days
-- would make "is this covered?" a question with two answers, which is how a fail-closed gate turns
-- into a coin toss. Same shape as the Phase 1M-A finding overlap guard.
CREATE TRIGGER store_licence_drug_coverage_no_overlap_insert
BEFORE INSERT ON store_licence_drug_coverage
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM store_licence_drug_coverage other
    WHERE other.status = 'active'
      AND other.store_id = NEW.store_id
      AND other.product_id = NEW.product_id
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'licence_drug_coverage_period_overlaps');
END;

CREATE TRIGGER store_licence_drug_coverage_no_overlap_update
BEFORE UPDATE ON store_licence_drug_coverage
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM store_licence_drug_coverage other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.store_id = NEW.store_id
      AND other.product_id = NEW.product_id
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'licence_drug_coverage_period_overlaps');
END;

CREATE TRIGGER store_licence_drug_coverage_no_delete
BEFORE DELETE ON store_licence_drug_coverage
BEGIN
    SELECT RAISE(ABORT, 'licence_drug_coverage_is_history');
END;

-- ---------------------------------------------------------------------------------------------
-- 5. The audit log learns the new entity type.
--
-- Same recreate-and-copy shape as 0014, 0017, 0021, 0022 and 0023. Every existing event is carried
-- across unchanged; payloads for this type carry identifiers, dates and states, never a licence
-- document or anything about a patient.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1md1a;

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
        'backup', 'restore_operation',
        'store_profile', 'store_address', 'store_licence',
        'product_regulatory_classification', 'product_regulatory_attributes',
        'product_pack_regulatory_attributes',
        'store_professional', 'store_compliance_licence', 'store_record_election',
        'prescriber', 'prescription', 'prescription_dispensing', 'prescription_dispensing_reversal',
        'prescription_supply_record',
        'prescription_h1_register_entry', 'prescription_h1_register_annotation',
        'store_licence_drug_coverage'
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
SELECT * FROM master_change_events_phase1md1a;

DROP TABLE master_change_events_phase1md1a;

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
