-- 0019: the access-card economy.

-- Admin-tunable pricing / payout config.
CREATE TABLE IF NOT EXISTS billing_config (
    id INT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    unit_price_cents INTEGER NOT NULL DEFAULT 999,
    currency TEXT NOT NULL DEFAULT 'USD',
    crypto_address TEXT NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO billing_config (id) VALUES (1) ON CONFLICT DO NOTHING;

-- Cards. One ACTIVE, unexpired card = one month of playback.
CREATE TABLE IF NOT EXISTS access_cards (
    id UUID PRIMARY KEY,
    card_number TEXT NOT NULL UNIQUE,
    number_hmac TEXT NOT NULL,
    issued_by TEXT NOT NULL DEFAULT 'system',
    creator_merchant UUID REFERENCES users (id) ON DELETE SET NULL,
    owner UUID REFERENCES users (id) ON DELETE SET NULL,
    duration_days INTEGER NOT NULL DEFAULT 30,
    status TEXT NOT NULL DEFAULT 'unused'
        CHECK (status IN ('unused', 'active', 'used', 'held')),
    activated_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    held_reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_cards_owner ON access_cards (owner, expires_at DESC);
CREATE INDEX IF NOT EXISTS idx_cards_merchant ON access_cards (creator_merchant, created_at DESC);

-- Trial anti-abuse: exactly one trial card per account, ever.
CREATE UNIQUE INDEX IF NOT EXISTS one_trial_per_user
    ON access_cards (owner) WHERE issued_by = 'trial';

CREATE TABLE IF NOT EXISTS merchant_profile (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    banned BOOLEAN NOT NULL DEFAULT FALSE,
    banned_at TIMESTAMPTZ,
    total_purchased INTEGER NOT NULL DEFAULT 0,
    total_sold INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Crypto purchases pending admin approval.
CREATE TABLE IF NOT EXISTS card_purchases (
    id UUID PRIMARY KEY,
    merchant UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    quantity INTEGER NOT NULL CHECK (quantity BETWEEN 1 AND 500),
    unit_price_cents INTEGER NOT NULL,
    total_cents INTEGER NOT NULL,
    tx_reference TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    decided_by UUID REFERENCES users (id) ON DELETE SET NULL,
    decided_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_card_purchases_status ON card_purchases (status, created_at DESC);

-- 20-minute transfer codes: buyer generates, merchant redeems N cards against it.
CREATE TABLE IF NOT EXISTS card_transfer_codes (
    id UUID PRIMARY KEY,
    code_hash TEXT NOT NULL UNIQUE,
    requester UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    used_by UUID REFERENCES users (id) ON DELETE SET NULL,
    cards_moved INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
