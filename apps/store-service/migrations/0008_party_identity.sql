-- Phase 1E party identity, the supplier role, tax registration identity, and postal addresses only.
-- Purchase, sales, payment terms, price lists, and accounting remain deferred.
--
-- A Party is an identity, never an account. These tables carry NO balance, outstanding, credit
-- limit, ageing bucket, or amount of any kind, and none may ever be added: a future accounting
-- ledger will reference party_id and derive every balance by summing its own postings, exactly as
-- inventory_movements is the sole authority for quantity. Recording a Party grants no credit and
-- creates no transaction.

-- Extend the append-only audit stream without editing the earlier migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1d;

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
        'state_code', 'party', 'party_role', 'party_address'
    )),
    entity_id TEXT NOT NULL CHECK (
        length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'
        AND lower(substr(entity_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

INSERT INTO master_change_events
SELECT * FROM master_change_events_phase1d;
DROP TABLE master_change_events_phase1d;

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

-- The jurisdiction's State list, used as the place of supply and on postal addresses.
--
-- A reference master rather than a hardcoded CHECK because the list genuinely changes: Dadra and
-- Nagar Haveli and Daman and Diu merged into code 26 in 2020. Obsolete codes stay seeded so that a
-- supplier's historic GSTIN still validates; a store that does not want one archives it.
CREATE TABLE state_codes (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    jurisdiction TEXT NOT NULL CHECK (
        jurisdiction = upper(trim(jurisdiction)) AND length(jurisdiction) BETWEEN 2 AND 16
    ),
    -- The two-character code a GSTIN carries in its first two positions.
    state_code TEXT NOT NULL CHECK (
        length(state_code) = 2 AND state_code NOT GLOB '*[^0-9A-Z]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 100),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (jurisdiction, state_code)
) STRICT;

-- One business entity this store deals with. Identity only.
--
-- No store_id: a Party is an identity in the same sense a Product or a Brand is, and this database
-- is authoritative for exactly one store. What the Party is *to this store* lives in party_roles.
CREATE TABLE parties (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    -- What the operator typed and what every list shows.
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 200),
    -- The registered name as printed on the GST certificate, which is frequently different.
    legal_name TEXT CHECK (legal_name IS NULL OR length(trim(legal_name)) BETWEEN 1 AND 250),
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 200
    ),
    -- 'unregistered' is a positive assertion that this party holds no GSTIN; 'unknown' means nobody
    -- has captured it yet. They look identical in a nullable column but are not the same fact.
    gst_registration_status TEXT NOT NULL DEFAULT 'unknown' CHECK (
        gst_registration_status IN ('registered', 'unregistered', 'unknown')
    ),
    gstin TEXT,
    normalized_gstin TEXT CHECK (
        normalized_gstin IS NULL OR (
            length(normalized_gstin) = 15
            AND normalized_gstin = upper(normalized_gstin)
            AND normalized_gstin GLOB '[0-9][0-9][A-Z][A-Z][A-Z][A-Z][A-Z][0-9][0-9][0-9][0-9][A-Z][1-9A-Z]Z[0-9A-Z]'
        )
    ),
    pan TEXT,
    normalized_pan TEXT CHECK (
        normalized_pan IS NULL OR (
            length(normalized_pan) = 10
            AND normalized_pan GLOB '[A-Z][A-Z][A-Z][A-Z][A-Z][0-9][0-9][0-9][0-9][A-Z]'
        )
    ),
    place_of_supply_state_id TEXT REFERENCES state_codes(id) ON DELETE RESTRICT,
    -- Stored already normalised: a telephone number has no meaningful "as printed" form.
    primary_phone TEXT CHECK (
        primary_phone IS NULL OR (
            length(primary_phone) BETWEEN 6 AND 16
            AND primary_phone GLOB '[+0-9][0-9]*'
            AND primary_phone NOT GLOB '*[^+0-9]*'
        )
    ),
    primary_email TEXT CHECK (
        primary_email IS NULL OR (
            primary_email = lower(trim(primary_email))
            AND length(primary_email) BETWEEN 3 AND 254
            AND primary_email NOT GLOB '* *'
            AND primary_email GLOB '?*@?*.?*'
        )
    ),
    -- The licence string exactly as printed, commonly a pair such as '20B-1234 / 21B-5678'. No
    -- structural parsing: nothing reads this field programmatically.
    drug_licence_number TEXT CHECK (
        drug_licence_number IS NULL OR length(trim(drug_licence_number)) BETWEEN 1 AND 100
    ),
    drug_licence_valid_upto TEXT CHECK (
        drug_licence_valid_upto IS NULL OR drug_licence_valid_upto GLOB '????-??-??'
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    -- A GSTIN is present exactly when the party is asserted to be registered.
    CHECK (
        (gst_registration_status = 'registered' AND gstin IS NOT NULL AND normalized_gstin IS NOT NULL)
        OR (gst_registration_status IN ('unregistered', 'unknown')
            AND gstin IS NULL AND normalized_gstin IS NULL)
    ),
    -- A GSTIN encodes its state of registration, so the place of supply cannot be unknown.
    CHECK (normalized_gstin IS NULL OR place_of_supply_state_id IS NOT NULL),
    CHECK ((pan IS NULL) = (normalized_pan IS NULL)),
    -- Characters 3-12 of a GSTIN are the holder's PAN. A disagreement means one was mistyped.
    CHECK (
        normalized_pan IS NULL OR normalized_gstin IS NULL
        OR normalized_pan = substr(normalized_gstin, 3, 10)
    ),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

-- One active party per registration. SQLite treats NULLs as distinct in a unique index, so the
-- predicate is explicit rather than load-bearing.
CREATE UNIQUE INDEX parties_active_gstin_uq
ON parties(normalized_gstin)
WHERE status = 'active' AND normalized_gstin IS NOT NULL;

-- PAN is deliberately NOT unique: one legal entity registered in several states is recorded as
-- several Parties under the one-GSTIN model, and those Parties legitimately share a PAN. A shared
-- PAN is surfaced as a duplicate candidate, never as a conflict.
CREATE INDEX parties_pan_idx ON parties(normalized_pan);
CREATE INDEX parties_search_idx ON parties(normalized_search_name);

CREATE TRIGGER parties_integrity_insert
BEFORE INSERT ON parties
WHEN NEW.status = 'active' AND (
    (NEW.place_of_supply_state_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM state_codes WHERE id = NEW.place_of_supply_state_id AND status = 'active'
    ))
 OR (NEW.normalized_gstin IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM state_codes
        WHERE id = NEW.place_of_supply_state_id
          AND status = 'active'
          AND jurisdiction = 'IN'
          AND state_code = substr(NEW.normalized_gstin, 1, 2)
    ))
)
BEGIN
    SELECT RAISE(ABORT, 'party_conflict');
END;

CREATE TRIGGER parties_integrity_update
BEFORE UPDATE OF status, place_of_supply_state_id, normalized_gstin ON parties
WHEN NEW.status = 'active' AND (
    (NEW.place_of_supply_state_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM state_codes WHERE id = NEW.place_of_supply_state_id AND status = 'active'
    ))
 OR (NEW.normalized_gstin IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM state_codes
        WHERE id = NEW.place_of_supply_state_id
          AND status = 'active'
          AND jurisdiction = 'IN'
          AND state_code = substr(NEW.normalized_gstin, 1, 2)
    ))
)
BEGIN
    SELECT RAISE(ABORT, 'party_conflict');
END;

-- What the Party is to this store. A role has its own lifecycle, so a Party can stop being a
-- supplier without ceasing to exist.
--
-- 'customer' names the other half of the closed set of counterparty roles a pharmacy has, in the
-- same way Phase 1D named 'adjustment' alongside 'opening_stock'. The Phase 1E API rejects it: what
-- is deferred is the Customer workflow, not the word.
CREATE TABLE party_roles (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    party_id TEXT NOT NULL REFERENCES parties(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('supplier', 'customer')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX party_roles_active_uq
ON party_roles(party_id, role)
WHERE status = 'active';

CREATE INDEX party_roles_role_idx ON party_roles(role, status);

CREATE TRIGGER party_roles_integrity_insert
BEFORE INSERT ON party_roles
WHEN NOT EXISTS (SELECT 1 FROM parties WHERE id = NEW.party_id AND status = 'active')
BEGIN
    SELECT RAISE(ABORT, 'party_role_conflict');
END;

CREATE TRIGGER party_roles_integrity_update
BEFORE UPDATE OF status, party_id, role ON party_roles
WHEN NEW.status = 'active'
 AND NOT EXISTS (SELECT 1 FROM parties WHERE id = NEW.party_id AND status = 'active')
BEGIN
    SELECT RAISE(ABORT, 'party_role_conflict');
END;

-- Postal addresses. No contact person, no geocoding, no delivery window: those are CRM and
-- logistics, and neither belongs in an identity foundation.
CREATE TABLE party_addresses (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    party_id TEXT NOT NULL REFERENCES parties(id) ON DELETE RESTRICT,
    address_role TEXT NOT NULL CHECK (address_role IN ('billing', 'shipping')),
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
    is_primary INTEGER NOT NULL DEFAULT 0 CHECK (is_primary IN (0, 1)),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

-- At most one active primary address for each purpose.
CREATE UNIQUE INDEX party_addresses_primary_uq
ON party_addresses(party_id, address_role)
WHERE status = 'active' AND is_primary = 1;

CREATE INDEX party_addresses_party_idx
ON party_addresses(party_id, status, address_role);

CREATE TRIGGER party_addresses_integrity_insert
BEFORE INSERT ON party_addresses
WHEN NOT EXISTS (SELECT 1 FROM parties WHERE id = NEW.party_id AND status = 'active')
  OR (NEW.state_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM state_codes WHERE id = NEW.state_id AND status = 'active'))
BEGIN
    SELECT RAISE(ABORT, 'party_address_conflict');
END;

CREATE TRIGGER party_addresses_integrity_update
BEFORE UPDATE OF status, party_id, state_id ON party_addresses
WHEN NEW.status = 'active' AND (
    NOT EXISTS (SELECT 1 FROM parties WHERE id = NEW.party_id AND status = 'active')
 OR (NEW.state_id IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM state_codes WHERE id = NEW.state_id AND status = 'active'))
)
BEGIN
    SELECT RAISE(ABORT, 'party_address_conflict');
END;

-- The GST State code list. Identifier tails carry the state code itself so a row is recognisable.
-- Codes 25 and 28 are superseded but stay seeded: a supplier's historic GSTIN must still validate.
INSERT INTO state_codes (id, jurisdiction, state_code, display_name, created_at_utc, updated_at_utc) VALUES
('01997300-0000-7000-8000-000000000001', 'IN', '01', 'Jammu and Kashmir', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000002', 'IN', '02', 'Himachal Pradesh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000003', 'IN', '03', 'Punjab', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000004', 'IN', '04', 'Chandigarh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000005', 'IN', '05', 'Uttarakhand', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000006', 'IN', '06', 'Haryana', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000007', 'IN', '07', 'Delhi', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000008', 'IN', '08', 'Rajasthan', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000009', 'IN', '09', 'Uttar Pradesh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000010', 'IN', '10', 'Bihar', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000011', 'IN', '11', 'Sikkim', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000012', 'IN', '12', 'Arunachal Pradesh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000013', 'IN', '13', 'Nagaland', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000014', 'IN', '14', 'Manipur', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000015', 'IN', '15', 'Mizoram', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000016', 'IN', '16', 'Tripura', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000017', 'IN', '17', 'Meghalaya', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000018', 'IN', '18', 'Assam', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000019', 'IN', '19', 'West Bengal', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000020', 'IN', '20', 'Jharkhand', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000021', 'IN', '21', 'Odisha', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000022', 'IN', '22', 'Chhattisgarh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000023', 'IN', '23', 'Madhya Pradesh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000024', 'IN', '24', 'Gujarat', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000025', 'IN', '25', 'Daman and Diu (superseded)', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000026', 'IN', '26', 'Dadra and Nagar Haveli and Daman and Diu', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000027', 'IN', '27', 'Maharashtra', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000028', 'IN', '28', 'Andhra Pradesh (before division)', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000029', 'IN', '29', 'Karnataka', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000030', 'IN', '30', 'Goa', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000031', 'IN', '31', 'Lakshadweep', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000032', 'IN', '32', 'Kerala', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000033', 'IN', '33', 'Tamil Nadu', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000034', 'IN', '34', 'Puducherry', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000035', 'IN', '35', 'Andaman and Nicobar Islands', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000036', 'IN', '36', 'Telangana', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000037', 'IN', '37', 'Andhra Pradesh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000038', 'IN', '38', 'Ladakh', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997300-0000-7000-8000-000000000097', 'IN', '97', 'Other Territory', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
