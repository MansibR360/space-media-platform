CREATE TABLE media_markers (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('intro_start', 'intro_end', 'credits_start')),
    seconds DOUBLE PRECISION NOT NULL CHECK (seconds >= 0),
    source TEXT NOT NULL DEFAULT 'auto' CHECK (source IN ('auto', 'manual')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (file_id, kind)
);

CREATE INDEX idx_media_markers_file ON media_markers (file_id);

CREATE TABLE playback_handoffs (
    id UUID PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    file_id UUID NOT NULL REFERENCES media_files (id) ON DELETE CASCADE,
    position_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_playback_handoffs_user ON playback_handoffs (user_id);

CREATE TABLE party_reactions (
    id UUID PRIMARY KEY,
    party_id UUID NOT NULL REFERENCES parties (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    second_offset INTEGER NOT NULL,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_party_reactions_heatmap ON party_reactions (party_id, second_offset);

CREATE TABLE smart_collections (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    rules JSONB NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_id, name)
);

CREATE INDEX idx_smart_collections_owner ON smart_collections (owner_id);
