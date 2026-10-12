ALTER TABLE external_mappings ADD COLUMN IF NOT EXISTS confidence REAL NOT NULL DEFAULT 0;
ALTER TABLE media_files ADD COLUMN IF NOT EXISTS hdr BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE media_files ADD COLUMN IF NOT EXISTS replaygain_db DOUBLE PRECISION;

CREATE TABLE IF NOT EXISTS metadata_changes (
    id UUID PRIMARY KEY,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    field TEXT NOT NULL,
    old_value TEXT,
    new_value TEXT,
    source TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_metadata_changes_media ON metadata_changes (media_id, created_at DESC);

CREATE TABLE IF NOT EXISTS automations (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    when_event TEXT NOT NULL CHECK (when_event IN ('media.added', 'playback.started', 'payment.approved')),
    filter JSONB NOT NULL DEFAULT '{}',
    actions JSONB NOT NULL DEFAULT '[]',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    run_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE media DROP CONSTRAINT IF EXISTS media_media_type_check;
ALTER TABLE media ADD CONSTRAINT media_media_type_check
    CHECK (media_type IN ('movie', 'series', 'book', 'music', 'manga', 'podcast', 'audiobook'));
