# Playback Security Model

> [API.md](API.md#playback-security) · [SECURITY.md](../SECURITY.md) · [SECURITY-AUDIT.md](../SECURITY-AUDIT.md)

This document details the multi-layered playback protection system. Every layer is optional and off by default — enable via admin policy.

## Architecture Overview

```
Client request
    │
    ▼
┌──────────────────┐
│  1. Card Gate     │  No card? → 403 (everyone, including admins)
│  ensure_card_access
└──────────┬───────┘
           │
           ▼
┌──────────────────┐
│  2. Auth Check    │  Bearer token / API key / session
└──────────┬───────┘
           │
           ▼
┌──────────────────┐
│  3. Risk Engine   │  Signal scoring → Allow / Challenge / Revoke
│  (opt-in)        │  "off" until admin enables risk_policies
└──────────┬───────┘
           │
           ▼
┌──────────────────┐
│  4. Grant Mint    │  HMAC-signed, short-lived (60–3600s)
│  PlaybackGrant   │  Bound to user + media + quality + device
└──────────┬───────┘
           │
           ▼
┌──────────────────┐
│  5. Stream        │  Watermarked URLs, segment-scoped grants,
│  Protection      │  offline licenses, key injection
└──────────────────┘
```

## Layer 1: Card Gate

Nothing plays without an active, unexpired access card. This includes admins.

```rust
// src/cards.rs:ensure_card_access
SELECT EXISTS (
  SELECT 1 FROM access_cards
  WHERE owner = $1 AND status = 'active' AND expires_at > now()
)
```

- Cards are `SPC-{12hex}-{8hmac}` — the last 8 chars are an HMAC-SHA256 checksum
- Trial cards (7 days) are issued once per user on registration
- Default card duration: 30 days
- Brute-force protection: 5 failures in 15 minutes → 15-minute lockout

## Layer 2: Authentication

Three methods, all accepted:

| Method | Header | Use Case |
|---|---|---|
| Bearer token | `Authorization: Bearer <jwt>` | Primary (SPA, mobile) |
| API key | `X-API-Key: sk_...` | Machine-to-machine |
| Session cookie | `Cookie: space_session=...` | Browser |

Tokens are opaque (not JWTs), SHA-256 hashed at rest, with 15-min access TTL and 30-day rotating refresh TTL.

## Layer 3: Risk Engine

**Disabled by default.** When off, any authenticated user with a valid card gets a grant.

When enabled, the engine scores signals and maps to decisions:

### Signals

| Signal | Scoring | Max Points |
|---|---|---|
| `concurrent_sessions` | 25 base + 12 per session over limit (default: 2) | 50 |
| `distinct_devices_24h` | 15 base + 8 per device over limit (default: 3) | 40 |
| `resolves_per_hour` | 20 base + 4 per resolve over limit (default: 5) | 45 |
| `prior_flags_24h` | 10 × count of non-allow security events | 30 |
| `distinct_client_contexts_24h` | 18 base + 7 per context over limit (default: 3) | 35 |

### Decision Bands

| Score Range | Decision | Effect |
|---|---|---|
| 0 → threshold | `Allow` | Grant issued normally |
| threshold → revoke_threshold | `Challenge` | Client must acknowledge; new fingerprint recorded |
| > revoke_threshold | `Revoke` | All grants for user revoked; session killed |

Default thresholds: `challenge=40`, `revoke=80` (configurable via `PATCH /api/admin/security/policy`).

### Device Fingerprinting

Fingerprint priority (checked in order):
1. `X-JA4` header (edge-computed TLS fingerprint)
2. `X-Client-Fingerprint` header (client-computed)
3. `fingerprint` field in request body

Fingerprints are SHA-256 hashed before storage (never raw). Used for:
- Device counting (distinct devices in 24h)
- Grant binding (segment grants check device hash)
- Offline license binding

## Layer 4: Grant System

### Grant Lifecycle

```
POST /api/playback/session
    │
    ├─ verify card access
    ├─ authenticate user
    ├─ risk assessment (if enabled)
    ├─ issue HMAC-signed grant
    └─ return { grant, expires_in }
         │
         ▼
    Client uses grant for playback
         │
         ├─ GET /api/files/{id}/raw?grant=...
         ├─ GET /api/playback/keys/{file_id}?grant=...
         └─ POST /api/playback/grant/segment (for sub-grants)
```

### Grant Token Format

The grant token is an HMAC-SHA256 signature over a pipe-delimited payload:

```
grant_id|user_id|media_ref|quality|exp|scope
```

- `scope` is empty for full-access grants, or `seg:<n>` for segment-scoped grants
- Verified using constant-time comparison (`subtle::ConstantTimeEq`)
- Server-side state: every full-access grant has a DB row for revocation checking

### Grant Parameters

| Parameter | Default | Range | Description |
|---|---|---|---|
| `ttl` | 600s | 60–3600s | Grant lifetime |
| `quality` | — | `sd`, `hd`, `fhd`, `uhd` | Maximum allowed quality |
| `device_hash` | — | SHA-256 | Bound to specific device |

### Segment-Scoped Grants

For HLS streaming, the parent grant can mint child grants scoped to specific segments:

```bash
POST /api/playback/grant/segment
{ "authorization": "<parent-grant>", "segment": 42 }
```

- Child grant TTL: 120 seconds
- Scope: `seg:42`
- The parent grant must still be valid
- Prevents segment prefetching beyond the current playback position

### Grant Revocation

```bash
# Revoke a specific grant (admin)
POST /api/admin/security/grants/{id}/revoke

# Revoke all grants for a user (admin)
POST /api/admin/security/users/{user_id}/revoke-grants
```

Revocation is immediate — the grant ID is checked on every use.

## Layer 5: Stream Protection

### Forensic Watermarking

Every resolved stream URL carries a `wm=<hex>` parameter encoding:
- User ID
- Session reference
- Hourly time bucket

```rust
// src/security/watermark.rs
HMAC-SHA256(secret, user_id || session_ref || hourly_bucket)[..16]
```

If a stream is pirated, the watermark identifies the source user and time window.

### HLS Key Injection

For HLS streams with encryption:

```bash
GET /api/playback/keys/{file_id}?grant=<grant_token>
```

Returns the AES-128 content key for decrypting HLS segments. The key is derived from the media key via HKDF-SHA256.

### Key Hierarchy

```
Master Secret (MASTER_KEY env or DB-generated)
    │
    ├─ derive_kek(master, "media-key-wrap")
    │       │
    │       └─ AES-256-GCM wrap/unwrap media keys
    │
    └─ derive_kek(master, "hls-segment-aes128")
            │
            └─ HKDF → 16-byte HLS content key
```

- Master secret: from `MASTER_KEY` env var, or auto-generated and stored in `signing_secrets` table
- Media keys: 32-byte random keys, one per media item, versioned for rotation
- Content keys: 16-byte AES-128 keys derived from media keys

### Key Rotation

```bash
POST /api/admin/security/keys/{media_ref}/rotate
```

Creates a new version of the media key, retiring the old one. Old segments encrypted with the old key remain playable until their grants expire.

### Key Revocation (Instant)

```bash
POST /api/admin/security/keys/{media_ref}/revoke
```

Immediately revokes all keys for a media item. All active grants for that media become useless.

### Offline Licenses

For downloads (plane mode, etc.):

```bash
POST /api/playback/offline-keys
{ "media_ref": "...", "device_id": "...", "fingerprint": "..." }
```

- Returns the content key directly (the client stores it)
- TTL: 1–7 days (default 24h)
- Bound to user + media + device (all three must match on validation)
- Idempotent: re-requesting within the validity window returns the same license
- Revocable: `DELETE /api/playback/offline-keys/{id}`

## Lockdown Mode

Emergency site-wide lockdown:

```bash
POST /api/admin/security/lockdown
{ "enabled": true, "message": "Maintenance in progress" }
```

- All public routes return 503 with the custom message
- Cached in-process with 5-second TTL (fast toggle)
- Stored in `site_lockdown` table (survives restarts)
- Admin access unaffected

## Security Events

Every risk decision is logged to `security_events`:

```sql
CREATE TABLE security_events (
    id          UUID PRIMARY KEY,
    user_id     UUID REFERENCES users(id),
    signal_type TEXT,
    severity    TEXT,
    score       DOUBLE PRECISION,
    decision    TEXT,
    details     JSONB,
    created_at  TIMESTAMPTZ DEFAULT now()
);
```

Admin audit: `GET /api/admin/security/events?action=&limit=`

## Honeypot

`GET /api/admin/debug/dump-secrets` is a trap. Any request triggers:
1. IP hash banned in `admin_bans`
2. Authenticated user's sessions wiped
3. Watchdog trip

## Configuration

| Variable | Default | Effect |
|---|---|---|
| `ADMIN_LOCAL_ONLY` | `true` | Restrict admin routes to loopback |
| `ACCESS_TOKEN_TTL_SECS` | `900` | Access token lifetime |
| `REFRESH_TOKEN_TTL_SECS` | `2592000` | Refresh token lifetime (30 days) |
| `PASSWORD_LOCKOUT_ATTEMPTS` | `5` | Failed login lockout threshold |
| `PASSWORD_LOCKOUT_MINS` | `15` | Lockout duration |

Risk policy is tuned at runtime via `PATCH /api/admin/security/policy` — no restart needed.
