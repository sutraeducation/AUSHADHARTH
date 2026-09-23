-- Phase 1M-C — the Schedule H1 working record, and the Punjab unsupported-workflow boundary.
--
-- WHAT THE LAW SAYS, AND WHAT THIS MIGRATION DOES ABOUT IT
--
--   rule 65(3)(1)(h)  "the supply of a drug specified in Schedule H1 shall be recorded in a separate
--                     register at the time of the supply giving the name and address of the
--                     prescriber, the name of the patient, the name of the drug and the quantity
--                     supplied and such records shall be maintained for three years and be open for
--                     inspection."
--                     -> binding. The separate register is the pharmacy's physical book. This
--                        database keeps a WORKING RECORD of each entry, from which a hard copy is
--                        printed for that book. It is not the statutory register and never says so.
--
--   48th DCC, 24-07-2015, agenda item 11
--                     "if the records are maintained electronically, for the purpose of compliance
--                     to the provisions relating to Schedule H1 drugs, hard copies of the data as
--                     required for Schedule H1 should be pasted in the register and duly
--                     authenticated by the registered pharmacist."
--                     -> an official CDSCO-hosted DCC recommendation, not proven to amend or
--                        independently replace rule 65. AUSHADHARTH follows it conservatively: an
--                        entry is confirmed only when a dispensing user records that the hard copy
--                        was placed in the separate register AND the registered pharmacist
--                        authenticated it by hand. Rule 65(3)(1)(h) itself lists no signature; the
--                        authentication comes from this computerised-record arrangement. The
--                        software never signs anything.
--
--   Timing            "at the time of the supply". The DCC endorsed no end-of-day report (that was
--                     the retailers' proposal, not the committee's), so each supply's entry is
--                     prepared, placed, authenticated and confirmed before its Sale posts, and
--                     finalized in the same transaction as the Sale. There is no batching path.
--
--   Serial            clause (h) prescribes none. Each working entry carries an immutable
--                     AUSHADHARTH Reference (AH1-000001, ...), which is an internal identifier, not
--                     a register serial or page number.
--
--   Punjab            Punjab notification No. 9/16/21-3H6/1039 dated 25-03-2021 requires a
--                     departmental permission, and monthly statements, for eight habit-forming
--                     drugs. AUSHADHARTH does not implement that workflow. It adds a separate,
--                     owner-recorded, citation-backed regulatory axis `punjab_restricted_supply`
--                     and refuses, as an UNSUPPORTED STATE WORKFLOW, a medicine sold from a store
--                     whose premises are in Punjab when that axis applies or is unknown. Nothing
--                     is classified automatically and no drug name is matched.
--
-- Nothing here backfills anything. A Sale posted before this migration keeps exactly what it froze.
-- ---------------------------------------------------------------------------------------------


-- ---------------------------------------------------------------------------------------------
-- 1. The Punjab axis joins the classification table.
--
-- The scheme list is a CHECK, deliberately: a scheme is a legal category, not data. Widening it is
-- a rebuild, copy and replace; no row changes. `punjab_restricted_supply` is NOT Schedule H1, NOT
-- NDPS and NOT an external-order H1 condition. It is its own finding about its own instrument.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE product_regulatory_classifications RENAME TO product_regulatory_classifications_phase1mb;

CREATE TABLE product_regulatory_classifications (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    scheme TEXT NOT NULL CHECK (scheme IN (
        'schedule_h', 'schedule_h1', 'schedule_x', 'schedule_c', 'schedule_c1', 'ndps_purview',
        'punjab_restricted_supply'
    )),
    applies INTEGER NOT NULL CHECK (applies IN (0, 1)),
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    source_citation TEXT NOT NULL CHECK (length(trim(source_citation)) BETWEEN 3 AND 300),
    reason TEXT CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 500),
    determined_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
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

INSERT INTO product_regulatory_classifications
SELECT * FROM product_regulatory_classifications_phase1mb;

DROP TABLE product_regulatory_classifications_phase1mb;

CREATE INDEX product_regulatory_classifications_lookup_idx
ON product_regulatory_classifications(product_id, scheme, effective_from)
WHERE status = 'active';

CREATE TRIGGER product_regulatory_classifications_no_overlap_insert
BEFORE INSERT ON product_regulatory_classifications
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM product_regulatory_classifications other
    WHERE other.status = 'active'
      AND other.product_id = NEW.product_id
      AND other.scheme = NEW.scheme
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_classification_period_overlaps');
END;

CREATE TRIGGER product_regulatory_classifications_no_overlap_update
BEFORE UPDATE ON product_regulatory_classifications
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM product_regulatory_classifications other
    WHERE other.status = 'active'
      AND other.id <> NEW.id
      AND other.product_id = NEW.product_id
      AND other.scheme = NEW.scheme
      AND (other.effective_to IS NULL OR other.effective_to > NEW.effective_from)
      AND (NEW.effective_to IS NULL OR NEW.effective_to > other.effective_from)
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_classification_period_overlaps');
END;


-- ---------------------------------------------------------------------------------------------
-- 2. The state boundary a posted line freezes.
--
-- The central answer stays in `regulatory_schemes_snapshot`, version 1, byte for byte as before:
-- the Punjab axis is a different kind of fact and is not folded into it. A line posted from now on
-- also freezes this object:
--
--   { "premises_state_code": "03" | "27" | ... | null,
--     "punjab_restricted_supply": "applies" | "does_not_apply" | "unknown"      -- premises in Punjab
--                                 | "not_applicable"                          -- premises elsewhere
--                                 | "undetermined" }                          -- premises unknown
--
-- `premises_state_code` is the GST state code of the store's recorded PREMISES address, never the
-- GSTIN prefix, the place of supply, the customer or the device. A line posted before this
-- migration has NULL here and is never reinterpreted.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_lines ADD COLUMN regulatory_state_snapshot TEXT
    CHECK (regulatory_state_snapshot IS NULL OR json_valid(regulatory_state_snapshot));

CREATE TRIGGER sale_lines_state_snapshot_draft_only_insert
BEFORE INSERT ON sale_lines
WHEN NEW.regulatory_state_snapshot IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_before_posting');
END;

CREATE TRIGGER sale_lines_state_snapshot_coherent_insert
BEFORE INSERT ON sale_lines
WHEN NEW.regulatory_state_snapshot IS NOT NULL AND NOT (
    json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply')
        IN ('applies', 'does_not_apply', 'unknown', 'not_applicable', 'undetermined')
    AND (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') IS NULL
         OR length(json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code')) = 2)
    AND (
        (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') IS NULL
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'undetermined')
     OR (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') = '03'
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply')
             IN ('applies', 'does_not_apply', 'unknown'))
     OR (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') <> '03'
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'not_applicable')
    )
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_incoherent');
END;

CREATE TRIGGER sale_lines_state_snapshot_coherent_update
BEFORE UPDATE OF regulatory_state_snapshot ON sale_lines
WHEN NEW.regulatory_state_snapshot IS NOT NULL AND NOT (
    json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply')
        IN ('applies', 'does_not_apply', 'unknown', 'not_applicable', 'undetermined')
    AND (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') IS NULL
         OR length(json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code')) = 2)
    AND (
        (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') IS NULL
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'undetermined')
     OR (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') = '03'
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply')
             IN ('applies', 'does_not_apply', 'unknown'))
     OR (json_extract(NEW.regulatory_state_snapshot, '$.premises_state_code') <> '03'
         AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'not_applicable')
    )
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_snapshot_incoherent');
END;

-- A line may not arrive in an already-posted document carrying a state boundary this software does
-- not support. Direct-SQL path only; the document-level gate below covers every real posting.
CREATE TRIGGER sale_lines_state_boundary_insert
BEFORE INSERT ON sale_lines
WHEN EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
) AND (
    NEW.regulatory_state_snapshot IS NULL
 OR json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'applies'
 OR EXISTS (
        SELECT 1 FROM products product
        WHERE product.id = NEW.product_id AND product.product_kind = 'medicine'
          AND json_extract(NEW.regulatory_state_snapshot, '$.punjab_restricted_supply')
              IN ('unknown', 'undetermined'))
)
BEGIN
    SELECT RAISE(ABORT, 'state_regulatory_boundary_refuses_posting');
END;

-- The state gate on posting. Every line must carry the state snapshot; it must name the store's
-- premises state as it stands under the posting lock; in Punjab it must carry the finding in force
-- on the Sale's business date (so a forged "does_not_apply" is refused); and it may not be
-- 'applies', nor — for a medicine — 'unknown' or 'undetermined'.
CREATE TRIGGER sale_documents_state_boundary_update
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_state_snapshot IS NULL
     OR json_extract(line.regulatory_state_snapshot, '$.premises_state_code') IS NOT (
            SELECT CASE WHEN address.country_code = 'IN' AND code.jurisdiction = 'IN'
                        THEN code.state_code END
            FROM store_addresses address
            LEFT JOIN state_codes code ON code.id = address.state_id
            WHERE address.store_id = NEW.store_id)
     OR (json_extract(line.regulatory_state_snapshot, '$.premises_state_code') = '03'
         AND json_extract(line.regulatory_state_snapshot, '$.punjab_restricted_supply') IS NOT (
             SELECT CASE
                 WHEN EXISTS (
                     SELECT 1 FROM product_regulatory_classifications finding
                     WHERE finding.product_id = line.product_id
                       AND finding.scheme = 'punjab_restricted_supply' AND finding.status = 'active'
                       AND finding.applies = 1 AND finding.effective_from <= NEW.business_date
                       AND (finding.effective_to IS NULL OR finding.effective_to > NEW.business_date))
                 THEN 'applies'
                 WHEN EXISTS (
                     SELECT 1 FROM product_regulatory_classifications finding
                     WHERE finding.product_id = line.product_id
                       AND finding.scheme = 'punjab_restricted_supply' AND finding.status = 'active'
                       AND finding.applies = 0 AND finding.effective_from <= NEW.business_date
                       AND (finding.effective_to IS NULL OR finding.effective_to > NEW.business_date))
                 THEN 'does_not_apply'
                 ELSE 'unknown' END))
     OR json_extract(line.regulatory_state_snapshot, '$.punjab_restricted_supply') = 'applies'
     OR (product.product_kind = 'medicine'
         AND json_extract(line.regulatory_state_snapshot, '$.punjab_restricted_supply')
             IN ('unknown', 'undetermined'))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'state_regulatory_boundary_refuses_posting');
END;


-- ---------------------------------------------------------------------------------------------
-- 3. The central posting gate, rebuilt once more.
--
-- Phase 1M-B refused every Schedule H1 line. A Schedule H1 line may now post exactly as a Schedule
-- H line does — carrying a dispensing against a prescription — and, in addition, its separate H1
-- working entry must be finalized (section 5). Schedule X, C and C(1) stay refused, and a medicine
-- of unknown central position stays refused. The insert-as-posted variant from 0022 is unchanged:
-- it still refuses Schedule H and H1 outright, because a document forged straight into the posted
-- state can carry neither a dispensing nor an entry.
-- ---------------------------------------------------------------------------------------------
DROP TRIGGER sale_documents_regulatory_gate_update;

CREATE TRIGGER sale_documents_regulatory_gate_update
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     OR ((json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
          OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies')
         AND NOT EXISTS (
             SELECT 1 FROM prescription_dispensings dispensing
             WHERE dispensing.sale_line_id = line.id
         ))
     OR (product.product_kind = 'medicine' AND (
            json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'unknown'
         OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'unknown'))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_gate_refuses_posting');
END;


-- ---------------------------------------------------------------------------------------------
-- 4. The Schedule H1 working record.
--
-- One entry per Schedule H1 Sale line: one drug, one quantity, one prescription. Two H1 drugs in a
-- supply are two entries, never one merged row, so each statutory drug/quantity pair stays
-- unambiguous on the printed copy.
--
-- The four particulars clause (h) names are frozen from the prescription and the line, and the
-- database proves they match. The date of supply is not a listed particular; it is recorded because
-- "at the time of the supply" cannot otherwise be shown and three years cannot otherwise be
-- counted. Every internal identifier is exactly that: internal.
--
--   prepared   the working entry exists; the Sale is a draft; nothing is sold.
--   confirmed  a dispensing user recorded that the printed hard copy was placed in the separate
--              physical H1 register AND the registered pharmacist authenticated it by hand. Both
--              flags, together, or neither.
--   finalized  linked to its dispensing inside the posting transaction; immutable from then on.
--   void       cancelled before supply; kept, with its reason; its reference is never reused.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_h1_register_entries (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    sale_document_id TEXT NOT NULL REFERENCES sale_documents(id) ON DELETE RESTRICT,
    sale_line_id TEXT NOT NULL REFERENCES sale_lines(id) ON DELETE RESTRICT,
    supply_record_id TEXT NOT NULL REFERENCES prescription_supply_records(id) ON DELETE RESTRICT,
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    prescription_item_id TEXT NOT NULL REFERENCES prescription_items(id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,

    -- The AUSHADHARTH Reference: internal, immutable, never reused. Not a register serial.
    reference_value INTEGER NOT NULL CHECK (reference_value >= 1),
    reference TEXT NOT NULL CHECK (reference = 'AH1-' || printf('%06d', reference_value)),

    date_of_supply TEXT NOT NULL CHECK (date_of_supply GLOB '????-??-??'),
    prescriber_name TEXT NOT NULL CHECK (length(trim(prescriber_name)) BETWEEN 1 AND 200),
    prescriber_address TEXT NOT NULL CHECK (length(trim(prescriber_address)) BETWEEN 1 AND 500),
    patient_name TEXT NOT NULL CHECK (length(trim(patient_name)) BETWEEN 1 AND 200),
    drug_name TEXT NOT NULL CHECK (length(trim(drug_name)) BETWEEN 1 AND 300),
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms > 0),
    quantity_unit_label TEXT,

    -- The registered pharmacist responsible for the supply, from the Phase 1M-A professional
    -- record. The person who confirms the physical acts is recorded separately below.
    supervising_professional_id TEXT NOT NULL REFERENCES store_professionals(id) ON DELETE RESTRICT,
    supervising_professional_name TEXT NOT NULL CHECK (length(trim(supervising_professional_name)) > 0),
    supervising_registration_number TEXT NOT NULL CHECK (length(trim(supervising_registration_number)) > 0),

    status TEXT NOT NULL DEFAULT 'prepared'
        CHECK (status IN ('prepared', 'confirmed', 'finalized', 'void')),
    prepared_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    prepared_at_utc TEXT NOT NULL CHECK (prepared_at_utc GLOB '????-??-??T??:??:??*Z'),

    hard_copy_placed_in_register INTEGER NOT NULL DEFAULT 0
        CHECK (hard_copy_placed_in_register IN (0, 1)),
    pharmacist_authenticated_hard_copy INTEGER NOT NULL DEFAULT 0
        CHECK (pharmacist_authenticated_hard_copy IN (0, 1)),
    confirmed_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    confirmed_at_utc TEXT CHECK (confirmed_at_utc IS NULL OR confirmed_at_utc GLOB '????-??-??T??:??:??*Z'),

    dispensing_id TEXT UNIQUE REFERENCES prescription_dispensings(id) ON DELETE RESTRICT,
    finalized_at_utc TEXT CHECK (finalized_at_utc IS NULL OR finalized_at_utc GLOB '????-??-??T??:??:??*Z'),

    voided_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    voided_at_utc TEXT CHECK (voided_at_utc IS NULL OR voided_at_utc GLOB '????-??-??T??:??:??*Z'),
    void_reason TEXT CHECK (void_reason IS NULL OR length(trim(void_reason)) BETWEEN 1 AND 500),

    CHECK (hard_copy_placed_in_register = pharmacist_authenticated_hard_copy),
    CHECK ((hard_copy_placed_in_register = 1) = (confirmed_by_user_id IS NOT NULL)),
    CHECK ((confirmed_by_user_id IS NULL) = (confirmed_at_utc IS NULL)),
    CHECK (status <> 'prepared' OR (hard_copy_placed_in_register = 0 AND dispensing_id IS NULL
                                   AND finalized_at_utc IS NULL)),
    CHECK (status NOT IN ('confirmed', 'finalized') OR hard_copy_placed_in_register = 1),
    CHECK ((status = 'finalized') = (finalized_at_utc IS NOT NULL)),
    CHECK ((status = 'finalized') = (dispensing_id IS NOT NULL)),
    CHECK (
        (status = 'void' AND voided_by_user_id IS NOT NULL AND voided_at_utc IS NOT NULL
         AND void_reason IS NOT NULL)
        OR (status <> 'void' AND voided_by_user_id IS NULL AND voided_at_utc IS NULL
            AND void_reason IS NULL)
    )
) STRICT;

CREATE UNIQUE INDEX prescription_h1_register_entries_reference_uq
ON prescription_h1_register_entries(store_id, reference_value);

-- One live entry per line. A void entry stays, beside its replacement.
CREATE UNIQUE INDEX prescription_h1_register_entries_live_uq
ON prescription_h1_register_entries(sale_line_id) WHERE status <> 'void';

CREATE INDEX prescription_h1_register_entries_register_idx
ON prescription_h1_register_entries(store_id, date_of_supply, reference_value);

CREATE INDEX prescription_h1_register_entries_sale_idx
ON prescription_h1_register_entries(sale_document_id);

-- An entry is written only for a draft line that is dispensed on a human patient's prescription
-- of this Store, against that line's own item, for that line's product and quantity, beside a live
-- rule 65(3)(1) entry for the same Sale and prescription, supervised by the same registered
-- pharmacist, and only while Schedule H1 applies to the product on the Sale's business date. Its
-- particulars must be the prescription's, its drug the product's, and its reference the next one.
CREATE TRIGGER prescription_h1_register_entries_coherent_insert
BEFORE INSERT ON prescription_h1_register_entries
WHEN NOT (
    NEW.status = 'prepared'
    AND NEW.hard_copy_placed_in_register = 0
    AND NEW.confirmed_by_user_id IS NULL
    AND NEW.dispensing_id IS NULL
    AND NEW.reference_value = (
        SELECT COALESCE(MAX(reference_value), 0) + 1 FROM prescription_h1_register_entries
        WHERE store_id = NEW.store_id)
    AND EXISTS (
        SELECT 1 FROM sale_documents document
        JOIN sale_lines line ON line.sale_document_id = document.id
        JOIN prescription_items item ON item.id = line.prescription_item_id
        JOIN prescriptions prescription ON prescription.id = item.prescription_id
        JOIN prescription_supply_records record ON record.id = NEW.supply_record_id
        JOIN products product ON product.id = line.product_id
        JOIN store_professionals professional ON professional.id = NEW.supervising_professional_id
        JOIN users preparer ON preparer.id = NEW.prepared_by_user_id
        WHERE document.id = NEW.sale_document_id
          AND document.status = 'draft'
          AND document.store_id = NEW.store_id
          AND document.business_date = NEW.date_of_supply
          AND line.id = NEW.sale_line_id
          AND line.product_id = NEW.product_id
          AND line.quantity_atoms = NEW.quantity_atoms
          AND item.id = NEW.prescription_item_id
          AND item.product_id = NEW.product_id
          AND prescription.id = NEW.prescription_id
          AND prescription.store_id = NEW.store_id
          AND prescription.status = 'active'
          AND prescription.subject_kind = 'human'
          AND prescription.prescriber_name = NEW.prescriber_name
          AND prescription.prescriber_address = NEW.prescriber_address
          AND prescription.subject_name = NEW.patient_name
          AND product.display_name = NEW.drug_name
          AND record.store_id = NEW.store_id
          AND record.sale_document_id = NEW.sale_document_id
          AND record.prescription_id = NEW.prescription_id
          AND record.status IN ('prepared', 'confirmed')
          AND record.supervising_professional_id = NEW.supervising_professional_id
          AND record.date_of_supply = NEW.date_of_supply
          AND professional.store_id = NEW.store_id
          AND professional.status = 'active'
          AND professional.capacity = 'registered_pharmacist'
          AND professional.full_name = NEW.supervising_professional_name
          AND professional.registration_number = NEW.supervising_registration_number
          AND preparer.role IN ('owner_admin', 'pharmacist')
          AND EXISTS (
              SELECT 1 FROM product_regulatory_classifications finding
              WHERE finding.product_id = NEW.product_id AND finding.scheme = 'schedule_h1'
                AND finding.status = 'active' AND finding.applies = 1
                AND finding.effective_from <= document.business_date
                AND (finding.effective_to IS NULL OR finding.effective_to > document.business_date))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_entry_incoherent');
END;

-- The particulars never change. The only moves are prepared -> confirmed (by a dispensing user,
-- with both physical-act flags, while the Sale is a draft and its pharmacist is still a registered
-- pharmacist on record), prepared/confirmed -> void (while the Sale is a draft), and confirmed ->
-- finalized (while the Sale is a draft, linked to the dispensing of this very line).
CREATE TRIGGER prescription_h1_register_entries_transition
BEFORE UPDATE ON prescription_h1_register_entries
WHEN NOT (
    NEW.id = OLD.id AND NEW.store_id = OLD.store_id
    AND NEW.sale_document_id = OLD.sale_document_id AND NEW.sale_line_id = OLD.sale_line_id
    AND NEW.supply_record_id = OLD.supply_record_id AND NEW.prescription_id = OLD.prescription_id
    AND NEW.prescription_item_id = OLD.prescription_item_id AND NEW.product_id = OLD.product_id
    AND NEW.reference_value = OLD.reference_value AND NEW.reference = OLD.reference
    AND NEW.date_of_supply = OLD.date_of_supply
    AND NEW.prescriber_name = OLD.prescriber_name
    AND NEW.prescriber_address = OLD.prescriber_address
    AND NEW.patient_name = OLD.patient_name
    AND NEW.drug_name = OLD.drug_name
    AND NEW.quantity_atoms = OLD.quantity_atoms
    AND NEW.quantity_unit_label IS OLD.quantity_unit_label
    AND NEW.supervising_professional_id = OLD.supervising_professional_id
    AND NEW.supervising_professional_name = OLD.supervising_professional_name
    AND NEW.supervising_registration_number = OLD.supervising_registration_number
    AND NEW.prepared_by_user_id = OLD.prepared_by_user_id
    AND NEW.prepared_at_utc = OLD.prepared_at_utc
    AND EXISTS (
        SELECT 1 FROM sale_documents document
        WHERE document.id = NEW.sale_document_id AND document.status = 'draft')
    AND (
        (OLD.status = 'prepared' AND NEW.status = 'confirmed'
         AND NEW.dispensing_id IS NULL
         AND EXISTS (
             SELECT 1 FROM users confirmer
             WHERE confirmer.id = NEW.confirmed_by_user_id
               AND confirmer.role IN ('owner_admin', 'pharmacist'))
         AND EXISTS (
             SELECT 1 FROM store_professionals professional
             WHERE professional.id = NEW.supervising_professional_id
               AND professional.status = 'active'
               AND professional.capacity = 'registered_pharmacist'))
     OR (OLD.status IN ('prepared', 'confirmed') AND NEW.status = 'void'
         AND NEW.hard_copy_placed_in_register = OLD.hard_copy_placed_in_register
         AND NEW.confirmed_by_user_id IS OLD.confirmed_by_user_id
         AND NEW.confirmed_at_utc IS OLD.confirmed_at_utc
         AND NEW.dispensing_id IS NULL)
     OR (OLD.status = 'confirmed' AND NEW.status = 'finalized'
         AND NEW.hard_copy_placed_in_register = OLD.hard_copy_placed_in_register
         AND NEW.confirmed_by_user_id = OLD.confirmed_by_user_id
         AND NEW.confirmed_at_utc = OLD.confirmed_at_utc
         AND EXISTS (
             SELECT 1 FROM prescription_dispensings dispensing
             WHERE dispensing.id = NEW.dispensing_id
               AND dispensing.sale_document_id = NEW.sale_document_id
               AND dispensing.sale_line_id = NEW.sale_line_id
               AND dispensing.prescription_item_id = NEW.prescription_item_id
               AND dispensing.product_id = NEW.product_id
               AND dispensing.quantity_atoms = NEW.quantity_atoms))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_entry_immutable');
END;

CREATE TRIGGER prescription_h1_register_entries_no_delete
BEFORE DELETE ON prescription_h1_register_entries
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_entry_immutable');
END;


-- ---------------------------------------------------------------------------------------------
-- 5. The H1 posting guard.
--
-- A Sale may become posted only if every line that is Schedule H1 on its business date is NOT also
-- within (or of unknown) NDPS purview — an unsupported NDPS-intersection workflow, a boundary of
-- this software rather than a requirement stated by rule 65 — and carries a finalized working entry
-- with both physical-act confirmations, linked to that line's own dispensing, for that product and
-- quantity. No entry of the Sale may be left prepared or confirmed, and no finalized entry may
-- belong to a line that is not Schedule H1. A multi-line supply therefore finalizes completely or
-- not at all.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER sale_documents_h1_register_required
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND (
    EXISTS (
        SELECT 1 FROM sale_lines line
        WHERE line.sale_document_id = NEW.id
          AND json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
          AND (
              json_extract(line.regulatory_schemes_snapshot, '$.ndps_purview') IS NOT 'does_not_apply'
           OR NOT EXISTS (
                  SELECT 1 FROM prescription_h1_register_entries entry
                  JOIN prescription_dispensings dispensing ON dispensing.id = entry.dispensing_id
                  WHERE entry.sale_line_id = line.id
                    AND entry.sale_document_id = NEW.id
                    AND entry.status = 'finalized'
                    AND entry.hard_copy_placed_in_register = 1
                    AND entry.pharmacist_authenticated_hard_copy = 1
                    AND entry.product_id = line.product_id
                    AND entry.quantity_atoms = line.quantity_atoms
                    AND dispensing.sale_line_id = line.id)
          )
    )
    OR EXISTS (
        SELECT 1 FROM prescription_h1_register_entries entry
        WHERE entry.sale_document_id = NEW.id AND entry.status IN ('prepared', 'confirmed')
    )
    OR EXISTS (
        SELECT 1 FROM prescription_h1_register_entries entry
        JOIN sale_lines line ON line.id = entry.sale_line_id
        WHERE entry.sale_document_id = NEW.id AND entry.status = 'finalized'
          AND json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') IS NOT 'applies'
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_entry_missing');
END;


-- ---------------------------------------------------------------------------------------------
-- 6. Annotations on a finalized entry.
--
-- The original entry never changes. A later correction or note is appended beside it, with who,
-- when and why. Rule 65 prescribes no correction format and none is claimed; this is AUSHADHARTH's
-- own append-only record of what a dispensing user noted afterwards.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_h1_register_annotations (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entry_id TEXT NOT NULL REFERENCES prescription_h1_register_entries(id) ON DELETE RESTRICT,
    note TEXT NOT NULL CHECK (length(trim(note)) BETWEEN 3 AND 1000),
    created_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE INDEX prescription_h1_register_annotations_entry_idx
ON prescription_h1_register_annotations(entry_id, created_at_utc);

CREATE TRIGGER prescription_h1_register_annotations_coherent_insert
BEFORE INSERT ON prescription_h1_register_annotations
WHEN NOT EXISTS (
    SELECT 1 FROM prescription_h1_register_entries entry
    JOIN users author ON author.id = NEW.created_by_user_id
    WHERE entry.id = NEW.entry_id AND entry.status = 'finalized'
      AND author.role IN ('owner_admin', 'pharmacist')
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_annotation_incoherent');
END;

CREATE TRIGGER prescription_h1_register_annotations_no_update
BEFORE UPDATE ON prescription_h1_register_annotations
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_annotation_immutable');
END;

CREATE TRIGGER prescription_h1_register_annotations_no_delete
BEFORE DELETE ON prescription_h1_register_annotations
BEGIN
    SELECT RAISE(ABORT, 'prescription_h1_register_annotation_immutable');
END;


-- ---------------------------------------------------------------------------------------------
-- 7. The audit log learns the new entity types.
--
-- Same recreate-and-copy shape as 0014, 0017, 0021 and 0022. Payloads for these types carry
-- identifiers, references and states, never a patient's or prescriber's name or address.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1mb;

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
        'prescription_h1_register_entry', 'prescription_h1_register_annotation'
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
SELECT * FROM master_change_events_phase1mb;

DROP TABLE master_change_events_phase1mb;

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
