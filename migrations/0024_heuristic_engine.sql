-- Heuristic Engine (0024): SPACE's built-in learning system.
-- An append-only behavior log fed automatically from in-app actions
-- (progress saves, completions, rewatches, ratings, favorites).
-- The intelligence layer aggregates this stream at read time.

CREATE TABLE heuristic_events (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID REFERENCES media (id) ON DELETE CASCADE,
    episode_id UUID,
    action TEXT NOT NULL CHECK (action IN ('play', 'complete', 'rewatch', 'rate', 'favorite', 'drop')),
    weight REAL NOT NULL DEFAULT 1.0,
    percent REAL,
    rating REAL,
    effective_seconds INTEGER NOT NULL DEFAULT 0,
    dedupe_key TEXT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotent recording: repeated position saves inside one hour count once.
CREATE UNIQUE INDEX idx_heuristic_events_dedupe
    ON heuristic_events (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL;

CREATE INDEX idx_heuristic_events_user_time ON heuristic_events (user_id, occurred_at DESC);
CREATE INDEX idx_heuristic_events_user_media ON heuristic_events (user_id, media_id);
