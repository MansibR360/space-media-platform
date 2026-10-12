-- Permanent TMDB cache.
--
-- Every source keyed by TMDB asks it the same questions: the same search for a
-- popular title, the same detail lookup for the same id, once per source per
-- request. That is slow and burns a rate limit on answers that do not change.
--
-- So they are kept. Like search_cache this is deliberately unbounded: TMDB's
-- answer for "movie 157336" is the same today as next year, and an entry that
-- is never evicted is an entry that never has to be fetched twice.

CREATE TABLE tmdb_cache (
    -- the request path and query, minus the api key
    request_key TEXT PRIMARY KEY,
    payload JSONB NOT NULL,
    hits BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_hit_at TIMESTAMPTZ
);

-- "what gets asked for" — for reporting, and for refreshing the busy entries
-- first if that is ever wanted.
CREATE INDEX idx_tmdb_cache_hits ON tmdb_cache (hits DESC);
