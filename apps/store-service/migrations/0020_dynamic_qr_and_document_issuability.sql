-- Phase 1L-A4 — Dynamic B2C QR applicability, the payment cross-reference, and document
-- issuability.
--
-- Three things, each additive:
--
--   1. A Store fact the owner records: whether Notification No. 14/2020-Central Tax (as amended by
--      No. 71/2020-CT) applies to this business. It notifies that a B2C invoice — "a tax invoice
--      issued to an unregistered person" (Circular No. 146/02/2021-GST, S. No. 1) — by a registered
--      person whose aggregate turnover exceeded Rs 500 crore "in any preceding financial year from
--      2017-18 onwards", other than the excluded classes, "shall have Dynamic Quick Response (QR)
--      code". Turnover history and the exclusions live outside this database, so the owner records
--      the answer. It is NOT the Rule 46(s) declaration and NOT Rule 48(4) e-invoicing: three
--      separate columns answer three separate questions, and none is derived from another.
--
--      AUSHADHARTH generates no QR. Every Sale it posts is paid in full at posting (the tender must
--      equal the total), so none is ever paid after issue. For such an invoice, Circular No.
--      146/02/2021-GST (S. Nos. 3 and 5) treats a cross-reference of the payment on the invoice —
--      transaction id, date, time, amount and mode for an electronic payment; amount and date for
--      cash — as compliance. This phase freezes the applicability and requires the facts that
--      cross-reference needs.
--
--   2. The frozen payment facts. Every tender already records its method, amount, optional
--      reference and created_at_utc, written once when posting records the payment; a posted
--      tender cannot be updated or deleted (0013). What was missing is a guard against ADDING a
--      tender to a Sale after it is posted, which would forge a cross-reference. That guard is
--      added here. Nothing is backfilled: historical references stay as recorded, NULL included.
--
--   3. A registered seller's Sale to a REGISTERED recipient that mixes taxable and untaxed supplies
--      is refused before it posts. Rule 46A's invoice-cum-bill of supply covers supplies to an
--      UNREGISTERED person only, so the document such a Sale needs is not one AUSHADHARTH can issue
--      today. Existing posted Sales are untouched: a posted row cannot change status again.
-- ---------------------------------------------------------------------------------------------

-- 'unknown'      : not recorded. A registered Store's B2C tax invoice or invoice-cum-bill of supply
--                  cannot post in this state. No other document depends on it.
-- 'not_required' : the business is outside the notification (turnover, or an excluded class).
-- 'required'     : it is inside. Each such invoice records the payment cross-reference, and a card
--                  or UPI payment must carry its transaction reference.
ALTER TABLE store_identity ADD COLUMN dynamic_qr_applicability TEXT NOT NULL DEFAULT 'unknown'
    CHECK (dynamic_qr_applicability IN ('unknown', 'not_required', 'required'));

-- 0 = posted before this fact existed: its applicability is UNKNOWN, not "not required".
-- 1 = resolved at posting. dynamic_qr_applicability_snapshot is then set exactly when the
--     document is treated as within the notification's reach — registered seller, recipient not
--     registered, at least one taxable line — and NULL on every other document ("cannot apply",
--     never "not required"). That includes a Rule 46A invoice-cum-bill of supply: no primary
--     source settles whether the notification reaches it, so it is included as a conservative
--     product decision, not as a legal conclusion.
ALTER TABLE sale_documents ADD COLUMN dynamic_qr_snapshot_version INTEGER NOT NULL DEFAULT 0
    CHECK (dynamic_qr_snapshot_version IN (0, 1));
ALTER TABLE sale_documents ADD COLUMN dynamic_qr_applicability_snapshot TEXT
    CHECK (dynamic_qr_applicability_snapshot IS NULL
        OR dynamic_qr_applicability_snapshot IN ('not_required', 'required'));

-- A draft carries neither; posting writes them once.
CREATE TRIGGER sale_documents_dynamic_qr_draft_only
BEFORE UPDATE ON sale_documents
WHEN OLD.status = 'draft' AND NEW.status = 'draft' AND (
    NEW.dynamic_qr_snapshot_version <> 0 OR NEW.dynamic_qr_applicability_snapshot IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'dynamic_qr_snapshot_before_posting');
END;

CREATE TRIGGER sale_documents_dynamic_qr_draft_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'draft' AND (
    NEW.dynamic_qr_snapshot_version <> 0 OR NEW.dynamic_qr_applicability_snapshot IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'dynamic_qr_snapshot_before_posting');
END;

-- A version-1 snapshot is coherent, whoever writes it:
--   * it sits on a document whose compliance facts were also resolved (compliance version 1);
--   * the applicability is recorded exactly when the document is treated as within the
--     notification's reach (see the column note above);
--   * under 'required', no card or UPI tender lacks its transaction reference.
-- Posting writes lines and tenders before the header, so both are final when this reads them.
-- Immutability after posting needs no new trigger: sale_documents_posted_no_update already
-- refuses every UPDATE of a posted row.
CREATE TRIGGER sale_documents_dynamic_qr_snapshot_update
BEFORE UPDATE ON sale_documents
WHEN NEW.dynamic_qr_snapshot_version >= 1 AND (
    NEW.compliance_snapshot_version < 1
 OR ((NEW.store_gst_registration_status = 'registered'
      AND COALESCE(NEW.customer_gst_registration_status, '') <> 'registered'
      AND EXISTS (SELECT 1 FROM sale_lines line
                  WHERE line.sale_document_id = NEW.id AND line.tax_treatment_kind = 'taxable'))
     <> (NEW.dynamic_qr_applicability_snapshot IS NOT NULL))
 OR (NEW.dynamic_qr_applicability_snapshot = 'required' AND EXISTS (
        SELECT 1 FROM sale_tenders tender
        WHERE tender.sale_document_id = NEW.id AND tender.method IN ('card', 'upi')
          AND (tender.reference_text IS NULL OR length(trim(tender.reference_text)) = 0)))
)
BEGIN
    SELECT RAISE(ABORT, 'dynamic_qr_snapshot_incomplete');
END;

-- A directly inserted version-1 row can be judged only on its header: its lines and tenders do not
-- exist yet. An applicability recorded for a seller that is not registered, or for a registered
-- recipient, is refused here; the rest is refused by the update trigger above.
CREATE TRIGGER sale_documents_dynamic_qr_snapshot_insert
BEFORE INSERT ON sale_documents
WHEN NEW.dynamic_qr_snapshot_version >= 1 AND (
    NEW.compliance_snapshot_version < 1
 OR (NEW.dynamic_qr_applicability_snapshot IS NOT NULL AND (
        COALESCE(NEW.store_gst_registration_status, '') <> 'registered'
     OR COALESCE(NEW.customer_gst_registration_status, '') = 'registered'))
)
BEGIN
    SELECT RAISE(ABORT, 'dynamic_qr_snapshot_incomplete');
END;

-- A posted Sale's payment evidence is complete: nothing may be added to it afterwards. (Updating
-- or deleting a posted tender is already refused by 0013.)
CREATE TRIGGER sale_tenders_posted_no_insert
BEFORE INSERT ON sale_tenders
WHEN EXISTS (
    SELECT 1 FROM sale_documents WHERE id = NEW.sale_document_id AND status = 'posted'
)
BEGIN
    SELECT RAISE(ABORT, 'sale_document_is_posted');
END;

-- A registered seller cannot post a Sale to a registered recipient that mixes taxable and untaxed
-- supplies: no document AUSHADHARTH issues covers it (see 3 above). Judged on the transition to
-- 'posted', from the lines' frozen treatments, which posting writes before the header.
CREATE TRIGGER sale_documents_registered_mixed_supply_refused
BEFORE UPDATE OF status ON sale_documents
WHEN OLD.status = 'draft' AND NEW.status = 'posted'
 AND NEW.store_gst_registration_status = 'registered'
 AND NEW.customer_gst_registration_status = 'registered'
 AND EXISTS (SELECT 1 FROM sale_lines line
             WHERE line.sale_document_id = NEW.id AND line.tax_treatment_kind = 'taxable')
 AND EXISTS (SELECT 1 FROM sale_lines line
             WHERE line.sale_document_id = NEW.id
               AND line.tax_treatment_kind IN ('exempt', 'nil_rated', 'non_gst'))
BEGIN
    SELECT RAISE(ABORT, 'registered_recipient_mixed_supply');
END;
