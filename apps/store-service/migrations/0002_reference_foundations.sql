-- Phase 1A reference foundations only. Product, pack, batch, and stock tables
-- are deliberately excluded.

-- A database may contain no store only during initial setup. Once configured,
-- it is the locally authoritative database for exactly one store.
CREATE TRIGGER store_identity_single_store_insert
BEFORE INSERT ON store_identity
WHEN EXISTS (SELECT 1 FROM store_identity)
BEGIN
    SELECT RAISE(ABORT, 'store_identity_allows_exactly_one_store');
END;

CREATE TRIGGER store_identity_no_delete
BEFORE DELETE ON store_identity
BEGIN
    SELECT RAISE(ABORT, 'store_identity_cannot_be_deleted');
END;

CREATE TABLE units_of_measure (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    canonical_code TEXT NOT NULL CHECK (
        canonical_code = lower(trim(canonical_code))
        AND length(canonical_code) BETWEEN 1 AND 32
        AND canonical_code NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 100),
    dimension TEXT NOT NULL CHECK (dimension IN ('count', 'container', 'volume', 'mass')),
    is_discrete INTEGER NOT NULL CHECK (is_discrete IN (0, 1)),
    allowed_scale INTEGER NOT NULL CHECK (allowed_scale BETWEEN 0 AND 6),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (canonical_code)
) STRICT;

CREATE TABLE dosage_forms (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    canonical_code TEXT NOT NULL CHECK (
        canonical_code = lower(trim(canonical_code))
        AND length(canonical_code) BETWEEN 1 AND 32
        AND canonical_code NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 100),
    description TEXT,
    route_hint TEXT,
    release_hint TEXT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (canonical_code)
) STRICT;

CREATE TABLE pharmaceutical_companies (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 200),
    legal_name TEXT,
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 200
    ),
    city TEXT,
    state TEXT,
    country_code TEXT CHECK (country_code IS NULL OR (length(country_code) = 2 AND country_code = upper(country_code))),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE TABLE company_identifiers (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    company_id TEXT NOT NULL REFERENCES pharmaceutical_companies(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    namespace TEXT NOT NULL CHECK (
        namespace = lower(trim(namespace)) AND length(namespace) BETWEEN 1 AND 64
        AND namespace NOT GLOB '*[^a-z0-9._-]*'
    ),
    normalized_value TEXT NOT NULL CHECK (
        normalized_value = upper(trim(normalized_value)) AND length(normalized_value) BETWEEN 1 AND 128
    ),
    verification_state TEXT NOT NULL CHECK (verification_state IN ('unverified', 'verified', 'rejected')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX company_identifiers_verified_active_uq
ON company_identifiers(namespace, normalized_value)
WHERE status = 'active' AND verification_state = 'verified';

CREATE TABLE brands (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 200),
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 200
    ),
    brand_owner_company_id TEXT REFERENCES pharmaceutical_companies(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX brands_search_idx ON brands(normalized_search_name);
CREATE INDEX companies_search_idx ON pharmaceutical_companies(normalized_search_name);

CREATE TABLE hsn_codes (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    jurisdiction TEXT NOT NULL CHECK (
        jurisdiction = upper(trim(jurisdiction)) AND length(jurisdiction) BETWEEN 2 AND 16
    ),
    hsn_code TEXT NOT NULL CHECK (
        hsn_code = upper(trim(hsn_code)) AND length(hsn_code) BETWEEN 2 AND 16
        AND hsn_code NOT GLOB '*[^A-Z0-9]*'
    ),
    description TEXT NOT NULL CHECK (length(trim(description)) BETWEEN 1 AND 500),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (jurisdiction, hsn_code)
) STRICT;

CREATE TABLE tax_categories (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    jurisdiction TEXT NOT NULL CHECK (
        jurisdiction = upper(trim(jurisdiction)) AND length(jurisdiction) BETWEEN 2 AND 16
    ),
    category_code TEXT NOT NULL CHECK (
        category_code = lower(trim(category_code)) AND length(category_code) BETWEEN 1 AND 32
        AND category_code NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 100),
    tax_treatment TEXT NOT NULL CHECK (tax_treatment IN ('taxable', 'exempt', 'nil_rated', 'non_gst')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (jurisdiction, category_code)
) STRICT;

-- Rate scale is basis points: 100 = 1.00%, 10000 = 100.00%.
-- Effective periods use [effective_from, effective_to); NULL effective_to is open-ended.
CREATE TABLE tax_rate_versions (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    tax_category_id TEXT NOT NULL REFERENCES tax_categories(id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    effective_from TEXT NOT NULL CHECK (effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    cgst_basis_points INTEGER NOT NULL CHECK (cgst_basis_points BETWEEN 0 AND 10000),
    sgst_basis_points INTEGER NOT NULL CHECK (sgst_basis_points BETWEEN 0 AND 10000),
    igst_basis_points INTEGER NOT NULL CHECK (igst_basis_points BETWEEN 0 AND 10000),
    cess_basis_points INTEGER NOT NULL DEFAULT 0 CHECK (cess_basis_points BETWEEN 0 AND 10000),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE TRIGGER tax_rate_versions_no_overlap_insert
BEFORE INSERT ON tax_rate_versions
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM tax_rate_versions existing
    WHERE existing.tax_category_id = NEW.tax_category_id
      AND existing.status = 'active'
      AND existing.effective_from < COALESCE(NEW.effective_to, '9999-12-31')
      AND NEW.effective_from < COALESCE(existing.effective_to, '9999-12-31')
)
BEGIN
    SELECT RAISE(ABORT, 'tax_rate_effective_period_overlap');
END;

CREATE TRIGGER tax_rate_versions_no_overlap_update
BEFORE UPDATE ON tax_rate_versions
WHEN NEW.status = 'active' AND EXISTS (
    SELECT 1 FROM tax_rate_versions existing
    WHERE existing.tax_category_id = NEW.tax_category_id
      AND existing.id <> NEW.id
      AND existing.status = 'active'
      AND existing.effective_from < COALESCE(NEW.effective_to, '9999-12-31')
      AND NEW.effective_from < COALESCE(existing.effective_to, '9999-12-31')
)
BEGIN
    SELECT RAISE(ABORT, 'tax_rate_effective_period_overlap');
END;

CREATE TABLE regulatory_categories (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36 AND substr(id, 15, 1) = '7'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    jurisdiction TEXT NOT NULL CHECK (
        jurisdiction = upper(trim(jurisdiction)) AND length(jurisdiction) BETWEEN 2 AND 16
    ),
    category_system TEXT NOT NULL CHECK (
        category_system = lower(trim(category_system)) AND length(category_system) BETWEEN 1 AND 64
        AND category_system NOT GLOB '*[^a-z0-9._-]*'
    ),
    category_code TEXT NOT NULL CHECK (
        category_code = upper(trim(category_code)) AND length(category_code) BETWEEN 1 AND 64
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 200),
    effective_from TEXT CHECK (effective_from IS NULL OR effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    source_reference TEXT,
    verification_state TEXT NOT NULL CHECK (verification_state IN ('unverified', 'verified', 'rejected')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (jurisdiction, category_system, category_code)
) STRICT;

CREATE TABLE master_change_events (
    event_id TEXT PRIMARY KEY NOT NULL CHECK (length(event_id) = 36 AND substr(event_id, 15, 1) = '7'),
    entity_type TEXT NOT NULL CHECK (entity_type IN (
        'unit_of_measure', 'dosage_form', 'pharmaceutical_company', 'company_identifier',
        'brand', 'hsn_code', 'tax_category', 'tax_rate_version', 'regulatory_category'
    )),
    entity_id TEXT NOT NULL CHECK (length(entity_id) = 36 AND substr(entity_id, 15, 1) = '7'),
    entity_revision INTEGER NOT NULL CHECK (entity_revision >= 1),
    action TEXT NOT NULL CHECK (action IN ('created', 'updated', 'archived', 'restored')),
    occurred_at_utc TEXT NOT NULL CHECK (occurred_at_utc GLOB '????-??-??T??:??:??*Z'),
    reason TEXT,
    payload_schema_version INTEGER NOT NULL CHECK (payload_schema_version >= 1),
    change_payload TEXT NOT NULL CHECK (json_valid(change_payload)),
    actor_id TEXT,
    terminal_id TEXT
) STRICT;

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

-- Small canonical unit seed. IDs are stable UUIDv7-compatible identifiers.
INSERT INTO units_of_measure (
    id, canonical_code, display_name, dimension, is_discrete, allowed_scale,
    created_at_utc, updated_at_utc
) VALUES
('01997000-0000-7000-8000-000000000001', 'tablet',   'Tablet',   'count',     1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000002', 'capsule',  'Capsule',  'count',     1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000003', 'piece',    'Piece',    'count',     1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000004', 'strip',    'Strip',    'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000005', 'box',      'Box',      'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000006', 'bottle',   'Bottle',   'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000007', 'vial',     'Vial',     'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000008', 'ampoule',  'Ampoule',  'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000009', 'tube',     'Tube',     'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000010', 'sachet',   'Sachet',   'container', 1, 0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000011', 'ml',       'Millilitre','volume',   0, 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000012', 'litre',    'Litre',    'volume',    0, 6, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000013', 'mg',       'Milligram','mass',      0, 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000014', 'gram',     'Gram',     'mass',      0, 6, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997000-0000-7000-8000-000000000015', 'kilogram', 'Kilogram', 'mass',      0, 6, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
