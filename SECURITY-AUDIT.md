# SPACE — Full-Project Security Assessment

Scope: entire repository as of this pass. Method: manual static review of
every auth path, middleware, money path, crypto construction and query
surface, plus executable unit tests for security-critical pure logic.
Result: **all findings below fixed in-tree; 43/43 tests green.**

---

## 1. Findings & resolutions

| ID | Sev | Finding | Status |
|----|-----|---------|--------|
| F-01 | High | **Honeypot unreachable for remote attackers.** The gate's loopback policy answered `403 localhost-only` before the honeypot handler could run, so remote probers never got banned. | **Fixed** — gate intercepts `/api/admin/debug/dump-secrets` first and calls `honeypot_trip()` regardless of origin; shared logic in `security::routes` |
| F-02 | High | **Card gate gaps.** `hls_master`, `hls_asset` and `request_download_url` served media without the access-card check — violating the "no card = nothing plays" invariant. | **Fixed** — gates added; playback surface now fully covered (resolve · session · offline · local raw · HLS master/asset · download issuance) |
| F-03 | Med | **Multi-statement SQL** in `block_user` (`INSERT …; DELETE …`) fails under prepared-statement protocol at runtime. | **Fixed** — split into two prepared statements |
| F-04 | Med | **Journey filter bypass** — reverse-direction adaptation steps skipped the "already consumed" check, recommending seen titles. | **Fixed** — direction-agnostic `!seen` filter |
| F-05 | Low | **Cookie name hard-coded** in console-page gate while config allows renaming the session cookie. | **Fixed** — uses `SESSION_COOKIE` constant |
| F-06 | Low | **Unbounded search cache** growth on adversarial query variety. | **Fixed** — 800-entry cap with TTL sweep |
| F-07 | Low | **Invisible presence broadcast** leaked status changes for users who chose invisible over WS. | **Fixed** — stored, never broadcast |
| F-08 | Info | Advertised endpoints (`storage/analytics`, `files/{id}/events`) existed but were unmounted. Dead legacy playlist writer removed; trivial unused imports cleaned. | **Fixed** |

## 2. Threat model → control matrix

| Threat | Control(s) |
|---|---|
| Credential stuffing / brute force | per-(identity,IP) lockouts · admin escalation bans (4× window) · rate limits · TOTP |
| Stolen bearer/session tokens | HttpOnly cookies · hashed refresh storage · logout-all · honeypot session wipe · instant grant revocation |
| Stolen playback grants replayed elsewhere | HMAC grants bound to user+media+quality+expiry **and** TLS/device fingerprint (`X-JA4`) — mismatch = 403 |
| Bulk scraping after initial access | segment-scoped child grants (`seg:N`, ≤120 s) · one-time 20-min transfer codes |
| Key theft | plaintext keys never stored; HKDF purpose KEKs; AES-256-GCM authenticated wrapping; rotate/revoke APIs |
| Leak tracing | forensic `wm=` watermark binding user+session+hour on every resolved URL |
| Direct-to-origin media theft | `X-Origin-Token` gate on key/download endpoints (CDN-only pattern documented) |
| Admin page attacks | loopback default · watchdog lockout · permanent honeypot bans · escalated lockouts · TOTP · server-gated console delivery |
| Site abuse during incident | site-wide lockdown switch (public 503, admin+health alive) |
| Insider/merchant fraud | purchase approval queue · instant card hold · merchant ban · merchant→holder chain audit |
| Trial farming | partial unique index — exactly one trial card per account, race-free |
| SQL injection | parameterized queries everywhere; dynamic SET clauses built from whitelisted fields only; multi-statement bug eliminated |
| XSS/injection via client markup | API returns JSON only; no server-rendered user content |
| DoS on hot paths | bounded caches (search ≤800 entries) · circuit breakers · fan-out timeouts · rate limiting |

## 3. Crypto inventory

| Purpose | Construction | Notes |
|---|---|---|
| Access/refresh tokens | random 32 B, SHA-256 hashed at rest | rotation on refresh |
| Playback/download/card signatures | HMAC-SHA256, constant-time compare | secrets per-purpose in `signing_secrets` |
| Media keys at rest | AES-256-GCM under HKDF-SHA256 KEKs | fresh 96-bit nonce per wrap |
| HLS segments | AES-128 content key derived via HKDF from media key | released only against live grants |
| TOTP | RFC-6238 HMAC-SHA1/30 s/6 digits ±1 step | RFC test vectors unit-tested |
| Card numbers | body + HMAC checksum group | tamper-evident offline |

## 4. Executable security tests (this pass)

* `watchdog::tests::per_ip_pattern_trips_and_resets` — proves the detector trips on repeated failures and that reset restores service.
* `cards::tests::checksum_detects_tampering` — checksum equality, tamper rejection, secret-sensitivity.
* `grants::tests::segment_scope_semantics` — scoped tickets cover exactly one segment; full-access covers all.
* `keys::tests::aead_roundtrip_and_tamper_rejection` — GCM auth failure on ciphertext bit-flip and wrong KEK.
* `totp::tests::rfc6238_test_vector` + window/reject cases.
* `watermark::tests::*` — deterministic codes per hour bucket, URL re-stamping replaces rather than stacks, fragments preserved.
* `aggregator::tests::*` — preference ranking order, duplicate-title merge keys.

## 5. Residual risks (documented, by design)

1. **Offline copies**: once a key is delivered, server cannot claw back bytes — mitigated by 24 h license windows + media-key rotation killing future re-delivery.
2. **Watermark is URL-level**: determined redistributors can strip query strings; pixel-level marking stays a transcode-pipeline hook.
3. **JA4 requires edge cooperation**: without a proxy computing it, fingerprinting falls back to client-supplied values (still binds grants; trust is weaker and documented).
4. **KMS/HSM**: master secret lives in env/generated store; swapping to a KMS only replaces `master_secret()`.
5. **Watchdog memory reset**: a process restart clears an active trip (DB bans persist) — acceptable; localhost recovery path unaffected.
