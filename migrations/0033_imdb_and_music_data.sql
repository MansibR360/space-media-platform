-- IMDb identity/ratings on the catalog, and the global music dataset.
--
-- Two separable halves:
--   1. `media` gains IMDb identity and the ratings that hang off it, so a title
--      can be matched to IMDb regardless of which provider imported it.
--   2. Music gains a *global* layer. Track analysis and engagement are shared
--      by every user (one waveform, one set of descriptors, one play count);
--      only `music_affinity` and `music_listens` are per-user. That split is
--      what lets the dataset compound as the userbase listens.

-- ========================================================================
-- IMDb
-- ========================================================================

ALTER TABLE media
    ADD COLUMN imdb_id        TEXT,
    ADD COLUMN imdb_rating    DOUBLE PRECISION,
    ADD COLUMN imdb_votes     INTEGER,
    ADD COLUMN metascore      INTEGER,
    ADD COLUMN content_rating TEXT,
    ADD COLUMN imdb_synced_at TIMESTAMPTZ;

-- Partial unique: many rows legitimately have no IMDb match, but a given
-- tconst must never map to two catalog rows.
CREATE UNIQUE INDEX idx_media_imdb_id ON media (imdb_id) WHERE imdb_id IS NOT NULL;
CREATE INDEX idx_media_imdb_rating ON media (imdb_rating DESC NULLS LAST);
-- Drives the backfill sweep: never-synced rows sort first.
CREATE INDEX idx_media_imdb_stale ON media (imdb_synced_at NULLS FIRST)
    WHERE imdb_id IS NOT NULL;

-- ========================================================================
-- Advanced audio analysis (global, one row per track)
-- ========================================================================

ALTER TABLE music_analysis
    -- container tags: the cheapest metadata there is, and previously dropped
    ADD COLUMN artist             TEXT,
    ADD COLUMN album              TEXT,
    ADD COLUMN album_artist       TEXT,
    ADD COLUMN track_no           INTEGER,
    ADD COLUMN disc_no            INTEGER,
    ADD COLUMN release_year       INTEGER,
    ADD COLUMN tag_genre          TEXT,
    ADD COLUMN tags               JSONB,

    -- second envelope: peaks show transients, RMS shows perceived level
    ADD COLUMN rms_envelope       JSONB,

    -- spectral shape, averaged over the analysis frames
    ADD COLUMN spectral_centroid  DOUBLE PRECISION,
    ADD COLUMN spectral_rolloff   DOUBLE PRECISION,
    ADD COLUMN spectral_flatness  DOUBLE PRECISION,
    ADD COLUMN spectral_bandwidth DOUBLE PRECISION,
    ADD COLUMN spectral_flux      DOUBLE PRECISION,
    ADD COLUMN zero_crossing_rate DOUBLE PRECISION,

    -- tonal content: 12 pitch classes, folded to a key estimate
    ADD COLUMN chroma             JSONB,
    ADD COLUMN musical_key        SMALLINT,   -- 0=C, 1=C#, ... 11=B
    ADD COLUMN musical_mode       SMALLINT,   -- 0=minor, 1=major
    ADD COLUMN key_confidence     DOUBLE PRECISION,

    -- rhythm
    ADD COLUMN tempo_confidence   DOUBLE PRECISION,
    ADD COLUMN onset_rate         DOUBLE PRECISION,
    ADD COLUMN beat_grid          JSONB,

    -- high-level descriptors, all 0..1
    ADD COLUMN danceability       DOUBLE PRECISION,
    ADD COLUMN valence            DOUBLE PRECISION,
    ADD COLUMN acousticness       DOUBLE PRECISION,
    ADD COLUMN instrumentalness   DOUBLE PRECISION,
    ADD COLUMN speechiness        DOUBLE PRECISION,
    ADD COLUMN liveness           DOUBLE PRECISION,

    -- timbre (MFCC-like) and the normalized vector used for similarity search
    ADD COLUMN timbre             JSONB,
    ADD COLUMN embedding          JSONB,

    -- coarse structural segmentation from the novelty curve
    ADD COLUMN segments           JSONB,

    ADD COLUMN analysis_ms        INTEGER;

CREATE INDEX idx_music_analysis_key   ON music_analysis (musical_key, musical_mode);
CREATE INDEX idx_music_analysis_dance ON music_analysis (danceability DESC NULLS LAST);
-- Re-analysis sweep: pick up rows produced by an older analyzer.
CREATE INDEX idx_music_analysis_version ON music_analysis (analysis_version);

-- ========================================================================
-- Global engagement. This is the half that grows with the userbase.
-- ========================================================================

-- One row per track, aggregated across every listener.
CREATE TABLE music_track_stats (
    media_id         UUID PRIMARY KEY REFERENCES media(id) ON DELETE CASCADE,
    plays            BIGINT NOT NULL DEFAULT 0,
    completions      BIGINT NOT NULL DEFAULT 0,
    skips            BIGINT NOT NULL DEFAULT 0,
    unique_listeners BIGINT NOT NULL DEFAULT 0,
    seconds_played   DOUBLE PRECISION NOT NULL DEFAULT 0,
    -- running mean of percent_played, maintained incrementally
    avg_percent      DOUBLE PRECISION NOT NULL DEFAULT 0,
    completion_rate  DOUBLE PRECISION NOT NULL DEFAULT 0,
    skip_rate        DOUBLE PRECISION NOT NULL DEFAULT 0,
    -- Wilson lower bound on completion rate: ranks by evidence, so one lucky
    -- play cannot outrank a track with a thousand.
    quality          DOUBLE PRECISION NOT NULL DEFAULT 0,
    -- plays in the last 7 days over plays in the 7 before that
    trend            DOUBLE PRECISION NOT NULL DEFAULT 0,
    first_played_at  TIMESTAMPTZ,
    last_played_at   TIMESTAMPTZ,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_music_stats_quality ON music_track_stats (quality DESC);
CREATE INDEX idx_music_stats_plays   ON music_track_stats (plays DESC);
CREATE INDEX idx_music_stats_trend   ON music_track_stats (trend DESC);

-- Presence of a row is what makes `unique_listeners` cheap to maintain: the
-- insert reports whether this user is new to the track.
CREATE TABLE music_track_listeners (
    media_id       UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    user_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plays          INTEGER NOT NULL DEFAULT 0,
    seconds_played DOUBLE PRECISION NOT NULL DEFAULT 0,
    first_played_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_played_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (media_id, user_id)
);

CREATE INDEX idx_music_track_listeners_user ON music_track_listeners (user_id);

-- "People who played this also played that." Stored in both directions so a
-- lookup is one index scan. Weight decays as it is rewritten, so the graph
-- follows current listening rather than accumulating forever.
CREATE TABLE music_cooccurrence (
    media_a    UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    media_b    UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    weight     DOUBLE PRECISION NOT NULL DEFAULT 0,
    pairs      INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (media_a, media_b),
    CHECK (media_a <> media_b)
);

CREATE INDEX idx_music_cooc_a ON music_cooccurrence (media_a, weight DESC);

-- Content-based neighbours, recomputed from `embedding` after analysis.
-- Independent of listening, so it works on day one for a brand new track.
CREATE TABLE music_similarity (
    media_a     UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    media_b     UUID NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    similarity  DOUBLE PRECISION NOT NULL,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (media_a, media_b),
    CHECK (media_a <> media_b)
);

CREATE INDEX idx_music_similarity_a ON music_similarity (media_a, similarity DESC);
