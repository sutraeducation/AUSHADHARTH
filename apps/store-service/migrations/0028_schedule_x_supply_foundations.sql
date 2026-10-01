-- Phase 1M-D3-B — Schedule X lot provenance, supply working-entry wiring, and the rule 65(3)
-- Schedule X exclusion.
--
-- This migration does NOT enable a Schedule X sale. The sale gate is untouched and a Schedule X
-- line is still refused with `schedule_x_workflow_not_available`. What it builds is the last of the
-- data integrity a later slice needs before that gate can responsibly be narrowed.
--
-- WHAT THIS MIGRATION ADDS, AND WHY
--
--   rule 65(3)(1)  "The supply of any drug [other than those specified in Schedule X] on a
--                  prescription of a Registered Medical Practitioner shall be recorded at the time
--                  of supply in a prescription register specially maintained for the purpose..."
--                  (the Schedule X exclusion was inserted by G.S.R. 462(E), dated 22.6.1982.)
--                  -> section 4: the Phase 1M-B posting guard demands that register record for ANY
--                     dispensing on the sale, with no scheme test. For Schedule X the rule excludes
--                     it in its own opening words. The guard is narrowed for Schedule X and for
--                     nothing else.
--
--   rule 65(21)(b)(vi)  "Batch No. or Lot No."
--                  -> section 1: the working entry recorded the batch as TEXT. Batch identity
--                     exists in this database, and a register entry that names a batch by text
--                     alone cannot say WHICH lot left the shelf. A supply entry now carries the
--                     lot itself.
--
--   rule 65(21)(b)(ii)  "Quantity received, if any, the name and address of the supplier and the
--                  number of the relevant license held by the supplier"
--                  -> section 3: the register is a running record of what came in and what went
--                     out. Supplying from a lot whose receipt was never written into the physical
--                     register would leave that register unable to account for the stock. So a
--                     supply working entry may only be prepared for a lot every sellable unit of
--                     which arrived by a posted purchase whose own receipt entry has been written
--                     and authenticated in the bound register.
--
-- WHAT IT DELIBERATELY DOES NOT DO
--
--   * it does not finalize anything. Section 3 makes a FINALIZED supply entry structurally
--     impossible while a Schedule X sale cannot post, so no "completed supply" can exist for a sale
--     that never happened;
--   * it does not touch the Schedule H1 register guard, which is a separate trigger keyed on its
--     own scheme snapshot, so narrowing rule 65(3) for Schedule X cannot suppress it;
--   * it does not broaden `supply_basis`, which stays `'prescription'`: institutional supply under
--     rule 65(9)(b) and wholesale under Form 20-G remain unrepresentable;
--   * it adds no NDPS, Punjab, Schedule H2 or supplier-authority logic. Those are independent
--     overlays and are not Schedule X predicates.
--
-- NOTHING IS BACKFILLED. The new column starts NULL on every existing row, and no existing row
-- acquires provenance it never had.

-- ---------------------------------------------------------------------------------------------
-- 1. The lot itself, not merely its printed number.
--
-- `batch_number` is the text rule 65(21)(b)(vi) asks to be written in the register, and it stays
-- exactly as it was. `batch_id` is this database's identity for the physical lot, and it is what
-- makes provenance checkable: two deliveries can carry the same printed batch number, and a lot is
-- the thing stock actually moves in and out of.
--
-- Nullable, and NULL on every row that exists today. A receipt entry written before this migration
-- said nothing about lot identity and still says nothing; the trigger in section 3 requires the
-- column only where it is about to mean something, which is a SUPPLY entry.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE store_schedule_x_register_entries
    ADD COLUMN batch_id TEXT REFERENCES product_batches(id) ON DELETE RESTRICT;

CREATE INDEX store_schedule_x_register_entries_batch_idx
ON store_schedule_x_register_entries(store_id, batch_id);

-- ---------------------------------------------------------------------------------------------
-- 2. What the existing coherence trigger already proves about a supply entry, and what it does not.
--
-- 0026's `store_schedule_x_register_entries_coherent_insert` already binds a supply entry to its
-- Sale, its line, the product, the quantity, the prescription item, the prescription and its
-- subject, and requires the product to be inside Schedule X on the transaction date. All of that
-- stands unchanged; this migration adds a SECOND trigger rather than rewriting the first, so
-- nothing already proven can be lost while something is added.
--
-- The second trigger adds what 0026 left open, because in 0026 nothing could reach it:
--
--   * the Sale must still be a DRAFT. A supply working entry is prepared before supply happens;
--     one appearing against a posted Sale would be a record written after the fact.
--   * the Sale line must point at the very prescription item the entry names. 0026 checked that the
--     item names the product; it did not check that the LINE was linked to that item.
--   * the prescription must be active.
--   * the entry must name the lot, and it must be the lot the Sale line is taking stock from.
--   * and the lot must be provably ours to supply — section 3.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- 3. THE LOT PROVENANCE INVARIANT.
--
-- A Schedule X supply working entry may be prepared for a lot only when EVERY sellable unit that
-- ever entered that lot, in this store, came in on a posted purchase whose Schedule X receipt
-- working entry has been written into the physical register and authenticated.
--
-- WHY IT IS EXPRESSED AS "NO DISQUALIFYING INWARD MOVEMENT"
--
-- Stock inside one lot is fungible. If fifty units of a lot arrived on a qualifying purchase and
-- fifty arrived as opening stock, nobody can say which fifty are on the counter. So the test is not
-- "does this lot have a qualifying purchase" — it is "did anything enter this lot that is NOT a
-- qualifying purchase". One good delivery never launders a bad one.
--
-- WHICH MOVEMENTS COUNT
--
-- Only movements that ADD to the SELLABLE balance of the lot, because that is the balance a sale
-- draws on (`inventory_movements` CHECK: a sale is negative, sellable, and names a batch). By the
-- table's own constraints the movement types that can produce a positive sellable row are:
--
--   purchase              carries `purchase_line_id`; the only qualifying kind
--   opening_stock         always positive and sellable, never carries a purchase line
--   adjustment            signed, any status, never carries a purchase line
--   stock_count           a counted variance, never carries a purchase line
--   disposition_transfer  a status transfer INTO sellable — which, by the 0015 constraint
--                         `to_status <> 'sellable' OR from_status = 'quarantined'`, is how goods a
--                         customer returned come back. Its provenance is the return, not a purchase
--
-- `sales_return` can never be positive-and-sellable (0015 forces quarantined or non_sellable), so
-- returned goods reach the counter only through the transfer above — and that transfer fails this
-- test. That is deliberate: a Schedule X drug that left the premises and came back cannot have its
-- chain of custody reconstructed from anything this database holds, so it stays unsellable until
-- somebody researches what the law actually requires of it.
--
-- WHAT MAKES A RECEIPT ENTRY QUALIFY
--
-- `confirmed` or `finalized`, never `prepared` and never `void`. 0026's own CHECK
-- (`status NOT IN ('confirmed','finalized') OR particulars_entered_in_physical_register = 1`) means
-- those two statuses are exactly the states in which the physical register acts have been attested.
-- `prepared` means the working record exists and nothing has been written on paper, which is not a
-- register entry. `finalized` is the closure of the software record and carries no statutory
-- meaning of its own, so it is accepted beside `confirmed` rather than demanded instead of it —
-- demanding it would invent a requirement rule 65(21) does not make.
--
-- QUANTITY IS NOT RE-DERIVED HERE. A purchase return reduces the lot's sellable balance without
-- being an inward movement, and the Phase 1H posting path already refuses a sale line that exceeds
-- the sellable balance of its lot. Restating that arithmetic here would be a second opinion that
-- could eventually disagree with the first.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER store_schedule_x_register_entries_supply_provenance
BEFORE INSERT ON store_schedule_x_register_entries
WHEN NEW.entry_kind = 'supply' AND NOT (
    -- The entry names a lot, and it is the lot the Sale line is taking stock from.
    NEW.batch_id IS NOT NULL
    AND EXISTS (
        SELECT 1 FROM sale_documents document
        JOIN sale_lines line ON line.sale_document_id = document.id
        JOIN prescriptions prescription ON prescription.id = NEW.prescription_id
        WHERE document.id = NEW.sale_document_id
          AND document.store_id = NEW.store_id
          AND document.status = 'draft'
          AND line.id = NEW.sale_line_id
          AND line.batch_id = NEW.batch_id
          AND line.prescription_item_id = NEW.prescription_item_id
          AND prescription.status = 'active')
    -- At least one qualifying purchase actually brought this lot in, so a lot with no inward
    -- history at all cannot pass by having nothing to disqualify it.
    AND EXISTS (
        SELECT 1 FROM inventory_movements inward
        JOIN store_schedule_x_register_entries receipt
          ON receipt.purchase_line_id = inward.purchase_line_id
        JOIN purchase_documents source ON source.id = receipt.purchase_document_id
        WHERE inward.store_id = NEW.store_id
          AND inward.batch_id = NEW.batch_id
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
    -- And nothing else ever entered the sellable balance of this lot.
    AND NOT EXISTS (
        SELECT 1 FROM inventory_movements inward
        WHERE inward.store_id = NEW.store_id
          AND inward.batch_id = NEW.batch_id
          AND inward.stock_status = 'sellable'
          AND inward.quantity_delta_atoms > 0
          AND NOT (
              inward.movement_type = 'purchase'
              AND EXISTS (
                  SELECT 1 FROM store_schedule_x_register_entries receipt
                  JOIN purchase_documents source ON source.id = receipt.purchase_document_id
                  WHERE receipt.entry_kind = 'receipt'
                    AND receipt.purchase_line_id = inward.purchase_line_id
                    AND receipt.store_id = NEW.store_id
                    AND receipt.status IN ('confirmed', 'finalized')
                    AND receipt.particulars_entered_in_physical_register = 1
                    AND receipt.physical_entry_authenticated = 1
                    AND source.store_id = NEW.store_id
                    AND source.status = 'posted')))
)
BEGIN
    SELECT RAISE(ABORT, 'schedule_x_supply_lot_provenance_unresolved');
END;


-- ---------------------------------------------------------------------------------------------
-- 4. A supply entry cannot be closed while there is nothing to close it against.
--
-- Rule 65(21)(a) records a supply that HAPPENED. A finalized supply working entry asserting a
-- supply for a Sale that never posted would be a lie in the pharmacy's own records, and a failed
-- Schedule X posting attempt must never be able to leave one behind.
--
-- So a supply entry may reach `finalized` only when it carries the dispensing it records. No
-- transition in this trigger lets `dispensing_id` change, and a dispensing exists only once a Sale
-- has posted — which for Schedule X cannot happen yet. A finalized Schedule X supply entry is
-- therefore structurally unreachable in this phase, which is exactly the intent.
--
-- This one clause is the hand-off point for the slice that enables the sale: that slice widens
-- `NEW.dispensing_id IS OLD.dispensing_id` so the dispensing can be bound in the same transaction
-- that posts the Sale, and this condition then becomes the thing that makes the binding mandatory.
--
-- The trigger text below was generated mechanically from the 0027 trigger. The only change is the
-- single added condition in the confirmed -> finalized branch; every other clause is what 0027
-- wrote, which is itself what 0026 wrote plus the two rule 65(2) date predicates.
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
         AND NEW.dispensing_id IS OLD.dispensing_id
         AND (NEW.entry_kind <> 'supply' OR NEW.dispensing_id IS NOT NULL))
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
-- 5. Rule 65(3)(1): the generic prescription register, which Schedule X is excluded from.
--
-- VERIFIED STATUTORY TEXT (Drugs and Cosmetics Rules, 1945, rule 65(3)(1)):
--
--   "The supply of any drug [other than those specified in Schedule X] on a prescription of a
--    Registered Medical Practitioner shall be recorded at the time of supply in a prescription
--    register specially maintained for the purpose and the serial number of entry in this regard
--    shall be entered on the prescription."
--
-- The words "other than those specified in Schedule X" were inserted by G.S.R. 462(E), dated
-- 22.6.1982 (w.e.f. 22.6.1982). The only later amendment inside rule 65(3) is G.S.R. 588(E) dated
-- 30.8.2013, which substituted "Schedule H and Schedule H1" for "Schedule H" in clause (f) of the
-- particulars — it does not touch the exclusion.
--
-- The Phase 1M-B guard demanded a finalized rule 65(3) record for ANY dispensing on the posting
-- Sale, with no scheme test at all. For Schedule X that demanded a register entry the rule
-- excludes in its own opening words.
--
-- THIS IS NOT "SCHEDULE X NEEDS NO RECORD". Schedule X has its own, stricter controls, none of
-- which this migration touches: the duplicate prescription copy of rule 65(9)(a); the dispensing
-- rules of 65(11) and the no-substitution rule of 65(11A); the note on the prescription required
-- by 65(11)(c); the bound, serially page-numbered register of rule 65(21); personal supervision by
-- a registered pharmacist under 65(2); and the Form 20-F authority of rule 61(3). What changes is
-- only WHICH register the law asks for.
--
-- The narrowing reads the line's own frozen posting-time scheme snapshot, exactly as the Schedule
-- H1 guard does, so the answer is the position on the Sale's own date and cannot be changed by a
-- later classification. The H1 register guard is a SEPARATE trigger on its own snapshot key, so
-- nothing here can suppress it; and the second limb below, which refuses a Sale leaving a prepared
-- or confirmed 65(3) record dangling, is untouched.
--
-- The trigger text below was generated mechanically from the 0022 trigger; the only change is the
-- added exclusion in the first limb.
-- ---------------------------------------------------------------------------------------------
DROP TRIGGER sale_documents_prescription_record_required;

CREATE TRIGGER sale_documents_prescription_record_required
BEFORE UPDATE ON sale_documents
WHEN NEW.status = 'posted' AND OLD.status <> 'posted' AND (
    EXISTS (
        SELECT 1 FROM prescription_dispensings dispensing
        WHERE dispensing.sale_document_id = NEW.id
          -- Rule 65(3)(1) excludes Schedule X in its own opening words, so a Schedule X
          -- dispensing is not held to the generic prescription register. Read from the line's
          -- own frozen posting-time snapshot, the same way the Schedule H1 guard reads its.
          AND NOT EXISTS (
              SELECT 1 FROM sale_lines excluded
              WHERE excluded.id = dispensing.sale_line_id
                AND json_extract(excluded.regulatory_schemes_snapshot, '$.schedule_x')
                    = 'applies')
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
