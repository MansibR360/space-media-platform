-- Learned web sources ("galaxy recipes").
--
-- A web source turns any media website into a searchable/playable catalog by
-- storing a scrape recipe inferred by the bundled galaxy scraper
-- (`galaxy source learn`).

CREATE TABLE IF NOT EXISTS web_sources (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL DEFAULT '',
    site TEXT NOT NULL,
    category TEXT NOT NULL DEFAULT 'movie',
    search_url TEXT NOT NULL,
    sample_item TEXT,
    recipe JSONB NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    status TEXT NOT NULL DEFAULT 'learned',
    last_error TEXT,
    items_sampled INTEGER NOT NULL DEFAULT 0,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_web_sources_enabled ON web_sources (enabled) WHERE enabled;
