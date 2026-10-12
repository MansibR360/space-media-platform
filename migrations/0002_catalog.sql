CREATE TABLE media (
    id UUID PRIMARY KEY,
    media_type TEXT NOT NULL CHECK (media_type IN ('movie', 'series')),
    is_anime BOOLEAN NOT NULL DEFAULT FALSE,
    title TEXT NOT NULL,
    original_title TEXT,
    overview TEXT NOT NULL DEFAULT '',
    release_date DATE,
    end_date DATE,
    runtime_minutes INTEGER,
    status TEXT NOT NULL DEFAULT 'released'
        CHECK (status IN ('announced', 'releasing', 'released', 'ongoing', 'finished', 'cancelled')),
    age_rating TEXT,
    poster_url TEXT,
    backdrop_url TEXT,
    trailer_url TEXT,
    country TEXT,
    original_language TEXT,
    popularity INTEGER NOT NULL DEFAULT 0,
    rating_avg DOUBLE PRECISION NOT NULL DEFAULT 0,
    rating_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_media_title ON media (LOWER(title));
CREATE INDEX idx_media_type ON media (media_type);
CREATE INDEX idx_media_popularity ON media (popularity DESC);
CREATE INDEX idx_media_created ON media (created_at DESC);
CREATE INDEX idx_media_rating ON media (rating_avg DESC);

ALTER TABLE media ADD COLUMN search_vector tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
    setweight(to_tsvector('english', coalesce(original_title, '')), 'B') ||
    setweight(to_tsvector('english', coalesce(overview, '')), 'C')
) STORED;

CREATE INDEX idx_media_fts ON media USING GIN (search_vector);

CREATE TABLE seasons (
    id UUID PRIMARY KEY,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    title TEXT,
    overview TEXT NOT NULL DEFAULT '',
    air_date DATE,
    poster_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (media_id, number)
);

CREATE TABLE episodes (
    id UUID PRIMARY KEY,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    season_id UUID NOT NULL REFERENCES seasons (id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    absolute_number INTEGER,
    title TEXT NOT NULL,
    overview TEXT NOT NULL DEFAULT '',
    air_date DATE,
    runtime_minutes INTEGER,
    still_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (season_id, number)
);

CREATE INDEX idx_episodes_media ON episodes (media_id);
CREATE INDEX idx_episodes_season ON episodes (season_id, number);

CREATE TABLE genres (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE
);

CREATE TABLE people (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    image_url TEXT,
    bio TEXT NOT NULL DEFAULT ''
);

CREATE TABLE studios (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    country TEXT
);

CREATE TABLE media_genres (
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    genre_id UUID NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
    PRIMARY KEY (media_id, genre_id)
);

CREATE INDEX idx_media_genres_genre ON media_genres (genre_id);

CREATE TABLE media_studios (
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    studio_id UUID NOT NULL REFERENCES studios (id) ON DELETE CASCADE,
    PRIMARY KEY (media_id, studio_id)
);

CREATE TABLE credits (
    id UUID PRIMARY KEY,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    person_id UUID NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    credit_type TEXT NOT NULL CHECK (credit_type IN ('cast', 'director', 'writer', 'producer', 'composer', 'voice')),
    character_name TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE (media_id, person_id, credit_type)
);

CREATE INDEX idx_credits_media ON credits (media_id);

CREATE TABLE external_mappings (
    id UUID PRIMARY KEY,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('tmdb', 'imdb', 'mal', 'anilist', 'kitsu', 'tvdb')),
    external_id TEXT NOT NULL,
    synced_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL DEFAULT '{}',
    UNIQUE (source, external_id)
);

CREATE INDEX idx_external_mappings_media ON external_mappings (media_id);
