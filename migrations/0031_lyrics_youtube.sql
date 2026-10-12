-- Lyrics pipeline: multi-provider fetch (Better Lyrics, LRCLIB, NetEase,
-- Genius) with a server-side cache so repeat lookups never leave the box.
CREATE TABLE lyrics_cache (
    id UUID PRIMARY KEY,
    cache_key TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    artist TEXT NOT NULL DEFAULT '',
    media_id UUID REFERENCES media (id) ON DELETE SET NULL,
    video_id TEXT,
    synced BOOLEAN NOT NULL DEFAULT FALSE,
    format TEXT NOT NULL DEFAULT 'lrc' CHECK (format IN ('lrc', 'ttml', 'plain')),
    content JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_lyrics_media ON lyrics_cache (media_id);

-- YouTube playback history is deliberately SEPARATE from watch_history:
-- YouTube-sourced tracks have no catalog row, and mixing them into the
-- library rails would pollute continue-watching and popularity signals.
CREATE TABLE youtube_history (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    video_id TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    channel TEXT NOT NULL DEFAULT '',
    thumbnail TEXT,
    duration_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    position_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    plays INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, video_id)
);
CREATE INDEX idx_youtube_history_user ON youtube_history (user_id, updated_at DESC);
