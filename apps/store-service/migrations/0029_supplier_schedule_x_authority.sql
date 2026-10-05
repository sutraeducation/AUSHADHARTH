-- Phase 1M-D3-C1 — the supplier's Schedule X purchase-source authority, as documentary evidence
-- the owner recorded.
--
-- This migration does NOT enable a Schedule X sale, and it does not change the Phase 1M-D3-B lot
-- provenance chain. It adds the one fact D3-B proved missing, so a later slice can ask it.
--
-- WHAT D3-B PROVED MISSING
--
-- Rule 65(4)(4)(i) requires a retailer to KEEP RECORDS of purchases, showing "(b) the name and
-- address of the person from whom purchased and the number of the relevant license held by him".
-- Phase 1M-D1-A does exactly that: it freezes the supplier's name, address and licence TEXT onto
-- the posted Purchase. That is a record-keeping duty discharged, and nothing more.
--
-- The sale licence Forms carry a separate condition — "No drug shall be sold unless such drug is
-- purchased under cash or credit memo from a duly licensed dealer or a duly licensed manufacturer"
-- (verbatim in the printed conditions of Forms 20, 20-A, 20-B, 21, 21-A and 21-B; rule 65's opening
-- words bind Forms 20, 20-A, 20-B, 20-F, 20-G, 21 and 21-B to "the conditions stated therein").
-- A frozen licence string does not establish that the source was DULY LICENSED for the Schedule X
-- transaction on its date. Nothing in this database did. That is what the tables below record.
--
-- WHICH AUTHORITIES A SCHEDULE X SOURCE CAN HOLD, FROM THE RULES
--
--   rule 61(3)  "A licence [to sell, stock, exhibit or offer for sale or distribute] drugs
--               specified in Schedule X by retail or by wholesale shall be issued in Form 20-F or
--               Form 20-G as the case may be." (G.S.R. 462(E), 22.6.1982; the bracketed words
--               substituted by G.S.R. 788(E), 10.10.1985.)
--               -> a DEALER supplying Schedule X by wholesale holds Form 20-G.
--
--   rule 70     "licenses for manufacture of drugs included in Schedule X against application in
--               Form 24-F shall be granted in Form 25-F" (G.S.R. 462(E), 22.6.1982). Rule 71
--               governs the conditions of its grant.
--               -> a MANUFACTURER source holds Form 25-F.
--
-- DELIBERATELY UNSUPPORTED, AND WHY
--
--   Form 28-B   manufacture of drugs specified under Schedules C and C(1) is licensed in Form 28-B,
--               not Form 25-F. A Schedule X drug that is also Schedule C or C(1) therefore has a
--               different manufacturing authority — and this software refuses Schedule C and C(1)
--               outright, so representing 28-B here would be modelling a channel that cannot be
--               used. It stays unsupported and fails closed.
--   Form 20-F   is the RETAIL Schedule X licence. Whether a retailer counts as a "duly licensed
--               dealer" for onward purchase is not answered by any rule text read for this phase,
--               so it is not represented. Unresolved, fail-closed, not guessed.
--
-- WHAT THIS IS NOT
--
-- It is NOT verification. AUSHADHARTH does not contact any licensing authority and cannot know that
-- a licence is genuine, current or unsuspended. Every column below records what an OPERATOR read
-- off a document and when they recorded it. The naming says so throughout, and nothing in this
-- phase is called verified, valid or government-checked.
--
-- NOTHING IS BACKFILLED. No frozen D1-A licence string becomes an authority record. Every
-- historical Schedule X purchase stays unresolved until somebody records evidence that covers its
-- supplier, its date and its drug.

-- ---------------------------------------------------------------------------------------------
-- 1. The authority the operator says a source held.
--
-- Shaped on Phase 1M-D1-B's `store_compliance_licences`, which models the pharmacy's OWN Form 20F,
-- because this is the same kind of fact about a different party. The two differences are deliberate:
-- this one names a supplier Party, and it carries `recorded_at_utc` separately from its effective
-- period, because an operator may inspect a document today that speaks about last year.
--
--   effective_from / effective_to   WHAT THE DOCUMENT ASSERTS. Half-open, `effective_to` exclusive,
--                                   matching every other effective-dated model in this database.
--   recorded_at_utc                 WHEN AUSHADHARTH LEARNED IT. Never backdated, so a retrospective
--                                   record is visibly retrospective.
--   legal_status                     what the document says about the period it covers. A period the
--                                   document shows as suspended or cancelled is recorded as such and
--                                   never establishes authority. `unknown` never establishes either:
--                                   the Phase 1M-D1-B lesson is that a nullable "probably fine" is
--                                   exactly the ambiguity a fail-closed gate must not inherit.
--   validity_basis                   `perpetual` or `fixed_term`, never inferred from a blank end
--                                   date. Current licence forms may run perpetually subject to
--                                   compliance assessment; an older format carried an expiry. The
--                                   two are not the same fact and are not stored as one.
--   source_citation                  the document inspected. Required: an authority nobody can say
--                                   where they read is not evidence.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE supplier_schedule_x_authorities (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    -- The exact Party a Purchase names as its supplier. Never a name, a GSTIN or a licence string:
    -- two suppliers may print the same number, and a renamed supplier is the same party.
    supplier_party_id TEXT NOT NULL REFERENCES parties(id) ON DELETE RESTRICT,

    -- The legal basis, as an enumeration. Free text cannot be the discriminator for a legal fact.
    authority_kind TEXT NOT NULL CHECK (authority_kind IN (
        -- Rule 61(3): the Schedule X WHOLESALE licence a dealer supplies under.
        'form_20g',
        -- Rule 70: the licence to MANUFACTURE drugs included in Schedule X.
        'form_25f'
    )),

    authority_number TEXT NOT NULL CHECK (length(trim(authority_number)) BETWEEN 1 AND 100),
    -- Alphanumerics only, upper-cased. Exists ONLY to notice the same document entered twice.
    -- Nothing reads it as a licence and nothing prints it, exactly as Phase 1L-A3 decided for the
    -- store's own licences.
    normalized_authority_number TEXT NOT NULL CHECK (
        normalized_authority_number = upper(normalized_authority_number)
        AND length(trim(normalized_authority_number)) > 0
    ),
    issuing_authority TEXT CHECK (
        issuing_authority IS NULL OR length(trim(issuing_authority)) BETWEEN 1 AND 160
    ),

    legal_status TEXT NOT NULL CHECK (
        legal_status IN ('in_force', 'suspended', 'cancelled', 'unknown')
    ),
    validity_basis TEXT NOT NULL CHECK (
        validity_basis IN ('perpetual', 'fixed_term', 'unknown')
    ),
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),

    source_citation TEXT NOT NULL CHECK (length(trim(source_citation)) BETWEEN 1 AND 300),
    note TEXT CHECK (note IS NULL OR length(trim(note)) BETWEEN 1 AND 500),

    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    -- When this software learned the fact, which is not when the document says it began.
    recorded_at_utc TEXT NOT NULL CHECK (recorded_at_utc GLOB '????-??-??T??:??:??*Z'),

    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK (effective_to IS NULL OR effective_to > effective_from),
    -- The end date and the basis say the same thing or the row is not written. A fixed term needs
    -- its end; a perpetual authority has none; `unknown` is neither and establishes nothing.
    CHECK (
        (validity_basis = 'fixed_term' AND effective_to IS NOT NULL)
        OR (validity_basis = 'perpetual' AND effective_to IS NULL)
        OR validity_basis = 'unknown'
    ),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL
            AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX supplier_schedule_x_authorities_lookup_idx
ON supplier_schedule_x_authorities(store_id, supplier_party_id, status, effective_from);

CREATE INDEX supplier_schedule_x_authorities_kind_idx
ON supplier_schedule_x_authorities(store_id, authority_kind, status);

-- ---------------------------------------------------------------------------------------------
-- 2. Which Schedule X drugs the operator says that authority covered.
--
-- Shaped on `store_licence_drug_coverage` from Phase 1M-D1-B, for the same reason it exists there:
-- a Schedule X licence is not a blanket permission, and assuming one authority covers every
-- Schedule X product is exactly the inference a fail-closed model must refuse. Coverage is named
-- per PRODUCT, by stable identity — never by drug name, because a renamed product is the same drug
-- and two products may share a name.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE supplier_schedule_x_authority_coverage (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    -- The authority this coverage is written on. A coverage row without its authority is not a fact.
    authority_id TEXT NOT NULL REFERENCES supplier_schedule_x_authorities(id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    source_citation TEXT NOT NULL CHECK (length(trim(source_citation)) BETWEEN 1 AND 300),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 300),
    recorded_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    recorded_at_utc TEXT NOT NULL CHECK (recorded_at_utc GLOB '????-??-??T??:??:??*Z'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL
            AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX supplier_schedule_x_authority_coverage_lookup_idx
ON supplier_schedule_x_authority_coverage(store_id, product_id, status, effective_from);

CREATE INDEX supplier_schedule_x_authority_coverage_authority_idx
ON supplier_schedule_x_authority_coverage(authority_id, status);

-- ---------------------------------------------------------------------------------------------
-- 3. What an authority record must agree with, whoever writes it.
--
-- The supplier must be a Party this store actually buys from — a Party holding the supplier role —
-- so an authority cannot be hung on a customer or on a party from nowhere. And the person recording
-- it must hold a role permitted to record compliance evidence; a cashier cannot self-attest a
-- supplier's licence in order to make a future sale pass.
--
-- `users` carries no store: this installation serves one store, so a store column on the actor is a
-- fact the schema cannot support. Role coherence is enforced instead, as Phase 1M-D3-A did.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER supplier_schedule_x_authorities_coherent_insert
BEFORE INSERT ON supplier_schedule_x_authorities
WHEN NOT EXISTS (
    SELECT 1 FROM parties supplier
    JOIN party_roles role ON role.party_id = supplier.id
    JOIN users recorder ON recorder.id = NEW.recorded_by_user_id
    WHERE supplier.id = NEW.supplier_party_id
      AND role.role = 'supplier'
      AND recorder.role IN ('owner_admin', 'pharmacist')
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_authority_incoherent');
END;

-- One ACTIVE authority of a given kind per supplier per store at a time. Two overlapping records
-- are an ambiguity, and the resolver must never settle an ambiguity by picking whichever row was
-- inserted last — so the ambiguity is refused at the door, and the resolver independently fails
-- closed if one ever exists.
CREATE TRIGGER supplier_schedule_x_authorities_no_overlap_insert
BEFORE INSERT ON supplier_schedule_x_authorities
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM supplier_schedule_x_authorities other
    WHERE other.status = 'active'
      AND other.store_id = NEW.store_id
      AND other.supplier_party_id = NEW.supplier_party_id
      AND other.authority_kind = NEW.authority_kind
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_authority_period_overlaps');
END;

CREATE TRIGGER supplier_schedule_x_authorities_no_overlap_update
BEFORE UPDATE ON supplier_schedule_x_authorities
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM supplier_schedule_x_authorities other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.store_id = NEW.store_id
      AND other.supplier_party_id = NEW.supplier_party_id
      AND other.authority_kind = NEW.authority_kind
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_authority_period_overlaps');
END;

-- The identity of the evidence never moves. Which store, which supplier, which legal basis, who
-- recorded it and when they recorded it are what make the row a historical fact; a correction to
-- the dates or the status is an ordinary revision, but rewriting whose authority it was, or
-- pre-dating when this software learned of it, is not a correction.
CREATE TRIGGER supplier_schedule_x_authorities_frozen_identity
BEFORE UPDATE ON supplier_schedule_x_authorities
WHEN NOT (
    NEW.id = OLD.id
    AND NEW.store_id = OLD.store_id
    AND NEW.supplier_party_id = OLD.supplier_party_id
    AND NEW.authority_kind = OLD.authority_kind
    AND NEW.recorded_by_user_id = OLD.recorded_by_user_id
    AND NEW.recorded_at_utc = OLD.recorded_at_utc
    AND NEW.created_at_utc = OLD.created_at_utc
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_authority_identity_frozen');
END;

-- Compliance evidence is not deleted. An authority entered in error is archived with a reason, so
-- the record of what the pharmacy believed, and when, survives for an inspection.
CREATE TRIGGER supplier_schedule_x_authorities_no_delete
BEFORE DELETE ON supplier_schedule_x_authorities
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_authority_no_delete');
END;

-- ---------------------------------------------------------------------------------------------
-- 4. What a coverage row must agree with.
--
-- The coverage, its authority and the product all belong to the same store, and a coverage period
-- cannot reach outside the authority it is written on: a licence cannot cover a drug on a day the
-- licence itself does not cover.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER supplier_schedule_x_authority_coverage_coherent_insert
BEFORE INSERT ON supplier_schedule_x_authority_coverage
WHEN NOT EXISTS (
    SELECT 1 FROM supplier_schedule_x_authorities authority
    JOIN users recorder ON recorder.id = NEW.recorded_by_user_id
    WHERE authority.id = NEW.authority_id
      AND authority.store_id = NEW.store_id
      AND authority.effective_from <= NEW.effective_from
      AND (
          authority.effective_to IS NULL
          OR (NEW.effective_to IS NOT NULL AND NEW.effective_to <= authority.effective_to)
      )
      AND recorder.role IN ('owner_admin', 'pharmacist')
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_coverage_incoherent');
END;

-- One ACTIVE coverage period per product per authority. Overlapping coverage is the same ambiguity
-- as overlapping authority and is refused for the same reason.
CREATE TRIGGER supplier_schedule_x_authority_coverage_no_overlap_insert
BEFORE INSERT ON supplier_schedule_x_authority_coverage
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM supplier_schedule_x_authority_coverage other
    WHERE other.status = 'active'
      AND other.authority_id = NEW.authority_id
      AND other.product_id = NEW.product_id
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_coverage_period_overlaps');
END;

CREATE TRIGGER supplier_schedule_x_authority_coverage_no_overlap_update
BEFORE UPDATE ON supplier_schedule_x_authority_coverage
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM supplier_schedule_x_authority_coverage other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.authority_id = NEW.authority_id
      AND other.product_id = NEW.product_id
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_coverage_period_overlaps');
END;

CREATE TRIGGER supplier_schedule_x_authority_coverage_frozen_identity
BEFORE UPDATE ON supplier_schedule_x_authority_coverage
WHEN NOT (
    NEW.id = OLD.id
    AND NEW.store_id = OLD.store_id
    AND NEW.authority_id = OLD.authority_id
    AND NEW.product_id = OLD.product_id
    AND NEW.recorded_by_user_id = OLD.recorded_by_user_id
    AND NEW.recorded_at_utc = OLD.recorded_at_utc
    AND NEW.created_at_utc = OLD.created_at_utc
)
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_coverage_identity_frozen');
END;

CREATE TRIGGER supplier_schedule_x_authority_coverage_no_delete
BEFORE DELETE ON supplier_schedule_x_authority_coverage
BEGIN
    SELECT RAISE(ABORT, 'supplier_schedule_x_coverage_no_delete');
END;

-- ---------------------------------------------------------------------------------------------
-- 5. The audit log learns the two new entity types.
--
-- Same recreate-and-copy shape as 0014, 0017, 0021, 0022, 0023, 0025, 0026 and 0027. Every
-- existing event is carried across unchanged and every column is preserved. Payloads for these
-- types carry the store, the supplier, the record, its kind and its lifecycle transition — never
-- the document text an operator typed, which belongs on the row and nowhere else.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1md3c1;

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
        'store_licence_drug_coverage',
        'schedule_x_register_entry', 'prescription_duplicate_copy_attestation',
        'schedule_x_prescription_annotation',
        'supplier_schedule_x_authority', 'supplier_schedule_x_authority_coverage'
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
SELECT * FROM master_change_events_phase1md3c1;

DROP TABLE master_change_events_phase1md3c1;

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
