-- Permanent magnet cache.
--
-- A magnet link never changes: the info hash *is* the content. So once a
-- torrent has been seen there is no reason to ask an index for it again, and
-- the same search should be answerable from here.
--
-- Seeder counts do change, and they are the only thing that decides whether a
-- torrent works — so those are refreshed in place whenever an index is
-- consulted, while the magnet itself is written once.

CREATE TABLE torrent_cache (
    -- the info hash identifies the torrent everywhere; two indexes listing the
    -- same release collapse to one row
    info_hash TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    -- normalized title, so the same film from different indexes can be
    -- recognised as the same film and only the best-seeded copy shown
    title_key TEXT NOT NULL,
    magnet TEXT NOT NULL,
    seeders INTEGER NOT NULL DEFAULT 0,
    size_bytes BIGINT NOT NULL DEFAULT 0,
    category TEXT NOT NULL DEFAULT 'movie',
    -- which index it came from, for reporting
    source TEXT NOT NULL DEFAULT '',
    -- the normalized query that found it, so a repeat search is served locally
    query_key TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Answering a repeat search without touching an index.
CREATE INDEX idx_torrent_cache_query ON torrent_cache (query_key);

-- Picking the best-seeded copy of the same release.
CREATE INDEX idx_torrent_cache_title ON torrent_cache (title_key, seeders DESC);
