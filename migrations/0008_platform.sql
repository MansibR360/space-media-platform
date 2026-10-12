ALTER TABLE media DROP CONSTRAINT media_media_type_check;
ALTER TABLE media ADD CONSTRAINT media_media_type_check
    CHECK (media_type IN ('movie', 'series', 'book'));

ALTER TABLE media ADD COLUMN IF NOT EXISTS page_count INTEGER;
ALTER TABLE media ADD COLUMN IF NOT EXISTS publisher TEXT;
ALTER TABLE media ADD COLUMN IF NOT EXISTS book_format TEXT;

CREATE INDEX IF NOT EXISTS idx_media_book ON media (publisher) WHERE media_type = 'book';

CREATE TABLE reading_progress (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    position_percent DOUBLE PRECISION NOT NULL DEFAULT 0,
    page INTEGER,
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_id)
);

CREATE INDEX idx_reading_progress_recent ON reading_progress (user_id, updated_at DESC);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT 'read',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX idx_api_keys_user ON api_keys (user_id);

CREATE TABLE devices (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    platform TEXT NOT NULL DEFAULT 'unknown',
    last_ip TEXT,
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);

CREATE INDEX idx_devices_user ON devices (user_id, last_seen_at DESC);

CREATE TABLE notifications (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('system','follow','party_invite','new_episode','new_library_item','download_ready')),
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    data JSONB NOT NULL DEFAULT '{}',
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_notifications_user ON notifications (user_id, created_at DESC);

CREATE TABLE signing_secrets (
    purpose TEXT PRIMARY KEY,
    secret TEXT NOT NULL,
    rotated_at TIMESTAMPTZ
);

CREATE TABLE download_grants (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    file_id UUID REFERENCES media_files (id) ON DELETE CASCADE,
    nonce TEXT NOT NULL UNIQUE,
    hit_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_download_grants_user ON download_grants (user_id, created_at DESC);

