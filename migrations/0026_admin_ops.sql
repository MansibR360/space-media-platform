-- 0026: admin operations — merchant card-dispense lockout.
--
-- A locked-out merchant keeps everything they already own: their cards stay
-- active and playable. What stops working is dispensing new cards to users
-- (direct gives, transfer-code redemptions) and ordering more inventory.

ALTER TABLE merchant_profile
    ADD COLUMN IF NOT EXISTS locked_out BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE merchant_profile
    ADD COLUMN IF NOT EXISTS locked_at TIMESTAMPTZ;
ALTER TABLE merchant_profile
    ADD COLUMN IF NOT EXISTS locked_reason TEXT NOT NULL DEFAULT '';
