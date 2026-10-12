-- Stremio-compatible extension system (separate from plugins/web sources).
--
-- Extensions are remote addons speaking the Stremio Addon SDK protocol:
--   GET {base}/manifest.json
--   GET {base}/catalog/{type}/{catalogId}/{encoded extras}.json
--   GET {base}/stream/{type}/{id}.json          (id: tt… | tmdb:… | mal:…)
--   GET {base}/meta/{type}/{id}.json
--   GET {base}/subtitles/{type}/{id}.json

CREATE TABLE IF NOT EXISTS extensions (
    id UUID PRIMARY KEY,
    manifest_url TEXT NOT NULL UNIQUE,
    manifest JSONB NOT NULL,
    addon_id TEXT NOT NULL,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_extensions_enabled ON extensions (enabled) WHERE enabled;
