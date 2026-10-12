-- 0017: offline playback licenses (24 h by default) + segment-scoped grant support.

CREATE TABLE IF NOT EXISTS offline_licenses (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_ref TEXT NOT NULL,
    device_hash TEXT NOT NULL DEFAULT '',
    issued_count INTEGER NOT NULL DEFAULT 1,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, media_ref, device_hash)
);

CREATE INDEX IF NOT EXISTS idx_offline_licenses_user
    ON offline_licenses (user_id, expires_at DESC);
