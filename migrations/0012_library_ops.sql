ALTER TABLE library_paths ADD COLUMN IF NOT EXISTS restricted BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE media_files ADD COLUMN IF NOT EXISTS last_seen_at TIMESTAMPTZ;
ALTER TABLE media_files ADD COLUMN IF NOT EXISTS missing BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE media_files ADD COLUMN IF NOT EXISTS probe_failed BOOLEAN NOT NULL DEFAULT FALSE;

CREATE TABLE IF NOT EXISTS file_events (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('added', 'replaced', 'removed', 'probe_failed', 'probe_ok')),
    detail JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_file_events_file ON file_events (file_id, created_at DESC);

CREATE TABLE IF NOT EXISTS search_history (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    query TEXT NOT NULL,
    result_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_search_history_user ON search_history (user_id, created_at DESC);

CREATE TABLE IF NOT EXISTS webhooks (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    secret TEXT NOT NULL,
    events TEXT NOT NULL DEFAULT '*',
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status TEXT,
    last_fired_at TIMESTAMPTZ
);

ALTER TABLE parties ADD COLUMN IF NOT EXISTS password_hash TEXT;
