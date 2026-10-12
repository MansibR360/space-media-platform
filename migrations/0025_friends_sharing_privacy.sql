-- 0025: friends, movie sharing (+ share history), per-user privacy,
-- and access-card activation hardening.

-- ---------------- card activation hardening ----------------
-- Failed-activation attempt tracking with automatic lockout. Brute-forcing
-- card numbers is rate limited server-side; this persists the penalty.
CREATE TABLE IF NOT EXISTS card_activation_locks (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    failures INTEGER NOT NULL DEFAULT 0,
    window_started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_until TIMESTAMPTZ
);

-- ---------------- friend system ----------------
CREATE TABLE IF NOT EXISTS friend_requests (
    id UUID PRIMARY KEY,
    requester UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    addressee UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'declined', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    responded_at TIMESTAMPTZ,
    UNIQUE (requester, addressee),
    CHECK (requester <> addressee)
);
CREATE INDEX IF NOT EXISTS idx_friend_requests_addressee
    ON friend_requests (addressee, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_friend_requests_requester
    ON friend_requests (requester, status, created_at DESC);

-- Symmetric friendship: both directions are stored in one transaction so
-- "my friends" is a single indexed lookup and removal deletes two rows.
CREATE TABLE IF NOT EXISTS friendships (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    friend_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, friend_id),
    CHECK (user_id <> friend_id)
);
CREATE INDEX IF NOT EXISTS idx_friendships_friend ON friendships (friend_id);

-- ---------------- movie sharing + share history ----------------
CREATE TABLE IF NOT EXISTS media_shares (
    id UUID PRIMARY KEY,
    sender UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    recipient UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_id UUID NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    note TEXT NOT NULL DEFAULT '',
    viewed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (sender <> recipient)
);
CREATE INDEX IF NOT EXISTS idx_media_shares_recipient
    ON media_shares (recipient, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_media_shares_sender
    ON media_shares (sender, created_at DESC);

-- ---------------- per-user privacy settings ----------------
-- Rows are optional: missing row == platform defaults (see src/privacy.rs).
CREATE TABLE IF NOT EXISTS privacy_settings (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    profile_visibility TEXT NOT NULL DEFAULT 'everyone'
        CHECK (profile_visibility IN ('everyone', 'friends', 'private')),
    show_watch_history BOOLEAN NOT NULL DEFAULT TRUE,
    show_activity_feed BOOLEAN NOT NULL DEFAULT TRUE,
    allow_friend_requests TEXT NOT NULL DEFAULT 'everyone'
        CHECK (allow_friend_requests IN ('everyone', 'friends_of_friends', 'nobody')),
    allow_media_shares TEXT NOT NULL DEFAULT 'friends'
        CHECK (allow_media_shares IN ('everyone', 'friends', 'nobody')),
    show_online_status BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
