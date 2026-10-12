-- PostgreSQL-only search accelerators. Applied opportunistically by db::migrate:
-- each statement runs independently and failures are ignored so the schema
-- stays portable to CockroachDB (and other PG-wire databases without these
-- extensions). Full-text queries fall back to ILIKE automatically when absent.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX IF NOT EXISTS idx_media_title_trgm ON media USING GIN (title gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_media_overview_trgm ON media USING GIN (overview gin_trgm_ops);
