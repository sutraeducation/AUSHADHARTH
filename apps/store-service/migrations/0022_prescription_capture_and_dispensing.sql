-- Phase 1M-B — prescription capture, dispensing, and the Schedule H retail workflow.
--
-- Phase 1M-A made every scheduled drug unsellable. This migration builds what the Drugs Rules,
-- 1945 require before a Schedule H drug may leave the counter, and lets it leave only when all of
-- it is present. Schedule H1 and Schedule X stay unsellable: their separate statutory records
-- (rule 65(3)(1)(h) and rule 65(21)) are later phases, and a prescription alone does not satisfy
-- either. Schedule C and C(1) stay unsellable for the same reason. A line in several schemes is
-- held to every one of them.
--
-- WHAT THE RULES REQUIRE, AND WHERE EACH REQUIREMENT LIVES HERE
--
--   rule 65(9)(a)  Schedule H substances "shall not be sold by retail except on and in accordance
--                  with the prescription of a Registered Medical Practitioner".
--                  -> a posted Schedule H line must carry a dispensing against a prescription item.
--   rule 65(10)    the prescription shall (a) be in writing, signed and dated by the prescriber;
--                  (b) specify the name and address of the person for whose treatment it is given,
--                  or of the owner of the animal; (c) indicate the total amount of the medicine to
--                  be supplied and the dose to be taken.
--                  -> prescriptions record the date, the subject (human, or animal owner) with name
--                     and address, and each item's total quantity and dose. The writing and the
--                     signature are on paper; the person entering it attests they saw them.
--   rule 65(11)(a) "the prescription must not be dispensed more than once unless the prescriber has
--                  stated thereon that it may be dispensed more than once";
--   rule 65(11)(b) "if the prescription contains a direction that it may be dispensed a stated
--                  number of times or at stated intervals it must not be dispensed otherwise";
--                  -> a repeat authority of 'once' unless a statement was recorded. Nothing is ever
--                     inferred from a blank.
--   rule 65(11)(c) at the time of dispensing, the seller's name and address and the date must be
--                  noted on the prescription above the prescriber's signature.
--                  -> the software cannot write on paper. The dispensing records who confirmed that
--                     the note was written, and when. It never claims to have written it.
--   rule 65(11A)   no person dispensing a Schedule H prescription "may supply any other
--                  preparation, whether containing the same substances or not in lieu thereof".
--                  -> a dispensing must be of the very product the prescription item names.
--   rule 65(2)     supply on a prescription "only by or under the personal supervision of a
--                  registered pharmacist".
--                  -> each dispensing snapshots the Phase 1M-A professional record of the
--                     registered pharmacist who supervised it — never `users.role`.
--   rule 65(3)(1)  the supply "shall be recorded at the time of supply in a prescription register
--                  specially maintained for the purpose and the serial number of entry in the
--                  register shall be entered on the prescription", with particulars (a)-(g); its
--                  first proviso allows a serially numbered cash or credit memo book instead, for
--                  drugs not compounded in the premises and supplied from or in the original
--                  containers.
--   rule 65(3)(2)  the choice between the two is made in writing to the Licensing Authority.
--                  -> section 8a: AUSHADHARTH prepares the entry and allocates its serial in the
--                     book the Store's election names; the registered pharmacist signs the physical
--                     entry and the serial is written on the prescription; a person confirms both;
--                     only then may the Sale post, finalizing the entry in the same transaction.
--                     The statutory record is the physical one the pharmacy keeps.
--
-- NOTHING IS BACKFILLED. Every table below starts empty. No historical Sale acquires a
-- prescription, a patient, a prescriber or a pharmacist it never had.

-- ---------------------------------------------------------------------------------------------
-- 1. Prescribers — a convenience for entry, never the record itself.
--
-- The Drugs Rules define a Registered Medical Practitioner by qualification and registration
-- (rule 2(ee)). This software has no way to check either, so it records what the prescription and
-- the prescriber state and claims nothing more. Every prescription copies these facts at entry; a
-- later edit here changes no prescription already written.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescribers (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    full_name TEXT NOT NULL CHECK (length(trim(full_name)) BETWEEN 1 AND 120),
    address_text TEXT NOT NULL CHECK (length(trim(address_text)) BETWEEN 1 AND 300),
    -- Optional: rule 65 does not put the registration number in the statutory record, but a
    -- pharmacy may well want it, and it is recorded exactly as given.
    registration_number TEXT CHECK (
        registration_number IS NULL OR length(trim(registration_number)) BETWEEN 1 AND 60
    ),
    registering_authority TEXT CHECK (
        registering_authority IS NULL OR length(trim(registering_authority)) BETWEEN 1 AND 160
    ),
    created_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX prescribers_store_idx ON prescribers(store_id, status, full_name);

-- ---------------------------------------------------------------------------------------------
-- 2. Prescriptions — the structured record of the paper the customer brought.
--
-- The prescriber and the patient are COPIED here, not referenced: a prescription says what it
-- said on the day it was written, whatever the master says later.
--
-- The patient is recorded only on a prescription, never as a customer master. Nothing about a
-- patient exists anywhere else in this database, and nothing is asked of anyone buying an
-- unrestricted item.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescriptions (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    -- The pharmacy's own handle for this prescription, e.g. RX-000042. Not an invoice number, not
    -- a register serial: a separate series, and nothing else is numbered from it.
    reference TEXT NOT NULL CHECK (reference GLOB 'RX-[0-9][0-9][0-9][0-9][0-9][0-9]*'),
    prescribed_on TEXT NOT NULL CHECK (prescribed_on GLOB '????-??-??'),

    -- The master used to fill in the prescriber, if one was. Informational only.
    prescriber_id TEXT REFERENCES prescribers(id) ON DELETE RESTRICT,
    prescriber_name TEXT NOT NULL CHECK (length(trim(prescriber_name)) BETWEEN 1 AND 120),
    prescriber_address TEXT NOT NULL CHECK (length(trim(prescriber_address)) BETWEEN 1 AND 300),
    prescriber_registration_number TEXT CHECK (
        prescriber_registration_number IS NULL
        OR length(trim(prescriber_registration_number)) BETWEEN 1 AND 60
    ),
    prescriber_registering_authority TEXT CHECK (
        prescriber_registering_authority IS NULL
        OR length(trim(prescriber_registering_authority)) BETWEEN 1 AND 160
    ),

    -- Rule 65(10)(b): the person for whose treatment it is given, or the owner of the animal.
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('human', 'animal')),
    subject_name TEXT NOT NULL CHECK (length(trim(subject_name)) BETWEEN 1 AND 120),
    subject_address TEXT NOT NULL CHECK (length(trim(subject_address)) BETWEEN 1 AND 300),

    directions_text TEXT CHECK (
        directions_text IS NULL OR length(trim(directions_text)) BETWEEN 1 AND 500
    ),

    -- Rule 65(11). 'once' is the rule's default and the only answer a blank can give.
    --   'once'                 not to be dispensed more than once;
    --   'stated_times'         the prescriber stated a number of times — repeat_times is the TOTAL
    --                          number of dispensing occasions, first one included;
    --   'stated_without_count' the prescriber stated it may be dispensed more than once and gave
    --                          no number — still bounded by the total quantity under rule 65(10)(c).
    repeat_authority TEXT NOT NULL DEFAULT 'once' CHECK (
        repeat_authority IN ('once', 'stated_times', 'stated_without_count')
    ),
    repeat_times INTEGER CHECK (repeat_times IS NULL OR repeat_times BETWEEN 2 AND 100),
    -- A stated interval, in whole days, where the prescriber gave one.
    repeat_interval_days INTEGER CHECK (
        repeat_interval_days IS NULL OR repeat_interval_days BETWEEN 1 AND 366
    ),

    -- Rule 65(10)(a): the person entering this saw a written prescription, signed and dated by the
    -- prescriber. An attestation of fact by that person, required, and never inferred.
    written_signed_dated_attested INTEGER NOT NULL CHECK (written_signed_dated_attested = 1),

    created_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,

    CHECK ((repeat_authority = 'stated_times') = (repeat_times IS NOT NULL)),
    CHECK (repeat_authority <> 'once' OR repeat_interval_days IS NULL),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX prescriptions_reference_uq ON prescriptions(store_id, reference);
CREATE INDEX prescriptions_listing_idx ON prescriptions(store_id, status, prescribed_on);

-- ---------------------------------------------------------------------------------------------
-- 3. What the prescription names, item by item.
--
-- An item names ONE product. Rule 65(11A) forbids supplying another preparation in lieu, "whether
-- containing the same substances or not", so the product is the preparation, and a dispensing must
-- be of that product and no other. `written_description` keeps the words on the paper beside it.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_items (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    line_number INTEGER NOT NULL CHECK (line_number >= 1),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    written_description TEXT NOT NULL CHECK (length(trim(written_description)) BETWEEN 1 AND 200),
    -- Rule 65(10)(c): the total amount to be supplied, in the product's base-unit atoms — the same
    -- integer basis every Sale line already uses, so no division ever enters the comparison.
    prescribed_quantity_atoms INTEGER NOT NULL CHECK (prescribed_quantity_atoms >= 1),
    -- Rule 65(10)(c): the dose to be taken.
    dose_text TEXT NOT NULL CHECK (length(trim(dose_text)) BETWEEN 1 AND 200),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE UNIQUE INDEX prescription_items_line_uq ON prescription_items(prescription_id, line_number);
CREATE INDEX prescription_items_product_idx ON prescription_items(product_id);

-- ---------------------------------------------------------------------------------------------
-- 4. The draft's pointers. Settable only while the Sale is a draft: the existing
--    sale_lines_posted_no_update and sale_documents_posted_no_update triggers freeze them after.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_lines ADD COLUMN prescription_item_id TEXT
    REFERENCES prescription_items(id) ON DELETE RESTRICT;

ALTER TABLE sale_documents ADD COLUMN supervising_professional_id TEXT
    REFERENCES store_professionals(id) ON DELETE RESTRICT;
-- Rule 65(11)(c): the person dispensing confirms the endorsement was written on the prescription.
ALTER TABLE sale_documents ADD COLUMN prescription_endorsement_confirmed INTEGER NOT NULL DEFAULT 0
    CHECK (prescription_endorsement_confirmed IN (0, 1));
-- Who confirmed it and when, so the dispensing can say so. Set together with the flag and cleared
-- with it whenever what is being dispensed changes.
ALTER TABLE sale_documents ADD COLUMN prescription_endorsement_confirmed_by_user_id TEXT
    REFERENCES users(id) ON DELETE RESTRICT;
ALTER TABLE sale_documents ADD COLUMN prescription_endorsement_confirmed_at_utc TEXT
    CHECK (prescription_endorsement_confirmed_at_utc IS NULL
        OR prescription_endorsement_confirmed_at_utc GLOB '????-??-??T??:??:??*Z');
-- Rule 65(3)(1), first proviso: the cash or credit memo book may stand in for the prescription
-- register only for drugs "not compounded in the premises" and "supplied from or in the original
-- containers". That is a fact about what physically left the counter, so the dispensing person
-- attests it. Without the attestation a Store that elected the memo book is refused — never quietly
-- moved to the register.
ALTER TABLE sale_documents ADD COLUMN prescription_original_container_confirmed INTEGER NOT NULL
    DEFAULT 0 CHECK (prescription_original_container_confirmed IN (0, 1));

-- ---------------------------------------------------------------------------------------------
-- 5. The dispensing ledger. Append-only.
--
-- One row per Sale line supplied against a prescription item. How much of a prescription is left
-- is DERIVED from these rows and their reversals — there is no remaining-quantity counter to
-- drift, to race, or to be edited.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_dispensings (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    prescription_item_id TEXT NOT NULL REFERENCES prescription_items(id) ON DELETE RESTRICT,
    sale_document_id TEXT NOT NULL REFERENCES sale_documents(id) ON DELETE RESTRICT,
    sale_line_id TEXT NOT NULL UNIQUE REFERENCES sale_lines(id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms >= 1),
    -- The Sale's business date: the date of supply the document records.
    dispensed_on TEXT NOT NULL CHECK (dispensed_on GLOB '????-??-??'),

    -- Rule 65(2): who supervised, as their professional record stood at posting.
    supervising_professional_id TEXT NOT NULL REFERENCES store_professionals(id) ON DELETE RESTRICT,
    supervising_professional_name TEXT NOT NULL CHECK (length(trim(supervising_professional_name)) > 0),
    supervising_registration_number TEXT NOT NULL CHECK (length(trim(supervising_registration_number)) > 0),
    supervising_registering_authority TEXT,

    -- Rule 65(11)(c): who confirmed the endorsement was written on the paper, and when. The name and
    -- address endorsed are the Sale's frozen seller particulars; the date is `dispensed_on`.
    endorsement_confirmed_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    endorsement_confirmed_at_utc TEXT NOT NULL CHECK (endorsement_confirmed_at_utc GLOB '????-??-??T??:??:??*Z'),

    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE INDEX prescription_dispensings_item_idx ON prescription_dispensings(prescription_item_id);
CREATE INDEX prescription_dispensings_prescription_idx ON prescription_dispensings(prescription_id);

CREATE TRIGGER prescription_dispensings_no_update
BEFORE UPDATE ON prescription_dispensings
BEGIN
    SELECT RAISE(ABORT, 'prescription_dispensings_are_append_only');
END;

CREATE TRIGGER prescription_dispensings_no_delete
BEFORE DELETE ON prescription_dispensings
BEGIN
    SELECT RAISE(ABORT, 'prescription_dispensings_are_append_only');
END;

-- A dispensing is structurally true of the Sale it belongs to, whoever writes it:
--   * it is written while the Sale is still a draft — posting writes it, and no dispensing can be
--     attached to a Sale already posted, so a historical line can never become prescription-backed;
--   * the Sale, the line, the prescription and the Store all agree;
--   * the line is the product the prescription item names (rule 65(11A)) and the quantity the
--     line supplies, and it points at that item;
--   * the prescription is active;
--   * the professional is a registered pharmacist of this Store;
--   * the item's total is not exceeded, counting every earlier dispensing net of its reversals;
--   * a prescription limited to a number of occasions is not dispensed on one more.
-- Validity dates, intervals and the prescription date are judged by the service, which holds the
-- write lock; they are not reproduced here.
CREATE TRIGGER prescription_dispensings_coherent_insert
BEFORE INSERT ON prescription_dispensings
WHEN NOT EXISTS (
    SELECT 1 FROM sale_documents document
    JOIN sale_lines line ON line.sale_document_id = document.id
    JOIN prescription_items item ON item.id = NEW.prescription_item_id
    JOIN prescriptions prescription ON prescription.id = item.prescription_id
    JOIN store_professionals professional ON professional.id = NEW.supervising_professional_id
    WHERE document.id = NEW.sale_document_id
      AND document.status = 'draft'
      AND document.store_id = NEW.store_id
      AND line.id = NEW.sale_line_id
      AND line.product_id = NEW.product_id
      AND line.quantity_atoms = NEW.quantity_atoms
      AND line.prescription_item_id = NEW.prescription_item_id
      AND item.product_id = NEW.product_id
      AND prescription.id = NEW.prescription_id
      AND prescription.store_id = NEW.store_id
      AND prescription.status = 'active'
      AND professional.store_id = NEW.store_id
      AND professional.capacity = 'registered_pharmacist'
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_dispensing_incoherent');
END;

CREATE TRIGGER prescription_dispensings_occasion_cap
BEFORE INSERT ON prescription_dispensings
WHEN EXISTS (
    SELECT 1 FROM prescriptions prescription
    WHERE prescription.id = NEW.prescription_id
      AND prescription.repeat_authority <> 'stated_without_count'
      AND (
          SELECT COUNT(DISTINCT dispensing.sale_document_id)
          FROM prescription_dispensings dispensing
          WHERE dispensing.prescription_id = NEW.prescription_id
            AND dispensing.sale_document_id <> NEW.sale_document_id
      ) + 1 > CASE prescription.repeat_authority
          WHEN 'once' THEN 1
          ELSE prescription.repeat_times
      END
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_repeat_not_authorised');
END;

-- ---------------------------------------------------------------------------------------------
-- 6. Reinstatement on return. Append-only.
--
-- A posted return of a prescription-controlled line gives back the quantity it returned, as a new
-- linked fact. The dispensing it reverses is never edited. Reinstatement restores QUANTITY, not
-- OCCASIONS: the supply happened and was endorsed on the paper, and pretending otherwise would let
-- a return manufacture a repeat the prescriber never stated.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_dispensing_reversals (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    dispensing_id TEXT NOT NULL REFERENCES prescription_dispensings(id) ON DELETE RESTRICT,
    return_line_id TEXT NOT NULL UNIQUE REFERENCES return_lines(id) ON DELETE RESTRICT,
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms >= 1),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE INDEX prescription_dispensing_reversals_dispensing_idx
ON prescription_dispensing_reversals(dispensing_id);

CREATE TRIGGER prescription_dispensing_reversals_no_update
BEFORE UPDATE ON prescription_dispensing_reversals
BEGIN
    SELECT RAISE(ABORT, 'prescription_reversals_are_append_only');
END;

CREATE TRIGGER prescription_dispensing_reversals_no_delete
BEFORE DELETE ON prescription_dispensing_reversals
BEGIN
    SELECT RAISE(ABORT, 'prescription_reversals_are_append_only');
END;

-- The item's total is not exceeded, counting every earlier dispensing net of its reversals.
-- Defined here, after the reversals table it reads.
CREATE TRIGGER prescription_dispensings_quantity_cap
BEFORE INSERT ON prescription_dispensings
WHEN (
    SELECT COALESCE(SUM(dispensing.quantity_atoms), 0) - COALESCE((
        SELECT SUM(reversal.quantity_atoms)
        FROM prescription_dispensing_reversals reversal
        JOIN prescription_dispensings earlier ON earlier.id = reversal.dispensing_id
        WHERE earlier.prescription_item_id = NEW.prescription_item_id
    ), 0)
    FROM prescription_dispensings dispensing
    WHERE dispensing.prescription_item_id = NEW.prescription_item_id
) + NEW.quantity_atoms > (
    SELECT prescribed_quantity_atoms FROM prescription_items WHERE id = NEW.prescription_item_id
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_quantity_exceeded');
END;

-- A reversal belongs to a return line of the very Sale line that was dispensed, and the reversals
-- of one dispensing can never add up to more than it supplied — so no route can create authority
-- the prescription never gave.
CREATE TRIGGER prescription_dispensing_reversals_coherent_insert
BEFORE INSERT ON prescription_dispensing_reversals
WHEN NOT EXISTS (
    SELECT 1 FROM prescription_dispensings dispensing
    JOIN return_lines line ON line.id = NEW.return_line_id
    WHERE dispensing.id = NEW.dispensing_id
      AND line.original_sale_line_id = dispensing.sale_line_id
      AND line.product_id = dispensing.product_id
)
OR (
    SELECT COALESCE(SUM(quantity_atoms), 0) FROM prescription_dispensing_reversals
    WHERE dispensing_id = NEW.dispensing_id
) + NEW.quantity_atoms > (
    SELECT quantity_atoms FROM prescription_dispensings WHERE id = NEW.dispensing_id
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_reversal_incoherent');
END;

-- ---------------------------------------------------------------------------------------------
-- 7. A prescription that has been dispensed is history.
--
-- Before its first dispensing a prescription may be corrected, and each correction is audited.
-- After it, the authority a dispensing relied on cannot be rewritten: the only change left is to
-- archive it, which stops further dispensing. A mistaken prescription is superseded by entering a
-- new one — never by editing the old one under a dispensing's feet.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER prescriptions_dispensed_are_history
BEFORE UPDATE ON prescriptions
WHEN EXISTS (SELECT 1 FROM prescription_dispensings WHERE prescription_id = OLD.id)
  AND (
      NEW.reference IS NOT OLD.reference
   OR NEW.store_id IS NOT OLD.store_id
   OR NEW.prescribed_on IS NOT OLD.prescribed_on
   OR NEW.prescriber_id IS NOT OLD.prescriber_id
   OR NEW.prescriber_name IS NOT OLD.prescriber_name
   OR NEW.prescriber_address IS NOT OLD.prescriber_address
   OR NEW.prescriber_registration_number IS NOT OLD.prescriber_registration_number
   OR NEW.prescriber_registering_authority IS NOT OLD.prescriber_registering_authority
   OR NEW.subject_kind IS NOT OLD.subject_kind
   OR NEW.subject_name IS NOT OLD.subject_name
   OR NEW.subject_address IS NOT OLD.subject_address
   OR NEW.directions_text IS NOT OLD.directions_text
   OR NEW.repeat_authority IS NOT OLD.repeat_authority
   OR NEW.repeat_times IS NOT OLD.repeat_times
   OR NEW.repeat_interval_days IS NOT OLD.repeat_interval_days
   OR NEW.written_signed_dated_attested IS NOT OLD.written_signed_dated_attested
   OR NEW.created_by_user_id IS NOT OLD.created_by_user_id
   OR NEW.created_at_utc IS NOT OLD.created_at_utc
   OR (OLD.status = 'archived' AND NEW.status = 'active')
  )
BEGIN
    SELECT RAISE(ABORT, 'prescription_is_dispensed');
END;

CREATE TRIGGER prescription_items_dispensed_no_update
BEFORE UPDATE ON prescription_items
WHEN EXISTS (
    SELECT 1 FROM prescription_dispensings WHERE prescription_id = OLD.prescription_id
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_is_dispensed');
END;

CREATE TRIGGER prescription_items_dispensed_no_delete
BEFORE DELETE ON prescription_items
WHEN EXISTS (
    SELECT 1 FROM prescription_dispensings WHERE prescription_id = OLD.prescription_id
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_is_dispensed');
END;

CREATE TRIGGER prescription_items_dispensed_no_insert
BEFORE INSERT ON prescription_items
WHEN EXISTS (
    SELECT 1 FROM prescription_dispensings WHERE prescription_id = NEW.prescription_id
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_is_dispensed');
END;

-- ---------------------------------------------------------------------------------------------
-- 8. The posting gate, rebuilt.
--
-- Phase 1M-A refused every Schedule H line. Now a Schedule H line may post exactly when it carries
-- a dispensing — which, by the triggers above, means a prescription item for that very product,
-- within its quantity and occasions, supervised by a registered pharmacist. Schedule H1, X, C and
-- C(1) are refused as before, whatever else the line carries, and a medicine of unknown position is
-- refused as before.
-- ---------------------------------------------------------------------------------------------
DROP TRIGGER sale_documents_regulatory_gate_update;
DROP TRIGGER sale_documents_regulatory_gate_insert;

CREATE TRIGGER sale_documents_regulatory_gate_update
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     OR (json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
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

CREATE TRIGGER sale_documents_regulatory_gate_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
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

-- A line pointing at a prescription item must BE dispensed against it by the time it posts, so a
-- link can never sit on a posted line as decoration.
CREATE TRIGGER sale_documents_prescription_link_dispensed
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    WHERE line.sale_document_id = NEW.id
      AND line.prescription_item_id IS NOT NULL
      AND NOT EXISTS (
          SELECT 1 FROM prescription_dispensings dispensing
          WHERE dispensing.sale_line_id = line.id
            AND dispensing.prescription_item_id = line.prescription_item_id
      )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_link_without_dispensing');
END;

-- ---------------------------------------------------------------------------------------------
-- 8a. The rule 65(3)(1) record of the supply.
--
-- Rule 65(3)(1): the supply of a drug on a prescription "shall be recorded at the time of supply in
-- a prescription register specially maintained for the purpose and the serial number of entry in
-- the register shall be entered on the prescription", with (a) the serial number of the entry,
-- (b) the date of supply, (c) the prescriber's name and address, (d) the patient's, or the animal
-- owner's, name and address, (e) the drug and the quantity, (f) for Schedule H the manufacturer,
-- batch number and date of expiry of potency, if any, and (g) the signature of the registered
-- pharmacist. Its first proviso lets particulars (a)-(g) go in a serially numbered cash or credit
-- memo book specially maintained for the purpose, for drugs not compounded in the premises and
-- supplied from or in the original containers; rule 65(3)(2) makes the choice between the two a
-- written election, recorded in Phase 1M-A as `store_record_elections`.
--
-- WHAT THIS TABLE IS, AND IS NOT. Nothing in the Drugs Rules provides for keeping the rule 65(3)
-- register electronically (Schedule M and Schedule L-I provide for electronic records; rule 65 does
-- not). So this is AUSHADHARTH's working record of the entry: it prepares the particulars and
-- allocates the serial. The statutory record is the physical register leaf or memo the pharmacy
-- keeps, signed by the registered pharmacist's own hand. The software never signs anything.
--
-- SIGNATURE BEFORE SUPPLY. Particular (g) is part of the record made "at the time of supply", and no
-- provision lets it follow later. So a Schedule H Sale may not post until its entry is confirmed:
--
--   prepared   the serial is allocated and the particulars are ready to print. Nothing is sold:
--              no stock moves, no dispensing exists, no tender, no number, no posted Sale.
--   confirmed  a pharmacist or the owner confirms the registered pharmacist signed the physical
--              entry by hand and its serial was written on the prescription.
--   finalized  the Sale posted, in the same transaction, against exactly these particulars.
--   void       cancelled before posting. The serial is kept and never given to another entry,
--              because it may already be on paper. A void entry supports no posting.
--
-- The serial is the next integer of its own book — PR- for the register, PM- for the memo book —
-- counting void entries, so no serial is ever issued twice. A voided serial is a visible, audited
-- gap, not a hidden one. Never the invoice number, never the prescription's RX- reference, never a
-- future H1 or Schedule X register serial.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE prescription_supply_records (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    sale_document_id TEXT NOT NULL REFERENCES sale_documents(id) ON DELETE RESTRICT,
    prescription_id TEXT NOT NULL REFERENCES prescriptions(id) ON DELETE RESTRICT,
    -- The book it is entered in, from the election in force — never chosen at the counter.
    record_method TEXT NOT NULL CHECK (
        record_method IN ('prescription_register', 'cash_or_credit_memo_book')
    ),
    election_id TEXT NOT NULL REFERENCES store_record_elections(id) ON DELETE RESTRICT,
    -- (a) the serial number of the entry.
    serial_value INTEGER NOT NULL CHECK (serial_value >= 1),
    serial_number TEXT NOT NULL,
    -- (b) the date of supply: the Sale's business date.
    date_of_supply TEXT NOT NULL CHECK (date_of_supply GLOB '????-??-??'),
    -- (c) and (d), as the prescription recorded them.
    prescriber_name TEXT NOT NULL CHECK (length(trim(prescriber_name)) > 0),
    prescriber_address TEXT NOT NULL CHECK (length(trim(prescriber_address)) > 0),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('human', 'animal')),
    subject_name TEXT NOT NULL CHECK (length(trim(subject_name)) > 0),
    subject_address TEXT NOT NULL CHECK (length(trim(subject_address)) > 0),
    -- (g) whose signature the physical entry needs. An identity, never a signature.
    supervising_professional_id TEXT NOT NULL REFERENCES store_professionals(id) ON DELETE RESTRICT,
    supervising_professional_name TEXT NOT NULL CHECK (length(trim(supervising_professional_name)) > 0),
    supervising_registration_number TEXT NOT NULL
        CHECK (length(trim(supervising_registration_number)) > 0),
    -- First proviso: the dispensing person's attestation, required for the memo book.
    original_container_confirmed INTEGER NOT NULL CHECK (original_container_confirmed IN (0, 1)),
    -- Second proviso: the entry recording this prescription's previous supply, if any.
    previous_record_id TEXT REFERENCES prescription_supply_records(id) ON DELETE RESTRICT,

    status TEXT NOT NULL DEFAULT 'prepared'
        CHECK (status IN ('prepared', 'confirmed', 'finalized', 'void')),
    prepared_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    prepared_at_utc TEXT NOT NULL CHECK (prepared_at_utc GLOB '????-??-??T??:??:??*Z'),
    -- The two manual acts, confirmed by a person — who is recorded separately from the pharmacist
    -- named in (g) and from whoever later operates the till.
    manual_signature_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (manual_signature_confirmed IN (0, 1)),
    serial_written_on_prescription INTEGER NOT NULL DEFAULT 0
        CHECK (serial_written_on_prescription IN (0, 1)),
    confirmed_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    confirmed_at_utc TEXT CHECK (confirmed_at_utc IS NULL OR confirmed_at_utc GLOB '????-??-??T??:??:??*Z'),
    finalized_at_utc TEXT CHECK (finalized_at_utc IS NULL OR finalized_at_utc GLOB '????-??-??T??:??:??*Z'),
    voided_by_user_id TEXT REFERENCES users(id) ON DELETE RESTRICT,
    voided_at_utc TEXT CHECK (voided_at_utc IS NULL OR voided_at_utc GLOB '????-??-??T??:??:??*Z'),
    void_reason TEXT,

    CHECK (serial_number = (CASE record_method WHEN 'prescription_register' THEN 'PR-' ELSE 'PM-' END)
        || printf('%06d', serial_value)),
    CHECK (record_method <> 'cash_or_credit_memo_book' OR original_container_confirmed = 1),
    -- Both manual acts, together, or neither.
    CHECK ((manual_signature_confirmed = 1) = (serial_written_on_prescription = 1)),
    CHECK ((manual_signature_confirmed = 1) = (confirmed_by_user_id IS NOT NULL)),
    CHECK ((confirmed_by_user_id IS NULL) = (confirmed_at_utc IS NULL)),
    CHECK (status <> 'prepared' OR (manual_signature_confirmed = 0 AND finalized_at_utc IS NULL)),
    CHECK (status NOT IN ('confirmed', 'finalized') OR manual_signature_confirmed = 1),
    CHECK ((status = 'finalized') = (finalized_at_utc IS NOT NULL)),
    CHECK (
        (status = 'void' AND voided_by_user_id IS NOT NULL AND voided_at_utc IS NOT NULL
            AND length(trim(COALESCE(void_reason, ''))) > 0)
        OR (status <> 'void' AND voided_by_user_id IS NULL AND voided_at_utc IS NULL
            AND void_reason IS NULL)
    )
) STRICT;

CREATE UNIQUE INDEX prescription_supply_records_serial_uq
ON prescription_supply_records(store_id, record_method, serial_value);
-- One live entry per prescription per Sale; a voided one may be followed by a new serial.
CREATE UNIQUE INDEX prescription_supply_records_live_uq
ON prescription_supply_records(sale_document_id, prescription_id) WHERE status <> 'void';
CREATE INDEX prescription_supply_records_prescription_idx
ON prescription_supply_records(prescription_id, serial_value);
CREATE INDEX prescription_supply_records_sale_idx
ON prescription_supply_records(sale_document_id, status);

-- (e) and (f), one row per Sale line entered. Prepared with the line's facts; linked to its
-- dispensing, and checked against the line as posting froze it, when the Sale is finalized.
CREATE TABLE prescription_supply_record_lines (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    record_id TEXT NOT NULL REFERENCES prescription_supply_records(id) ON DELETE RESTRICT,
    sale_line_id TEXT NOT NULL REFERENCES sale_lines(id) ON DELETE RESTRICT,
    dispensing_id TEXT UNIQUE REFERENCES prescription_dispensings(id) ON DELETE RESTRICT,
    drug_name TEXT NOT NULL CHECK (length(trim(drug_name)) > 0),
    quantity_atoms INTEGER NOT NULL CHECK (quantity_atoms >= 1),
    quantity_unit_label TEXT,
    manufacturer_name TEXT NOT NULL CHECK (length(trim(manufacturer_name)) > 0),
    batch_number TEXT NOT NULL CHECK (length(trim(batch_number)) > 0),
    batch_expires_on TEXT CHECK (batch_expires_on IS NULL OR batch_expires_on GLOB '????-??-??'),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE UNIQUE INDEX prescription_supply_record_lines_line_uq
ON prescription_supply_record_lines(record_id, sale_line_id);
CREATE INDEX prescription_supply_record_lines_sale_line_idx
ON prescription_supply_record_lines(sale_line_id);

-- An entry is prepared only against a draft Sale, from the election actually in force on its date,
-- as the next serial of its own book (void entries included), with the prescription's own
-- particulars and the Sale's own supervising registered pharmacist. Anything else is a forgery.
CREATE TRIGGER prescription_supply_records_coherent_insert
BEFORE INSERT ON prescription_supply_records
WHEN NOT EXISTS (
    SELECT 1 FROM sale_documents document
    JOIN prescriptions prescription ON prescription.id = NEW.prescription_id
    JOIN store_record_elections election ON election.id = NEW.election_id
    JOIN store_professionals professional ON professional.id = NEW.supervising_professional_id
    WHERE document.id = NEW.sale_document_id
      AND document.status = 'draft'
      AND document.store_id = NEW.store_id
      AND document.business_date = NEW.date_of_supply
      AND document.supervising_professional_id = NEW.supervising_professional_id
      AND document.prescription_original_container_confirmed = NEW.original_container_confirmed
      AND prescription.store_id = NEW.store_id
      AND prescription.status = 'active'
      AND prescription.prescriber_name = NEW.prescriber_name
      AND prescription.prescriber_address = NEW.prescriber_address
      AND prescription.subject_kind = NEW.subject_kind
      AND prescription.subject_name = NEW.subject_name
      AND prescription.subject_address = NEW.subject_address
      AND election.store_id = NEW.store_id
      AND election.election = 'rule_65_3_prescription_supply'
      AND election.status = 'active'
      AND election.method = NEW.record_method
      AND election.effective_from <= NEW.date_of_supply
      AND (election.effective_to IS NULL OR election.effective_to > NEW.date_of_supply)
      AND professional.store_id = NEW.store_id
      AND professional.status = 'active'
      AND professional.capacity = 'registered_pharmacist'
      AND professional.full_name = NEW.supervising_professional_name
      AND professional.registration_number = NEW.supervising_registration_number
)
OR NEW.status <> 'prepared'
OR NEW.manual_signature_confirmed <> 0
OR NEW.serial_value <> COALESCE((
    SELECT MAX(serial_value) FROM prescription_supply_records
    WHERE store_id = NEW.store_id AND record_method = NEW.record_method
), 0) + 1
OR (NEW.previous_record_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM prescription_supply_records previous
    WHERE previous.id = NEW.previous_record_id AND previous.prescription_id = NEW.prescription_id
      AND previous.status = 'finalized'
))
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_incoherent');
END;

-- The particulars never change. The status moves only along
--   prepared -> confirmed   (both manual acts confirmed, by a person, once),
--   prepared -> void, confirmed -> void   (before posting, with a reason, serial kept),
--   confirmed -> finalized  (only while its Sale is being posted, every line linked),
-- and finalized and void are final.
CREATE TRIGGER prescription_supply_records_transition
BEFORE UPDATE ON prescription_supply_records
WHEN NOT (
    NEW.id = OLD.id AND NEW.store_id = OLD.store_id
    AND NEW.sale_document_id = OLD.sale_document_id AND NEW.prescription_id = OLD.prescription_id
    AND NEW.record_method = OLD.record_method AND NEW.election_id = OLD.election_id
    AND NEW.serial_value = OLD.serial_value AND NEW.serial_number = OLD.serial_number
    AND NEW.date_of_supply = OLD.date_of_supply
    AND NEW.prescriber_name = OLD.prescriber_name AND NEW.prescriber_address = OLD.prescriber_address
    AND NEW.subject_kind = OLD.subject_kind AND NEW.subject_name = OLD.subject_name
    AND NEW.subject_address = OLD.subject_address
    AND NEW.supervising_professional_id = OLD.supervising_professional_id
    AND NEW.supervising_professional_name = OLD.supervising_professional_name
    AND NEW.supervising_registration_number = OLD.supervising_registration_number
    AND NEW.original_container_confirmed = OLD.original_container_confirmed
    AND NEW.previous_record_id IS OLD.previous_record_id
    AND NEW.prepared_by_user_id = OLD.prepared_by_user_id
    AND NEW.prepared_at_utc = OLD.prepared_at_utc
    AND (
        -- prepared -> confirmed
        (OLD.status = 'prepared' AND NEW.status = 'confirmed'
            AND NEW.manual_signature_confirmed = 1 AND NEW.serial_written_on_prescription = 1
            AND NEW.confirmed_by_user_id IS NOT NULL AND NEW.confirmed_at_utc IS NOT NULL
            AND NEW.finalized_at_utc IS NULL AND NEW.voided_by_user_id IS NULL)
        -- prepared or confirmed -> void, the confirmation (if any) kept as it was
        OR (OLD.status IN ('prepared', 'confirmed') AND NEW.status = 'void'
            AND NEW.manual_signature_confirmed = OLD.manual_signature_confirmed
            AND NEW.serial_written_on_prescription = OLD.serial_written_on_prescription
            AND NEW.confirmed_by_user_id IS OLD.confirmed_by_user_id
            AND NEW.confirmed_at_utc IS OLD.confirmed_at_utc
            AND NEW.finalized_at_utc IS NULL)
        -- confirmed -> finalized, only for a Sale still being posted, with every line linked
        OR (OLD.status = 'confirmed' AND NEW.status = 'finalized'
            AND NEW.manual_signature_confirmed = 1 AND NEW.serial_written_on_prescription = 1
            AND NEW.confirmed_by_user_id = OLD.confirmed_by_user_id
            AND NEW.confirmed_at_utc = OLD.confirmed_at_utc
            AND NEW.voided_by_user_id IS NULL
            AND EXISTS (SELECT 1 FROM sale_documents document
                        WHERE document.id = NEW.sale_document_id AND document.status = 'draft')
            AND EXISTS (SELECT 1 FROM prescription_supply_record_lines line
                        WHERE line.record_id = NEW.id)
            AND NOT EXISTS (SELECT 1 FROM prescription_supply_record_lines line
                            WHERE line.record_id = NEW.id AND line.dispensing_id IS NULL))
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_immutable');
END;

CREATE TRIGGER prescription_supply_records_no_delete
BEFORE DELETE ON prescription_supply_records
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_immutable');
END;

-- A line is added only while its entry is prepared, for a line of the entry's own Sale that is
-- linked to the entry's own prescription.
CREATE TRIGGER prescription_supply_record_lines_coherent_insert
BEFORE INSERT ON prescription_supply_record_lines
WHEN NOT EXISTS (
    SELECT 1 FROM prescription_supply_records record
    JOIN sale_documents document ON document.id = record.sale_document_id
    JOIN sale_lines line ON line.id = NEW.sale_line_id
    JOIN prescription_items item ON item.id = line.prescription_item_id
    WHERE record.id = NEW.record_id
      AND record.status = 'prepared'
      AND document.status = 'draft'
      AND line.sale_document_id = record.sale_document_id
      AND item.prescription_id = record.prescription_id
      AND line.quantity_atoms = NEW.quantity_atoms
)
OR NEW.dispensing_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_incoherent');
END;

-- The only change a line ever takes is its link to the dispensing, once, while its entry is
-- confirmed and its Sale is being posted — and only if the drug, quantity, manufacturer, batch and
-- expiry the entry was signed for are exactly what the Sale line froze.
CREATE TRIGGER prescription_supply_record_lines_link
BEFORE UPDATE ON prescription_supply_record_lines
WHEN NOT (
    OLD.dispensing_id IS NULL AND NEW.dispensing_id IS NOT NULL
    AND NEW.id = OLD.id AND NEW.record_id = OLD.record_id AND NEW.sale_line_id = OLD.sale_line_id
    AND NEW.drug_name = OLD.drug_name AND NEW.quantity_atoms = OLD.quantity_atoms
    AND NEW.quantity_unit_label IS OLD.quantity_unit_label
    AND NEW.manufacturer_name = OLD.manufacturer_name AND NEW.batch_number = OLD.batch_number
    AND NEW.batch_expires_on IS OLD.batch_expires_on AND NEW.created_at_utc = OLD.created_at_utc
    AND EXISTS (
        SELECT 1 FROM prescription_supply_records record
        JOIN sale_documents document ON document.id = record.sale_document_id
        JOIN prescription_dispensings dispensing ON dispensing.id = NEW.dispensing_id
        JOIN sale_lines line ON line.id = NEW.sale_line_id
        WHERE record.id = NEW.record_id
          AND record.status = 'confirmed'
          AND document.status = 'draft'
          AND dispensing.sale_line_id = NEW.sale_line_id
          AND dispensing.prescription_id = record.prescription_id
          AND dispensing.quantity_atoms = NEW.quantity_atoms
          AND line.regulatory_snapshot_version = 1
          AND line.product_display_name = NEW.drug_name
          AND line.manufacturer_name = NEW.manufacturer_name
          AND line.batch_number = NEW.batch_number
          AND line.batch_expires_on IS NEW.batch_expires_on
          AND line.base_unit_label IS NEW.quantity_unit_label
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_immutable');
END;

CREATE TRIGGER prescription_supply_record_lines_no_delete
BEFORE DELETE ON prescription_supply_record_lines
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_immutable');
END;

-- A Sale that dispensed may post only with every dispensing entered in a FINALIZED entry of its
-- own — one confirmed by a person before the supply and finalized with it — and with no entry of
-- its left prepared or confirmed but unused. Direct SQL cannot post a Schedule H supply whose
-- entry was never signed.
CREATE TRIGGER sale_documents_prescription_record_required
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND (
    EXISTS (
        SELECT 1 FROM prescription_dispensings dispensing
        WHERE dispensing.sale_document_id = NEW.id
          AND NOT EXISTS (
              SELECT 1 FROM prescription_supply_record_lines line
              JOIN prescription_supply_records record ON record.id = line.record_id
              WHERE line.dispensing_id = dispensing.id
                AND record.sale_document_id = NEW.id
                AND record.status = 'finalized'
                AND record.manual_signature_confirmed = 1
                AND record.serial_written_on_prescription = 1
          )
    )
    OR EXISTS (
        SELECT 1 FROM prescription_supply_records record
        WHERE record.sale_document_id = NEW.id AND record.status IN ('prepared', 'confirmed')
    )
)
BEGIN
    SELECT RAISE(ABORT, 'prescription_supply_record_missing');
END;


-- ---------------------------------------------------------------------------------------------
-- 9. The audit log learns the new entity types.
--
-- Same recreate-and-copy shape as 0014, 0017 and 0021. Audit payloads for these types carry
-- identifiers and references, never a patient's name or address.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1ma;

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
        'prescription_supply_record'
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
SELECT * FROM master_change_events_phase1ma;

DROP TABLE master_change_events_phase1ma;

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
