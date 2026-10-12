-- Library playlists: user-curated, ordered media collections.
-- Playlists hold media references ONLY — there is no text/content column by
-- design; free-form prose is rejected at the storage SPI boundary.
-- `kind`:
--   multimedia → mixed media types allowed (movies + music + books ...)
--   standard   → pinned to a single catalog media type
-- `layout` is frontend-owned presentation config (grid/sections/theme hints).
CREATE TABLE library_playlists (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) <= 120),
    kind TEXT NOT NULL DEFAULT 'multimedia' CHECK (kind IN ('multimedia', 'standard')),
    media_type TEXT,
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public', 'followers')),
    layout JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);

CREATE INDEX idx_library_playlists_user ON library_playlists (user_id, updated_at DESC);

-- Items are pure media references. The FK to media makes non-media entries
-- structurally impossible.
CREATE TABLE library_playlist_items (
    playlist_id UUID NOT NULL REFERENCES library_playlists (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    position DOUBLE PRECISION NOT NULL DEFAULT 0,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (playlist_id, media_id)
);

CREATE INDEX idx_library_playlist_items_media ON library_playlist_items (media_id);
CREATE INDEX idx_library_playlist_items_order ON library_playlist_items (playlist_id, position);
