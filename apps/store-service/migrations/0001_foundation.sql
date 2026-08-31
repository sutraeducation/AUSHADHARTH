-- Phase 0 identity and metadata only. No pharmacy business tables belong here.
CREATE TABLE application_metadata (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    schema_generation INTEGER NOT NULL CHECK (schema_generation >= 0),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

INSERT INTO application_metadata (singleton, schema_generation, created_at_utc)
VALUES (1, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE installation_identity (
    installation_id TEXT PRIMARY KEY NOT NULL,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;

CREATE TABLE store_identity (
    store_id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    business_time_zone TEXT NOT NULL,
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z')
) STRICT;
