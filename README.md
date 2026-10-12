# SPACE — Production-Grade Media Platform

![Rust](https://img.shields.io/badge/Rust-2024-orange) ![Axum](https://img.shields.io/badge/Axum-0.8-blue) ![PostgreSQL](https://img.shields.io/badge/PostgreSQL-17-336791) ![License](https://img.shields.io/badge/status-portfolio%20project-lightgrey)

A modular-monolith media server written in **Rust (Axum, SQLx, PostgreSQL)**: catalog, full-text and
"vibe" search, HLS streaming, watch tracking, social features, watch parties and WebSocket chat, plus a
**runtime plugin system** for metadata sources.

**What this project demonstrates**

- Designing a 25-module Rust backend (auth, catalog, search, streaming, playback, social, chat, parties, plugins)
- Security-first engineering: Argon2id, rotating hashed refresh sessions, RBAC, HMAC-signed expiring downloads, rate limiting. See [SECURITY.md](SECURITY.md)
- Real-time systems: WebSocket gateway with rooms, presence, and wall-clock-synced watch parties
- Operability: Prometheus metrics, structured logs, health checks, Docker Compose, nginx, backup/restore scripts, k6-style load tests

> **Disclaimer / reminder:** SPACE is an open-source project for **educational purposes only**. Some bundled
> plugins and sources can talk to third-party sites. The authors do not host, distribute or endorse
> copyrighted content, and you are solely responsible for using this software in line with the terms of any
> service you connect and the copyright laws of your country. Only use it with content you have the right to access.

## Quick start

```bash
docker compose up -d postgres redis      # infrastructure
cp .env.example .env                      # set DATABASE_URL etc.
cargo run                                 # migrations run automatically
# → http://localhost:8080          (web client)
# → http://localhost:8080/docs     (Swagger UI)
# → http://localhost:8080/admin.html (plugin console)
```

Admin bootstrap: set `ADMIN_EMAIL` + `ADMIN_PASSWORD`, they are provisioned on
first boot. Library scanning: point `LIBRARY_ROOTS` at your media, then
`POST /api/admin/library/scan`.

## Feature map

| Area | Highlights |
|---|---|
| Auth | Argon2id hashes, opaque rotating refresh sessions (hashed at rest), HttpOnly cookies, login lockout, single-use password reset tokens, RBAC (`user/moderator/admin`) + granular permissions |
| API keys | `sk_` scoped machine tokens (`read`/`write`/`downloads`) via `X-API-Key`; hashed at rest, prefix lookup, last-used tracking |
| Devices | Registered device list w/ platform + last-seen; download links bind to device ids |
| Catalog | Unified `media` (**movies · series · books**), seasons/episodes, genres/people/studios/credits, external ID mappings |
| Metadata | Built-ins: **TMDB**, **IMDb (OMDb)**, **AniList**; runtime HTTP metadata *and source* plugins |
| Search | FTS + trigram fuzzy (auto ILIKE fallback on CockroachDB), discover filters, keyset cursors, similar titles, trending windows |
| Query parser | `action 2020-2025 1080p english` → structured filters (`/api/search/parse?q=…`) |
| Tracking | Progress beacons w/ auto-complete, continue-watching, statuses, star ratings → aggregates, favorites, lists, **reading progress** for ebooks |
| Streaming | Storage abstraction, scanner (video **+ epub/pdf/mobi/azw3/fb2/cbz/cbr**), ffprobe tracks, HTTP range streaming, SRT/ASS→VTT |
| Playback | Sessions + heartbeats, HLS ladder transcodes, thumbnails, resume-across-devices, QR handoff codes |
| Social | Reviews, threaded comments, reactions, follows (+notification), activity feed |
| Chat | WebSocket gateway (rooms, typing, presence, receipts), DMs/groups/party rooms |
| Parties | Join codes, host controls, wall-clock-correct sync state, event replay reconnects, reaction heatmaps |
| Notifications | In-app feed + unread count; WS push; triggers on follow & party join; extensible kinds |
| Downloads | HMAC-signed, expiring (≤24 h), device-bound, nonce-budgeted URLs — see SECURITY.md |
| Lyrics | Multi-provider (Better Lyrics → LRCLIB → NetEase → Genius), synced LRC + TTML, in-memory + Postgres cache, `GET /api/lyrics`, `GET /api/media/{id}/lyrics` |
| YouTube | Five-layer source chain (InnerTube → Piped → Invidious → built-in scrape → embed), resilience loop, admin heal, separate per-user history (`/api/youtube/history`) |

### Extension system — Stremio addons, 1:1

SPACE natively speaks the Stremio Addon SDK: install any community addon by
manifest url (console → **Extensions**, with one-click browse of the central
index) and its catalogs join unified search while its `/stream` endpoints
join playback resolution — torrents become magnets automatically.
[docs/extensions.md](docs/extensions.md)

```bash
curl -X POST http://localhost:8080/api/admin/extensions \
  -H "Authorization: Bearer $TOKEN" -d '{"url":"https://torrentio.strem.fun/manifest.json"}'
```

### Differentiators

* **Taste DNA** — genre affinity, pace & era preferences per user (`GET /api/me/taste`)
* **Heuristic Engine** — built-in learning from every play, rewatch, rating & favorite: Wrapped recaps, chronotype habits, binge sessions, personas, explainable next-up picks (`GET /api/me/heuristic`) — see [docs/heuristic-engine.md](docs/heuristic-engine.md)
* **One-call Home** — `/api/home` composes all rails incl. because-you-watched
* **Vibe search** — deterministic natural-language mood engine, no external AI
* **Query parser** — free-form text becomes precise filters
* **Smart collections** — saved rule queries as living shelves
* **Plugin system** — metadata + full-library source plugins at runtime

### Books

Books are catalog items (`media_type=book`). The scanner ingests `epub pdf mobi
azw3 fb2 djvu cbz cbr`; clients track position via
`PUT /api/tracking/reading {media_id, position_percent, page}` and resume with
`GET /api/me/media/{id}/reading`. Files stream through the same signed-download
pipeline.

### Source plugins

Beyond metadata search, a plugin with `"kind": "source"` exposes
`GET {base}/items` returning `[{id,title,type,year,overview,poster}]`.
`POST /api/admin/plugins/{id}/sync` imports the whole library into the
catalog with dedupe via external mappings.

## Plugins

Register any HTTP service speaking the JSON protocol — at runtime, from the
admin console or the API. Ready-made examples live in `plugins/python/`:

| Plugin | Kind | What it does |
|---|---|---|
| `space_plugin.py` | source | minimal protocol reference |
| `leetx_plugin.py` | source | 1337x torrents → magnet streams ([docs](docs/torrents.md)) |
| `rivestream_plugin.py` | source | RiveStream movies/TV players via TMDB ids ([docs](docs/rivestream.md)) |
| `zstream / cineby / toustream / cubeflix / cinebytv` | source | TMDB-keyed movie & TV frontends ([docs](docs/streaming-plugins.md)) |
| `miruro / animepahe / kaa / animex / anikoto / reanime / lunastream / weebcentral / nyaa` | source | anime & manga (miruro real HLS; nyaa RSS magnets; luna trending) |
| `myanimelist / simkl / rottentomatoes` | source | catalog/ratings/tracking search bridges |
| `kissasian / dramanice / pahe / kisskh / goplay / asiaflix / asiancrush / flicker / comix / mangadot / noxx / bstsrs / primewire / hanime / comicbookplus / allcinema / everythingmoe` | source | dramas, encodes & passthrough catalogs |

```
GET  {base}/search?q=   → {"results":[{"id","title","overview","year","poster","type"}]}
GET  {base}/details?id= → {"title","overview","release_date","poster","media_type",
                           "genres":[],"studios":[],"cast":[{"name","character"}]}
```

```bash
curl -X POST /api/admin/plugins -H "$AUTH" -d '{
  "name":"tmdb-fr", "kind":"metadata",
  "base_url":"https://my-plugin.example.com", "enabled":true }'
```

Plugins appear instantly under `/api/metadata/sources`, work with
`/api/metadata/search` and `/api/metadata/import`. Write them in any language.

## Observability & operations

* Structured logs (`LOG_FORMAT=json`), request latency histograms, WS gauges,
  worker metrics → `/metrics` (Prometheus).
* `/health` (liveness), `/api/ready` (DB deep check), `/api/stats` dashboard.
* Production: `docker-compose.prod.yml` + `ops/nginx.conf` (TLS-ready, WS-aware).
* Backups: `ops/backup.sh` nightly dump w/ retention; `ops/restore.sh` includes
  restore *verification* steps — rehearse monthly.
* Load tests: `tests/load/browse_load.js` (500 VU browse mix),
  `ws_stress.js` (socket storms), `stream_stress.js` (range-request IO).
  Run against staging with thresholds; investigate regressions via `/metrics`.

## Documentation

Full docs live in [docs/](docs/):

- [API Reference](docs/API.md) — every endpoint with curl examples
- [WebSocket Protocol](docs/websocket.md)
- [Plugin Guide](docs/plugins.md)
- [Configuration](docs/configuration.md) — [Architecture](docs/architecture.md) — [Database](docs/database.md) — [Deployment](docs/deployment.md)

Interactive OpenAPI: /docs once the server is running.

### Built-in source plugins (native, zero-hop)

| Source | Category | What you get |
|---|---|---|
| ia-films | movies | Internet Archive public-domain films (streamable MP4s, torrent webseeds) |
| audius | music | Trending + searchable tracks with direct audio streaming |
| mangadex | manga | Licensed manga: search, chapter list, full page reader |
| gutenberg | books | 75k public-domain ebooks (read online / EPUB / Kindle) |

Sources are server-managed: clients call the unified endpoints and the backend
auto-selects providers, ranks results, picks the best stream, and fails over
automatically. Raw per-source routes exist only as admin debug tools.

Unified API:

```
GET  /api/search/all?q=&limit=&category=   aggregated results w/ opaque tokens
POST /api/playback/resolve                 { token } -> best stream URL
POST /api/playback/manga/open              { token } -> chapters + first pages
POST /api/admin/sources/{name}/sync        import N items into your catalog
```

### Learned web sources — any site, one field (homelab)

Paste a site url at `/console` → **Web Sources** (or
`POST /api/admin/websources {"site": "https://…"}`) and the bundled
[galaxy](docs/web-sources.md) scraper learns the site's search endpoint,
result cards and item pages automatically. Learned sites join unified search,
playback resolution with failover, health monitoring and sync like any other
source.

```
POST /api/admin/websources        { "site": "https://movies.example" }   → learned
POST /api/admin/websources/{id}/test                                     → live check
```

See [docs/web-sources.md](docs/web-sources.md).
