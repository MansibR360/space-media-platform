-- 0016: media protection core (key service), failure analysis, adaptive privacy.

-- Per-media encryption keys, stored wrapped under a purpose KEK derived from
-- the master secret (HKDF). Plaintext keys never touch the database.
CREATE TABLE IF NOT EXISTS media_keys (
    id UUID PRIMARY KEY,
    media_ref TEXT NOT NULL,
    key_version INTEGER NOT NULL DEFAULT 1,
    wrapped_key BYTEA NOT NULL,
    wrap_nonce BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    rotated_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    UNIQUE (media_ref, key_version)
);

CREATE INDEX IF NOT EXISTS idx_media_keys_ref ON media_keys (media_ref, key_version DESC);

-- Autonomous failure analysis: one row per 5xx, grouped views on top.
CREATE TABLE IF NOT EXISTS failure_events (
    id UUID PRIMARY KEY,
    method TEXT NOT NULL,
    route TEXT NOT NULL,
    status INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    detail TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_failures_created ON failure_events (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_failures_route ON failure_events (route, created_at DESC);

-- Adaptive privacy: daily aggregates replace raw platform events as they age.
CREATE TABLE IF NOT EXISTS event_rollups (
    event TEXT NOT NULL,
    day DATE NOT NULL,
    count BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (event, day)
);
