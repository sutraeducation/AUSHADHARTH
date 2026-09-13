-- Phase 1C-B ingredient, salt form, strength unit, and product composition identity only.
-- Substitution, equivalence, interactions, dosage, batch, expiry, stock, and pricing are deferred.

-- Extend the append-only audit stream without editing the Phase 1A or Phase 1B migrations.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1b;

CREATE TABLE master_change_events (
    event_id TEXT PRIMARY KEY NOT NULL CHECK (
        length(event_id) = 36 AND substr(event_id, 15, 1) = '7'
        AND lower(substr(event_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_type TEXT NOT NULL CHECK (entity_type IN (
        'unit_of_measure', 'dosage_form', 'pharmaceutical_company', 'company_identifier',
        'brand', 'hsn_code', 'tax_category', 'tax_rate_version', 'regulatory_category',
        'product', 'product_company_role', 'product_pack', 'store_pack_policy', 'barcode',
        'ingredient', 'salt_form', 'strength_unit', 'product_composition_component'
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
SELECT * FROM master_change_events_phase1b;
DROP TABLE master_change_events_phase1b;

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

-- The active moiety, independent of any salt or chemical form.
CREATE TABLE ingredients (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    canonical_code TEXT NOT NULL CHECK (
        canonical_code = lower(trim(canonical_code))
        AND length(canonical_code) BETWEEN 1 AND 64
        AND canonical_code NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 160),
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 160
    ),
    description TEXT,
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

CREATE INDEX ingredients_search_idx ON ingredients(normalized_search_name);

-- The chemical form modifier applied to an ingredient. Deliberately not the ingredient itself.
CREATE TABLE salt_forms (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    canonical_code TEXT NOT NULL CHECK (
        canonical_code = lower(trim(canonical_code))
        AND length(canonical_code) BETWEEN 1 AND 64
        AND canonical_code NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 120),
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 120
    ),
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

-- Units in which a pharmaceutical strength is expressed. Separate from units_of_measure, which
-- carries inventory subdivision semantics and cannot express activity or equivalent units.
CREATE TABLE strength_units (
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
    dimension TEXT NOT NULL CHECK (
        dimension IN ('mass', 'volume', 'count', 'activity', 'substance_equivalent')
    ),
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

-- Manufacturer-stated composition of one medicine Product. Business identity only: this table
-- carries no clinical, equivalence, or substitution meaning.
CREATE TABLE product_composition_components (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    ingredient_id TEXT NOT NULL REFERENCES ingredients(id) ON DELETE RESTRICT,
    salt_form_id TEXT REFERENCES salt_forms(id) ON DELETE RESTRICT,
    component_role TEXT NOT NULL DEFAULT 'active' CHECK (component_role IN ('active', 'inactive')),
    display_order INTEGER NOT NULL CHECK (display_order >= 0),
    strength_presentation TEXT NOT NULL CHECK (strength_presentation IN ('absolute', 'percentage')),
    strength_numerator_atoms INTEGER NOT NULL CHECK (
        strength_numerator_atoms > 0 AND strength_numerator_atoms <= 1000000000000
    ),
    strength_numerator_scale INTEGER NOT NULL CHECK (strength_numerator_scale BETWEEN 0 AND 6),
    strength_numerator_unit_id TEXT NOT NULL REFERENCES strength_units(id) ON DELETE RESTRICT,
    strength_denominator_atoms INTEGER CHECK (
        strength_denominator_atoms IS NULL
        OR (strength_denominator_atoms > 0 AND strength_denominator_atoms <= 1000000000000)
    ),
    strength_denominator_scale INTEGER CHECK (
        strength_denominator_scale IS NULL OR strength_denominator_scale BETWEEN 0 AND 6
    ),
    strength_denominator_unit_id TEXT REFERENCES strength_units(id) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'),
    archive_reason TEXT,
    -- A concentration is all three denominator parts or none of them.
    CHECK (
        (strength_denominator_atoms IS NULL AND strength_denominator_scale IS NULL
            AND strength_denominator_unit_id IS NULL)
        OR (strength_denominator_atoms IS NOT NULL AND strength_denominator_scale IS NOT NULL
            AND strength_denominator_unit_id IS NOT NULL)
    ),
    -- A percentage is an exact ratio out of one hundred; the scale is pinned so the value is exact.
    CHECK (
        strength_presentation = 'absolute'
        OR (strength_denominator_atoms = 100 AND strength_denominator_scale = 0)
    ),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

-- NULL salt forms compare as distinct in a unique index, so the no-salt case is coalesced. Without
-- this the same ingredient could be recorded twice on one Product.
CREATE UNIQUE INDEX product_composition_components_active_uq
ON product_composition_components(product_id, ingredient_id, COALESCE(salt_form_id, ''))
WHERE status = 'active';

CREATE INDEX product_composition_components_product_idx
ON product_composition_components(product_id, status, display_order);

CREATE INDEX product_composition_components_ingredient_idx
ON product_composition_components(ingredient_id);

CREATE TRIGGER product_composition_components_integrity_insert
BEFORE INSERT ON product_composition_components
WHEN NOT EXISTS (
        SELECT 1 FROM products
        WHERE id = NEW.product_id AND status = 'active' AND product_kind = 'medicine'
    )
 OR NOT EXISTS (
        SELECT 1 FROM ingredients WHERE id = NEW.ingredient_id AND status = 'active'
    )
 OR (NEW.salt_form_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM salt_forms WHERE id = NEW.salt_form_id AND status = 'active'
    ))
 OR NOT EXISTS (
        SELECT 1 FROM strength_units unit
        WHERE unit.id = NEW.strength_numerator_unit_id
          AND unit.status = 'active'
          AND NEW.strength_numerator_scale <= unit.allowed_scale
    )
 OR (NEW.strength_denominator_unit_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM strength_units unit
        WHERE unit.id = NEW.strength_denominator_unit_id
          AND unit.status = 'active'
          AND NEW.strength_denominator_scale <= unit.allowed_scale
    ))
BEGIN
    SELECT RAISE(ABORT, 'composition_component_conflict');
END;

CREATE TRIGGER product_composition_components_integrity_update
BEFORE UPDATE OF product_id, ingredient_id, salt_form_id, status,
    strength_numerator_scale, strength_numerator_unit_id,
    strength_denominator_scale, strength_denominator_unit_id
ON product_composition_components
WHEN NEW.status = 'active' AND (
    NOT EXISTS (
        SELECT 1 FROM products
        WHERE id = NEW.product_id AND status = 'active' AND product_kind = 'medicine'
    )
 OR NOT EXISTS (
        SELECT 1 FROM ingredients WHERE id = NEW.ingredient_id AND status = 'active'
    )
 OR (NEW.salt_form_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM salt_forms WHERE id = NEW.salt_form_id AND status = 'active'
    ))
 OR NOT EXISTS (
        SELECT 1 FROM strength_units unit
        WHERE unit.id = NEW.strength_numerator_unit_id
          AND unit.status = 'active'
          AND NEW.strength_numerator_scale <= unit.allowed_scale
    )
 OR (NEW.strength_denominator_unit_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM strength_units unit
        WHERE unit.id = NEW.strength_denominator_unit_id
          AND unit.status = 'active'
          AND NEW.strength_denominator_scale <= unit.allowed_scale
    ))
)
BEGIN
    SELECT RAISE(ABORT, 'composition_component_conflict');
END;

INSERT INTO salt_forms (
    id, canonical_code, display_name, normalized_search_name, created_at_utc, updated_at_utc
) VALUES
('01997100-0000-7000-8000-000000000001', 'sodium',        'Sodium',        'sodium',        strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000002', 'potassium',     'Potassium',     'potassium',     strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000003', 'calcium',       'Calcium',       'calcium',       strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000004', 'magnesium',     'Magnesium',     'magnesium',     strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000005', 'hydrochloride', 'Hydrochloride', 'hydrochloride', strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000006', 'sulphate',      'Sulphate',      'sulphate',      strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000007', 'phosphate',     'Phosphate',     'phosphate',     strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000008', 'maleate',       'Maleate',       'maleate',       strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000009', 'tartrate',      'Tartrate',      'tartrate',      strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000010', 'besylate',      'Besylate',      'besylate',      strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000011', 'mesylate',      'Mesylate',      'mesylate',      strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000012', 'fumarate',      'Fumarate',      'fumarate',      strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000013', 'succinate',     'Succinate',     'succinate',     strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000014', 'acetate',       'Acetate',       'acetate',       strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000015', 'citrate',       'Citrate',       'citrate',       strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000016', 'monohydrate',   'Monohydrate',   'monohydrate',   strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000017', 'dihydrate',     'Dihydrate',     'dihydrate',     strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997100-0000-7000-8000-000000000018', 'trihydrate',    'Trihydrate',    'trihydrate',    strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));

INSERT INTO strength_units (
    id, canonical_code, display_name, dimension, allowed_scale, created_at_utc, updated_at_utc
) VALUES
('01997200-0000-7000-8000-000000000001', 'mcg',       'Microgram',           'mass',                 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000002', 'mg',        'Milligram',           'mass',                 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000003', 'g',         'Gram',                'mass',                 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000004', 'ml',        'Millilitre',          'volume',               3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000005', 'litre',     'Litre',               'volume',               3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000006', 'iu',        'International Unit',  'activity',             3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000007', 'meq',       'Milliequivalent',     'substance_equivalent', 3, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000008', 'tablet',    'Tablet',              'count',                0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000009', 'capsule',   'Capsule',             'count',                0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000010', 'drop',      'Drop',                'count',                0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')),
('01997200-0000-7000-8000-000000000011', 'actuation', 'Actuation',           'count',                0, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'));
