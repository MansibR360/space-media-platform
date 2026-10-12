-- Music intelligence: per-track audio analysis, a listen log, and the derived
-- per-user taste profile that powers recommendations.
--
-- Also repairs source_ratings.score: declared REAL (FLOAT4) but decoded as f64
-- (FLOAT8), which made the ratings reload fail on every boot.

ALTER TABLE source_ratings ALTER COLUMN score TYPE DOUBLE PRECISION;

-- One row per music item. Populated by the analyzer (ffprobe/ffmpeg); rows may
-- exist with analyzed_at NULL while queued, or with last_error set on failure.
CREATE TABLE music_analysis (
    media_id            UUID PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
    file_id             UUID,
    duration_seconds    DOUBLE PRECISION,
    sample_rate         INTEGER,
    channels            INTEGER,
    bitrate_kbps        INTEGER,
    codec               TEXT,
    -- integrated loudness + true peak, from ffmpeg's ebur128 filter
    loudness_lufs       DOUBLE PRECISION,
    peak_db             DOUBLE PRECISION,
    -- derived descriptors, 0..1 unless noted
    tempo_bpm           DOUBLE PRECISION,
    energy              DOUBLE PRECISION,
    brightness          DOUBLE PRECISION,
    dynamic_range       DOUBLE PRECISION,
    -- normalized peak envelope: JSON array of `waveform_buckets` floats in 0..1
    waveform            JSONB,
    waveform_buckets    INTEGER,
    analysis_version    INTEGER NOT NULL DEFAULT 1,
    analyzed_at         TIMESTAMPTZ,
    last_error          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_music_analysis_pending ON music_analysis (analyzed_at) WHERE analyzed_at IS NULL;
CREATE INDEX idx_music_analysis_tempo  ON music_analysis (tempo_bpm);
CREATE INDEX idx_music_analysis_energy ON music_analysis (energy);

-- Append-only listen log. Every play the client reports lands here; the
-- affinity table is a derived rollup, so this stays the source of truth.
CREATE TABLE music_listens (
    id              UUID PRIMARY KEY,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_id        UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    seconds_played  DOUBLE PRECISION NOT NULL DEFAULT 0,
    track_seconds   DOUBLE PRECISION,
    percent_played  DOUBLE PRECISION,
    completed       BOOLEAN NOT NULL DEFAULT FALSE,
    -- abandoned early; a strong negative preference signal
    skipped         BOOLEAN NOT NULL DEFAULT FALSE,
    source          TEXT,
    device          TEXT,
    -- where the play came from: playlist | radio | search | recommendation
    context         TEXT,
    hour_of_day     SMALLINT,
    weekday         SMALLINT
);

CREATE INDEX idx_music_listens_user_time ON music_listens (user_id, started_at DESC);
CREATE INDEX idx_music_listens_media     ON music_listens (media_id);
CREATE INDEX idx_music_listens_user_media ON music_listens (user_id, media_id);

-- Derived taste, one row per (user, dimension, key). Weight is decayed at read
-- time against last_seen so stale interests fade without a sweep job.
CREATE TABLE music_affinity (
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    dimension  TEXT NOT NULL,
    key        TEXT NOT NULL,
    weight     DOUBLE PRECISION NOT NULL DEFAULT 0,
    plays      INTEGER NOT NULL DEFAULT 0,
    skips      INTEGER NOT NULL DEFAULT 0,
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, dimension, key)
);

CREATE INDEX idx_music_affinity_user ON music_affinity (user_id, weight DESC);
