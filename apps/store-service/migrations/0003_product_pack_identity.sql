-- Phase 1B product/formulation and pack/SKU identity only.
-- Composition, batch, stock, pricing, tax assignment, and rack location are deferred.

-- Extend the append-only audit stream without editing the Phase 1A migration.
ALTER TABLE master_change_events RENAME TO master_change_events_phase1a;

CREATE TABLE master_change_events (
    event_id TEXT PRIMARY KEY NOT NULL CHECK (
        length(event_id) = 36 AND substr(event_id, 15, 1) = '7'
        AND lower(substr(event_id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    entity_type TEXT NOT NULL CHECK (entity_type IN (
        'unit_of_measure', 'dosage_form', 'pharmaceutical_company', 'company_identifier',
        'brand', 'hsn_code', 'tax_category', 'tax_rate_version', 'regulatory_category',
        'product', 'product_company_role', 'product_pack', 'store_pack_policy', 'barcode'
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
SELECT * FROM master_change_events_phase1a;
DROP TABLE master_change_events_phase1a;

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

CREATE TABLE products (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    product_kind TEXT NOT NULL CHECK (
        product_kind IN ('medicine', 'device', 'general_pharmacy_item')
    ),
    brand_id TEXT REFERENCES brands(id) ON DELETE RESTRICT,
    dosage_form_id TEXT REFERENCES dosage_forms(id) ON DELETE RESTRICT,
    base_unit_id TEXT NOT NULL REFERENCES units_of_measure(id) ON DELETE RESTRICT,
    quantity_scale INTEGER NOT NULL CHECK (quantity_scale BETWEEN 0 AND 6),
    formulation_descriptor TEXT,
    route_descriptor TEXT,
    release_descriptor TEXT,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 240),
    normalized_search_name TEXT NOT NULL CHECK (
        normalized_search_name = lower(trim(normalized_search_name))
        AND length(normalized_search_name) BETWEEN 1 AND 240
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    CHECK (product_kind <> 'medicine' OR dosage_form_id IS NOT NULL),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE INDEX products_search_idx ON products(normalized_search_name);
CREATE INDEX products_brand_idx ON products(brand_id);
CREATE INDEX products_dosage_form_idx ON products(dosage_form_id);

CREATE TRIGGER products_unit_compatibility_insert
BEFORE INSERT ON products
WHEN NOT EXISTS (
    SELECT 1 FROM units_of_measure unit
    WHERE unit.id = NEW.base_unit_id
      AND unit.status = 'active'
      AND NEW.quantity_scale <= unit.allowed_scale
      AND (unit.is_discrete = 0 OR NEW.quantity_scale = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'product_quantity_scale_conflict');
END;

CREATE TRIGGER products_unit_compatibility_update
BEFORE UPDATE OF base_unit_id, quantity_scale, status ON products
WHEN NEW.status = 'active' AND NOT EXISTS (
    SELECT 1 FROM units_of_measure unit
    WHERE unit.id = NEW.base_unit_id
      AND unit.status = 'active'
      AND NEW.quantity_scale <= unit.allowed_scale
      AND (unit.is_discrete = 0 OR NEW.quantity_scale = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'product_quantity_scale_conflict');
END;

CREATE TRIGGER products_active_references_insert
BEFORE INSERT ON products
WHEN (NEW.brand_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM brands WHERE id = NEW.brand_id AND status = 'active'
    ))
 OR (NEW.dosage_form_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM dosage_forms WHERE id = NEW.dosage_form_id AND status = 'active'
    ))
BEGIN
    SELECT RAISE(ABORT, 'product_reference_conflict');
END;

CREATE TRIGGER products_active_references_update
BEFORE UPDATE OF brand_id, dosage_form_id, status ON products
WHEN NEW.status = 'active' AND (
    (NEW.brand_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM brands WHERE id = NEW.brand_id AND status = 'active'
    ))
    OR (NEW.dosage_form_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM dosage_forms WHERE id = NEW.dosage_form_id AND status = 'active'
    ))
)
BEGIN
    SELECT RAISE(ABORT, 'product_reference_conflict');
END;

CREATE TABLE product_company_roles (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    company_id TEXT NOT NULL REFERENCES pharmaceutical_companies(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK (role IN ('manufacturer', 'marketer', 'brand_owner', 'importer')),
    effective_from TEXT CHECK (effective_from IS NULL OR effective_from GLOB '????-??-??'),
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to GLOB '????-??-??'),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to > effective_from),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX product_company_roles_active_uq
ON product_company_roles(
    product_id, company_id, role,
    COALESCE(effective_from, ''), COALESCE(effective_to, '')
)
WHERE status = 'active';
CREATE INDEX product_company_roles_company_idx ON product_company_roles(company_id, product_id);

CREATE TRIGGER product_company_roles_active_references_insert
BEFORE INSERT ON product_company_roles
WHEN NOT EXISTS (SELECT 1 FROM products WHERE id = NEW.product_id AND status = 'active')
 OR NOT EXISTS (SELECT 1 FROM pharmaceutical_companies WHERE id = NEW.company_id AND status = 'active')
BEGIN
    SELECT RAISE(ABORT, 'product_company_role_reference_conflict');
END;

CREATE TRIGGER product_company_roles_active_references_update
BEFORE UPDATE OF product_id, company_id, status ON product_company_roles
WHEN NEW.status = 'active' AND (
    NOT EXISTS (SELECT 1 FROM products WHERE id = NEW.product_id AND status = 'active')
    OR NOT EXISTS (SELECT 1 FROM pharmaceutical_companies WHERE id = NEW.company_id AND status = 'active')
)
BEGIN
    SELECT RAISE(ABORT, 'product_company_role_reference_conflict');
END;

CREATE TABLE product_packs (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    container_unit_id TEXT NOT NULL REFERENCES units_of_measure(id) ON DELETE RESTRICT,
    base_quantity_atoms INTEGER NOT NULL CHECK (base_quantity_atoms BETWEEN 1 AND 9000000000000000),
    contained_pack_id TEXT REFERENCES product_packs(id) ON DELETE RESTRICT,
    contained_pack_count INTEGER CHECK (
        contained_pack_count IS NULL OR contained_pack_count BETWEEN 1 AND 9000000000000000
    ),
    sku_code TEXT CHECK (
        sku_code IS NULL OR (
            sku_code = upper(trim(sku_code)) AND length(sku_code) BETWEEN 1 AND 64
            AND sku_code NOT GLOB '*[^A-Z0-9._/-]*'
        )
    ),
    sku_store_id TEXT REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    display_label TEXT CHECK (
        display_label IS NULL OR length(trim(display_label)) BETWEEN 1 AND 200
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    CHECK (
        (contained_pack_id IS NULL AND contained_pack_count IS NULL)
        OR (contained_pack_id IS NOT NULL AND contained_pack_count IS NOT NULL)
    ),
    CHECK ((sku_code IS NULL AND sku_store_id IS NULL) OR (sku_code IS NOT NULL AND sku_store_id IS NOT NULL)),
    CHECK (contained_pack_id IS NULL OR contained_pack_id <> id),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    ),
    UNIQUE (id, product_id)
) STRICT;

CREATE UNIQUE INDEX product_packs_active_sku_uq
ON product_packs(sku_store_id, sku_code)
WHERE status = 'active' AND sku_code IS NOT NULL;
CREATE INDEX product_packs_product_idx ON product_packs(product_id, status);
CREATE INDEX product_packs_contained_idx ON product_packs(contained_pack_id);

CREATE TRIGGER product_packs_integrity_insert
BEFORE INSERT ON product_packs
WHEN
    NOT EXISTS (
        SELECT 1 FROM products product JOIN units_of_measure unit ON unit.id = NEW.container_unit_id
        WHERE product.id = NEW.product_id AND product.status = 'active' AND unit.status = 'active'
          AND (
              unit.dimension = 'container'
              OR (unit.id = product.base_unit_id AND NEW.base_quantity_atoms = CASE product.quantity_scale
                  WHEN 0 THEN 1 WHEN 1 THEN 10 WHEN 2 THEN 100 WHEN 3 THEN 1000
                  WHEN 4 THEN 10000 WHEN 5 THEN 100000 WHEN 6 THEN 1000000 END)
          )
    )
    OR (
        NEW.contained_pack_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM product_packs contained
            WHERE contained.id = NEW.contained_pack_id
              AND contained.product_id = NEW.product_id
              AND contained.status = 'active'
              AND NEW.base_quantity_atoms = contained.base_quantity_atoms * NEW.contained_pack_count
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'pack_conversion_conflict');
END;

CREATE TRIGGER product_packs_integrity_update
BEFORE UPDATE OF product_id, container_unit_id, base_quantity_atoms, contained_pack_id, contained_pack_count, status
ON product_packs
WHEN
    NOT EXISTS (
        SELECT 1 FROM products product JOIN units_of_measure unit ON unit.id = NEW.container_unit_id
        WHERE product.id = NEW.product_id AND product.status = 'active' AND unit.status = 'active'
          AND (
              unit.dimension = 'container'
              OR (unit.id = product.base_unit_id AND NEW.base_quantity_atoms = CASE product.quantity_scale
                  WHEN 0 THEN 1 WHEN 1 THEN 10 WHEN 2 THEN 100 WHEN 3 THEN 1000
                  WHEN 4 THEN 10000 WHEN 5 THEN 100000 WHEN 6 THEN 1000000 END)
          )
    )
    OR (
        NEW.contained_pack_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM product_packs contained
            WHERE contained.id = NEW.contained_pack_id
              AND contained.product_id = NEW.product_id
              AND contained.status = 'active'
              AND NEW.base_quantity_atoms = contained.base_quantity_atoms * NEW.contained_pack_count
        )
    )
    OR EXISTS (
        WITH RECURSIVE ancestors(id) AS (
            SELECT NEW.contained_pack_id
            UNION ALL
            SELECT pack.contained_pack_id
            FROM product_packs pack JOIN ancestors ON pack.id = ancestors.id
            WHERE pack.contained_pack_id IS NOT NULL
        )
        SELECT 1 FROM ancestors WHERE id = NEW.id
    )
BEGIN
    SELECT RAISE(ABORT, 'pack_conversion_conflict');
END;

CREATE TABLE store_pack_policies (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    store_id TEXT NOT NULL REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    product_id TEXT NOT NULL REFERENCES products(id) ON DELETE RESTRICT,
    pack_id TEXT NOT NULL,
    purchase_enabled INTEGER NOT NULL CHECK (purchase_enabled IN (0, 1)),
    sale_enabled INTEGER NOT NULL CHECK (sale_enabled IN (0, 1)),
    whole_pack_only_purchase INTEGER NOT NULL CHECK (whole_pack_only_purchase IN (0, 1)),
    fractional_sale_allowed INTEGER NOT NULL CHECK (fractional_sale_allowed IN (0, 1)),
    minimum_sale_increment_atoms INTEGER NOT NULL CHECK (
        minimum_sale_increment_atoms BETWEEN 1 AND 9000000000000000
    ),
    default_purchase_pack INTEGER NOT NULL CHECK (default_purchase_pack IN (0, 1)),
    default_sale_pack INTEGER NOT NULL CHECK (default_sale_pack IN (0, 1)),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    FOREIGN KEY (pack_id, product_id) REFERENCES product_packs(id, product_id) ON DELETE RESTRICT,
    CHECK (default_purchase_pack = 0 OR purchase_enabled = 1),
    CHECK (default_sale_pack = 0 OR sale_enabled = 1),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX store_pack_policies_active_pack_uq
ON store_pack_policies(store_id, pack_id) WHERE status = 'active';
CREATE UNIQUE INDEX store_pack_policies_default_purchase_uq
ON store_pack_policies(store_id, product_id)
WHERE status = 'active' AND default_purchase_pack = 1;
CREATE UNIQUE INDEX store_pack_policies_default_sale_uq
ON store_pack_policies(store_id, product_id)
WHERE status = 'active' AND default_sale_pack = 1;

CREATE TRIGGER store_pack_policies_quantity_insert
BEFORE INSERT ON store_pack_policies
WHEN NOT EXISTS (
    SELECT 1 FROM products product JOIN product_packs pack ON pack.id = NEW.pack_id
    WHERE product.id = NEW.product_id AND product.status = 'active' AND pack.status = 'active'
      AND NEW.minimum_sale_increment_atoms <= pack.base_quantity_atoms
      AND (product.quantity_scale > 0 OR NEW.fractional_sale_allowed = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'pack_policy_conflict');
END;

CREATE TRIGGER store_pack_policies_quantity_update
BEFORE UPDATE OF product_id, pack_id, fractional_sale_allowed, minimum_sale_increment_atoms, status
ON store_pack_policies
WHEN NEW.status = 'active' AND NOT EXISTS (
    SELECT 1 FROM products product JOIN product_packs pack ON pack.id = NEW.pack_id
    WHERE product.id = NEW.product_id AND product.status = 'active'
      AND pack.product_id = NEW.product_id AND pack.status = 'active'
      AND NEW.minimum_sale_increment_atoms <= pack.base_quantity_atoms
      AND (product.quantity_scale > 0 OR NEW.fractional_sale_allowed = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'pack_policy_conflict');
END;

CREATE TABLE barcodes (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    pack_id TEXT NOT NULL REFERENCES product_packs(id) ON DELETE RESTRICT,
    namespace TEXT NOT NULL CHECK (
        namespace = lower(trim(namespace)) AND length(namespace) BETWEEN 1 AND 32
        AND namespace NOT GLOB '*[^a-z0-9._-]*'
    ),
    normalized_value TEXT NOT NULL CHECK (
        normalized_value = upper(trim(normalized_value))
        AND length(normalized_value) BETWEEN 1 AND 64
        AND normalized_value NOT GLOB '*[^A-Z0-9._/-]*'
    ),
    symbology TEXT CHECK (
        symbology IS NULL OR (symbology = lower(trim(symbology)) AND length(symbology) BETWEEN 1 AND 32)
    ),
    scope TEXT NOT NULL CHECK (scope IN ('global', 'store')),
    store_id TEXT REFERENCES store_identity(store_id) ON DELETE RESTRICT,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    archived_at_utc TEXT CHECK (
        archived_at_utc IS NULL OR archived_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    archive_reason TEXT,
    CHECK ((scope = 'global' AND store_id IS NULL) OR (scope = 'store' AND store_id IS NOT NULL)),
    CHECK (namespace <> 'internal' OR scope = 'store'),
    CHECK (
        (status = 'active' AND archived_at_utc IS NULL AND archive_reason IS NULL)
        OR (status = 'archived' AND archived_at_utc IS NOT NULL AND length(trim(archive_reason)) > 0)
    )
) STRICT;

CREATE UNIQUE INDEX barcodes_active_global_uq
ON barcodes(namespace, normalized_value)
WHERE status = 'active' AND scope = 'global';
CREATE UNIQUE INDEX barcodes_active_store_uq
ON barcodes(store_id, namespace, normalized_value)
WHERE status = 'active' AND scope = 'store';
CREATE INDEX barcodes_pack_idx ON barcodes(pack_id, status);

CREATE TRIGGER barcodes_pack_active_insert
BEFORE INSERT ON barcodes
WHEN NOT EXISTS (SELECT 1 FROM product_packs WHERE id = NEW.pack_id AND status = 'active')
BEGIN
    SELECT RAISE(ABORT, 'barcode_pack_conflict');
END;

CREATE TRIGGER barcodes_pack_active_update
BEFORE UPDATE OF pack_id, status ON barcodes
WHEN NEW.status = 'active'
 AND NOT EXISTS (SELECT 1 FROM product_packs WHERE id = NEW.pack_id AND status = 'active')
BEGIN
    SELECT RAISE(ABORT, 'barcode_pack_conflict');
END;
