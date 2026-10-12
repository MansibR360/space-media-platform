-- Daily source benchmarks & ratings.
--
-- One row per source (native | web | hub | extension | plugin), refreshed by
-- the daily benchmark worker and blended over time so scores stay current
-- without flapping. `score` (0..100) drives provider ordering everywhere:
-- fan-out order, failover priority, and the fastest-stream tiebreaker.

CREATE TABLE IF NOT EXISTS source_ratings (
    source TEXT PRIMARY KEY,
    last_checked TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_ok TIMESTAMPTZ,
    latency_ms INTEGER,
    search_ok BOOLEAN NOT NULL DEFAULT FALSE,
    resolve_ok BOOLEAN NOT NULL DEFAULT FALSE,
    streams_found INTEGER NOT NULL DEFAULT 0,
    speed_kbps INTEGER,
    score REAL NOT NULL DEFAULT 50
);
