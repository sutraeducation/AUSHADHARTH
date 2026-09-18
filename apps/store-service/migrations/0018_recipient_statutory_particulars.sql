-- Phase 1L-A2 — recipient statutory particulars and an immutable recipient address snapshot.
--
-- Rule 46 of the CGST Rules says what a tax invoice must show about the person it is issued to:
--
--   (d) a REGISTERED recipient: name, address and GSTIN — at any value;
--   (e) an UNREGISTERED recipient, where the value of the TAXABLE supply is fifty thousand rupees or
--       more: name, address, address of delivery, and the name of the State with its code;
--   (f) the same particulars below that value, when the recipient asks for them to be recorded.
--
-- Phase 1H froze the recipient's name, registration status, GSTIN and State code. It froze no
-- address, so none of those three documents could be produced. These columns close that gap and
-- nothing more.
--
-- Every column is additive. Nothing here is backfilled: a Sale posted before this migration keeps
-- `recipient_snapshot_version = 0` and NULL in every new column, because nobody recorded where its
-- customer lived and inventing it now — least of all from today's Party record — would put a false
-- address on a historical document. The two flags below are deliberately NULLABLE for the same
-- reason: a default of "not requested" or "delivered to the same address" would be a claim about an
-- old Sale that nobody ever made.
-- ---------------------------------------------------------------------------------------------

-- 0 = issued before recipient particulars were evaluated (unknown, not "none");
-- 1 = every recipient fact below is the fact as it stood at posting, and an absent address means no
--     rule required one and none was recorded.
ALTER TABLE sale_documents ADD COLUMN recipient_snapshot_version INTEGER NOT NULL DEFAULT 0
    CHECK (recipient_snapshot_version IN (0, 1));

-- Rule 46(f): the operator records that the customer ASKED for their details on the invoice. It is
-- never inferred from a name being typed or a party being chosen.
ALTER TABLE sale_documents ADD COLUMN recipient_particulars_requested INTEGER
    CHECK (recipient_particulars_requested IS NULL OR recipient_particulars_requested IN (0, 1));

-- Where the recipient address came from. 'party' is the customer's own record, read at posting;
-- 'counter' is what was typed at the counter for a walk-in who has no record and wants none.
ALTER TABLE sale_documents ADD COLUMN recipient_address_source TEXT
    CHECK (recipient_address_source IS NULL OR recipient_address_source IN ('party', 'counter'));

-- On a walk-in DRAFT these hold what the counter typed. On a POSTED Sale they are the snapshot.
-- Bounds mirror `party_addresses`, so the same address fits in both places.
ALTER TABLE sale_documents ADD COLUMN recipient_address_line1 TEXT
    CHECK (recipient_address_line1 IS NULL OR length(trim(recipient_address_line1)) BETWEEN 1 AND 200);
ALTER TABLE sale_documents ADD COLUMN recipient_address_line2 TEXT
    CHECK (recipient_address_line2 IS NULL OR length(trim(recipient_address_line2)) BETWEEN 1 AND 200);
ALTER TABLE sale_documents ADD COLUMN recipient_city TEXT
    CHECK (recipient_city IS NULL OR length(trim(recipient_city)) BETWEEN 1 AND 100);
ALTER TABLE sale_documents ADD COLUMN recipient_postal_code TEXT
    CHECK (recipient_postal_code IS NULL OR (
        length(recipient_postal_code) BETWEEN 3 AND 16
        AND recipient_postal_code NOT GLOB '*[^0-9A-Z -]*'
    ));
ALTER TABLE sale_documents ADD COLUMN recipient_state_id TEXT
    REFERENCES state_codes(id) ON DELETE RESTRICT;
-- The State as printed, frozen as text so a posted Sale never joins `state_codes` to render itself.
-- Distinct from `customer_state_code`, which is the customer's place of supply from Phase 1H: for a
-- registered customer the two usually agree, but no rule says they must, and neither is derived
-- from the other.
ALTER TABLE sale_documents ADD COLUMN recipient_state_name TEXT;
ALTER TABLE sale_documents ADD COLUMN recipient_state_code TEXT;

-- Rule 46(e)/(f) "address of delivery". Not a logistics model: one flag and, only when the goods
-- go somewhere other than the recipient's address, that one address.
ALTER TABLE sale_documents ADD COLUMN delivery_same_as_recipient INTEGER
    CHECK (delivery_same_as_recipient IS NULL OR delivery_same_as_recipient IN (0, 1));
ALTER TABLE sale_documents ADD COLUMN delivery_address_line1 TEXT
    CHECK (delivery_address_line1 IS NULL OR length(trim(delivery_address_line1)) BETWEEN 1 AND 200);
ALTER TABLE sale_documents ADD COLUMN delivery_address_line2 TEXT
    CHECK (delivery_address_line2 IS NULL OR length(trim(delivery_address_line2)) BETWEEN 1 AND 200);
ALTER TABLE sale_documents ADD COLUMN delivery_city TEXT
    CHECK (delivery_city IS NULL OR length(trim(delivery_city)) BETWEEN 1 AND 100);
ALTER TABLE sale_documents ADD COLUMN delivery_postal_code TEXT
    CHECK (delivery_postal_code IS NULL OR (
        length(delivery_postal_code) BETWEEN 3 AND 16
        AND delivery_postal_code NOT GLOB '*[^0-9A-Z -]*'
    ));
ALTER TABLE sale_documents ADD COLUMN delivery_state_id TEXT
    REFERENCES state_codes(id) ON DELETE RESTRICT;
ALTER TABLE sale_documents ADD COLUMN delivery_state_name TEXT;
ALTER TABLE sale_documents ADD COLUMN delivery_state_code TEXT;

-- ---------------------------------------------------------------------------------------------
-- Draft integrity.
--
-- A customer with a record supplies their address from that record, at posting; the counter may
-- not type a second one beside it, which would be a second customer master by the back door. A
-- delivery address exists only when the operator has said delivery is somewhere else. And the
-- snapshot-only columns stay empty until posting writes them.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER sale_documents_recipient_draft_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'draft' AND (
    (NEW.customer_party_id IS NOT NULL AND (
        NEW.recipient_address_line1 IS NOT NULL OR NEW.recipient_address_line2 IS NOT NULL
        OR NEW.recipient_city IS NOT NULL OR NEW.recipient_postal_code IS NOT NULL
        OR NEW.recipient_state_id IS NOT NULL))
 OR (COALESCE(NEW.delivery_same_as_recipient, 1) = 1 AND (
        NEW.delivery_address_line1 IS NOT NULL OR NEW.delivery_address_line2 IS NOT NULL
        OR NEW.delivery_city IS NOT NULL OR NEW.delivery_postal_code IS NOT NULL
        OR NEW.delivery_state_id IS NOT NULL))
 OR NEW.recipient_address_source IS NOT NULL
 OR NEW.recipient_state_name IS NOT NULL OR NEW.recipient_state_code IS NOT NULL
 OR NEW.delivery_state_name IS NOT NULL OR NEW.delivery_state_code IS NOT NULL
 OR NEW.recipient_snapshot_version <> 0
)
BEGIN
    SELECT RAISE(ABORT, 'recipient_draft_conflict');
END;

CREATE TRIGGER sale_documents_recipient_draft_update
BEFORE UPDATE ON sale_documents
WHEN OLD.status = 'draft' AND NEW.status = 'draft' AND (
    (NEW.customer_party_id IS NOT NULL AND (
        NEW.recipient_address_line1 IS NOT NULL OR NEW.recipient_address_line2 IS NOT NULL
        OR NEW.recipient_city IS NOT NULL OR NEW.recipient_postal_code IS NOT NULL
        OR NEW.recipient_state_id IS NOT NULL))
 OR (COALESCE(NEW.delivery_same_as_recipient, 1) = 1 AND (
        NEW.delivery_address_line1 IS NOT NULL OR NEW.delivery_address_line2 IS NOT NULL
        OR NEW.delivery_city IS NOT NULL OR NEW.delivery_postal_code IS NOT NULL
        OR NEW.delivery_state_id IS NOT NULL))
 OR NEW.recipient_address_source IS NOT NULL
 OR NEW.recipient_state_name IS NOT NULL OR NEW.recipient_state_code IS NOT NULL
 OR NEW.delivery_state_name IS NOT NULL OR NEW.delivery_state_code IS NOT NULL
 OR NEW.recipient_snapshot_version <> 0
)
BEGIN
    SELECT RAISE(ABORT, 'recipient_draft_conflict');
END;

-- ---------------------------------------------------------------------------------------------
-- Snapshot completeness.
--
-- A version-1 snapshot must be internally coherent AND must carry the particulars Rule 46 requires
-- for the recipient it names. The service refuses first, with a list of what is missing; this is
-- the database refusing to hold a document that would claim otherwise, whoever wrote it.
--
-- The rule is written out rather than summarised:
--   * the request flag is recorded (never NULL on a version-1 Sale); the delivery flag is recorded
--     exactly where Rule 46(e)/(f) asks for an address of delivery, and NULL means none was asked;
--   * an address source exists exactly when an address line exists, and matches whether a party is
--     named; an address State is frozen as id, name AND code, or not at all;
--   * delivery unrecorded or "same as recipient" carries no delivery address; "elsewhere" carries a complete one
--     beside a recipient address;
--   * Rule 46(d): a registered recipient has a name, a GSTIN and an address;
--   * Rule 46(e)/(f): an unregistered recipient whose TAXABLE lines are worth >= 5,000,000 paise,
--     or one who asked, has a name, an address and a State.
--
-- "Taxable lines" is deliberate. The header `taxable_value_paise` is NOT the value of the taxable
-- supply in this repository: the line computation records the whole value of an exempt, nil-rated
-- or non-GST line in that line's `taxable_value_paise` too, so the header sums the entire basket.
-- The threshold therefore reads the lines, filtered on their frozen treatment. Posting writes every
-- line's treatment before it writes this header, so the lines are final when the trigger reads them.
--
-- Immutability after posting needs no new trigger: the frozen `sale_documents_posted_no_update`
-- already refuses every UPDATE of a posted row, and these columns inherit it.
-- ---------------------------------------------------------------------------------------------
CREATE TRIGGER sale_documents_recipient_snapshot_insert
BEFORE INSERT ON sale_documents
WHEN NEW.recipient_snapshot_version >= 1 AND (
    NEW.recipient_particulars_requested IS NULL
 OR ((NEW.recipient_address_source IS NULL) <> (NEW.recipient_address_line1 IS NULL))
 OR (NEW.recipient_address_source = 'party' AND NEW.customer_party_id IS NULL)
 OR (NEW.recipient_address_source = 'counter' AND NEW.customer_party_id IS NOT NULL)
 OR ((NEW.recipient_state_id IS NULL) <> (NEW.recipient_state_code IS NULL))
 OR ((NEW.recipient_state_code IS NULL) <> (NEW.recipient_state_name IS NULL))
 OR (NEW.recipient_address_line1 IS NULL AND (
        NEW.recipient_address_line2 IS NOT NULL OR NEW.recipient_city IS NOT NULL
        OR NEW.recipient_postal_code IS NOT NULL OR NEW.recipient_state_id IS NOT NULL))
 OR (COALESCE(NEW.delivery_same_as_recipient, 1) = 1 AND (
        NEW.delivery_address_line1 IS NOT NULL OR NEW.delivery_address_line2 IS NOT NULL
        OR NEW.delivery_city IS NOT NULL OR NEW.delivery_postal_code IS NOT NULL
        OR NEW.delivery_state_id IS NOT NULL OR NEW.delivery_state_name IS NOT NULL
        OR NEW.delivery_state_code IS NOT NULL))
 OR (NEW.delivery_same_as_recipient = 0 AND (
        NEW.recipient_address_line1 IS NULL
        OR NEW.delivery_address_line1 IS NULL
        OR NEW.delivery_state_id IS NULL OR NEW.delivery_state_name IS NULL
        OR NEW.delivery_state_code IS NULL))
 OR (NEW.customer_gst_registration_status = 'registered' AND (
        NEW.customer_display_name IS NULL OR length(trim(NEW.customer_display_name)) = 0
        OR NEW.customer_normalized_gstin IS NULL
        OR NEW.recipient_address_line1 IS NULL))
 OR (COALESCE(NEW.customer_gst_registration_status, '') <> 'registered'
     AND ((SELECT COALESCE(SUM(line.taxable_value_paise), 0) FROM sale_lines line
           WHERE line.sale_document_id = NEW.id AND line.tax_treatment_kind = 'taxable') >= 5000000
          OR NEW.recipient_particulars_requested = 1)
     AND (
        length(trim(COALESCE(NEW.customer_display_name, NEW.customer_name_text, ''))) = 0
        OR NEW.recipient_address_line1 IS NULL
        OR NEW.recipient_state_code IS NULL
        OR NEW.delivery_same_as_recipient IS NULL))
)
BEGIN
    SELECT RAISE(ABORT, 'recipient_snapshot_incomplete');
END;

CREATE TRIGGER sale_documents_recipient_snapshot_update
BEFORE UPDATE ON sale_documents
WHEN NEW.recipient_snapshot_version >= 1 AND (
    NEW.recipient_particulars_requested IS NULL
 OR ((NEW.recipient_address_source IS NULL) <> (NEW.recipient_address_line1 IS NULL))
 OR (NEW.recipient_address_source = 'party' AND NEW.customer_party_id IS NULL)
 OR (NEW.recipient_address_source = 'counter' AND NEW.customer_party_id IS NOT NULL)
 OR ((NEW.recipient_state_id IS NULL) <> (NEW.recipient_state_code IS NULL))
 OR ((NEW.recipient_state_code IS NULL) <> (NEW.recipient_state_name IS NULL))
 OR (NEW.recipient_address_line1 IS NULL AND (
        NEW.recipient_address_line2 IS NOT NULL OR NEW.recipient_city IS NOT NULL
        OR NEW.recipient_postal_code IS NOT NULL OR NEW.recipient_state_id IS NOT NULL))
 OR (COALESCE(NEW.delivery_same_as_recipient, 1) = 1 AND (
        NEW.delivery_address_line1 IS NOT NULL OR NEW.delivery_address_line2 IS NOT NULL
        OR NEW.delivery_city IS NOT NULL OR NEW.delivery_postal_code IS NOT NULL
        OR NEW.delivery_state_id IS NOT NULL OR NEW.delivery_state_name IS NOT NULL
        OR NEW.delivery_state_code IS NOT NULL))
 OR (NEW.delivery_same_as_recipient = 0 AND (
        NEW.recipient_address_line1 IS NULL
        OR NEW.delivery_address_line1 IS NULL
        OR NEW.delivery_state_id IS NULL OR NEW.delivery_state_name IS NULL
        OR NEW.delivery_state_code IS NULL))
 OR (NEW.customer_gst_registration_status = 'registered' AND (
        NEW.customer_display_name IS NULL OR length(trim(NEW.customer_display_name)) = 0
        OR NEW.customer_normalized_gstin IS NULL
        OR NEW.recipient_address_line1 IS NULL))
 OR (COALESCE(NEW.customer_gst_registration_status, '') <> 'registered'
     AND ((SELECT COALESCE(SUM(line.taxable_value_paise), 0) FROM sale_lines line
           WHERE line.sale_document_id = NEW.id AND line.tax_treatment_kind = 'taxable') >= 5000000
          OR NEW.recipient_particulars_requested = 1)
     AND (
        length(trim(COALESCE(NEW.customer_display_name, NEW.customer_name_text, ''))) = 0
        OR NEW.recipient_address_line1 IS NULL
        OR NEW.recipient_state_code IS NULL
        OR NEW.delivery_same_as_recipient IS NULL))
)
BEGIN
    SELECT RAISE(ABORT, 'recipient_snapshot_incomplete');
END;
