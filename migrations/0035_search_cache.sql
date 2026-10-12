-- Persistent search cache.
--
-- A unified search fans out to every enabled source, which costs seconds and a
-- few hundred upstream requests. The answer for a given query rarely changes
-- between one viewer and the next, so it is kept: the first person to search a
-- term pays for it, everyone after that is served from here.
--
-- Deliberately unbounded. There is no eviction and no size cap; entries are
-- refreshed in place rather than replaced, so the table grows with the number
-- of distinct queries ever asked and stays useful indefinitely.

CREATE TABLE search_cache (
    -- normalized query + category + limit, so callers asking the same thing
    -- share an entry regardless of spacing or case
    query_key TEXT PRIMARY KEY,
    -- the query as the first caller typed it, for reporting
    query TEXT NOT NULL,
    category TEXT,
    result_limit INTEGER NOT NULL,
    -- the exact response body that was served
    payload JSONB NOT NULL,
    result_count INTEGER NOT NULL,
    -- how much work this entry has saved
    hits BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_hit_at TIMESTAMPTZ
);

-- Finding entries due for a background refresh.
CREATE INDEX idx_search_cache_refreshed ON search_cache (refreshed_at);

-- "What do people actually search for" — powers reporting and lets a future
-- warmer prioritise the popular queries.
CREATE INDEX idx_search_cache_hits ON search_cache (hits DESC);
