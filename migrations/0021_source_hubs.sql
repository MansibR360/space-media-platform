-- Remote source hubs: other SPACE instances (or compatible services) this
-- server falls back to when local providers come up empty, and targets for
-- pushing/pulling learned recipes.

CREATE TABLE IF NOT EXISTS source_hubs (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    base_url TEXT NOT NULL,
    token TEXT,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_sync_at TIMESTAMPTZ,
    created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
