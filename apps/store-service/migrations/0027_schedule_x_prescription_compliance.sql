-- Phase 1M-D3-A — the prescription-side Schedule X compliance facts.
--
-- Phase 1M-D2 built the Schedule X working record for rule 65(21). This migration adds the two
-- prescription-side facts the Phase 1M-D3 gate identified, and nothing else. It does NOT enable a
-- Schedule X sale: the sale gate is untouched, and a Schedule X line is still refused with
-- `schedule_x_workflow_not_available`.
--
-- WHAT THIS MIGRATION ADDS, AND WHY
--
--   rule 65(11)(c)  "at the time of dispensing there must be noted on the prescription above the
--                   signature of the prescriber, the name and address of the seller and the date on
--                   which the prescription is dispensed."
--                   -> section 1: an append-only attestation that this was done on the paper, with
--                      the seller name, the seller address and the dispensing date FROZEN as the
--                      person confirmed them. Software cannot write on a prescription and this
--                      never claims to have; what it records is that a person did.
--
--   rule 65(2)      supply on a prescription "only by or under the personal supervision of a
--                   registered pharmacist".
--                   -> section 2: the Schedule X register attestation of rule 65(21)(b)(x) already
--                      demanded an ACTIVE registered pharmacist of this store. It did not demand
--                      that the professional record was valid on the date of the transaction being
--                      attested. It does now.
--
-- WHY 65(11)(c) NEEDS A NEW FACT AT ALL, WHEN 0022 ALREADY HAS ONE
--
-- Phase 1M-B records the same rule twice: `sale_documents.prescription_endorsement_confirmed` while
-- the Sale is a draft, copied onto `prescription_dispensings.endorsement_confirmed_by_user_id` when
-- it posts. For Schedule H that is enough. For Schedule X it is not, for four reasons:
--
--   1. the draft flag is per DOCUMENT. A basket may carry lines from several prescriptions, and one
--      tick would assert the note was written on every one of them. Rule 65(11)(c) speaks of THE
--      prescription, at the time of ITS dispensing.
--   2. the draft flag is MUTABLE and is cleared without trace whenever a line is re-linked. An
--      attestation about a physical act is not a setting; once the pen has touched the paper, no
--      later click unwrites it.
--   3. the draft flag freezes NOTHING. The seller particulars it refers to are frozen at POSTING,
--      and `sale_documents_seller_snapshot_draft_only` actively forbids a draft holding them. So a
--      Store Profile edited between the tick and the posting would silently change what the record
--      says was written on the paper.
--   4. the immutable copy lives on `prescription_dispensings`, which exists only once the Sale has
--      posted. Schedule X posting is fail-closed, so for Schedule X that copy is unreachable: today
--      no durable 65(11)(c) evidence can exist for a Schedule X supply at all.
--
-- Nothing in section 1 changes, reads, or relaxes the Phase 1M-B facts. Schedule H and Schedule H1
-- behave exactly as before, and this table cannot be written for a drug that is not in Schedule X.
--
-- NOTHING IS BACKFILLED. The table below starts empty.

-- ---------------------------------------------------------------------------------------------
-- 1. Rule 65(11)(c): the seller particulars noted on the physical prescription.
--
-- ONE ROW = ONE DISPENSING OCCASION. The occasion is identified, before posting, by the draft Sale
-- line that will become the dispensing — the same pre-post anchor `prescription_supply_records` and
-- the Phase 1M-C H1 entries already use. The dispensing id itself does not exist until posting, so
-- it is not referenced here; a later slice that finalizes a Schedule X supply is the place to bind
-- the two, and until then this row stands on the line it names.
--
-- WHAT IS FROZEN, AND WHY EACH ONE
--
--   seller_name, seller_address   what the operator is confirming was written. Resolved from the
--                                 store legal profile AT CONFIRMATION and copied here, because a
--                                 profile edited tomorrow must not change what this record says was
--                                 written on a piece of paper today. Neither may be blank: rule
--                                 65(11)(c) names both, and a record that cannot say what was
--                                 written is not evidence of anything.
--   dispensing_date               the date of supply the Sale records — `sale_documents.
--                                 business_date`, the same date `prescription_dispensings.
--                                 dispensed_on` and `prescription_supply_records.date_of_supply`
--                                 carry. It is NOT supplied by the caller: the coherence trigger
--                                 below requires it to equal the draft's own business date, so no
--                                 date typed into a screen can become the authoritative one.
--   attested_on_store_date        the store's own calendar day when the confirmation was made. Kept
--                                 BESIDE the dispensing date, never instead of it, so that a
--                                 backdated draft is visible as backdated rather than silently
--                                 becoming the day the pen moved. A dispensing date AFTER the day
--                                 of attestation is refused outright: a prescription cannot have
--                                 been dispensed on a day that has not happened.
--
-- The fact is recorded only as TRUE. There is no row that says the note was not written, because
-- "not yet done" is the absence of a row, and a stored `0` would be a compliance state to toggle.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE schedule_x_prescription_annotations (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,

    -- The dispensing occasion, before it is a dispensing.
    sale_document_id TEXT NOT NULL REFERENCES sale_documents(id) ON DELETE RESTRICT,
    sale_line_id TEXT NOT NULL REFERENCES sale_lines(id) ON DELETE RESTRICT,
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    prescription_item_id TEXT NOT NULL REFERENCES prescription_items(id) ON DELETE RESTRICT,
    -- Rule 65(11A): the preparation named, and no other. Frozen so a consumer must compare rather
    -- than assume the line still points where it pointed.
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,

    seller_particulars_noted_on_prescription INTEGER NOT NULL
        CHECK (seller_particulars_noted_on_prescription = 1),

    seller_name TEXT NOT NULL CHECK (length(trim(seller_name)) BETWEEN 1 AND 200),
    seller_address TEXT NOT NULL CHECK (length(trim(seller_address)) BETWEEN 1 AND 500),
    dispensing_date TEXT NOT NULL CHECK (dispensing_date GLOB '????-??-??'),
    attested_on_store_date TEXT NOT NULL CHECK (attested_on_store_date GLOB '????-??-??'),

    -- Who made the statement. Recorded separately from any registered pharmacist: rule 65(11)(c)
    -- requires the note to be MADE and says nothing about who makes it, and inventing that
    -- requirement would be inventing law.
    attested_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    attested_at_utc TEXT NOT NULL CHECK (attested_at_utc GLOB '????-??-??T??:??:??*Z'),
    note TEXT CHECK (note IS NULL OR length(trim(note)) BETWEEN 1 AND 300),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),

    CHECK (dispensing_date <= attested_on_store_date)
) STRICT;

-- One attestation per dispensing occasion. A retry finds this row and reuses it; a second
-- confirmation cannot be written at all.
CREATE UNIQUE INDEX schedule_x_prescription_annotations_occasion_uq
ON schedule_x_prescription_annotations(sale_line_id);

CREATE INDEX schedule_x_prescription_annotations_prescription_idx
ON schedule_x_prescription_annotations(store_id, prescription_id);

CREATE INDEX schedule_x_prescription_annotations_document_idx
ON schedule_x_prescription_annotations(store_id, sale_document_id);

-- An attestation is structurally true of the occasion it names, whoever writes it — service, script
-- or sqlite3 shell:
--   * the Sale is a DRAFT of this store, and its business date is the frozen dispensing date;
--   * the line belongs to that Sale, supplies this product, and points at this prescription item;
--   * the item names this product (rule 65(11A)) and belongs to this active prescription of this
--     store;
--   * the person making the statement holds a role permitted to make it — never a cashier;
--   * and the product is recorded as inside SCHEDULE X on the dispensing date.
--
-- The last condition is what keeps this table Schedule-X-only. An ordinary Schedule H prescription
-- cannot acquire a row here, so no existing sale acquires a new requirement.
--
-- `users` carries no store: this installation serves one store, and a store column on the actor
-- would be a fact the schema cannot support. Role coherence is enforced instead.
CREATE TRIGGER schedule_x_prescription_annotations_coherent_insert
BEFORE INSERT ON schedule_x_prescription_annotations
WHEN NOT EXISTS (
    SELECT 1 FROM sale_documents document
    JOIN sale_lines line ON line.sale_document_id = document.id
    JOIN prescription_items item ON item.id = NEW.prescription_item_id
    JOIN prescriptions prescription ON prescription.id = item.prescription_id
    JOIN users confirmer ON confirmer.id = NEW.attested_by_user_id
    WHERE document.id = NEW.sale_document_id
      AND document.store_id = NEW.store_id
      AND document.status = 'draft'
      AND document.business_date = NEW.dispensing_date
      AND line.id = NEW.sale_line_id
      AND line.product_id = NEW.product_id
      AND line.prescription_item_id = NEW.prescription_item_id
      AND item.product_id = NEW.product_id
      AND prescription.id = NEW.prescription_id
      AND prescription.store_id = NEW.store_id
      AND prescription.status = 'active'
      AND confirmer.role IN ('owner_admin', 'pharmacist')
      AND EXISTS (
          SELECT 1 FROM product_regulatory_classifications finding
          WHERE finding.product_id = NEW.product_id
            AND finding.scheme = 'schedule_x'
            AND finding.status = 'active'
            AND finding.applies = 1
            AND finding.effective_from <= NEW.dispensing_date
            AND (finding.effective_to IS NULL OR finding.effective_to > NEW.dispensing_date))
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_prescription_annotation_incoherent');
END;

-- Append-only, in both directions. There is no "unconfirm", and voiding or cancelling the Sale does
-- not reach this row: the paper was written on, and that remains true whatever happens to the sale.
CREATE TRIGGER schedule_x_prescription_annotations_no_update
BEFORE UPDATE ON schedule_x_prescription_annotations
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_prescription_annotation_is_append_only');
END;

CREATE TRIGGER schedule_x_prescription_annotations_no_delete
BEFORE DELETE ON schedule_x_prescription_annotations
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_prescription_annotation_is_append_only');
END;

-- ---------------------------------------------------------------------------------------------
-- 2. Rule 65(2): the registered pharmacist must have been registered ON THE DAY.
--
-- Phase 1M-D2 required the person named under rule 65(21)(b)(x) to be a registered pharmacist of
-- this store whose professional record is ACTIVE. Active is not the same as valid: a record can
-- carry a registration that began after, or expired before, the transaction being attested, and
-- the trigger let it through. Phase 1M-B already judges exactly this on the Sale path
-- (`professional_valid_on`, on the business date and on the actual posting day); the Schedule X
-- register path did not.
--
-- The trigger below judges the date the ROW ITSELF carries — `transaction_date` — because that is
-- the only date a trigger can know deterministically, and it must give the same answer on every
-- machine and after every restore. The store's ACTUAL calendar day at confirmation is the other
-- half of the established policy and cannot be resolved in SQL for a store outside Asia/Kolkata,
-- so the service enforces that half, on the same connection and under the same lock, exactly as
-- 0022 already splits the work. Neither half is a substitute for the other and both are tested.
--
-- The convention is the repository's own, from `professional_valid_on`: an absent bound is open,
-- and a recorded bound is INCLUSIVE.
--
-- SQLite cannot alter a trigger in place, so this drops and recreates it. The recreated text was
-- generated mechanically from the 0026 trigger: the two conditions below are the only additions,
-- and every other clause is byte-for-byte what 0026 wrote. Rows already confirmed are untouched —
-- this is a BEFORE UPDATE guard and reaches only transitions made from now on.
-- ---------------------------------------------------------------------------------------------
DROP TRIGGER store_schedule_x_register_entries_transition;

CREATE TRIGGER store_schedule_x_register_entries_transition
BEFORE UPDATE ON store_schedule_x_register_entries
WHEN NOT (
    NEW.id = OLD.id AND NEW.store_id = OLD.store_id
    AND NEW.entry_kind = OLD.entry_kind
    AND NEW.reference_value = OLD.reference_value AND NEW.reference = OLD.reference
    AND NEW.transaction_date = OLD.transaction_date
    AND NEW.drug_name = OLD.drug_name AND NEW.product_id = OLD.product_id
    AND NEW.batch_state = OLD.batch_state AND NEW.batch_number IS OLD.batch_number
    AND NEW.manufacturer_state = OLD.manufacturer_state
    AND NEW.manufacturer_name IS OLD.manufacturer_name
    AND NEW.quantity_atoms = OLD.quantity_atoms AND NEW.quantity_packs IS OLD.quantity_packs
    AND NEW.bill_number IS OLD.bill_number AND NEW.bill_date IS OLD.bill_date
    AND NEW.purchase_document_id IS OLD.purchase_document_id
    AND NEW.purchase_line_id IS OLD.purchase_line_id
    AND NEW.supplier_name IS OLD.supplier_name
    AND NEW.supplier_address_state IS OLD.supplier_address_state
    AND NEW.supplier_address IS OLD.supplier_address
    AND NEW.supplier_licence_state IS OLD.supplier_licence_state
    AND NEW.supplier_licence_number IS OLD.supplier_licence_number
    AND NEW.supply_basis IS OLD.supply_basis
    AND NEW.sale_document_id IS OLD.sale_document_id
    AND NEW.sale_line_id IS OLD.sale_line_id
    AND NEW.prescription_id IS OLD.prescription_id
    AND NEW.prescription_item_id IS OLD.prescription_item_id
    AND NEW.subject_kind IS OLD.subject_kind
    AND NEW.purchaser_name IS OLD.purchaser_name
    AND NEW.purchaser_address IS OLD.purchaser_address
    AND NEW.prescription_reference IS OLD.prescription_reference
    AND NEW.prepared_by_user_id = OLD.prepared_by_user_id
    AND NEW.prepared_at_utc = OLD.prepared_at_utc
    AND NEW.created_at_utc = OLD.created_at_utc
    AND (
        (OLD.status = 'prepared' AND NEW.status = 'confirmed'
         AND NEW.particulars_entered_in_physical_register = 1
         AND NEW.physical_entry_authenticated = 1
         AND NEW.finalized_at_utc IS NULL
         AND NEW.dispensing_id IS OLD.dispensing_id
         AND EXISTS (
             SELECT 1 FROM users confirmer
             WHERE confirmer.id = NEW.confirmed_by_user_id
               AND confirmer.role IN ('owner_admin', 'pharmacist'))
         AND EXISTS (
             SELECT 1 FROM store_professionals professional
             WHERE professional.id = NEW.supervising_professional_id
               AND professional.store_id = NEW.store_id
               AND professional.status = 'active'
               AND professional.capacity = 'registered_pharmacist'
               AND (professional.valid_from IS NULL OR professional.valid_from <= NEW.transaction_date)
               AND (professional.valid_upto IS NULL OR professional.valid_upto >= NEW.transaction_date)
               AND professional.full_name = NEW.supervising_professional_name
               AND professional.registration_number = NEW.supervising_registration_number))
     OR (OLD.status = 'confirmed' AND NEW.status = 'finalized'
         AND NEW.particulars_entered_in_physical_register
             = OLD.particulars_entered_in_physical_register
         AND NEW.physical_entry_authenticated = OLD.physical_entry_authenticated
         AND NEW.confirmed_by_user_id = OLD.confirmed_by_user_id
         AND NEW.confirmed_at_utc = OLD.confirmed_at_utc
         AND NEW.supervising_professional_id = OLD.supervising_professional_id
         AND NEW.supervising_professional_name = OLD.supervising_professional_name
         AND NEW.supervising_registration_number = OLD.supervising_registration_number
         AND NEW.finalized_by_user_id IS NOT NULL
         AND NEW.dispensing_id IS OLD.dispensing_id)
     OR (OLD.status IN ('prepared', 'confirmed') AND NEW.status = 'void'
         AND NEW.particulars_entered_in_physical_register
             = OLD.particulars_entered_in_physical_register
         AND NEW.physical_entry_authenticated = OLD.physical_entry_authenticated
         AND NEW.confirmed_by_user_id IS OLD.confirmed_by_user_id
         AND NEW.confirmed_at_utc IS OLD.confirmed_at_utc
         AND NEW.supervising_professional_id IS OLD.supervising_professional_id
         AND NEW.finalized_at_utc IS NULL
         AND NEW.dispensing_id IS OLD.dispensing_id)
    )
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_register_entry_immutable');
END;

-- ---------------------------------------------------------------------------------------------
-- 3. The audit log learns the new entity type.
--
-- Same recreate-and-copy shape as 0014, 0017, 0021, 0022, 0023, 0025 and 0026. Every existing
-- event is carried across unchanged and every column is preserved. The payload for this type
-- carries identifiers and the fact type only — never a patient's name or address, never a
-- prescriber's, and never the frozen seller particulars, which live on the row itself.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1md3a;

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
        'schedule_x_prescription_annotation'
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
SELECT * FROM master_change_events_phase1md3a;

DROP TABLE master_change_events_phase1md3a;

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
