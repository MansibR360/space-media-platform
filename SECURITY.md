# Security & Reliability Posture

Maps the audit checklist (Phases 12–15) to concrete mechanisms in this codebase.

## Authentication
- Argon2id (default params, per-password salt) — `src/auth/password.rs`
- Opaque bearer tokens; **only SHA-256 hashes stored**; rotation on refresh with
  replay detection (`secure_login`, `refresh` in `src/auth/service.rs`)
- HttpOnly + SameSite=Lax (+Secure in prod) session cookies
- Brute-force lockout: per (identifier, IP) sliding window → timed lock
  (`login_lockouts`), thresholds via env
- Password policy gate before Argon2 (length + class mixing)
- Password reset: single-use hashed tokens, 30-minute TTL, all sessions revoked
  on completion; request endpoint is enumeration-safe

## Authorization
- Role hierarchy (`user < moderator < admin`) + DB grants for granular
  permissions (`user_permissions`, `permissions::require`)
- Admin-only surfaces: catalog writes, library scan/probe/transcode/markers,
  plugin management, bulk user listing
- Object ownership enforced server-side (lists, reviews, comments, sessions)
- WS room joins re-validate membership per event (`chat/ws::authorize_room`)

## Input validation & injection
- All SQL through parameterized sqlx bindings (no string interpolation of user
  data); dynamic filters built with `QueryBuilder` + bound values only
- Length/charset checks on usernames, emails, bodies, emoji, list names
- Path traversal: storage `resolve()` and every file join verify the resolved
  path stays under its root; HLS asset route re-checks prefix

## Abuse resistance
- Fixed-window rate limiting (Redis INCR w/ memory fallback): global per-IP +
  strict auth bucket; edge layer adds nginx `limit_req` for defense in depth
- Party capacity limits, private/friends visibility gates, host-only controls

## Downloads (Phase 21)
Signed download URLs implement the **industry-standard authorization model**:
- HMAC-SHA256 over `(file, user, device, expiry, nonce)` with a server-only
  secret (auto-generated, DB-stored); signature verified in constant time
- Hard expiry (default 30 min, cap 24 h) + per-grant hit budget → link sharing
  dies quickly; device binding ties a URL to a registered device
- Issuance requires the explicit `download` permission grant or admin role;
  every issuance is audited (`download_grants`)
- `Content-Disposition: attachment`, `Cache-Control: private, no-store`

**Honest limitation:** no software can make media *unstealable* — any client
that renders bytes can copy them. Claims of "uncrackable for 1000 years" are
marketing, not engineering. What raises real cost for leakage: server-side
envelope encryption of offline packages with per-device key wrapping, expiring
license blobs with remote kill-switch, and forensic watermarking. These are
the M5 roadmap items and follow Widevine-style trust models.


- Config exclusively from environment (`.env` git-ignored); tokens never logged
- `cargo audit` / Dependabot in CI recommended; pin workspace versions

## Reliability drills
- Redis down ⇒ cache & rate limiter degrade to in-process fallbacks (tested by
  stopping redis during load runs)
- ffmpeg missing ⇒ transcode jobs fail *gracefully* with explicit status,
  streaming endpoints stay healthy
- Restart/recovery ⇒ graceful shutdown drains connections; migrations are
  idempotent; sessions survive restarts (Postgres-backed)

## Secrets & dependencies
- Config from environment only; tokens never logged
