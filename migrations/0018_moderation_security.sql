-- 0018: moderation & safety, presence, audit logs, admin fortress.

CREATE TABLE IF NOT EXISTS reports (
    id UUID PRIMARY KEY,
    reporter UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN
        ('media', 'metadata', 'comment', 'chat_message', 'party_user', 'user')),
    target_ref TEXT NOT NULL,
    reason TEXT NOT NULL,
    details TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'reviewing', 'resolved', 'dismissed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_reports_status ON reports (status, created_at DESC);

CREATE TABLE IF NOT EXISTS user_blocks (
    blocker UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    blocked UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (blocker, blocked)
);

CREATE TABLE IF NOT EXISTS user_mutes (
    muter UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    muted UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (muter, muted)
);

ALTER TABLE messages ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS presence (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'online'
        CHECK (status IN ('online', 'idle', 'watch_party', 'dnd', 'invisible')),
    custom_text TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Global security/admin audit trail.
CREATE TABLE IF NOT EXISTS audit_logs (
    id UUID PRIMARY KEY,
    actor UUID REFERENCES users (id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT '',
    details JSONB NOT NULL DEFAULT '{}',
    ip TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_logs (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_action ON audit_logs (action, created_at DESC);

-- Site-wide lockdown switch (admin can kill public access instantly).
CREATE TABLE IF NOT EXISTS site_lockdown (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    message TEXT NOT NULL DEFAULT 'service temporarily unavailable',
    enabled_at TIMESTAMPTZ,
    enabled_by UUID REFERENCES users (id) ON DELETE SET NULL
);
INSERT INTO site_lockdown (id) VALUES (1) ON CONFLICT DO NOTHING;

-- Admin surface bans: honeypot / watchdog / lockout escalation.
CREATE TABLE IF NOT EXISTS admin_bans (
    id UUID PRIMARY KEY,
    identifier TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('honeypot', 'watchdog', 'lockout')),
    reason TEXT NOT NULL DEFAULT '',
    until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (identifier, kind)
);
CREATE INDEX IF NOT EXISTS idx_admin_bans_identifier ON admin_bans (identifier);

-- Behavioral baseline for admin logins.
CREATE TABLE IF NOT EXISTS admin_login_history (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    ip_prefix TEXT NOT NULL,
    hour_of_day INTEGER NOT NULL,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_admin_login_history ON admin_login_history (user_id, at DESC);

ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_secret TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS totp_enabled BOOLEAN NOT NULL DEFAULT FALSE;
