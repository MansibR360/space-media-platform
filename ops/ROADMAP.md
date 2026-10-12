# SPACE — Roadmap & Implementation Status

Legend: ✅ shipped · 🟡 core shipped, depth pending · ⬜ planned (needs
external infrastructure, third-party keys, or a dedicated milestone)

---

## M1 — Foundation ✅
Axum/Tokio/SQLx · migrations · AppState · uniform ProblemJson errors ·
env config · tracing + Prometheus metrics · graceful shutdown.

## M2 — Auth & Users ✅
Argon2id · rotating hashed sessions · login lockout · single-use password
resets · RBAC + granular permissions (incl. `download`) · **email
verification codes** · profiles · settings blob · scoped API keys · device
registry · search history (+clear).

## M3 — Catalog & Metadata ✅ / 🟡
✅ Unified media (movie/series/book/music/manga) · seasons/episodes · genres/
people/studios/credits · external mappings · TMDB + IMDb(OMDb) + AniList ·
manual override.
✅ **Metadata refresh scheduler** (6h loop, 25 stale mappings) · **change
history** (`metadata_changes`) · **confidence scores** on every mapping ·
**conflict queue** (`GET /api/admin/quality/conflicts`). Provider priority =
registry order; UI chain pending.

## M4 — Search ✅ / 🟡
✅ FTS + trigram (ILIKE fallback on CockroachDB) · discover filters · keyset
cursors · query parser · typo suggestions (Levenshtein, tested) · unified
cross-source search · **facets: person/language/min-height/codec filters** ·
**search history with chips + clear**.
✅ Search history + chips + clear. ⬜ Abandonment detection, personalized
ranking weights.

## M5 — Tracking ✅
Progress beacons + auto-complete · continue-watching · statuses · ratings →
aggregates · favorites · lists · book reading progress.

## M6 — Streaming & Library Ops ✅ / 🟡
✅ Storage abstraction · scanner (video + 8 book formats) · ffprobe tracks ·
range streaming · SRT/ASS→VTT · external subs · **incremental scanning**
(unchanged-file fast path) · **missing-file sweep + review queue** ·
**replacement tracking via file_events ledger** · **probe-failure ledger** ·
**restricted libraries enforced for non-librarians** · **storage analytics**
(per-library usage, largest files, health flags).
🟡 Per-user library ACLs beyond the restricted flag · storage tiering/cold
storage · compression recommendations.

## M7 — Playback ✅ / 🟡
✅ Sessions+heartbeats · HLS ladder w/ progress API · thumbnails · resume
anywhere · QR handoff · skip-intro detection · chapters · per-episode resume.
🟡 Skip-outro markers · HDR detect/tonemap · hwaccel encoders · per-title
ladders · CDN segment rules.

## M8 — Chat / Social / Parties ✅ / 🟡
✅ WS gateway (rooms/typing/presence/receipts) · DMs/groups/party rooms ·
follows/reviews/comments/reactions/feed · parties with join codes,
private/friends visibility, **passwords**, **host transfer**, **co-host
promotion**, wall-clock sync projection, event replay reconnects, reaction
heatmaps.
⬜ Latency telemetry overlay rendering client-side · per-viewer track
application is event-driven (server broadcasts `track.sync`)

## M9 — Sources Platform ✅ / 🟡
✅ Native backend plugins (ia-films · audius · mangadex · gutenberg — compiled,
zero-hop, any complexity supported in Rust)
✅ HTTP plugins with rich protocol (`streams[]`, `chapters[]` in details)
✅ Intelligent aggregator: intent classification · parallel fan-out with
timeouts · relevance ranking · opaque tokens · best-stream picker ·
cross-source failover
✅ Sources ≠ Search separation (`browse` vs `search/all`)
✅ Health circuit breaker + background prober (`/api/sources/status`)
✅ One-paste auto-detect registration (`register-url`)
🟡 Manifest capability negotiation · /items pagination · OAuth-signed plugin
calls · JS transform sandbox · marketplace UI

## M10 — Predictive & Invisible Systems 🟡
✅ Next-episode prediction + preload hint endpoint + client countdown
✅ Contextual recommendations (session/device/local-hour re-ranking)
✅ Search typo intelligence
✅ Self-healing jobs (stuck transcode retry-once → fail loudly)
✅ Duplicate-title quality queue
✅ **Predictive transcode cancellation** (newer queued job supersedes older)
✅ **Behavior profile** (`GET /api/me/behavior`: per-genre completion, binge
days, avg daily minutes, total hours)
⬜ Bandwidth-gated prefetching · intent shadowing · exit prediction ·
per-device bitrate learning · drop/skip probability models · network context

## M11 — Billing & Trust ✅ / 🟡
✅ Payment requests + admin approval queue → capability grants + user
notification · email verification codes wired into UI
⬜ Real PSP webhooks (Stripe mode), invoice PDFs

## M12 — Verticals ⬜ (needs external infra/keys)
Live RTMP/SRT ingest (mediamtx sidecar) · stream keys · recording-to-library ·
music artist/album pages + playlists + ReplayGain · podcast feed sync ·
audiobook chapters · offline encrypted packages with device-bound licenses +
watermark hooks. *Torrent transport is available today for legal indexes
(Archive.org webseeds); a native BitTorrent engine (rqbit integration) is the
planned upgrade.*

## M13 — Platform hardening 🟡
✅ Webhook dispatcher (HMAC signatures, retries, test-fire) · `X-API-Version`
header · `/v1/*` compatibility namespace with legacy tagging · admin
localhost-only gate · automation rules engine (WHEN/IF/THEN with
notify_all/notify_users/webhook actions) · rate limiting · metrics ·
health/readiness/stats
⬜ `/v1` namespace + deprecation headers · service accounts · partner request
signing · automation rules engine (WHEN/IF/THEN) · PITR via WAL archiving ·
Grafana dashboard pack

## Database posture
CockroachDB runs unchanged (PG wire). PG-only FTS extras applied
opportunistically; ILIKE fallback automatic. TiDB requires a MySQL-driver port —
not planned unless demand justifies it.

## Running it
No Docker needed locally: native Postgres + `cargo run --release` →
app at `:8080` · admin console at `/admin.html` (localhost-only) · Swagger at
`/docs`. Production path via `docker-compose.prod.yml` + `ops/nginx.conf`.
