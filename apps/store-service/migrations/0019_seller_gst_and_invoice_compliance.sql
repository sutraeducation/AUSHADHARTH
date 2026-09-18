-- Phase 1L-A3 — seller GST authority and the invoice-compliance facts printing will need.
--
-- Four things are corrected or recorded here, each additive:
--
--   1. The Store's own GST registration now decides whether a Sale charges GST. A pharmacy that
--      is not GST-registered may not collect "any amount by way of tax" (CGST Act s.32(1)); until
--      this phase the tax engine applied the product's GST rate whatever the Store's status.
--      That is corrected in the service. This migration adds the guard that makes a newly posted
--      unregistered-seller Sale unable to carry GST at all.
--
--   2. The same registration now gates the CGST Rule 46 recipient particulars frozen by 0018.
--      Rule 46 prescribes the particulars of a tax invoice issued by a REGISTERED person; an
--      unregistered seller issues none, so Rule 46(d), (e) and (f) cannot be what requires
--      particulars on its retail cash memo. 0018's two recipient triggers are recreated below with
--      that gate and are otherwise unchanged. 0018 itself is not edited.
--
--   3. Facts only the pharmacy can know are recorded as operator-maintained Store facts, never
--      computed from local Sales. Each is a separate legal proposition and has its own column:
--        * whether the Rule 46(s) declaration applies to the Store's non-IRN invoices — Rule 46(s)
--          turns on aggregate turnover "in any preceding financial year from 2017-18 onwards"
--          exceeding the turnover notified under Rule 48(4);
--        * whether the Store is required by Rule 48(4) to issue e-invoices for supplies to
--          registered persons — Notification No. 13/2020-CT, as amended, which also excludes
--          named classes of registered person and can be relaxed by exemption notification;
--        * the aggregate-turnover band that fixes how many HSN digits an invoice must carry
--          (Notification No. 78/2020-Central Tax), a fact about ONE preceding financial year and
--          so recorded together with the financial year of invoices it governs;
--        * which licences the operator designates for printing on the retail drug memo
--          (Drugs Rules r.65(4)(3)(i)(a): "sale licence number of the dealer").
--      The first two are NOT derived from each other anywhere, here or in the service: a
--      high-turnover taxpayer's B2C invoice needs the declaration without being an e-invoice, and
--      an exempted taxpayer can need the declaration on every invoice while issuing none.
--
--   4. Every newly posted Sale freezes the outcome of those facts, so a reprint years later never
--      consults today's Store settings.
--
-- Nothing is backfilled. Every existing Store starts at 'unknown' for every turnover fact, every
-- existing licence starts undesignated, and every existing Sale keeps
-- compliance_snapshot_version = 0 with NULL in each new column: nobody recorded these facts at the
-- time, and inventing them now would put claims on historical documents that nobody made.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- Store facts.
-- ---------------------------------------------------------------------------------------------

-- CGST Rule 46(s), as the owner records it for this business:
-- 'not_applicable' : aggregate turnover has not exceeded the turnover notified under Rule 48(4) in
--                    any financial year from 2017-18 onwards, so no invoice carries the declaration;
-- 'applicable'     : it has, so every invoice NOT issued in the Rule 48(4) manner carries the
--                    declaration. AUSHADHARTH issues no e-invoice, so that is every invoice it
--                    issues for a registered seller;
-- 'unknown'        : not recorded. A registered Store cannot post a Sale in this state.
ALTER TABLE store_identity ADD COLUMN rule46s_declaration_applicability TEXT NOT NULL
    DEFAULT 'unknown'
    CHECK (rule46s_declaration_applicability IN ('unknown', 'not_applicable', 'applicable'));

-- CGST Rule 48(4), as the owner records it for this business's supplies to REGISTERED persons:
-- 'not_required' : the business is not in the class notified under Rule 48(4), or is exempted;
-- 'required'     : it is, so an invoice to a registered person must be an e-invoice. Rule 48(5):
--                  any other invoice "shall not be treated as an invoice". AUSHADHARTH cannot
--                  obtain an IRN, so such a Sale is refused rather than issued;
-- 'unknown'      : not recorded. Only a Sale to a registered person depends on it.
ALTER TABLE store_identity ADD COLUMN einvoice_applicability TEXT NOT NULL DEFAULT 'unknown'
    CHECK (einvoice_applicability IN ('unknown', 'not_required', 'required'));

-- Aggregate turnover in the financial year PRECEDING hsn_turnover_financial_year, which is the
-- year of the invoices the band governs. Notification No. 78/2020-CT: up to Rs 5 crore -> 4
-- digits (and none required on invoices to unregistered persons); more than Rs 5 crore -> 6.
ALTER TABLE store_identity ADD COLUMN hsn_turnover_band TEXT NOT NULL DEFAULT 'unknown'
    CHECK (hsn_turnover_band IN ('unknown', 'up_to_5_crore', 'above_5_crore'));
ALTER TABLE store_identity ADD COLUMN hsn_turnover_financial_year TEXT
    CHECK (hsn_turnover_financial_year IS NULL OR (
        length(hsn_turnover_financial_year) = 7
        AND hsn_turnover_financial_year GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]'
    ));

-- A known band always says which year's invoices it governs, and an unknown one names no year.
CREATE TRIGGER store_identity_hsn_band_integrity_insert
BEFORE INSERT ON store_identity
WHEN (NEW.hsn_turnover_band = 'unknown') <> (NEW.hsn_turnover_financial_year IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'store_invoice_compliance_conflict');
END;

CREATE TRIGGER store_identity_hsn_band_integrity_update
BEFORE UPDATE OF hsn_turnover_band, hsn_turnover_financial_year ON store_identity
WHEN (NEW.hsn_turnover_band = 'unknown') <> (NEW.hsn_turnover_financial_year IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'store_invoice_compliance_conflict');
END;

-- The operator's designation that this licence's number is printed on retail drug memos. A
-- factual designation by the pharmacy, not a legal classification made by the software, and never
-- inferred from the free-text licence_type.
ALTER TABLE store_licences ADD COLUMN include_on_retail_memo INTEGER NOT NULL DEFAULT 0
    CHECK (include_on_retail_memo IN (0, 1));

-- ---------------------------------------------------------------------------------------------
-- Rule 46 recipient particulars: the seller-registration gate.
--
-- 0018's triggers are recreated word for word except that the Rule 46(d) clause and the Rule
-- 46(e)/(f) clause apply only when the frozen seller status is not 'unregistered'. The coherence
-- clauses still apply to every version-1 recipient snapshot. A Sale whose seller status is unknown
-- or absent keeps 0018's behaviour exactly: only a POSITIVELY unregistered seller is released, and
-- only from the particulars a GST tax invoice would need. Existing rows are untouched — triggers
-- act on writes, and a posted row cannot be written.
-- ---------------------------------------------------------------------------------------------
DROP TRIGGER sale_documents_recipient_snapshot_insert;
DROP TRIGGER sale_documents_recipient_snapshot_update;

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
 OR (COALESCE(NEW.store_gst_registration_status, '') <> 'unregistered'
     AND NEW.customer_gst_registration_status = 'registered' AND (
        NEW.customer_display_name IS NULL OR length(trim(NEW.customer_display_name)) = 0
        OR NEW.customer_normalized_gstin IS NULL
        OR NEW.recipient_address_line1 IS NULL))
 OR (COALESCE(NEW.store_gst_registration_status, '') <> 'unregistered'
     AND COALESCE(NEW.customer_gst_registration_status, '') <> 'registered'
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
 OR (COALESCE(NEW.store_gst_registration_status, '') <> 'unregistered'
     AND NEW.customer_gst_registration_status = 'registered' AND (
        NEW.customer_display_name IS NULL OR length(trim(NEW.customer_display_name)) = 0
        OR NEW.customer_normalized_gstin IS NULL
        OR NEW.recipient_address_line1 IS NULL))
 OR (COALESCE(NEW.store_gst_registration_status, '') <> 'unregistered'
     AND COALESCE(NEW.customer_gst_registration_status, '') <> 'registered'
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

-- ---------------------------------------------------------------------------------------------
-- Posted-Sale snapshot.
--
-- 0 = posted before these facts existed: its tax is whatever was frozen then, and none of the
--     facts below is known.
-- 1 = the seller's GST status governed the tax actually charged, and every fact below is the fact
--     as it stood at posting.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_documents ADD COLUMN compliance_snapshot_version INTEGER NOT NULL DEFAULT 0
    CHECK (compliance_snapshot_version IN (0, 1));

-- The designated retail-memo licences, rendered deterministically as the existing
-- seller_licence_text is. NULL when none was designated and none was required.
ALTER TABLE sale_documents ADD COLUMN seller_retail_licence_text TEXT
    CHECK (seller_retail_licence_text IS NULL OR length(trim(seller_retail_licence_text)) > 0);

-- Whether this document, when rendered, carries the Rule 46(s) declaration. NULL for an
-- unregistered seller, which issues no GST document at all.
ALTER TABLE sale_documents ADD COLUMN rule46s_declaration_snapshot TEXT
    CHECK (rule46s_declaration_snapshot IS NULL
        OR rule46s_declaration_snapshot IN ('not_applicable', 'applicable'));

-- The Rule 48(4) determination this document was issued under. Recorded only where the
-- determination was needed — a registered seller's Sale to a registered recipient — and there it
-- can only be 'not_required': a non-IRN document to a registered person from a seller required to
-- e-invoice "shall not be treated as an invoice" (Rule 48(5)), so none is ever posted. NULL
-- everywhere else means "not needed for this document", never "not required".
ALTER TABLE sale_documents ADD COLUMN einvoice_applicability_snapshot TEXT
    CHECK (einvoice_applicability_snapshot IS NULL
        OR einvoice_applicability_snapshot = 'not_required');

-- The HSN policy in force at posting. The band may be 'unknown' only when every line already
-- carried a six-digit numeric HSN, which satisfies any band; hsn_required_digits is then NULL
-- ("not determined") rather than a number nobody established.
ALTER TABLE sale_documents ADD COLUMN hsn_turnover_band_snapshot TEXT
    CHECK (hsn_turnover_band_snapshot IS NULL
        OR hsn_turnover_band_snapshot IN ('unknown', 'up_to_5_crore', 'above_5_crore'));
ALTER TABLE sale_documents ADD COLUMN hsn_turnover_financial_year_snapshot TEXT;
ALTER TABLE sale_documents ADD COLUMN hsn_required_digits INTEGER
    CHECK (hsn_required_digits IS NULL OR hsn_required_digits IN (0, 4, 6));

-- A draft carries no compliance snapshot; posting writes it once.
CREATE TRIGGER sale_documents_compliance_draft_only
BEFORE UPDATE ON sale_documents
WHEN OLD.status = 'draft' AND NEW.status = 'draft' AND (
    NEW.compliance_snapshot_version <> 0
 OR NEW.seller_retail_licence_text IS NOT NULL
 OR NEW.rule46s_declaration_snapshot IS NOT NULL
 OR NEW.einvoice_applicability_snapshot IS NOT NULL
 OR NEW.hsn_turnover_band_snapshot IS NOT NULL
 OR NEW.hsn_turnover_financial_year_snapshot IS NOT NULL
 OR NEW.hsn_required_digits IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'compliance_snapshot_before_posting');
END;

CREATE TRIGGER sale_documents_compliance_draft_insert
BEFORE INSERT ON sale_documents
WHEN NEW.status = 'draft' AND (
    NEW.compliance_snapshot_version <> 0
 OR NEW.seller_retail_licence_text IS NOT NULL
 OR NEW.rule46s_declaration_snapshot IS NOT NULL
 OR NEW.einvoice_applicability_snapshot IS NOT NULL
 OR NEW.hsn_turnover_band_snapshot IS NOT NULL
 OR NEW.hsn_turnover_financial_year_snapshot IS NOT NULL
 OR NEW.hsn_required_digits IS NOT NULL
)
BEGIN
    SELECT RAISE(ABORT, 'compliance_snapshot_before_posting');
END;

-- A version-1 snapshot is coherent, whoever writes it:
--   * the seller's GST status is resolved — never 'unknown';
--   * an UNREGISTERED seller charged no GST: every header and line tax amount and rate is zero,
--     and no GST-document fact is recorded;
--   * a REGISTERED seller recorded a resolved Rule 46(s) fact and an HSN policy; a known band
--     names its year and a digit requirement;
--   * a REGISTERED seller's Sale to a registered recipient recorded the Rule 48(4) determination,
--     and no other Sale records one. The Rule 46(s) fact plays no part in that clause;
--   * a Sale containing a medicine line froze a designated retail-memo licence. Only 'medicine':
--     the catalogue's 'device' kind carries no regulatory classification from which Drugs Rule 65
--     could be shown to govern every device, so the database does not assert that it does.
--
-- Immutability after posting needs no new trigger: the frozen sale_documents_posted_no_update
-- and sale_lines_posted_no_update already refuse every UPDATE of a posted row.
CREATE TRIGGER sale_documents_compliance_snapshot_update
BEFORE UPDATE ON sale_documents
WHEN NEW.compliance_snapshot_version >= 1 AND (
    COALESCE(NEW.store_gst_registration_status, 'unknown') NOT IN ('registered', 'unregistered')
 OR (NEW.store_gst_registration_status = 'unregistered' AND (
        NEW.cgst_paise <> 0 OR NEW.sgst_paise <> 0 OR NEW.igst_paise <> 0 OR NEW.cess_paise <> 0
     OR EXISTS (
            SELECT 1 FROM sale_lines line WHERE line.sale_document_id = NEW.id AND (
                line.cgst_paise <> 0 OR line.sgst_paise <> 0 OR line.igst_paise <> 0
             OR line.cess_paise <> 0 OR line.cgst_basis_points <> 0
             OR line.sgst_basis_points <> 0 OR line.igst_basis_points <> 0
             OR line.cess_basis_points <> 0))
     OR NEW.rule46s_declaration_snapshot IS NOT NULL
     OR NEW.einvoice_applicability_snapshot IS NOT NULL
     OR NEW.hsn_turnover_band_snapshot IS NOT NULL
     OR NEW.hsn_turnover_financial_year_snapshot IS NOT NULL
     OR NEW.hsn_required_digits IS NOT NULL))
 OR (NEW.store_gst_registration_status = 'registered' AND (
        NEW.rule46s_declaration_snapshot IS NULL
     OR NEW.hsn_turnover_band_snapshot IS NULL
     OR ((NEW.hsn_turnover_band_snapshot = 'unknown')
         <> (NEW.hsn_turnover_financial_year_snapshot IS NULL))
     OR ((NEW.hsn_turnover_band_snapshot = 'unknown') <> (NEW.hsn_required_digits IS NULL))
     OR ((COALESCE(NEW.customer_gst_registration_status, '') = 'registered')
         <> (NEW.einvoice_applicability_snapshot IS NOT NULL))))
 OR (NEW.seller_retail_licence_text IS NULL AND EXISTS (
        SELECT 1 FROM sale_lines line JOIN products product ON product.id = line.product_id
        WHERE line.sale_document_id = NEW.id AND product.product_kind = 'medicine'))
)
BEGIN
    SELECT RAISE(ABORT, 'compliance_snapshot_incomplete');
END;

CREATE TRIGGER sale_documents_compliance_snapshot_insert
BEFORE INSERT ON sale_documents
WHEN NEW.compliance_snapshot_version >= 1 AND (
    COALESCE(NEW.store_gst_registration_status, 'unknown') NOT IN ('registered', 'unregistered')
 OR (NEW.store_gst_registration_status = 'unregistered' AND (
        NEW.cgst_paise <> 0 OR NEW.sgst_paise <> 0 OR NEW.igst_paise <> 0 OR NEW.cess_paise <> 0
     OR NEW.rule46s_declaration_snapshot IS NOT NULL
     OR NEW.einvoice_applicability_snapshot IS NOT NULL
     OR NEW.hsn_turnover_band_snapshot IS NOT NULL
     OR NEW.hsn_turnover_financial_year_snapshot IS NOT NULL
     OR NEW.hsn_required_digits IS NOT NULL))
 OR (NEW.store_gst_registration_status = 'registered' AND (
        NEW.rule46s_declaration_snapshot IS NULL
     OR NEW.hsn_turnover_band_snapshot IS NULL
     OR ((NEW.hsn_turnover_band_snapshot = 'unknown')
         <> (NEW.hsn_turnover_financial_year_snapshot IS NULL))
     OR ((NEW.hsn_turnover_band_snapshot = 'unknown') <> (NEW.hsn_required_digits IS NULL))
     OR ((COALESCE(NEW.customer_gst_registration_status, '') = 'registered')
         <> (NEW.einvoice_applicability_snapshot IS NOT NULL))))
)
BEGIN
    SELECT RAISE(ABORT, 'compliance_snapshot_incomplete');
END;
