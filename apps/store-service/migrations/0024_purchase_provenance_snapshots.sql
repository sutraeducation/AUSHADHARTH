-- Phase 1M-D1-A — immutable purchase provenance.
--
-- A receipt of medicine is the first half of its history. Rule 65(21)(b) of the Drugs Rules, 1945
-- asks a Schedule X register to show, for what came IN: the quantity received, "the name and
-- address of the supplier and the number of the relevant licence held by the supplier", the name
-- of the drug, the manufacturer's name, the batch or lot number, and the bill number and date.
--
-- Phase 1G froze a purchase's commercial identity: who the supplier was, their GSTIN, their place
-- of supply, the invoice number and date, and what was received. It froze no address, no licence,
-- no manufacturer, and neither the drug's name nor the lot's number — those were left to be read
-- back from the Party, Product and Batch masters, every one of which can be edited afterwards. A
-- posted receipt could therefore not reproduce its own provenance, and a later reading of it would
-- be today's masters wearing yesterday's date.
--
-- These columns close that gap and nothing else. This migration does NOT create a Schedule X
-- register, does not touch the Schedule X refusal, and grants no new authority to sell anything:
-- a Schedule X sale is still refused as an unsupported workflow after it, exactly as before.
--
-- Every column is additive and NOTHING is backfilled. A purchase posted before this migration
-- keeps `purchase_provenance_snapshot_version = 0` and NULL in every new column, because nobody
-- recorded these facts at the time and reconstructing them now — least of all from today's
-- masters — would put invented history on a document that has already been acted on. Version 0 is
-- "not captured under this architecture", never "there was nothing to capture".
--
-- Immutability after posting needs no new trigger: `purchase_documents_posted_no_update` and
-- `purchase_lines_posted_no_update` of Phase 1G already refuse EVERY update of a posted row, so
-- these columns inherit that protection — a legacy receipt cannot be dressed up as version 1, and
-- a version-1 receipt cannot have its provenance erased.
-- ---------------------------------------------------------------------------------------------

-- 0 = posted before provenance was captured under this architecture (unknown, never "none");
-- 1 = every provenance fact below is the fact as it stood inside the posting transaction, and an
--     absent fact is recorded as absent rather than left ambiguous.
--
-- The check is open-ended on purpose. Phase 1M-C learned what `IN (0, 1)` costs: widening such a
-- check means rebuilding the table, which is why the State answer went into a column of its own
-- rather than into the central snapshot. A later version number here costs nothing.
ALTER TABLE purchase_documents ADD COLUMN purchase_provenance_snapshot_version INTEGER NOT NULL
    DEFAULT 0 CHECK (purchase_provenance_snapshot_version >= 0);

-- Whether an address was there to freeze. "not_recorded" is a fact about the Party at posting
-- time, and is the only truthful thing to write when the supplier has no address on file. An
-- ordinary purchase still posts in that state; nothing is invented to fill the gap.
ALTER TABLE purchase_documents ADD COLUMN supplier_address_state TEXT CHECK (
    supplier_address_state IS NULL OR supplier_address_state IN ('recorded', 'not_recorded')
);

-- Which Party address this was taken from, kept so the snapshot can be explained later. It is
-- provenance of the provenance: the address text below, not this reference, is what the document
-- says, and editing or archiving the referenced row never changes the text.
ALTER TABLE purchase_documents ADD COLUMN supplier_address_id TEXT
    REFERENCES party_addresses(id) ON DELETE RESTRICT;

ALTER TABLE purchase_documents ADD COLUMN supplier_address_line1 TEXT CHECK (
    supplier_address_line1 IS NULL OR length(trim(supplier_address_line1)) BETWEEN 1 AND 200
);
ALTER TABLE purchase_documents ADD COLUMN supplier_address_line2 TEXT CHECK (
    supplier_address_line2 IS NULL OR length(trim(supplier_address_line2)) BETWEEN 1 AND 200
);
ALTER TABLE purchase_documents ADD COLUMN supplier_address_city TEXT CHECK (
    supplier_address_city IS NULL OR length(trim(supplier_address_city)) BETWEEN 1 AND 100
);
ALTER TABLE purchase_documents ADD COLUMN supplier_address_postal_code TEXT CHECK (
    supplier_address_postal_code IS NULL OR (
        length(supplier_address_postal_code) BETWEEN 3 AND 16
        AND supplier_address_postal_code NOT GLOB '*[^0-9A-Z -]*'
    )
);
ALTER TABLE purchase_documents ADD COLUMN supplier_address_country_code TEXT CHECK (
    supplier_address_country_code IS NULL OR (
        length(supplier_address_country_code) = 2
        AND supplier_address_country_code = upper(supplier_address_country_code)
        AND supplier_address_country_code NOT GLOB '*[^A-Z]*'
    )
);

-- The State of the ADDRESS, which is a different fact from the place of supply already frozen for
-- GST. A tax state is not an address, and rule 65(21)(b)(ii) asks for the address.
ALTER TABLE purchase_documents ADD COLUMN supplier_address_state_id TEXT
    REFERENCES state_codes(id) ON DELETE RESTRICT;
ALTER TABLE purchase_documents ADD COLUMN supplier_address_state_name TEXT CHECK (
    supplier_address_state_name IS NULL OR length(trim(supplier_address_state_name)) > 0
);
ALTER TABLE purchase_documents ADD COLUMN supplier_address_state_code TEXT CHECK (
    supplier_address_state_code IS NULL OR length(trim(supplier_address_state_code)) > 0
);

-- The supplier's drug licence AS RECORDED on the Party, frozen verbatim. The number is not parsed
-- and no form is inferred from it: a string beginning "20B" is a string beginning "20B", not a
-- finding that the supplier holds a Form 20B. This freezes what provenance was supplied; it
-- certifies nothing about the licence's legal validity, and no reading of it may say the supplier
-- is duly licensed.
ALTER TABLE purchase_documents ADD COLUMN supplier_drug_licence_state TEXT CHECK (
    supplier_drug_licence_state IS NULL
    OR supplier_drug_licence_state IN ('recorded', 'not_recorded')
);
ALTER TABLE purchase_documents ADD COLUMN supplier_drug_licence_number TEXT CHECK (
    supplier_drug_licence_number IS NULL
    OR length(trim(supplier_drug_licence_number)) BETWEEN 1 AND 100
);
ALTER TABLE purchase_documents ADD COLUMN supplier_drug_licence_valid_upto TEXT CHECK (
    supplier_drug_licence_valid_upto IS NULL
    OR supplier_drug_licence_valid_upto GLOB '????-??-??'
);

-- ---------------------------------------------------------------------------------------------
-- The line's own three facts: what the drug was called, which lot arrived, and who made it.
-- ---------------------------------------------------------------------------------------------

-- The product's name as it stood at posting. A product may be renamed afterwards for perfectly
-- good reasons; the receipt keeps the name under which the stock was actually received.
ALTER TABLE purchase_lines ADD COLUMN drug_display_name TEXT CHECK (
    drug_display_name IS NULL OR length(trim(drug_display_name)) BETWEEN 1 AND 250
);

-- The lot's number as text, frozen beside `batch_id`. Both are kept: the identity so the stock can
-- be followed, and the text because `product_batches.batch_number` is editable and a corrected
-- master must not silently rewrite what a receipt says arrived.
ALTER TABLE purchase_lines ADD COLUMN batch_number TEXT CHECK (
    batch_number IS NULL OR length(trim(batch_number)) BETWEEN 1 AND 60
);

-- The manufacturer of what physically arrived. A product can have several manufacturers over time,
-- and a marketer or brand owner is not the maker, so this is never guessed: it is the sole
-- manufacturer in force on the receipt's date, or the one the operator chose, or nothing at all.
-- On a draft the company may stand alone as the operator's selection; a posted line carries the
-- pair or neither, and says which through `manufacturer_state`.
ALTER TABLE purchase_lines ADD COLUMN manufacturer_company_id TEXT
    REFERENCES pharmaceutical_companies(id) ON DELETE RESTRICT;
ALTER TABLE purchase_lines ADD COLUMN manufacturer_name TEXT CHECK (
    manufacturer_name IS NULL OR length(trim(manufacturer_name)) BETWEEN 1 AND 250
);
ALTER TABLE purchase_lines ADD COLUMN manufacturer_state TEXT CHECK (
    manufacturer_state IS NULL OR manufacturer_state IN ('recorded', 'not_recorded')
);

-- ---------------------------------------------------------------------------------------------
-- Line shape. These guards hold at every moment of a line's life, draft or posted, so they cannot
-- depend on the document's version: the document is still a draft while its lines are being
-- written, and only becomes version 1 in the same transaction's last statement.
--
-- A selected manufacturer must really be a manufacturer OF THIS PRODUCT. That is what stops a
-- company from another product, or a marketer of this one, being written in as the maker.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER purchase_lines_provenance_shape_insert
BEFORE INSERT ON purchase_lines
WHEN (NEW.manufacturer_state = 'recorded'
      AND (NEW.manufacturer_company_id IS NULL OR NEW.manufacturer_name IS NULL))
  OR (NEW.manufacturer_state = 'not_recorded'
      AND (NEW.manufacturer_company_id IS NOT NULL OR NEW.manufacturer_name IS NOT NULL))
  OR (NEW.manufacturer_name IS NOT NULL AND NEW.manufacturer_state IS NOT 'recorded')
  OR (NEW.batch_number IS NOT NULL AND NEW.batch_id IS NULL)
  OR (NEW.manufacturer_company_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM product_company_roles role
        WHERE role.product_id = NEW.product_id AND role.company_id = NEW.manufacturer_company_id
          AND role.role = 'manufacturer' AND role.status = 'active'))
BEGIN
    SELECT RAISE(ABORT, 'purchase_provenance_incoherent');
END;

CREATE TRIGGER purchase_lines_provenance_shape_update
BEFORE UPDATE ON purchase_lines
WHEN (NEW.manufacturer_state = 'recorded'
      AND (NEW.manufacturer_company_id IS NULL OR NEW.manufacturer_name IS NULL))
  OR (NEW.manufacturer_state = 'not_recorded'
      AND (NEW.manufacturer_company_id IS NOT NULL OR NEW.manufacturer_name IS NOT NULL))
  OR (NEW.manufacturer_name IS NOT NULL AND NEW.manufacturer_state IS NOT 'recorded')
  OR (NEW.batch_number IS NOT NULL AND NEW.batch_id IS NULL)
  OR (NEW.manufacturer_company_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM product_company_roles role
        WHERE role.product_id = NEW.product_id AND role.company_id = NEW.manufacturer_company_id
          AND role.role = 'manufacturer' AND role.status = 'active'))
BEGIN
    SELECT RAISE(ABORT, 'purchase_provenance_incoherent');
END;

-- ---------------------------------------------------------------------------------------------
-- Document shape. Version 1 is claimed only by a posted document whose own provenance is coherent
-- and every one of whose lines carries its three facts. Version 0 must stay empty, so a legacy
-- receipt cannot be given half a provenance.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER purchase_documents_provenance_insert
BEFORE INSERT ON purchase_documents
WHEN (NEW.purchase_provenance_snapshot_version = 0 AND (
        NEW.supplier_address_state IS NOT NULL OR NEW.supplier_address_id IS NOT NULL
     OR NEW.supplier_address_line1 IS NOT NULL OR NEW.supplier_address_line2 IS NOT NULL
     OR NEW.supplier_address_city IS NOT NULL OR NEW.supplier_address_postal_code IS NOT NULL
     OR NEW.supplier_address_country_code IS NOT NULL
     OR NEW.supplier_address_state_id IS NOT NULL OR NEW.supplier_address_state_name IS NOT NULL
     OR NEW.supplier_address_state_code IS NOT NULL
     OR NEW.supplier_drug_licence_state IS NOT NULL
     OR NEW.supplier_drug_licence_number IS NOT NULL
     OR NEW.supplier_drug_licence_valid_upto IS NOT NULL))
  OR (NEW.purchase_provenance_snapshot_version >= 1 AND (
        NEW.status <> 'posted'
     OR NEW.supplier_address_state IS NULL OR NEW.supplier_drug_licence_state IS NULL
     OR (NEW.supplier_address_state = 'recorded' AND (
            NEW.supplier_address_id IS NULL OR NEW.supplier_address_line1 IS NULL
         OR NEW.supplier_address_country_code IS NULL))
     OR (NEW.supplier_address_state = 'not_recorded' AND (
            NEW.supplier_address_id IS NOT NULL OR NEW.supplier_address_line1 IS NOT NULL
         OR NEW.supplier_address_line2 IS NOT NULL OR NEW.supplier_address_city IS NOT NULL
         OR NEW.supplier_address_postal_code IS NOT NULL
         OR NEW.supplier_address_country_code IS NOT NULL
         OR NEW.supplier_address_state_id IS NOT NULL
         OR NEW.supplier_address_state_name IS NOT NULL
         OR NEW.supplier_address_state_code IS NOT NULL))
     OR ((NEW.supplier_address_state_id IS NULL) <> (NEW.supplier_address_state_code IS NULL))
     OR ((NEW.supplier_address_state_code IS NULL) <> (NEW.supplier_address_state_name IS NULL))
     OR (NEW.supplier_drug_licence_state = 'recorded'
         AND NEW.supplier_drug_licence_number IS NULL)
     OR (NEW.supplier_drug_licence_state = 'not_recorded' AND (
            NEW.supplier_drug_licence_number IS NOT NULL
         OR NEW.supplier_drug_licence_valid_upto IS NOT NULL))
     OR EXISTS (
            SELECT 1 FROM purchase_lines line WHERE line.purchase_document_id = NEW.id AND (
                line.drug_display_name IS NULL OR line.manufacturer_state IS NULL
             OR (line.batch_id IS NOT NULL AND line.batch_number IS NULL)))))
BEGIN
    SELECT RAISE(ABORT, 'purchase_provenance_incomplete');
END;

CREATE TRIGGER purchase_documents_provenance_update
BEFORE UPDATE ON purchase_documents
WHEN (NEW.purchase_provenance_snapshot_version = 0 AND (
        NEW.supplier_address_state IS NOT NULL OR NEW.supplier_address_id IS NOT NULL
     OR NEW.supplier_address_line1 IS NOT NULL OR NEW.supplier_address_line2 IS NOT NULL
     OR NEW.supplier_address_city IS NOT NULL OR NEW.supplier_address_postal_code IS NOT NULL
     OR NEW.supplier_address_country_code IS NOT NULL
     OR NEW.supplier_address_state_id IS NOT NULL OR NEW.supplier_address_state_name IS NOT NULL
     OR NEW.supplier_address_state_code IS NOT NULL
     OR NEW.supplier_drug_licence_state IS NOT NULL
     OR NEW.supplier_drug_licence_number IS NOT NULL
     OR NEW.supplier_drug_licence_valid_upto IS NOT NULL))
  OR (NEW.purchase_provenance_snapshot_version >= 1 AND (
        NEW.status <> 'posted'
     OR NEW.supplier_address_state IS NULL OR NEW.supplier_drug_licence_state IS NULL
     OR (NEW.supplier_address_state = 'recorded' AND (
            NEW.supplier_address_id IS NULL OR NEW.supplier_address_line1 IS NULL
         OR NEW.supplier_address_country_code IS NULL))
     OR (NEW.supplier_address_state = 'not_recorded' AND (
            NEW.supplier_address_id IS NOT NULL OR NEW.supplier_address_line1 IS NOT NULL
         OR NEW.supplier_address_line2 IS NOT NULL OR NEW.supplier_address_city IS NOT NULL
         OR NEW.supplier_address_postal_code IS NOT NULL
         OR NEW.supplier_address_country_code IS NOT NULL
         OR NEW.supplier_address_state_id IS NOT NULL
         OR NEW.supplier_address_state_name IS NOT NULL
         OR NEW.supplier_address_state_code IS NOT NULL))
     OR ((NEW.supplier_address_state_id IS NULL) <> (NEW.supplier_address_state_code IS NULL))
     OR ((NEW.supplier_address_state_code IS NULL) <> (NEW.supplier_address_state_name IS NULL))
     OR (NEW.supplier_drug_licence_state = 'recorded'
         AND NEW.supplier_drug_licence_number IS NULL)
     OR (NEW.supplier_drug_licence_state = 'not_recorded' AND (
            NEW.supplier_drug_licence_number IS NOT NULL
         OR NEW.supplier_drug_licence_valid_upto IS NOT NULL))
     OR EXISTS (
            SELECT 1 FROM purchase_lines line WHERE line.purchase_document_id = NEW.id AND (
                line.drug_display_name IS NULL OR line.manufacturer_state IS NULL
             OR (line.batch_id IS NOT NULL AND line.batch_number IS NULL)))))
BEGIN
    SELECT RAISE(ABORT, 'purchase_provenance_incomplete');
END;
