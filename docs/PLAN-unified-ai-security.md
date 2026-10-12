# PLAN — Unified Media Resolution Pipeline + Security Layer + Admin AI Layer

Status: **A–J + hardening shipped** · **changeset 5 shipped** · **changeset 6 shipped** — completed/toplists/random/upcoming/availability/summary/export/meta QoL APIs, plus the full security audit (`SECURITY-AUDIT.md`).

## Shipped in changeset 5

| Area | Delivered |
|---|---|
| Moderation | `POST /api/reports` (media/metadata/comment/chat/party/user) with admin review queue; user block & mute (`POST /api/users/{id}/block|mute`, delete+lists) enforced in social feeds and DMs |
| Chat | message edit (owner, `PATCH`) and soft delete (`DELETE`, owner/mod) on `/api/chat/messages/{id}`; blocked users can't DM each other |
| Presence | `PUT /api/me/presence` (online/idle/watch_party/dnd/invisible + custom text), batch query endpoint, WS `presence.update` event |
| Audit | `audit()` helper at every sensitive action; `GET /api/admin/audit-logs` |
| DX | WS schema doc finalized; unified-search result cache (~90 s TTL); Python source-plugin template + guide (stdlib HTTP server implementing the plugin contract) |
| Admin fortress | TOTP 2FA for admins (RFC-6238, provisioning URI); behavioral check on new IP-prefix; ≥5 failed admin logins → escalated ban; honeypot route + hidden field → instant permanent ban; watchdog trips remote-admin lockout (localhost-only) on attack patterns; site-wide lockdown switch; bans table consulted before every admin request |
| Card economy | Cryptographic access cards (HMAC checksum numbers). No active card → nothing plays (resolve/session/offline/local streaming — admins included). 7-day trial card on registration (unique per account). Merchants buy cards with crypto (admin approves), give or transfer them via 20-min codes; full merchant→user chain visible; admin holds cards/bans merchants instantly |

## Shipped in changeset 4 (anti-theft hardening)

| Feature | Delivered |
|---|---|
| Device/TLS fingerprinting | Grants bind to a client-context hash: edge-computed `X-JA4` preferred, else `X-Client-Fingerprint` (client-computed JA4 or stable device hash). Bound at authorization (`POST /api/playback/session`, `/api/resolve`), verified on every key/segment fetch — a stolen HMAC grant replayed from a different TLS/client context fails authentication. Risk engine gains a *distinct client contexts* signal |
| Segment-scoped grants | Parent grant mints child tickets scoped to `seg:<index>` (TTL ≤ 120 s) via `POST /api/playback/grant/segment`. Scope rides inside the signed payload; verification rejects out-of-scope use — initial access no longer authorizes scraping the whole file |
| Offline keys | `POST /api/playback/offline-keys` issues a 24 h license: AES content key delivered once over TLS, bound to user+media+device, idempotent until expiry, revocable. Client contract (OS keystore storage) documented; TTL clamped 1–7 days via env |

## Shipped in changeset 3 (phase J + ops hardening)

| Phase | Delivered |
|---|---|
| J | Key service (`src/security/keys.rs`): master secret (env `MASTER_KEY` or generated+persisted) → HKDF-derived purpose KEKs → AES-256-GCM-wrapped per-media keys (`media_keys` table) with rotation + instant revocation; HLS AES-128 key endpoint `/api/playback/keys/{file_id}` gated by signed playback grants; `EXT-X-KEY` injection when `HLS_ENCRYPTION=true`; forensic watermark codes bound to user+session+time stamped onto every resolved stream URL |
| Origin | `ORIGIN_INTERNAL_TOKEN`: when set, key/download endpoints require the proxy header `X-Origin-Token` — private-origin behind CDN pattern documented in docs/deployment.md |
| Failure analysis | 5xx recorder middleware → `failure_events`, grouped timeline at `GET /api/admin/failures` (top routes, p95), AI heuristic proposes investigation on failure spikes |
| Adaptive privacy | maintenance tick rolls up platform events older than 30 d into daily `event_rollups`, deletes raw rows older than 90 d, scrubs aged security-event details |
| Self-organizing library | `POST /api/admin/quality/duplicates/auto-resolve`: merges duplicate catalog entries into the best-mapped keeper (re-points files/tracking/social, moves seasons when unambiguous, skips conflicting series groups with a report) |

## Shipped in changeset 2 (phases F–I)

| Phase | Delivered |
|---|---|
| F | `media_relationships` graph (12 relation types incl. adaptation_of/sequel_of/spin_off_of…), admin link/unlink API, `GET /api/media/{id}/related` grouped by relation, `GET /api/media/{id}/journey` franchise traversal (BFS ≤3 hops), AI action `add_media_relationship` under new `graph` scope |
| G | Universal progress engine: `unified_progress` view (seconds for video/music/podcast/audiobook from `watch_history`, page+percent for book/manga from `reading_progress`), single write endpoint `PUT /api/progress` routing by media_type, merged cross-type rail `GET /api/continue` |
| H | Event bus (`events` table + WS bridge envelopes on the global room, emit points in resolver/controller/progress), personal media twin (`GET /api/me/twin`, formats/completion/pace/creators/discovery/current interests + daily snapshot), predictive cache warmer loop (recs for online users + similar for trending), cross-media journeys (`GET /api/discover/journeys` — adaptation chains from finished anchors excluding seen) |
| I | Proper WS: server heartbeat (30 s ping, dead-connection reaping), personal `user:{uuid}` rooms auto-subscribed & auth-scoped to self, notifications/security/AI pushes land live, documented protocol |

## Shipped in changeset 1 (phases A–E)

| Phase | Delivered |
|---|---|
| A | `NativeSource.capabilities()/manifest()/browse()` defaults; duplicate-title merge across sources (`items[].sources[]`); HealthRegistry op telemetry (search/resolve success rates, active-request gauge, circuit state) exposed at `/api/sources/status`; `/api/sources/{name}/browse` now uses the native hook |
| B | `POST /api/resolve`: identify → circuit-aware providers → preference ranking (quality distance, language/subtitle hints, kind weight, health bonus) → best stream + ranked fallbacks + short-lived signed authorization; legacy `/api/playback/resolve` preserved |
| C | Playback Security Controller: HMAC-SHA256 grants (instant revocation), risk engine (concurrency/devices/resolves/prior flags → allow/challenge/revoke), audit trail, admin policy/events/grants endpoints, layer off by default |
| D | AI Copilot: heuristic + OpenAI-compatible providers, persisted governance (mode/autonomy/scopes/security-integration), whitelisted actions, auto-actor loop, full audit log, admin UI tab |
| E | Migration 0014, env vars, docs/API.md §15–17, admin UI (Security + AI Copilot tabs), unit tests green |

## Changeset 3 design notes

```
KEY SERVICE                      WATERMARKING                  ORIGIN
MASTER_KEY env | generated       wm = hash(user,session,time)  X-Origin-Token header
  └ HKDF purpose KEKs            appended ?wm=<code> on every    required on key/dl endpoints
      └ AES-GCM wrapped media        resolved stream URL          (private origin behind CDN)
          keys (rotate/revoke)     leak → traceable session
HLS: EXT-X-KEY → /api/playback/keys/{file} verifies grant before releasing bytes

FAILURE ANALYSIS                 ADAPTIVE PRIVACY               AUTO-MERGE
5xx → failure_events             events >30d → event_rollups    duplicates → keeper =
timeline + p95 grouping          raw >90d deleted                 most external mappings
AI rule: spike → notify          security details scrubbed       (files/tracking moved,
                                                                  conflicting series skipped)
```

## Shipped in changeset 2 (phases F–I)

| Phase | Delivered |
|---|---|
| F | `media_relationships` graph (12 relation types incl. adaptation_of/sequel_of/spin_off_of…), admin link/unlink API, `GET /api/media/{id}/related` grouped by relation, `GET /api/media/{id}/journey` franchise traversal (BFS ≤3 hops), AI action `add_media_relationship` under new `graph` scope |
| G | Universal progress engine: `unified_progress` view (seconds for video/music/podcast/audiobook from `watch_history`, page+percent for book/manga from `reading_progress`), single write endpoint `PUT /api/progress` routing by media_type, merged cross-type rail `GET /api/continue` |
| H | Event bus (`events` table + WS bridge envelopes on the global room, emit points in resolver/controller/progress), personal media twin (`GET /api/me/twin`, formats/completion/pace/creators/discovery/current interests + daily snapshot), predictive cache warmer loop (recs for online users + similar for trending), cross-media journeys (`GET /api/discover/journeys` — adaptation chains from finished anchors excluding seen) |
| I | Proper WS: server heartbeat (30 s ping, dead-connection reaping), personal `user:{uuid}` rooms auto-subscribed & auth-scoped to self, notifications/security/AI pushes land live, documented protocol |

## Shipped in changeset 1 (phases A–E)

| Phase | Delivered |
|---|---|
| A | `NativeSource.capabilities()/manifest()/browse()` defaults; duplicate-title merge across sources (`items[].sources[]`); HealthRegistry op telemetry (search/resolve success rates, active-request gauge, circuit state) exposed at `/api/sources/status`; `/api/sources/{name}/browse` now uses the native hook |
| B | `POST /api/resolve`: identify → circuit-aware providers → preference ranking (quality distance, language/subtitle hints, kind weight, health bonus) → best stream + ranked fallbacks + short-lived signed authorization; legacy `/api/playback/resolve` preserved |
| C | Playback Security Controller: HMAC-SHA256 grants (instant revocation), risk engine (concurrency/devices/resolves/prior flags → allow/challenge/revoke), audit trail, admin policy/events/grants endpoints, layer off by default |
| D | AI Copilot: heuristic + OpenAI-compatible providers, persisted governance (mode/autonomy/scopes/security-integration), whitelisted actions, auto-actor loop, full audit log, admin UI tab |
| E | Migration 0014, env vars, docs/API.md §15–17, admin UI (Security + AI Copilot tabs), unit tests green |

## Changeset 2 design notes

```
MEDIA GRAPH                     EVENT BUS                      UNIVERSAL PROGRESS
media_relationships             events table + broadcast       unified_progress VIEW
  adaptation_of … parent_of     emit points:                   watch_history  → seconds
BFS journey(depth≤3)              resolution.issued            reading_progress → page/%
  related (grouped)               playback.authorized          PUT /api/progress (routes by type)
  journeys (cross-media,          progress.updated             GET /api/continue (all types)
    adaptation chains)            security.*, search.performed

PERSONAL TWIN                    CACHE WARMER                   WEBSOCKET
twin computed from signals       every 10 min:                  heartbeat ping/pong + reaping
  genres · formats · completion    online users → rec caches    user:{uuid} personal rooms
  pace · creators · discovery      trending → similar caches    platform.event envelopes
daily snapshot persisted         metrics warmed_total           docs/websocket.md v2
```

Migration `0015_universal_progress.sql`: `media_relationships`, `events`,
`twin_snapshots`, view `unified_progress`.

## Later phase (deliberately out of scope)

- J: Forensic watermarking hook on stream serving + AEAD (AES-GCM) segment
  encryption behind the key service

---

## Historical detail (changesets 1–2 planning)

```
Client ──▶ GET /api/search/all            ──▶ aggregator fan-out ──▶ normalized items {id,title,type,sources[]}
Client ──▶ POST /api/playback/session     ──▶ Playback Security Controller
                                                ├─ authenticate
                                                ├─ risk assessment (optional layer, admin-tuned)
                                                ├─ decision: allow | challenge | revoke
                                                └─ short-lived signed playback grant
Client ──▶ POST /api/resolve              ──▶ central resolver pipeline
        identify ▶ sources ▶ health ▶ candidates ▶ rank(prefs: quality/language/subs)
        ▶ select ▶ failover list ▶ grant-bound authorization ▶ playable stream (+expires_at)
Admin  ──▶ /api/admin/security/*          ──▶ policy, live events, instant revocation
Admin  ──▶ /api/admin/ai/*                ──▶ AI copilot: config, analyze, apply, audit
                                         (works with the security layer or fully without it)
Ops    ──▶ GET /api/sources/status        ──▶ per-source latency, search/resolve success rates,
                                             active requests, circuit state
```

---

## Phase A — Source abstraction + normalized results + telemetry

1. **Extend `NativeSource`** with defaulted async methods (no breakage for the 6 built-ins):
   - `capabilities()` → `{categories, kinds, subtitles: bool}`
   - `manifest()` → self-describing JSON (name/label/category/capabilities)
   - `browse(page)` → optional catalog browse hook (default `None`)
2. **Normalize `/api/search/all` output** to the target contract:
   ```json
   {"items":[{"id":"<opaque-token>","title":"…","type":"movie","year":2024,
              "poster":"…","overview":"…","sources":["source_a","source_b"],
              "relevance":98.5}],
    "results":[ …legacy array preserved verbatim for web/app.js compat… ]}
   ```
   - aggregator tracks the producing source per result and **merges duplicates**
     (same normalized title+year+category) into one item with `sources: []`.
3. **Telemetry upgrade** (`HealthRegistry`):
   - per-op counters: `search_total/success`, `resolve_total/success`
   - `active_requests: AtomicUsize` guard incremented around every provider call
   - derived `success_rate`, `search_success_rate`, `resolve_success_rate`,
     `circuit` (`closed|open|half-open`)
   - aggregator records resolve outcomes (currently never recorded)
   - `/api/sources/status` returns the enriched snapshot

## Phase B — First-class central resolver: `POST /api/resolve`

Request:
```json
{"token":"…","episode_id":null,
 "preferences":{"quality":"1080p","language":"en","subtitles":["en"],"direct_play":true}}
```
Pipeline implemented in `aggregator::resolve_with_preferences`:
1. parse token → primary provider first, healthy providers after (circuit-aware)
2. fetch candidate streams; score each: quality number parsed from label/kind,
   language match, subtitle availability hint, kind weight (hls > original > mp4…),
   source health bonus (latency + success rate)
3. best candidate selected; remaining ranked streams become structured `fallbacks`
4. issue **short-lived playback grant** (Phase C) bound to user+media+quality+expiry;
   response carries `authorization` + `expires_at`
5. response:
```json
{"resolution_id":"…","title":"…","url":"…","kind":"hls","quality":"1080p",
 "audio":"en","subtitles":["en"],"expires_at":"…","authorization":"<signed>",
 "fallbacks":[{"source":"…","kind":"mp4","url":"…"}]}
```
`/api/playback/resolve` stays as a thin backwards-compatible alias.
Preference scoring is a pure function → unit-tested.

## Phase C — Playback Security Controller (risk layer)

New `src/security/` module:

- **Grants** (`grants.rs`): HMAC-SHA256-signed tokens reusing the `signing_secrets`
  table (purpose `playback`), payload `gid|uid|media|quality|exp`, constant-time
  verify, DB-backed **instant revocation** (`revoked_at`). TTL default 600 s
  (env `PLAYBACK_GRANT_TTL_SECS`, clamped 60–3600).
- **Risk engine** (`risk.rs`), off-by-default single-row policy table:
  - signals: concurrent active sessions, distinct devices (24 h), resolutions/hour,
    repeated challenge history
  - weighted score → decision `allow | challenge | revoke`
  - thresholds admin-tunable; every non-allow decision written to `security_events`
- **Endpoints**:
  - `POST /api/playback/session` — controller entry: auth → risk gate → grant
    (`403 challenge_required` on suspicious; clients retry with
    `acknowledge_challenge:true` which is logged)
  - `GET/PATCH /api/admin/security/policy`
  - `GET /api/admin/security/events`
  - `GET /api/admin/security/grants` + `POST /api/admin/security/grants/{id}/revoke`
- **Layer toggle**: `risk_policies.enabled = false` ⇒ `/api/resolve` and the
  controller pass straight through (the AI layer works identically either way).
- Migration `0014_ai_security.sql`: `playback_grants`, `security_events`,
  `risk_policies` (+ Phase D tables).

## Phase D — Admin AI layer ("AI Copilot")

New `src/aiops/` module — pluggable so the real brain can be added later:

- **Providers** (`provider.rs`): trait `AiProvider::analyze(ctx) -> Vec<Recommendation>`
  - `heuristic` (built-in, zero-config): rule-based over live context
    (dead/degraded sources, low success rate, stuck transcodes, risk spikes,
    empty library, backlog…) → concrete recommendations
  - `llm` (optional): OpenAI-compatible `/chat/completions`
    (`AI_API_URL` / `AI_API_KEY` / `AI_MODEL` env defaults, admin-editable);
    failures degrade gracefully to heuristic
- **Governance** (`engine.rs`): persisted config `ai_config`
  - `mode`: `off | heuristic | llm`
  - `autonomy`: `observe` (log only) · `suggest` (admin clicks Apply) ·
    `act` (background loop auto-applies whitelisted actions)
  - `scopes`: `["sources","security","library","notifications","grants"]`
  - `security_integration`: bool — when false the AI never sees/touches the
    security domain (works without the layer)
  - **whitelisted actions only**: toggle source plugin, patch risk policy fields,
    revoke grants (per user/all), notify admins, run library scan — every apply
    gated by scope + autonomy + written to `ai_events`
- **Endpoints** (all admin-gated):
  - `GET|PATCH /api/admin/ai/config`
  - `POST /api/admin/ai/analyze` → context summary + recommendations
  - `POST /api/admin/ai/apply` `{recommendation_id}` (manual, any mode ≠ off)
  - `GET /api/admin/ai/events`, `GET /api/admin/ai/status`
- Background actor loop spawned from `AppState` (cancel-token aware) when
  `mode != off && autonomy == act`.
- **Admin UI**: new “AI Copilot” tab (config form + Analyze + recommendation cards
  with Apply + audit log) and new “Security” tab (policy editor, live events,
  grant list with revoke buttons).

## Phase E — Wiring + docs + verification

- `state.rs`: build `AiEngine` handle, spawn actor loop
- `router.rs` / `main.rs` / `docs.rs` (OpenAPI): register everything
- `.env.example`: AI + playback-grant vars
- `docs/API.md`: new endpoints section
- Verify: `cargo check`, `cargo test` (unit tests: preference ranking, duplicate
  merge, risk scoring bands, grant sign/verify/expiry/revoke), then `cargo build`

## Later phases (deliberately out of this changeset, follow the plan file)

- F: Universal media graph (`media_relationships`, canonical identity, aliases)
- G: Universal progress engine across video/audio/text (single `MediaProgress`)
- H: Forensic watermarking hook on stream serving + AEAD (AES-GCM) segment
  encryption behind the key service
- I: Event bus extraction (`events` table + subscribers) replacing direct
  webhook/automation dispatch calls
- J: Predictive cache warming / demand prediction feeding off the event bus

