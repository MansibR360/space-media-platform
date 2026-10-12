# Card Economy

> [API.md](API.md#card-economy) · [SECURITY.md](../SECURITY.md)

SPACE uses an access-card system to gate playback. Every user — including admins — must have an active card to stream.

## Card Basics

A card is a cryptographic token in the format:

```
SPC-{12hex}-{8hmac}
```

The first group is a random body. The second group is an HMAC-SHA256 checksum over the body, truncated to 8 hex characters. This means:
- **Tamper detection:** Any modification to the card number is caught offline
- **Forgery prevention:** Creating valid checksums requires the server-side signing secret
- **Format verification:** `verify_card_format()` recomputes the HMAC and does constant-time comparison

## Card Lifecycle

```
┌──────────┐     ┌──────────┐     ┌──────────┐
│  Minted   │────▶│  Active   │────▶│ Expired  │
│  (issued) │     │ (playing) │     │          │
└──────────┘     └─────┬────┘     └──────────┘
                       │
                       ├─ admin hold ──▶ On Hold
                       │
                       └─ admin revoke ──▶ Revoked
```

| Status | Meaning |
|---|---|
| `active` | Normal, usable for playback |
| `expired` | Past `expires_at` timestamp |
| `held` | Admin has frozen the card (temporary) |
| `revoked` | Permanently disabled |

## Trial Cards

Every new user automatically receives a 7-day trial card on registration:

```rust
// Issue exactly once per user (partial unique index enforces this)
INSERT INTO access_cards (owner, issued_by, duration_days)
VALUES ($1, 'trial', 7)
-- ON CONFLICT (owner) WHERE issued_by = 'trial' → no-op
```

Trial cards are issued at registration time. If a user somehow loses their trial (e.g., database manipulation), the partial unique index prevents re-issuance.

## Minting Cards

### Admin Minting

```bash
POST /api/admin/cards/mint
{ "count": 100, "duration_days": 30 }
```

Mints up to 500 cards at once. Each card gets:
- A unique `SPC-{body}-{hmac}` number
- Full HMAC stored separately (`number_hmac`) for indexed lookups
- `issued_by: "admin"`
- `duration_days` (clamped 1–365)

### Merchant Minting

Merchants purchase cards with cryptocurrency, then admin approves:

```bash
# Merchant registers
POST /api/merchant/register

# Merchant purchases cards
POST /api/merchant/cards/purchase
{ "count": 10, "payment_ref": "btc-tx-hash" }

# Admin approves the purchase
POST /api/admin/card-purchases/{id}/approve
```

## Activation

### Activating a Card

```bash
POST /api/cards/activate
{ "card_number": "SPC-A1B2C3D4E5F6-D7E8F9A0" }
```

**Activation hardening:**
- 5 failed attempts in 15 minutes → 15-minute lockout
- Lockout is persisted (survives restarts)
- Failed attempts are tracked per (user, IP) pair
- Metric: `card_activations_total{result="success|failure"}`

On activation:
1. Card number is verified (HMAC check)
2. Card must be `active` status and unexpired
3. Card must not already have an owner (or must be the same owner)
4. `expires_at` is set to `now() + duration_days`
5. Card is bound to the activating user

## Transfer

Users can gift cards to each other via one-time transfer codes:

```bash
# Generate a transfer code (valid 20 minutes)
POST /api/cards/transfer-code
# → { "code": "...", "expires_at": "..." }

# Redeem on the recipient's account
POST /api/merchant/cards/redeem-transfer
{ "code": "..." }
```

Transfer codes are single-use and time-limited.

## Merchant Workflow

### Registration

```bash
POST /api/merchant/register
```

Creates a merchant record. Merchants can:
- View their inventory
- Purchase cards (pending admin approval)
- Give cards to users by username
- Redeem user transfer codes

### Inventory

```bash
GET /api/merchant/inventory
# → { "available": 42, "given": 15, "pending": 3 }
```

### Giving Cards

```bash
POST /api/merchant/cards/give
{ "username": "alice", "count": 5 }
```

### Approval Flow

1. Merchant submits purchase request with crypto payment reference
2. Request enters pending queue
3. Admin reviews: `POST /api/admin/card-purchases/{id}/approve` or `reject`
4. On approval: cards are minted and added to merchant inventory

## Admin Operations

### Card Search

```bash
GET /api/admin/cards?owner=&status=&limit=50
```

### Grant Cards Directly

```bash
POST /api/admin/cards/grant
{ "user_id": "...", "count": 5, "duration_days": 30 }
```

### Instant Hold/Release

```bash
POST /api/admin/cards/{card_number}/hold
{ "hold": true }
```

Hold is temporary — the card resumes when released.

### Card Chain Visibility

```bash
GET /api/admin/cards/chain?card_number=...
# → Full lineage: who minted it, who gave it, who holds it
```

### Merchant Management

```bash
# List merchants
GET /api/admin/merchants

# Lock a merchant (freeze inventory)
POST /api/admin/merchants/{id}/lock

# Ban a merchant (permanent)
POST /api/admin/merchants/{id}/ban
{ "reason": "fraud" }
```

## Billing Configuration

```bash
# View pricing
GET /api/admin/billing/config
# → { "card_price_usd": 9.99, "trial_days": 7, ... }

# Update pricing
PATCH /api/admin/billing/config
{ "card_price_usd": 14.99 }
```

## Quick Status Check

```bash
GET /api/me/card
# → { "active": true, "expires_at": "...", "days_remaining": 23 }

GET /api/me/cards/summary
# → { "total": 3, "active": 2, "expired": 1, "days_remaining": 23 }
```

## Key Metrics

| Metric | Labels | Description |
|---|---|---|
| `playback_blocked_no_card_total` | — | Playbacks rejected (no card) |
| `card_activations_total` | `result` | Activation attempts |
| `cards_minted_total` | `issued_by` | Cards created (`admin`, `trial`, `merchant`) |

## Security Properties

- Card numbers are not sequential or predictable (random hex body)
- HMAC prevents offline forgery
- Constant-time comparison prevents timing attacks
- Brute-force protection with lockout
- Full chain of custody visible to admins
- Admin can hold/revoke any card instantly
- Cards expire server-side (no client-side expiration)
