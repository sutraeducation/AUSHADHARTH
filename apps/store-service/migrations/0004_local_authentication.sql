-- Phase 1C-A local user identity, opaque sessions, and bounded login protection.

CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    login_identifier TEXT NOT NULL CHECK (length(login_identifier) BETWEEN 3 AND 64),
    normalized_login_identifier TEXT NOT NULL UNIQUE CHECK (
        normalized_login_identifier = lower(trim(normalized_login_identifier))
        AND length(normalized_login_identifier) BETWEEN 3 AND 64
        AND normalized_login_identifier NOT GLOB '*[^a-z0-9._-]*'
    ),
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) BETWEEN 1 AND 120),
    password_hash TEXT NOT NULL CHECK (password_hash LIKE '$argon2id$%'),
    role TEXT NOT NULL CHECK (role IN ('owner_admin', 'pharmacist', 'cashier')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled', 'archived')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    updated_at_utc TEXT NOT NULL CHECK (updated_at_utc GLOB '????-??-??T??:??:??*Z'),
    last_login_at_utc TEXT CHECK (
        last_login_at_utc IS NULL OR last_login_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    status_changed_at_utc TEXT CHECK (
        status_changed_at_utc IS NULL OR status_changed_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    status_reason TEXT,
    CHECK (
        (status = 'active' AND status_changed_at_utc IS NULL AND status_reason IS NULL)
        OR (status IN ('disabled', 'archived') AND status_changed_at_utc IS NOT NULL
            AND length(trim(status_reason)) > 0)
    )
) STRICT;

CREATE INDEX users_status_idx ON users(status, normalized_login_identifier);

CREATE TABLE user_sessions (
    id TEXT PRIMARY KEY NOT NULL CHECK (
        length(id) = 36 AND substr(id, 15, 1) = '7'
        AND lower(substr(id, 20, 1)) IN ('8', '9', 'a', 'b')
    ),
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    token_hash TEXT NOT NULL UNIQUE CHECK (
        length(token_hash) = 64 AND token_hash = lower(token_hash)
        AND token_hash NOT GLOB '*[^a-f0-9]*'
    ),
    created_at_utc TEXT NOT NULL CHECK (created_at_utc GLOB '????-??-??T??:??:??*Z'),
    expires_at_utc TEXT NOT NULL CHECK (expires_at_utc GLOB '????-??-??T??:??:??*Z'),
    last_seen_at_utc TEXT NOT NULL CHECK (last_seen_at_utc GLOB '????-??-??T??:??:??*Z'),
    revoked_at_utc TEXT CHECK (
        revoked_at_utc IS NULL OR revoked_at_utc GLOB '????-??-??T??:??:??*Z'
    ),
    CHECK (expires_at_utc > created_at_utc),
    CHECK (last_seen_at_utc >= created_at_utc)
) STRICT;

CREATE INDEX user_sessions_user_idx ON user_sessions(user_id, expires_at_utc);
CREATE INDEX user_sessions_active_idx ON user_sessions(token_hash, expires_at_utc)
WHERE revoked_at_utc IS NULL;

CREATE TABLE login_attempts (
    identifier_hash TEXT PRIMARY KEY NOT NULL CHECK (
        length(identifier_hash) = 64 AND identifier_hash = lower(identifier_hash)
        AND identifier_hash NOT GLOB '*[^a-f0-9]*'
    ),
    failure_count INTEGER NOT NULL CHECK (failure_count BETWEEN 1 AND 1000),
    window_started_at_utc TEXT NOT NULL CHECK (window_started_at_utc GLOB '????-??-??T??:??:??*Z'),
    last_failed_at_utc TEXT NOT NULL CHECK (last_failed_at_utc GLOB '????-??-??T??:??:??*Z'),
    cooldown_until_utc TEXT CHECK (
        cooldown_until_utc IS NULL OR cooldown_until_utc GLOB '????-??-??T??:??:??*Z'
    )
) STRICT;

CREATE INDEX login_attempts_last_failed_idx ON login_attempts(last_failed_at_utc);
