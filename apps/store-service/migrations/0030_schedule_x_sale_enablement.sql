-- -------------------------------------------------------------------------------------------------
-- Phase 1M-D3-C2 — Schedule X retail Sale enablement.
--
-- This migration adds NO table, NO column and NO compliance fact. Every fact a supported Schedule X
-- retail supply rests on was already modelled:
--
--   D1-B   the store's Form 20-F authority and its per-product drug coverage;
--   D2     the Schedule X working register entry, its prepared/confirmed/finalized/void lifecycle,
--          the two physical attestations, and the retained duplicate prescription copy;
--   D3-A   the rule 65(11)(c) seller-and-date annotation;
--   D3-B   the exact-lot provenance invariant and the rule 65(3) Schedule X exclusion;
--   D3-C1  the supplier's Schedule X purchase-source authority, per source, per product, per date.
--
-- What was missing was the ability for an already-confirmed working entry to take part in a posting.
-- Two things stood in the way, both of them deliberate D3-B stops:
--
--   1. `dispensing_id`, `bill_number` and `bill_date` were frozen against every UPDATE, while
--      finalization requires `dispensing_id` to be non-NULL — so a supply entry could never be
--      finalized at all. That was the fail-closed guarantee that D3-B could not enable a Sale.
--   2. The three regulatory gate triggers refused any posting touching a Schedule X line outright.
--
-- SQLite cannot ALTER a trigger, so each is dropped and recreated. Every limb that is not about
-- Schedule X is reproduced verbatim: Schedule H, H1, C, C(1), the unknown-position refusal for a
-- medicine, the H1 register requirement, the rule 65(3) requirement and the State axis are all left
-- exactly as they were.
--
-- The three one-time bindings are permitted ONLY on `confirmed -> finalized`, ONLY from NULL, and
-- ONLY for a supply entry. They cannot be written while prepared, on the prepared -> confirmed step,
-- on a void, or ever again afterwards, and no finalized supply entry may leave any of them NULL.
--
-- Where a predicate rests on something outside the database — a cupboard, a signature, a sheet of
-- paper — these triggers verify the RECORDED ATTESTATION AND ITS LINKAGE. They do not pretend the
-- database inspected the physical world.
-- -------------------------------------------------------------------------------------------------


-- -------------------------------------------------------------------------------------------------
-- 1. The document-level regulatory gate, on INSERT.
--
-- A posted `sale_documents` row is never inserted by the service: a Sale is created as a draft and
-- updated to posted, which is what section 2 governs. This trigger exists for the direct-SQL case,
-- so the Schedule X limb here demands the end state of the whole evidence chain — a finalized supply
-- entry for the line, with its dispensing, its bill particulars and both physical attestations.
--
-- That is sufficient rather than merely indicative, because a supply entry can only REACH
-- `finalized` by passing section 5, which requires the confirmed physical attestations, the named
-- supervising registered pharmacist valid on the transaction date, and the three bindings.
-- -------------------------------------------------------------------------------------------------
DROP TRIGGER IF EXISTS sale_documents_regulatory_gate_insert;

CREATE TRIGGER sale_documents_regulatory_gate_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     -- Phase 1M-D3-C2: Schedule X, narrowed from an unconditional refusal.
     OR (json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
         AND (
             json_extract(line.regulatory_schemes_snapshot, '$.ndps_purview')
                 IS NOT 'does_not_apply'
          OR NOT EXISTS (
                 SELECT 1 FROM store_schedule_x_register_entries entry
                 JOIN prescription_dispensings dispensing ON dispensing.id = entry.dispensing_id
                 WHERE entry.sale_document_id = NEW.id
                   AND entry.sale_line_id = line.id
                   AND entry.entry_kind = 'supply'
                   AND entry.status = 'finalized'
                   AND entry.particulars_entered_in_physical_register = 1
                   AND entry.physical_entry_authenticated = 1
                   AND entry.product_id = line.product_id
                   AND entry.quantity_atoms = line.quantity_atoms
                   AND entry.batch_id = line.batch_id
                   AND entry.bill_number IS NOT NULL
                   AND entry.bill_date IS NOT NULL
                   AND dispensing.sale_line_id = line.id)))
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


-- -------------------------------------------------------------------------------------------------
-- 2. The document-level regulatory gate, on the draft -> posted transition.
--
-- This is the live gate every real posting passes through. The Schedule X limb is narrowed exactly
-- the way Phase 1M-C narrowed Schedule H and H1 in migration 0023: from "this scheme applies, so
-- refuse" to "this scheme applies and its dispensing is absent, so refuse". The rest of the Schedule
-- X chain is proved independently by section 6, which raises its own specific error so the counter is
-- told which requirement is unmet rather than being handed a generic gate refusal.
--
-- NDPS stays here as well as in section 6. The intersection is AUSHADHARTH's own unsupported
-- boundary and it must be impossible to reach a posted state without it having been asked.
-- -------------------------------------------------------------------------------------------------
DROP TRIGGER IF EXISTS sale_documents_regulatory_gate_update;

CREATE TRIGGER sale_documents_regulatory_gate_update
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND EXISTS (
    SELECT 1 FROM sale_lines line
    JOIN products product ON product.id = line.product_id
    WHERE line.sale_document_id = NEW.id AND (
        line.regulatory_snapshot_version <> 1
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
     OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
     OR ((json_extract(line.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
          OR json_extract(line.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies')
         AND NOT EXISTS (
             SELECT 1 FROM prescription_dispensings dispensing
             WHERE dispensing.sale_line_id = line.id
         ))
     -- Phase 1M-D3-C2: Schedule X, narrowed on the same pattern, plus the NDPS boundary.
     OR (json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
         AND (
             json_extract(line.regulatory_schemes_snapshot, '$.ndps_purview')
                 IS NOT 'does_not_apply'
          OR NOT EXISTS (
                 SELECT 1 FROM prescription_dispensings dispensing
                 WHERE dispensing.sale_line_id = line.id)))
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


-- -------------------------------------------------------------------------------------------------
-- 3. The line-level gate: no new line may be inserted onto an already posted document.
--
-- Narrowed identically to section 1, and for the same reason: this path is only ever reached by
-- direct SQL, so it demands the finished chain rather than an indication of it.
-- -------------------------------------------------------------------------------------------------
DROP TRIGGER IF EXISTS sale_lines_regulatory_gate_insert;

CREATE TRIGGER sale_lines_regulatory_gate_insert
BEFORE INSERT ON sale_lines
WHEN EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
) AND (
    NEW.regulatory_snapshot_version <> 1
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c') = 'applies'
 OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1') = 'applies'
 -- Phase 1M-D3-C2: Schedule X.
 OR (json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
     AND (
         json_extract(NEW.regulatory_schemes_snapshot, '$.ndps_purview') IS NOT 'does_not_apply'
      OR NOT EXISTS (
             SELECT 1 FROM store_schedule_x_register_entries entry
             JOIN prescription_dispensings dispensing ON dispensing.id = entry.dispensing_id
             WHERE entry.sale_document_id = NEW.sale_document_id
               AND entry.sale_line_id = NEW.id
               AND entry.entry_kind = 'supply'
               AND entry.status = 'finalized'
               AND entry.particulars_entered_in_physical_register = 1
               AND entry.physical_entry_authenticated = 1
               AND entry.product_id = NEW.product_id
               AND entry.quantity_atoms = NEW.quantity_atoms
               AND entry.batch_id = NEW.batch_id
               AND entry.bill_number IS NOT NULL
               AND entry.bill_date IS NOT NULL
               AND dispensing.sale_line_id = NEW.id)))
 OR EXISTS (
        SELECT 1 FROM products product
        WHERE product.id = NEW.product_id AND product.product_kind = 'medicine' AND (
            json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_h1') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_x') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c') = 'unknown'
         OR json_extract(NEW.regulatory_schemes_snapshot, '$.schedule_c1') = 'unknown'))
)
BEGIN
    SELECT RAISE(ABORT, 'regulatory_gate_refuses_posting');
END;


-- -------------------------------------------------------------------------------------------------
-- 4. The working entry's INSERT coherence, with one added clause.
--
-- Everything migration 0026 proved is reproduced unchanged. The addition is the other half of the
-- one-time-binding rule in section 5: a SUPPLY entry is born with `dispensing_id`, `bill_number` and
-- `bill_date` all NULL. A Sale that has not posted has no bill number, and an entry that claimed one
-- in advance would be recording a particular that does not yet exist.
--
-- Receipt entries are untouched: they carry the supplier's own invoice number and date from the
-- posted Purchase, as 0026 requires and verifies below.
-- -------------------------------------------------------------------------------------------------
DROP TRIGGER IF EXISTS store_schedule_x_register_entries_coherent_insert;

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
    -- Phase 1M-D3-C2: the three posting-time bindings start empty on a supply entry.
    AND (
        NEW.entry_kind <> 'supply'
        OR (NEW.dispensing_id IS NULL AND NEW.bill_number IS NULL AND NEW.bill_date IS NULL)
    )
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


-- -------------------------------------------------------------------------------------------------
-- 5. The working entry's transitions, with the three one-time bindings.
--
-- Every field migration 0028 froze stays frozen, and all three transition arms keep every condition
-- they had. `bill_number` and `bill_date` move out of the blanket identity freeze at the top and
-- into the arms, so that each arm can state its own rule rather than one rule covering all of them:
--
--   prepared  -> confirmed   all three stay exactly as they are. Confirmation is a statement about
--                            paper and a signature; it binds nothing about a Sale that has not
--                            posted.
--   confirmed -> finalized   each of the three may go from NULL to a value, once, and only on a
--                            supply entry. A finalized supply entry must have all three.
--   * -> void                all three stay exactly as they are.
--
-- There is no arm whose OLD.status is 'finalized' or 'void', so a finalized entry cannot be touched
-- again and a void one cannot be revived. There is no confirmed -> confirmed arm either, so the
-- bindings cannot be slipped in by an update that leaves the status alone.
-- -------------------------------------------------------------------------------------------------
DROP TRIGGER IF EXISTS store_schedule_x_register_entries_transition;

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
    AND NEW.batch_id IS OLD.batch_id
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
         AND NEW.bill_number IS OLD.bill_number
         AND NEW.bill_date IS OLD.bill_date
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
         -- Phase 1M-D3-C2 — the three one-time bindings. Each either stays exactly as it was, or
         -- goes from NULL to a value on a supply entry. Nothing here can overwrite a value.
         AND (NEW.dispensing_id IS OLD.dispensing_id
              OR (NEW.entry_kind = 'supply'
                  AND OLD.dispensing_id IS NULL AND NEW.dispensing_id IS NOT NULL))
         AND (NEW.bill_number IS OLD.bill_number
              OR (NEW.entry_kind = 'supply'
                  AND OLD.bill_number IS NULL AND NEW.bill_number IS NOT NULL))
         AND (NEW.bill_date IS OLD.bill_date
              OR (NEW.entry_kind = 'supply'
                  AND OLD.bill_date IS NULL AND NEW.bill_date IS NOT NULL))
         -- A finalized supply entry is the rule 65(21)(b) record of a supply that happened. It
         -- carries its dispensing and its bill particulars, or it does not exist.
         AND (NEW.entry_kind <> 'supply'
              OR (NEW.dispensing_id IS NOT NULL
                  AND NEW.bill_number IS NOT NULL
                  AND NEW.bill_date IS NOT NULL))
         -- The dispensing is this entry's own: same Sale line, same prescription, same quantity.
         AND (NEW.entry_kind <> 'supply'
              OR EXISTS (
                  SELECT 1 FROM prescription_dispensings dispensing
                  WHERE dispensing.id = NEW.dispensing_id
                    AND dispensing.store_id = NEW.store_id
                    AND dispensing.sale_line_id = NEW.sale_line_id
                    AND dispensing.sale_document_id = NEW.sale_document_id
                    AND dispensing.prescription_id = NEW.prescription_id
                    AND dispensing.prescription_item_id = NEW.prescription_item_id
                    AND dispensing.product_id = NEW.product_id
                    AND dispensing.quantity_atoms = NEW.quantity_atoms)))
     OR (OLD.status IN ('prepared', 'confirmed') AND NEW.status = 'void'
         AND NEW.particulars_entered_in_physical_register
             = OLD.particulars_entered_in_physical_register
         AND NEW.physical_entry_authenticated = OLD.physical_entry_authenticated
         AND NEW.confirmed_by_user_id IS OLD.confirmed_by_user_id
         AND NEW.confirmed_at_utc IS OLD.confirmed_at_utc
         AND NEW.supervising_professional_id IS OLD.supervising_professional_id
         AND NEW.finalized_at_utc IS NULL
         AND NEW.dispensing_id IS OLD.dispensing_id
         AND NEW.bill_number IS OLD.bill_number
         AND NEW.bill_date IS OLD.bill_date)
    )
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_register_entry_immutable');
END;


-- -------------------------------------------------------------------------------------------------
-- 6. The Schedule X register requirement at posting.
--
-- The database's own verdict on the whole supported chain, independent of any service check. It
-- follows the architecture of `sale_documents_h1_register_required` from migration 0023 — a line
-- limb, a straggler limb and a spurious-entry limb — but it does not borrow H1's semantics: Schedule
-- X needs the exact lot, the duplicate prescription copy, the rule 65(11)(c) annotation, the store's
-- Form 20-F authority with coverage for this very product, and every supplier source authorised.
--
-- Rule 65(21)(a) requires a BOUND, SERIALLY PAGE NUMBERED register and 65(21)(b)(x) a SIGNATURE.
-- Neither is something a trigger can inspect. What is verified here is that the two attestations
-- were recorded, by whom, and against which named registered pharmacist — the linkage, not the act.
-- -------------------------------------------------------------------------------------------------
CREATE TRIGGER sale_documents_schedule_x_register_required
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND (
    -- (a) A Schedule X line whose chain is not complete.
    EXISTS (
        SELECT 1 FROM sale_lines line
        WHERE line.sale_document_id = NEW.id
          AND json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') = 'applies'
          AND (
              -- NDPS is an independent axis and must be established NOT to apply.
              json_extract(line.regulatory_schemes_snapshot, '$.ndps_purview')
                  IS NOT 'does_not_apply'

              -- The finalized working entry, bound to this line's own dispensing.
           OR NOT EXISTS (
                  SELECT 1 FROM store_schedule_x_register_entries entry
                  JOIN prescription_dispensings dispensing ON dispensing.id = entry.dispensing_id
                  WHERE entry.sale_document_id = NEW.id
                    AND entry.sale_line_id = line.id
                    AND entry.store_id = NEW.store_id
                    AND entry.entry_kind = 'supply'
                    AND entry.supply_basis = 'prescription'
                    AND entry.status = 'finalized'
                    AND entry.particulars_entered_in_physical_register = 1
                    AND entry.physical_entry_authenticated = 1
                    -- Rule 65(2), asked again at posting rather than only at confirmation. The
                    -- transition trigger proved the named professional qualified on the entry's
                    -- transaction date when it was confirmed; a registration can lapse, or a person
                    -- can be archived, between the signature and the till.
                    --
                    -- The window matches `domain::prescriptions::professional_valid_on` as the Sale
                    -- path calls it: valid on the business date AND on the store's own day, the
                    -- stricter of the two. The store's day is read the way
                    -- `domain::schedule_x::store_days` reads it — one day for a store keeping Indian
                    -- time, and otherwise a deliberately conservative window a day either side.
                    AND EXISTS (
                        SELECT 1 FROM store_professionals professional
                        WHERE professional.id = entry.supervising_professional_id
                          AND professional.store_id = NEW.store_id
                          AND professional.status = 'active'
                          AND professional.capacity = 'registered_pharmacist'
                          AND professional.registration_number IS NOT NULL
                          AND length(trim(professional.registration_number)) > 0
                          AND (professional.valid_from IS NULL
                               OR professional.valid_from <= min(
                                   NEW.business_date,
                                   (SELECT CASE
                                       WHEN identity.business_time_zone = 'Asia/Kolkata'
                                       THEN strftime('%Y-%m-%d', 'now', '+5 hours', '+30 minutes')
                                       ELSE strftime('%Y-%m-%d', 'now', '-1 day')
                                   END FROM store_identity identity LIMIT 1)))
                          AND (professional.valid_upto IS NULL
                               OR professional.valid_upto >= max(
                                   NEW.business_date,
                                   (SELECT CASE
                                       WHEN identity.business_time_zone = 'Asia/Kolkata'
                                       THEN strftime('%Y-%m-%d', 'now', '+5 hours', '+30 minutes')
                                       ELSE strftime('%Y-%m-%d', 'now', '+1 day')
                                   END FROM store_identity identity LIMIT 1))))
                    AND entry.product_id = line.product_id
                    AND entry.quantity_atoms = line.quantity_atoms
                    AND entry.batch_id = line.batch_id
                    AND entry.bill_number IS NOT NULL
                    AND entry.bill_date IS NOT NULL
                    AND dispensing.sale_line_id = line.id
                    AND dispensing.product_id = line.product_id
                    AND dispensing.quantity_atoms = line.quantity_atoms
                    -- Rule 65(9)(a): the retained duplicate prescription copy, affirmatively.
                    AND EXISTS (
                        SELECT 1 FROM prescription_duplicate_copy_attestations attestation
                        WHERE attestation.store_id = NEW.store_id
                          AND attestation.prescription_id = entry.prescription_id
                          AND attestation.retained_duplicate_prescription_copy_confirmed = 1)
                    -- Rule 65(11)(c): the seller's name and address and the date of dispensing,
                    -- noted on the prescription itself.
                    AND EXISTS (
                        SELECT 1 FROM schedule_x_prescription_annotations annotation
                        WHERE annotation.store_id = NEW.store_id
                          AND annotation.sale_document_id = NEW.id
                          AND annotation.sale_line_id = line.id
                          AND annotation.prescription_id = entry.prescription_id
                          AND annotation.prescription_item_id = entry.prescription_item_id
                          AND annotation.product_id = line.product_id
                          AND annotation.seller_particulars_noted_on_prescription = 1))

              -- Form 20-F: the store's own Schedule X RETAIL authority, covering THIS product on
              -- the Sale's business date. Item 2 of the Form is "Names of drugs", so coverage is
              -- per product and a licence alone is never blanket permission.
           OR NOT EXISTS (
                  SELECT 1 FROM store_compliance_licences licence
                  JOIN store_licence_drug_coverage coverage ON coverage.licence_id = licence.id
                  WHERE licence.store_id = NEW.store_id
                    AND licence.licence_form = 'form_20f'
                    AND licence.status = 'active'
                    AND (licence.valid_from IS NULL OR licence.valid_from <= NEW.business_date)
                    AND (licence.valid_upto IS NULL OR licence.valid_upto >= NEW.business_date)
                    AND coverage.store_id = NEW.store_id
                    AND coverage.product_id = line.product_id
                    AND coverage.status = 'active'
                    AND coverage.effective_from <= NEW.business_date
                    AND (coverage.effective_to IS NULL
                         OR coverage.effective_to > NEW.business_date))

              -- D3-B, re-asserted at posting rather than only at preparation: at least one
              -- qualifying purchase brought this lot in.
           OR NOT EXISTS (
                  SELECT 1 FROM inventory_movements inward
                  JOIN store_schedule_x_register_entries receipt
                    ON receipt.purchase_line_id = inward.purchase_line_id
                  JOIN purchase_documents source ON source.id = receipt.purchase_document_id
                  WHERE inward.store_id = NEW.store_id
                    AND inward.batch_id = line.batch_id
                    AND inward.stock_status = 'sellable'
                    AND inward.quantity_delta_atoms > 0
                    AND inward.movement_type = 'purchase'
                    AND receipt.entry_kind = 'receipt'
                    AND receipt.store_id = NEW.store_id
                    AND receipt.status IN ('confirmed', 'finalized')
                    AND receipt.particulars_entered_in_physical_register = 1
                    AND receipt.physical_entry_authenticated = 1
                    AND source.store_id = NEW.store_id
                    AND source.status = 'posted')

              -- ...and nothing else ever entered the sellable balance of this lot. Opening stock,
              -- an adjustment, a stock count, a disposition transfer and a released sales return
              -- are each a disqualifying inward, because stock inside one lot is fungible.
           OR EXISTS (
                  SELECT 1 FROM inventory_movements inward
                  WHERE inward.store_id = NEW.store_id
                    AND inward.batch_id = line.batch_id
                    AND inward.stock_status = 'sellable'
                    AND inward.quantity_delta_atoms > 0
                    AND NOT (
                        inward.movement_type = 'purchase'
                        AND EXISTS (
                            SELECT 1 FROM store_schedule_x_register_entries receipt
                            JOIN purchase_documents source
                              ON source.id = receipt.purchase_document_id
                            WHERE receipt.entry_kind = 'receipt'
                              AND receipt.purchase_line_id = inward.purchase_line_id
                              AND receipt.store_id = NEW.store_id
                              AND receipt.status IN ('confirmed', 'finalized')
                              AND receipt.particulars_entered_in_physical_register = 1
                              AND receipt.physical_entry_authenticated = 1
                              AND source.store_id = NEW.store_id
                              AND source.status = 'posted')))

              -- D3-C1: EVERY contributing purchase source carried a recorded Schedule X authority
              -- on ITS OWN invoice date, covering this product. One authorised supplier never
              -- legalises another, so this looks for any source that fails and refuses on it.
              --
              -- The two conditions are kept apart deliberately, and in this order, because
              -- `domain::schedule_x::resolve_supplier_schedule_x_authority` resolves them in this
              -- order: it selects every ACTIVE authority covering the day REGARDLESS of its
              -- recorded status, answers `Conflicting` if there is more than one, and only then asks
              -- whether the single one is in force, has a known validity basis and names the drug.
              --
              -- Counting only the authorities that already pass those later tests would make this
              -- trigger MORE PERMISSIVE than the service: a supplier with one in-force covered
              -- authority and one suspended authority over the same dates would be refused by the
              -- resolver as a conflict and accepted here. The database must never be the weaker of
              -- the two.
           OR EXISTS (
                  SELECT 1 FROM inventory_movements inward
                  JOIN purchase_lines pline ON pline.id = inward.purchase_line_id
                  JOIN purchase_documents pdoc ON pdoc.id = pline.purchase_document_id
                  WHERE inward.store_id = NEW.store_id
                    AND inward.batch_id = line.batch_id
                    AND inward.stock_status = 'sellable'
                    AND inward.quantity_delta_atoms > 0
                    AND inward.movement_type = 'purchase'
                    AND pdoc.status = 'posted'
                    AND (
                        -- Exactly one active authority may cover the invoice date. Two is a
                        -- conflict, and a conflict is not an establishment.
                        (
                            SELECT COUNT(*) FROM supplier_schedule_x_authorities authority
                            WHERE authority.store_id = NEW.store_id
                              AND authority.supplier_party_id = pdoc.supplier_party_id
                              AND authority.status = 'active'
                              AND authority.effective_from <= pdoc.invoice_date
                              AND (authority.effective_to IS NULL
                                   OR authority.effective_to > pdoc.invoice_date)
                        ) <> 1
                        -- ...and that one must be in force, with a known validity basis, naming
                        -- this drug on exactly one active coverage row over the same date.
                        OR NOT EXISTS (
                            SELECT 1 FROM supplier_schedule_x_authorities authority
                            WHERE authority.store_id = NEW.store_id
                              AND authority.supplier_party_id = pdoc.supplier_party_id
                              AND authority.status = 'active'
                              AND authority.effective_from <= pdoc.invoice_date
                              AND (authority.effective_to IS NULL
                                   OR authority.effective_to > pdoc.invoice_date)
                              AND authority.legal_status = 'in_force'
                              AND authority.validity_basis IN ('fixed_term', 'perpetual')
                              AND (
                                  SELECT COUNT(*)
                                  FROM supplier_schedule_x_authority_coverage coverage
                                  WHERE coverage.authority_id = authority.id
                                    AND coverage.store_id = NEW.store_id
                                    AND coverage.product_id = line.product_id
                                    AND coverage.status = 'active'
                                    AND coverage.effective_from <= pdoc.invoice_date
                                    AND (coverage.effective_to IS NULL
                                         OR coverage.effective_to > pdoc.invoice_date)
                              ) = 1)
                    ))
          )
    )

    -- (b) No straggler. An entry left prepared or confirmed means the paper and the software do not
    -- agree about what was supplied, so the posting waits.
    OR EXISTS (
        SELECT 1 FROM store_schedule_x_register_entries entry
        WHERE entry.sale_document_id = NEW.id AND entry.status IN ('prepared', 'confirmed')
    )

    -- (c) No spurious entry. A finalized supply entry against a line that is not Schedule X would
    -- be a register record of something the register does not govern.
    OR EXISTS (
        SELECT 1 FROM store_schedule_x_register_entries entry
        JOIN sale_lines line ON line.id = entry.sale_line_id
        WHERE entry.sale_document_id = NEW.id AND entry.status = 'finalized'
          AND json_extract(line.regulatory_schemes_snapshot, '$.schedule_x') IS NOT 'applies'
    )
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_register_entry_missing');
END;
