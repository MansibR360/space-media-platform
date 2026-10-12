-- 0014: AI copilot layer + playback security controller (risk assessment,
-- short-lived signed playback grants, instant revocation, audit trail).

-- ---------------------------------------------------------------- security

CREATE TABLE IF NOT EXISTS playback_grants (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    media_ref TEXT NOT NULL DEFAULT '',
    session_id UUID REFERENCES playback_sessions (id) ON DELETE SET NULL,
    quality TEXT NOT NULL DEFAULT '',
    device_hash TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_playback_grants_user
    ON playback_grants (user_id, expires_at DESC);
CREATE INDEX IF NOT EXISTS idx_playback_grants_active
    ON playback_grants (expires_at) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS security_events (
    id UUID PRIMARY KEY,
    user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    signal_type TEXT NOT NULL,
    severity TEXT NOT NULL DEFAULT 'info'
        CHECK (severity IN ('info', 'warning', 'critical')),
    score REAL NOT NULL DEFAULT 0,
    decision TEXT NOT NULL DEFAULT 'allow'
        CHECK (decision IN ('allow', 'challenge', 'revoke')),
    details JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_security_events_user
    ON security_events (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_security_events_recent
    ON security_events (created_at DESC);

-- Singleton policy row; the whole risk layer is OFF until an admin enables it.
CREATE TABLE IF NOT EXISTS risk_policies (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    challenge_threshold REAL NOT NULL DEFAULT 40,
    revoke_threshold REAL NOT NULL DEFAULT 75,
    max_concurrent_sessions INT NOT NULL DEFAULT 6,
    max_devices_per_day INT NOT NULL DEFAULT 8,
    max_resolves_per_hour INT NOT NULL DEFAULT 60,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO risk_policies (id) VALUES (1) ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------- ai copilot

CREATE TABLE IF NOT EXISTS ai_config (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    mode TEXT NOT NULL DEFAULT 'heuristic'
        CHECK (mode IN ('off', 'heuristic', 'llm')),
    autonomy TEXT NOT NULL DEFAULT 'suggest'
        CHECK (autonomy IN ('observe', 'suggest', 'act')),
    model TEXT NOT NULL DEFAULT '',
    api_url TEXT NOT NULL DEFAULT '',
    security_integration BOOLEAN NOT NULL DEFAULT TRUE,
    scopes JSONB NOT NULL DEFAULT '["sources","security","library","notifications","grants"]',
    last_analysis JSONB,
    last_analyzed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO ai_config (id) VALUES (1) ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS ai_events (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL
        CHECK (kind IN ('analysis', 'action_applied', 'action_rejected', 'error')),
    summary TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_ai_events_created ON ai_events (created_at DESC);
