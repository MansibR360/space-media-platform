CREATE TABLE watch_history (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    episode_id UUID REFERENCES episodes (id) ON DELETE CASCADE,
    position_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    duration_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX uq_watch_history_target
    ON watch_history (user_id, media_id, COALESCE(episode_id, '00000000-0000-0000-0000-000000000000'::uuid));
CREATE INDEX idx_watch_history_user_recent ON watch_history (user_id, updated_at DESC);
CREATE INDEX idx_watch_history_media ON watch_history (media_id, updated_at DESC);

CREATE TABLE list_entries (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('watching', 'completed', 'on_hold', 'dropped', 'planned')),
    score DOUBLE PRECISION CHECK (score IS NULL OR (score >= 0 AND score <= 10)),
    notes TEXT NOT NULL DEFAULT '',
    episodes_watched INTEGER NOT NULL DEFAULT 0,
    rewatch_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, media_id)
);

CREATE INDEX idx_list_entries_user_status ON list_entries (user_id, status, updated_at DESC);

CREATE TABLE favorites (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, media_id)
);

CREATE INDEX idx_favorites_media ON favorites (media_id);

CREATE TABLE user_lists (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL DEFAULT 'private' CHECK (visibility IN ('private', 'public', 'followers')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);

CREATE TABLE user_list_items (
    list_id UUID NOT NULL REFERENCES user_lists (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    position INTEGER NOT NULL DEFAULT 0,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (list_id, media_id)
);

CREATE INDEX idx_user_list_items_media ON user_list_items (media_id);
