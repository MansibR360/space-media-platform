-- 0027: appeals — users contest moderation decisions; admins accept or reject.

CREATE TABLE IF NOT EXISTS appeals (
    id UUID PRIMARY KEY,
    appellant UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- report | ban | suspension | content_removal | other
    target_type TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'rejected')),
    decided_by UUID REFERENCES users (id) ON DELETE SET NULL,
    decided_at TIMESTAMPTZ,
    decision_note TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_appeals_status ON appeals (status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_appeals_appellant ON appeals (appellant, created_at DESC);
