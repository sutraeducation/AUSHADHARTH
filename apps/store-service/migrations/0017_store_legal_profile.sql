-- ---------------------------------------------------------------------------------------------
-- Store legal profile, and the seller snapshot a posted Sale needs to stay historically true.
--
-- Until now the Store was a name, a time zone and a GST registration. That was enough to decide
-- CGST + SGST versus IGST, which is all Phase 1G asked of it. It is not enough to issue the
-- document a pharmacy is required to hand over:
--
--   * Rule 46(a) of the CGST Rules requires the supplier's NAME, ADDRESS and GSTIN on a tax
--     invoice. Two of the three do not exist in this schema.
--   * Rule 65(4)(3)(i) of the Drugs Rules, 1945 is stricter and applies to every retail drug sale,
--     GST-registered or not: "The supply by retail of any drug shall be made against a cash/credit
--     memo which shall contain the following particulars: (a) Name, address and sale licence number
--     of the dealer, (b) Serial number of the cash/credit memo, (c) the name and quantity of the
--     drug supplied." The sale licence number does not exist in this schema either.
--
-- So this migration adds the three missing seller facts — legal identity, operating address, and
-- pharmacy licences — and then freezes them onto every Sale posted from here on.
--
-- Additive throughout. No table is rebuilt except `master_change_events`, whose entity vocabulary
-- has to grow; that one is reproduced verbatim from 0016 with three values added, and
-- `tests/migration_0017.rs` proves against a populated database that nothing was lost.
--
-- Deliberately NOT here:
--   * PAN, bank details, FSSAI, logo, signature image — none is an invoice particular for a drug
--     retailer, and a field nobody prints is a field nobody maintains;
--   * any rule tying the operating-address State to the GSTIN State (audit item U2 is unresolved,
--     and a store may lawfully operate from premises addressed differently from its registration);
--   * licence renewal, expiry alerts or scheduling — the requirement is a durable record and a
--     correct printed number, not regulatory workflow.
-- ---------------------------------------------------------------------------------------------

-- ---------------------------------------------------------------------------------------------
-- Store identity: legal name and contact.
--
-- `display_name` stays what it has always been — the trading name the operator typed at first run,
-- shown on every screen. `legal_name` is the registered entity name as printed on the GST
-- certificate and the drug licence, which is frequently different ("Sharma Medical Stores" trading
-- as "Sharma Chemists"). The two are stored separately and neither is ever silently substituted for
-- the other, exactly as `parties` already distinguishes them.
--
-- SQLite cannot add a CHECK to an existing table, so the bounds that `parties` states as table
-- CHECKs are enforced here by a trigger with the same semantics, in the pattern 0010 established.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE store_identity ADD COLUMN legal_name TEXT;
ALTER TABLE store_identity ADD COLUMN primary_phone TEXT;
ALTER TABLE store_identity ADD COLUMN primary_email TEXT;

CREATE TRIGGER store_identity_profile_integrity_update
BEFORE UPDATE OF legal_name, primary_phone, primary_email
ON store_identity
WHEN
    (NEW.legal_name IS NOT NULL
        AND (length(trim(NEW.legal_name)) = 0 OR length(trim(NEW.legal_name)) > 250))
    -- Stored already normalised, as on a party: a telephone number has no meaningful "as printed"
    -- form, so the service hands this column digits with an optional leading plus.
 OR (NEW.primary_phone IS NOT NULL
        AND (length(NEW.primary_phone) < 6 OR length(NEW.primary_phone) > 16
             OR NEW.primary_phone NOT GLOB '[+0-9][0-9]*'
             OR NEW.primary_phone GLOB '*[^+0-9]*'))
 OR (NEW.primary_email IS NOT NULL
        AND (NEW.primary_email <> lower(trim(NEW.primary_email))
             OR length(NEW.primary_email) < 3 OR length(NEW.primary_email) > 254
             OR NEW.primary_email GLOB '* *'
             OR NEW.primary_email NOT GLOB '?*@?*.?*'))
BEGIN
    SELECT RAISE(ABORT, 'store_profile_conflict');
END;

-- ---------------------------------------------------------------------------------------------
-- Store operating address.
--
-- A child table rather than columns on `store_identity`, for two reasons that are about behaviour
-- rather than tidiness. It carries its own `revision`, so correcting a typo in the address does not
-- invalidate a GST edit somebody else has open. And it mirrors `party_addresses` column for column,
-- so the product has one idea of what an address is instead of two.
--
-- `UNIQUE (store_id)` makes "one authoritative operating address" a fact the database enforces
-- rather than a convention the service remembers. There is deliberately no `address_role` and no
-- `is_primary`: this installation serves one store from one counter, and a flag that can only ever
-- be true is a flag that teaches a future reader something false. Multi-branch is not being built
-- here and is not being pretended at either.
--
-- `postal_code` follows the frozen `party_addresses` rule exactly — 3 to 16 characters of letters,
-- digits, spaces and hyphens. A six-digit Indian PIN passes; so does a foreign code. The product's
-- existing permissiveness is deliberate and is not tightened here for the seller alone.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_addresses (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    line1 TEXT NOT NULL CHECK (length(trim(line1)) BETWEEN 1 AND 200),
    line2 TEXT CHECK (line2 IS NULL OR length(trim(line2)) BETWEEN 1 AND 200),
    city TEXT CHECK (city IS NULL OR length(trim(city)) BETWEEN 1 AND 100),
    state_id TEXT REFERENCES state_codes(id) ON DELETE RESTRICT,
    postal_code TEXT CHECK (
        postal_code IS NULL OR (
            length(postal_code) BETWEEN 3 AND 16 AND postal_code NOT GLOB '*[^0-9A-Z -]*'
        )
    ),
    country_code TEXT NOT NULL DEFAULT 'IN' CHECK (
        length(country_code) = 2 AND country_code = upper(country_code)
        AND country_code NOT GLOB '*[^A-Z]*'
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    UNIQUE (store_id)
) STRICT;

-- ---------------------------------------------------------------------------------------------
-- Store pharmacy licences.
--
-- Child records, not one text field, because a real pharmacy holds several: a Form 20 and a Form 21
-- for retail, often a 20B/21B for wholesale, a 20C for homoeopathy, a 20F/20G where Schedule X is
-- stocked. Flattening those into a single string would make it impossible to archive one when it is
-- surrendered while keeping the others.
--
-- `licence_type` is bounded free text and NOT an enumeration. The Drugs Rules prescribe licence
-- FORMS, but the text a State Drug Control authority prints on the certificate varies, and an enum
-- that refused a real licence would be worse than useless. The same reasoning the party domain
-- already recorded for the licence number applies to its type.
--
-- `licence_number` is preserved exactly as printed — commonly a compound such as
-- '20B-MH-1234 / 21B-MH-5678'. `normalized_licence_number` exists ONLY to detect that the same
-- licence has been entered twice; nothing reads it as a licence and nothing prints it. There is no
-- format regex, because no authority publishes one that holds across States.
--
-- There is no `is_primary`. Which licence to print is not a preference: §7 of this phase derives the
-- printable text from every ACTIVE licence in a deterministic order, so the answer is the same on
-- every machine and after every restore. A flag would let two installations print different
-- documents for the same sale.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE store_licences (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    licence_type TEXT NOT NULL CHECK (length(trim(licence_type)) BETWEEN 1 AND 60),
    licence_number TEXT NOT NULL CHECK (length(trim(licence_number)) BETWEEN 1 AND 100),
    -- Alphanumerics only, upper-cased: '20b-1234' and '20B 1234' are the same licence typed twice.
    normalized_licence_number TEXT NOT NULL CHECK (
        length(normalized_licence_number) BETWEEN 1 AND 100
        AND normalized_licence_number = upper(normalized_licence_number)
        AND normalized_licence_number NOT GLOB '*[^A-Z0-9]*'
    ),
    issuing_authority TEXT CHECK (
        issuing_authority IS NULL OR length(trim(issuing_authority)) BETWEEN 1 AND 160
    ),
    valid_from TEXT CHECK (valid_from IS NULL OR valid_from GLOB '????-??-??'),
    valid_upto TEXT CHECK (valid_upto IS NULL OR valid_upto GLOB '????-??-??'),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT CHECK (archive_reason IS NULL OR length(trim(archive_reason)) BETWEEN 1 AND 500),
    -- A validity window that ends before it starts is a typo, not a licence.
    CHECK (valid_from IS NULL OR valid_upto IS NULL OR valid_upto >= valid_from),
    -- The lifecycle columns agree with the status, as everywhere else in this schema.
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL)
    )
) STRICT;

-- The same licence cannot be active twice. An archived one may repeat, because surrendering a
-- licence and later being re-issued the same number is a thing that happens.
CREATE UNIQUE INDEX store_licences_active_number_uq
ON store_licences(store_id, normalized_licence_number)
WHERE status = 'active';

CREATE INDEX store_licences_listing_idx ON store_licences(store_id, status, licence_type);

-- ---------------------------------------------------------------------------------------------
-- Seller snapshot on a posted Sale.
--
-- Phase 1H already froze the Store's GSTIN, its State code and its place of supply onto every
-- posted Sale, and froze the product, pack, batch, expiry, MRP, HSN and every tax fact onto every
-- line. What it could not freeze was a name and an address, because the Store had neither.
--
-- These columns close that gap and nothing more. `store_normalized_gstin`,
-- `store_place_of_supply_state_id` and `store_state_code` are NOT duplicated under seller names:
-- they are already the posted truth, and a second copy would eventually disagree with the first.
--
-- Every column is nullable because there are already posted Sales in the wild that predate them,
-- and inventing a history for those Sales would be worse than admitting there isn't one. Which is
-- which is answered by `seller_snapshot_version`, not by a date: 0 means "issued before this
-- product recorded seller details", 1 means "every required seller fact below is the fact as it
-- stood at posting". A date discriminator would have been guesswork the moment a clock was wrong.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_documents ADD COLUMN seller_snapshot_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sale_documents ADD COLUMN seller_legal_name TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_trade_name TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_address_line1 TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_address_line2 TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_city TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_postal_code TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_state_name TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_phone TEXT;
ALTER TABLE sale_documents ADD COLUMN seller_email TEXT;
-- The printable licence line, already ordered and joined at posting. A posted Sale must never join
-- live licence rows to reprint itself: archiving a licence years later would silently rewrite a
-- document that was lawful when it was issued.
ALTER TABLE sale_documents ADD COLUMN seller_licence_text TEXT;

-- A version-1 snapshot must actually contain the three facts the Drugs Rules require of the memo:
-- the dealer's name, address and sale licence number. Nothing may claim to be a complete seller
-- snapshot while missing one of them, and a CHECK cannot be added to an existing table, so this is
-- the same trigger pattern 0010 used for the tax columns.
CREATE TRIGGER sale_documents_seller_snapshot_insert
BEFORE INSERT ON sale_documents
WHEN NEW.seller_snapshot_version >= 1
 AND (NEW.seller_legal_name IS NULL OR length(trim(NEW.seller_legal_name)) = 0
      OR NEW.seller_address_line1 IS NULL OR length(trim(NEW.seller_address_line1)) = 0
      OR NEW.seller_licence_text IS NULL OR length(trim(NEW.seller_licence_text)) = 0)
BEGIN
    SELECT RAISE(ABORT, 'seller_snapshot_incomplete');
END;

CREATE TRIGGER sale_documents_seller_snapshot_update
BEFORE UPDATE ON sale_documents
WHEN NEW.seller_snapshot_version >= 1
 AND (NEW.seller_legal_name IS NULL OR length(trim(NEW.seller_legal_name)) = 0
      OR NEW.seller_address_line1 IS NULL OR length(trim(NEW.seller_address_line1)) = 0
      OR NEW.seller_licence_text IS NULL OR length(trim(NEW.seller_licence_text)) = 0)
BEGIN
    SELECT RAISE(ABORT, 'seller_snapshot_incomplete');
END;

-- A seller snapshot can only ever be written once, on the way from draft to posted. The frozen
-- `sale_documents_posted_no_update` trigger already refuses every update to a posted row, so the
-- snapshot inherits that immutability rather than needing its own guard; this one closes the
-- remaining direction, which is a DRAFT quietly acquiring a version before it is posted.
CREATE TRIGGER sale_documents_seller_snapshot_draft_only
BEFORE UPDATE ON sale_documents
WHEN OLD.status = 'draft' AND NEW.status = 'draft' AND NEW.seller_snapshot_version <> 0
BEGIN
    SELECT RAISE(ABORT, 'seller_snapshot_before_posting');
END;

-- ---------------------------------------------------------------------------------------------
-- Quantity scale on a posted line.
--
-- A line stores an integer count of atoms, and how many decimal places that integer represents is
-- `products.quantity_scale`: 3 atoms at scale 0 is three tablets, at scale 1 it is 0.3 millilitres.
-- Without it a line cannot be rendered.
--
-- Reading it from the product at render time would be SAFE — `quantity_scale` is already frozen in
-- practice, because the catalogue refuses to change it once any pack exists and a sale line
-- requires a pack — but it would still be a live master lookup in the middle of assembling a
-- historical document, and this phase is about not doing that. One nullable integer removes the
-- last reason for the renderer to know that `products` exists.
--
-- NULL on lines posted before this migration. Those belong to Sales that already report themselves
-- as legacy, and their quantities render as whole atoms.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE sale_lines ADD COLUMN quantity_scale INTEGER
    CHECK (quantity_scale IS NULL OR quantity_scale BETWEEN 0 AND 6);

-- ---------------------------------------------------------------------------------------------
-- master_change_events — rebuilt to admit three more entity types.
--
-- Reproduced verbatim from 0016 apart from the three added values. A change to the Store's legal
-- identity, its address, or a licence is a business event: it changes what every invoice issued
-- afterwards will say about who sold the goods.
-- ---------------------------------------------------------------------------------------------
ALTER TABLE master_change_events RENAME TO master_change_events_phase1k;

DROP TRIGGER IF EXISTS master_change_events_no_update;
DROP TRIGGER IF EXISTS master_change_events_no_delete;
DROP INDEX IF EXISTS master_change_events_entity_idx;

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
        'store_profile', 'store_address', 'store_licence'
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
SELECT * FROM master_change_events_phase1k;

DROP TABLE master_change_events_phase1k;

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
