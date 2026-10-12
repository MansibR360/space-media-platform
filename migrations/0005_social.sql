CREATE TABLE follows (
    follower_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    followed_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (follower_id, followed_id),
    CHECK (follower_id <> followed_id)
);

CREATE INDEX idx_follows_followed ON follows (followed_id);

CREATE TABLE reviews (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    title TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL,
    score DOUBLE PRECISION CHECK (score IS NULL OR (score >= 0 AND score <= 10)),
    status TEXT NOT NULL DEFAULT 'published' CHECK (status IN ('published', 'hidden')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, media_id)
);

CREATE INDEX idx_reviews_media ON reviews (media_id, created_at DESC);
CREATE INDEX idx_reviews_user ON reviews (user_id, created_at DESC);

CREATE TABLE comments (
    id UUID PRIMARY KEY,
    author_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN ('review', 'media', 'party')),
    target_id UUID NOT NULL,
    parent_id UUID REFERENCES comments (id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'published' CHECK (status IN ('published', 'hidden')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    edited_at TIMESTAMPTZ
);

CREATE INDEX idx_comments_target ON comments (target_type, target_id, created_at);

CREATE TABLE reactions (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN ('review', 'comment', 'message')),
    target_id UUID NOT NULL,
    emoji TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, target_type, target_id, emoji)
);

CREATE INDEX idx_reactions_target ON reactions (target_type, target_id);

CREATE TABLE parties (
    id UUID PRIMARY KEY,
    host_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID REFERENCES media (id) ON DELETE SET NULL,
    episode_id UUID REFERENCES episodes (id) ON DELETE SET NULL,
    join_code TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL DEFAULT 'public' CHECK (visibility IN ('public', 'private', 'friends')),
    max_participants INTEGER NOT NULL DEFAULT 20,
    is_playing BOOLEAN NOT NULL DEFAULT FALSE,
    position_seconds DOUBLE PRECISION NOT NULL DEFAULT 0,
    state_updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_event_seq BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ
);

CREATE INDEX idx_parties_host ON parties (host_id);

CREATE TABLE party_members (
    party_id UUID NOT NULL REFERENCES parties (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'viewer' CHECK (role IN ('host', 'cohost', 'viewer')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (party_id, user_id)
);

CREATE TABLE party_events (
    seq BIGSERIAL PRIMARY KEY,
    party_id UUID NOT NULL REFERENCES parties (id) ON DELETE CASCADE,
    actor_id UUID REFERENCES users (id) ON DELETE SET NULL,
    event_kind TEXT NOT NULL CHECK (event_kind IN ('play', 'pause', 'seek', 'sync', 'chat', 'join', 'leave')),
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_party_events_party ON party_events (party_id, seq);

CREATE TABLE conversations (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('direct', 'group', 'party')),
    title TEXT,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    party_id UUID REFERENCES parties (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX uq_conversations_party ON conversations (party_id) WHERE party_id IS NOT NULL;

CREATE TABLE conversation_members (
    conversation_id UUID NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('owner', 'moderator', 'member')),
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_read_at TIMESTAMPTZ,
    PRIMARY KEY (conversation_id, user_id)
);

CREATE INDEX idx_conversation_members_user ON conversation_members (user_id);

CREATE TABLE messages (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    sender_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    attachment JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    edited_at TIMESTAMPTZ
);

CREATE INDEX idx_messages_conv_time ON messages (conversation_id, created_at DESC);

CREATE TABLE message_receipts (
    message_id UUID NOT NULL REFERENCES messages (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    delivered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    read_at TIMESTAMPTZ,
    PRIMARY KEY (message_id, user_id)
);
