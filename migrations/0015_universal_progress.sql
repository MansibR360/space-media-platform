-- 0015: universal media graph, event bus, personal media twin snapshots,
-- and the unified progress view (video/audio seconds + reading pages).

-- ---------------------------------------------------------------- graph

CREATE TABLE IF NOT EXISTS media_relationships (
    id UUID PRIMARY KEY,
    from_media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    to_media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    relation TEXT NOT NULL CHECK (relation IN (
        'adaptation_of', 'based_on', 'sequel_of', 'prequel_of',
        'spin_off_of', 'remake_of', 'side_story_of', 'parent_of',
        'same_universe', 'same_franchise', 'alternate_version_of', 'related_to'
    )),
    source TEXT NOT NULL DEFAULT 'manual',
    confidence REAL NOT NULL DEFAULT 1.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_media_id, to_media_id, relation)
);

CREATE INDEX IF NOT EXISTS idx_media_rel_from ON media_relationships (from_media_id);
CREATE INDEX IF NOT EXISTS idx_media_rel_to ON media_relationships (to_media_id);

-- ---------------------------------------------------------------- event bus

CREATE TABLE IF NOT EXISTS events (
    id UUID PRIMARY KEY,
    event TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    actor UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_events_created ON events (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_events_event ON events (event, created_at DESC);

-- ---------------------------------------------------------------- twin

CREATE TABLE IF NOT EXISTS twin_snapshots (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    snapshot_date DATE NOT NULL,
    twin JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, snapshot_date)
);

-- --------------------------------------------------- unified progress view

-- One shape for every media type:
--   video/music/podcast/audiobook -> position_kind 'seconds'
--   book/manga                    -> position_kind 'page' (+ percent)
CREATE OR REPLACE VIEW unified_progress AS
SELECT
    wh.user_id,
    wh.media_id,
    m.media_type,
    m.title,
    m.poster_url,
    'seconds'::text AS position_kind,
    wh.episode_id,
    wh.position_seconds AS position,
    wh.duration_seconds AS total,
    NULL::integer AS page,
    CASE WHEN wh.duration_seconds > 0
         THEN LEAST(wh.position_seconds / wh.duration_seconds * 100.0, 100.0)
         ELSE 0.0 END AS percent,
    wh.completed,
    wh.updated_at
FROM watch_history wh
JOIN media m ON m.id = wh.media_id
WHERE m.media_type IN ('movie', 'series', 'music', 'podcast', 'audiobook')
UNION ALL
SELECT
    rp.user_id,
    rp.media_id,
    m.media_type,
    m.title,
    m.poster_url,
    'page'::text AS position_kind,
    NULL::uuid AS episode_id,
    COALESCE(rp.page, 0)::float8 AS position,
    NULL::float8 AS total,
    rp.page,
    LEAST(rp.position_percent, 100.0) AS percent,
    rp.completed,
    rp.updated_at
FROM reading_progress rp
JOIN media m ON m.id = rp.media_id;
