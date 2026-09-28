-- ---------------------------------------------------------------------------------------------
-- Phase 1M-D2 — Schedule X register foundations: the working record.
--
-- Rule 65(21)(a) of the Drugs and Cosmetics Rules, 1945 requires the supply of a Schedule X drug to
-- be recorded AT THE TIME OF SUPPLY in a register that is BOUND, SERIALLY PAGE NUMBERED, specially
-- maintained, with SEPARATE PAGES ALLOTTED FOR EACH DRUG. Rule 65(21)(b) then lists ten particulars
-- spanning both sides of the transaction: what came in from the supplier, and what went out to the
-- patient or purchaser.
--
-- Three consequences shape everything below.
--
-- FIRST: a bound, page-numbered book is not something software can be. The word "electronic" does
-- not appear in rule 65, and nothing in it accommodates a database in place of the register. So what
-- this table holds is a WORKING RECORD — the particulars gathered, frozen and printable, so a person
-- can write the statutory register accurately — and never the register itself. The same honesty the
-- Phase 1M-C Schedule H1 register already practises: the software allocates its own reference,
-- records that the physical acts happened, and claims nothing more.
--
-- SECOND: there is no page number here, and there never will be from software. Rule 65(21) says
-- separate pages per drug and says nothing else about pages — nothing about what happens when a
-- page fills, whether a drug continues on another page, whether continuation pages cross-reference
-- each other, or whether a page may ever be reused. Inventing any of those rules would be inventing
-- law. The `reference` below is an AUSHADHARTH reference and is deliberately shaped so it can never
-- be mistaken for a register serial or a book page.
--
-- THIRD: rule 65(21) is ONE register carrying both receipt and supply. So this is one table with two
-- entry kinds rather than two compliance systems that would drift apart. A receipt entry freezes
-- what the Phase 1M-D1-A purchase provenance recorded at posting; a supply entry's shape exists so
-- the later supply slice has somewhere truthful to land. Schedule X SALE REMAINS REFUSED: this
-- migration adds no path to a posted Schedule X sale, and narrows no gate.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- 1. The working record.
--
-- Every legally material particular is FROZEN into the row. Nothing here is rendered by joining
-- today's Party, Product or Company master: a working record for a receipt in April must still read
-- as it did in April after the supplier is renamed, the product relabelled or the manufacturer
-- archived. The `*_state` columns carry the same `recorded` / `not_recorded` vocabulary D1-A
-- established, because a particular the store never recorded is a fact about the record and must not
-- be quietly filled in from a master that happens to hold something today.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_schedule_x_register_entries (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,

    -- One register, two kinds of entry, exactly as rule 65(21)(b) describes one register carrying
    -- both what was received and what was supplied.
    entry_kind TEXT NOT NULL CHECK (entry_kind IN ('receipt', 'supply')),

    -- The AUSHADHARTH reference: internal, immutable, never reused, store-scoped. It is NOT the
    -- statutory serial and NOT a page number. The shape is checked so it cannot drift into looking
    -- like one.
    reference_value INTEGER NOT NULL CHECK (reference_value >= 1),
    reference TEXT NOT NULL CHECK (reference = 'AXR-' || printf('%06d', reference_value)),

    -- Rule 65(21)(b)(i): date of transaction. For a receipt this is the supplier's invoice date;
    -- for a supply it will be the Sale's business date.
    transaction_date TEXT NOT NULL CHECK (transaction_date GLOB '????-??-??'),

    -- Rule 65(21)(b)(iii) name of the drug, (vi) batch or lot number, (v) manufacturer's name.
    -- All three frozen from the posted Purchase, never re-read from the product or the company.
    drug_name TEXT NOT NULL CHECK (length(trim(drug_name)) BETWEEN 1 AND 300),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    batch_state TEXT NOT NULL CHECK (batch_state IN ('recorded', 'not_recorded')),
    batch_number TEXT CHECK (batch_number IS NULL OR length(trim(batch_number)) BETWEEN 1 AND 60),
    manufacturer_state TEXT NOT NULL CHECK (manufacturer_state IN ('recorded', 'not_recorded')),
    manufacturer_name TEXT CHECK (
        manufacturer_name IS NULL OR length(trim(manufacturer_name)) BETWEEN 1 AND 200
    ),

    -- Rule 65(21)(b)(ii) quantity received / (iv) quantity supplied. Atoms are the ledger's own
    -- unit; packs are kept for a receipt because that is how the invoice reads.
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms > 0),
    quantity_packs INTEGER CHECK (quantity_packs IS NULL OR quantity_packs > 0),

    -- Rule 65(21)(b)(ix): bill number and date. The supplier's invoice for a receipt; the Sale's
    -- own document number for a supply.
    bill_number TEXT CHECK (bill_number IS NULL OR length(trim(bill_number)) BETWEEN 1 AND 60),
    bill_date TEXT CHECK (bill_date IS NULL OR bill_date GLOB '????-??-??'),

    -- --- Receipt side: rule 65(21)(b)(ii), the supplier's name, address and licence number -------
    purchase_document_id TEXT REFERENCES purchase_documents(id) ON DELETE RESTRICT,
    purchase_line_id TEXT REFERENCES purchase_lines(id) ON DELETE RESTRICT,
    supplier_name TEXT CHECK (supplier_name IS NULL OR length(trim(supplier_name)) BETWEEN 1 AND 200),
    supplier_address_state TEXT CHECK (
        supplier_address_state IS NULL OR supplier_address_state IN ('recorded', 'not_recorded')
    ),
    supplier_address TEXT CHECK (
        supplier_address IS NULL OR length(trim(supplier_address)) BETWEEN 1 AND 500
    ),
    supplier_licence_state TEXT CHECK (
        supplier_licence_state IS NULL OR supplier_licence_state IN ('recorded', 'not_recorded')
    ),
    supplier_licence_number TEXT CHECK (
        supplier_licence_number IS NULL OR length(trim(supplier_licence_number)) BETWEEN 1 AND 120
    ),

    -- --- Supply side: shape only in this phase ---------------------------------------------------
    -- Rule 65(9)(a) supplies a Schedule X drug only on a prescription, so the only basis this
    -- column admits is 'prescription'. The rule 65(9)(b) signed-order channel for Registered
    -- Medical Practitioners, hospitals, dispensaries and nursing homes is NOT modelled: an
    -- institutional supply therefore cannot be represented here at all, rather than being
    -- misrepresented as an ordinary prescription sale.
    supply_basis TEXT CHECK (supply_basis IS NULL OR supply_basis IN ('prescription')),
    sale_document_id TEXT REFERENCES sale_documents(id) ON DELETE RESTRICT,
    sale_line_id TEXT REFERENCES sale_lines(id) ON DELETE RESTRICT,
    prescription_id TEXT REFERENCES prescriptions(id) ON DELETE RESTRICT,
    prescription_item_id TEXT REFERENCES prescription_items(id) ON DELETE RESTRICT,
    dispensing_id TEXT REFERENCES prescription_dispensings(id) ON DELETE RESTRICT,
    -- Rule 65(21)(b)(vii): "Name and address of the patient/purchaser". Rule 65(10)(b) makes the
    -- owner of the animal the person named where the drug is for veterinary use, so one pair of
    -- columns serves both and `subject_kind` says which. No animal species or identification is
    -- required by rule 65(21) and none is invented here.
    subject_kind TEXT CHECK (subject_kind IS NULL OR subject_kind IN ('human', 'animal')),
    purchaser_name TEXT CHECK (
        purchaser_name IS NULL OR length(trim(purchaser_name)) BETWEEN 1 AND 200
    ),
    purchaser_address TEXT CHECK (
        purchaser_address IS NULL OR length(trim(purchaser_address)) BETWEEN 1 AND 500
    ),
    -- Rule 65(21)(b)(viii): prescription reference.
    prescription_reference TEXT CHECK (
        prescription_reference IS NULL OR length(trim(prescription_reference)) BETWEEN 1 AND 120
    ),

    -- --- Lifecycle -------------------------------------------------------------------------------
    -- 'prepared'  the particulars are frozen; the physical book has not been written yet.
    -- 'confirmed' a person has attested BOTH physical acts of rule 65(21): the particulars were
    --             entered in the bound register, and the required authentication by the person under
    --             whose supervision the transaction happened was completed on the page.
    -- 'finalized' the working entry is closed and can never change again.
    -- 'void'      withdrawn before closing, with a reason; the row and its reference are kept.
    status TEXT NOT NULL DEFAULT 'prepared'
        CHECK (status IN ('prepared', 'confirmed', 'finalized', 'void')),
    prepared_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    prepared_at_utc TEXT NOT NULL CHECK (prepared_at_utc GLOB '????-??-??T??:??:??*Z'),

    -- The two physical acts. Rule 65(21)(b)(x) is the SIGNATURE of the person under whose
    -- supervision the drug was supplied. Software cannot make that signature and does not pretend
    -- to: these columns record only that a person confirmed the physical acts were done, and the
    -- supervising professional is named from the Phase 1M-A record so the claim is checkable.
    particulars_entered_in_physical_register INTEGER NOT NULL DEFAULT 0
        CHECK (particulars_entered_in_physical_register IN (0, 1)),
    physical_entry_authenticated INTEGER NOT NULL DEFAULT 0
        CHECK (physical_entry_authenticated IN (0, 1)),
    supervising_professional_id TEXT REFERENCES store_professionals(id) ON DELETE RESTRICT,
    supervising_professional_name TEXT CHECK (
        supervising_professional_name IS NULL OR length(trim(supervising_professional_name)) > 0
    ),
    supervising_registration_number TEXT CHECK (
        supervising_registration_number IS NULL OR length(trim(supervising_registration_number)) > 0
    ),
    confirmed_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    confirmed_at_utc TEXT CHECK (confirmed_at_utc IS NULL OR confirmed_at_utc GLOB '????-??-??T??:??:??*Z'),

    finalized_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    finalized_at_utc TEXT CHECK (finalized_at_utc IS NULL OR finalized_at_utc GLOB '????-??-??T??:??:??*Z'),

    voided_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    voided_at_utc TEXT CHECK (voided_at_utc IS NULL OR voided_at_utc GLOB '????-??-??T??:??:??*Z'),
    void_reason TEXT CHECK (void_reason IS NULL OR length(trim(void_reason)) BETWEEN 1 AND 500),

    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),

    -- A receipt names a Purchase and nothing from the supply side; a supply names a Sale and nothing
    -- from the receipt side. Neither kind can borrow the other's provenance.
    CHECK (
        (entry_kind = 'receipt'
         AND purchase_document_id IS NOT NULL AND purchase_line_id IS NOT NULL
         AND supplier_name IS NOT NULL AND supplier_address_state IS NOT NULL
         AND supplier_licence_state IS NOT NULL
         AND supply_basis IS NULL AND sale_document_id IS NULL AND sale_line_id IS NULL
         AND prescription_id IS NULL AND prescription_item_id IS NULL AND dispensing_id IS NULL
         AND subject_kind IS NULL AND purchaser_name IS NULL AND purchaser_address IS NULL
         AND prescription_reference IS NULL)
        OR
        (entry_kind = 'supply'
         AND purchase_document_id IS NULL AND purchase_line_id IS NULL
         AND supplier_name IS NULL AND supplier_address_state IS NULL
         AND supplier_address IS NULL AND supplier_licence_state IS NULL
         AND supplier_licence_number IS NULL
         AND supply_basis IS NOT NULL AND sale_document_id IS NOT NULL AND sale_line_id IS NOT NULL
         AND prescription_id IS NOT NULL AND prescription_item_id IS NOT NULL
         AND subject_kind IS NOT NULL AND purchaser_name IS NOT NULL
         AND purchaser_address IS NOT NULL AND prescription_reference IS NOT NULL
         AND quantity_packs IS NULL)
    ),
    -- A particular is either recorded with its text or recorded as absent. It is never both and
    -- never neither.
    CHECK ((batch_state = 'recorded') = (batch_number IS NOT NULL)),
    CHECK ((manufacturer_state = 'recorded') = (manufacturer_name IS NOT NULL)),
    CHECK (supplier_address_state IS NULL
           OR (supplier_address_state = 'recorded') = (supplier_address IS NOT NULL)),
    CHECK (supplier_licence_state IS NULL
           OR (supplier_licence_state = 'recorded') = (supplier_licence_number IS NOT NULL)),

    -- Both physical acts are attested together or not at all: a page written but unsigned, or signed
    -- but unwritten, is not a state this software will record as confirmed.
    CHECK (particulars_entered_in_physical_register = physical_entry_authenticated),
    CHECK ((particulars_entered_in_physical_register = 1) = (confirmed_by_user_id IS NOT NULL)),
    CHECK ((confirmed_by_user_id IS NULL) = (confirmed_at_utc IS NULL)),
    -- Naming the supervising person is part of the attestation, not separable from it.
    CHECK ((confirmed_by_user_id IS NULL) = (supervising_professional_id IS NULL)),
    CHECK ((supervising_professional_id IS NULL) = (supervising_professional_name IS NULL)),
    CHECK ((supervising_professional_id IS NULL) = (supervising_registration_number IS NULL)),

    CHECK (status <> 'prepared' OR (particulars_entered_in_physical_register = 0
                                    AND finalized_at_utc IS NULL)),
    CHECK (status NOT IN ('confirmed', 'finalized')
           OR particulars_entered_in_physical_register = 1),
    CHECK ((status = 'finalized') = (finalized_at_utc IS NOT NULL)),
    CHECK ((finalized_by_user_id IS NULL) = (finalized_at_utc IS NULL)),
    CHECK (
        (status = 'void' AND voided_by_user_id IS NOT NULL AND voided_at_utc IS NOT NULL
         AND void_reason IS NOT NULL AND finalized_at_utc IS NULL)
        OR (status <> 'void' AND voided_by_user_id IS NULL AND voided_at_utc IS NULL
            AND void_reason IS NULL)
    )
) STRICT;

-- The register is read by store and date, and by the source document when a Purchase is opened.
CREATE INDEX store_schedule_x_register_entries_register_idx
ON store_schedule_x_register_entries(store_id, entry_kind, transaction_date, reference_value);

CREATE INDEX store_schedule_x_register_entries_purchase_idx
ON store_schedule_x_register_entries(purchase_line_id);

CREATE INDEX store_schedule_x_register_entries_sale_idx
ON store_schedule_x_register_entries(sale_line_id);

-- The reference is unique within the store. Void entries keep theirs, so a reference is never
-- reissued and the sequence never renumbers itself.
CREATE UNIQUE INDEX store_schedule_x_register_entries_reference_idx
ON store_schedule_x_register_entries(store_id, reference_value);

-- One live working entry per source event. A void entry no longer holds the slot, so a corrected
-- entry can be prepared after a mistaken one is withdrawn — but two live entries for one purchase
-- line can never exist, which is what would otherwise let a drug be written into the register twice.
CREATE UNIQUE INDEX store_schedule_x_register_entries_one_live_receipt_idx
ON store_schedule_x_register_entries(purchase_line_id)
WHERE entry_kind = 'receipt' AND status <> 'void';

CREATE UNIQUE INDEX store_schedule_x_register_entries_one_live_supply_idx
ON store_schedule_x_register_entries(sale_line_id)
WHERE entry_kind = 'supply' AND status <> 'void';

-- ---------------------------------------------------------------------------------------------
-- 2. What a new entry must agree with.
--
-- A receipt entry may only be written for a POSTED Purchase of this store, for one of that
-- document's own lines, for the product and quantity that line actually carries, with the
-- particulars taken from that document's and line's own D1-A provenance — and only while the
-- product is recorded as inside Schedule X on the transaction date by an active owner finding.
-- Nothing here reads a product's name: a product called "Schedule X" is not a Schedule X drug, and
-- a Schedule X drug called anything at all still is one.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER store_schedule_x_register_entries_coherent_insert
BEFORE INSERT ON store_schedule_x_register_entries
WHEN NOT (
    NEW.status = 'prepared'
    AND NEW.particulars_entered_in_physical_register = 0
    AND NEW.physical_entry_authenticated = 0
    AND NEW.confirmed_by_user_id IS NULL
    AND NEW.finalized_at_utc IS NULL
    AND NEW.voided_at_utc IS NULL
    AND NEW.reference_value = (
        SELECT COALESCE(MAX(reference_value), 0) + 1
        FROM store_schedule_x_register_entries WHERE store_id = NEW.store_id)
    AND EXISTS (SELECT 1 FROM users preparer WHERE preparer.id = NEW.prepared_by_user_id)
    AND EXISTS (
        SELECT 1 FROM product_regulatory_classifications finding
        WHERE finding.product_id = NEW.product_id AND finding.scheme = 'schedule_x'
          AND finding.status = 'active' AND finding.applies = 1
          AND finding.effective_from <= NEW.transaction_date
          AND (finding.effective_to IS NULL OR finding.effective_to > NEW.transaction_date))
    AND (
        NEW.entry_kind <> 'receipt'
        OR EXISTS (
            SELECT 1 FROM purchase_documents document
            JOIN purchase_lines line ON line.purchase_document_id = document.id
            WHERE document.id = NEW.purchase_document_id
              AND document.store_id = NEW.store_id
              AND document.status = 'posted'
              AND document.invoice_date = NEW.transaction_date
              AND document.supplier_display_name = NEW.supplier_name
              AND document.supplier_invoice_number IS NEW.bill_number
              AND document.invoice_date IS NEW.bill_date
              AND document.supplier_address_state IS NEW.supplier_address_state
              AND document.supplier_address_line1 IS NEW.supplier_address
              AND document.supplier_drug_licence_state IS NEW.supplier_licence_state
              AND document.supplier_drug_licence_number IS NEW.supplier_licence_number
              AND line.id = NEW.purchase_line_id
              AND line.product_id = NEW.product_id
              AND line.quantity_atoms = NEW.quantity_atoms
              AND line.quantity_packs IS NEW.quantity_packs
              AND line.drug_display_name IS NEW.drug_name
              AND line.manufacturer_state IS NEW.manufacturer_state
              AND line.manufacturer_name IS NEW.manufacturer_name
              AND (
                  (NEW.batch_state = 'recorded' AND line.batch_number = NEW.batch_number)
                  OR (NEW.batch_state = 'not_recorded' AND line.batch_number IS NULL)
              ))
    )
    AND (
        NEW.entry_kind <> 'supply'
        OR EXISTS (
            SELECT 1 FROM sale_documents document
            JOIN sale_lines line ON line.sale_document_id = document.id
            JOIN prescription_items item ON item.id = NEW.prescription_item_id
            JOIN prescriptions prescription ON prescription.id = item.prescription_id
            WHERE document.id = NEW.sale_document_id
              AND document.store_id = NEW.store_id
              AND document.business_date = NEW.transaction_date
              AND line.id = NEW.sale_line_id
              AND line.product_id = NEW.product_id
              AND line.quantity_atoms = NEW.quantity_atoms
              AND item.product_id = NEW.product_id
              AND prescription.id = NEW.prescription_id
              AND prescription.store_id = NEW.store_id
              AND prescription.subject_kind = NEW.subject_kind
              AND prescription.subject_name = NEW.purchaser_name
              AND prescription.subject_address = NEW.purchaser_address
              AND prescription.reference = NEW.prescription_reference)
    )
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_register_entry_incoherent');
END;

-- ---------------------------------------------------------------------------------------------
-- 3. What may change afterwards, and what never may.
--
-- No frozen particular moves, ever. The only transitions are prepared -> confirmed (both physical
-- acts attested, naming an ACTIVE registered pharmacist of this store), confirmed -> finalized
-- (closing the entry), and prepared/confirmed -> void (with a reason). A finalized entry is beyond
-- reach, a void entry cannot be resurrected, and no row may be deleted at all.
-- ---------------------------------------------------------------------------------------------
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

CREATE TRIGGER store_schedule_x_register_entries_no_delete
BEFORE DELETE ON store_schedule_x_register_entries
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_register_entry_immutable');
END;

-- ---------------------------------------------------------------------------------------------
-- 4. Rule 65(9)(a): the retained duplicate prescription.
--
-- "in the case of substances specified in Schedule X, the prescriptions shall be in duplicate, one
-- copy of which shall be retained by the licensee for a period of two years."
--
-- The duplicate is paper the prescriber wrote and the licensee keeps. Software cannot create it and
-- cannot be it. So this table holds exactly one thing: a person's attestation, at a time, that the
-- retained copy of a named prescription is held. A row saying it is NOT held is as meaningful as one
-- saying it is, and neither is a substitute for the paper.
--
-- No retention period is encoded and nothing here deletes itself after two years: rule 65(7) sets a
-- floor of two years from the last entry, not a licence to destroy on the anniversary.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_duplicate_copy_attestations (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    register_entry_id TEXT REFERENCES store_schedule_x_register_entries(id) ON DELETE RESTRICT,
    retained_duplicate_prescription_copy_confirmed INTEGER NOT NULL
        CHECK (retained_duplicate_prescription_copy_confirmed IN (0, 1)),
    attested_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    attested_at_utc TEXT NOT NULL CHECK (attested_at_utc GLOB '????-??-??T??:??:??*Z'),
    note TEXT CHECK (note IS NULL OR length(trim(note)) BETWEEN 1 AND 300)
) STRICT;

-- One attestation per prescription. It is a statement of fact about a piece of paper, not a setting
-- to be toggled, so it is written once and never rewritten.
CREATE UNIQUE INDEX prescription_duplicate_copy_attestations_prescription_idx
ON prescription_duplicate_copy_attestations(store_id, prescription_id);

CREATE TRIGGER prescription_duplicate_copy_attestations_coherent_insert
BEFORE INSERT ON prescription_duplicate_copy_attestations
WHEN NOT EXISTS (
    SELECT 1 FROM prescriptions prescription
    WHERE prescription.id = NEW.prescription_id
      AND prescription.store_id = NEW.store_id
)
BEGIN
    SELECT RAISE(ABORT, 'duplicate_copy_attestation_incoherent');
END;

CREATE TRIGGER prescription_duplicate_copy_attestations_no_update
BEFORE UPDATE ON prescription_duplicate_copy_attestations
BEGIN
    SELECT RAISE(ABORT, 'duplicate_copy_attestation_is_append_only');
END;

CREATE TRIGGER prescription_duplicate_copy_attestations_no_delete
BEFORE DELETE ON prescription_duplicate_copy_attestations
BEGIN
    SELECT RAISE(ABORT, 'duplicate_copy_attestation_is_append_only');
END;

-- ---------------------------------------------------------------------------------------------
-- 5. The audit log learns the two new entity types.
--
-- Same recreate-and-copy shape as 0014, 0017, 0021, 0022, 0023 and 0025. Every existing event is
-- carried across unchanged. Payloads for these types carry identifiers, references and state
-- transitions — never a patient's name or address, and never a prescriber's.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1md2;

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
        'schedule_x_register_entry', 'prescription_duplicate_copy_attestation'
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
SELECT * FROM master_change_events_phase1md2;

DROP TABLE master_change_events_phase1md2;

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
