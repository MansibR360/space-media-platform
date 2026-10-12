# Architecture

A modular monolith: one binary, strict module boundaries, Postgres as the
source of truth, Redis optional. Chosen over microservices for operational
simplicity at self-host scale; module seams make later extraction easy.

```
main ─▶ telemetry ─▶ config ─▶ db (migrate) ─▶ AppState::build ─▶ router
```

## Module map

| Module | Responsibility | Key files |
|---|---|---|
| `auth` | sessions, tokens, lockout, resets, permissions, extractors | service.rs, middleware.rs |
| `users` + `settings` | profiles, admin directory, per-user settings blob | repo.rs |
| `catalog` | media/seasons/episodes/genres/people/studios/credits, external mappings; **metadata registry** | repo.rs, metadata/* |
| `search` | FTS+trigram w/ ILIKE fallback, query parser, trending, similar | parser.rs, repo.rs |
| `tracking` | watch history, continue-watching, statuses, ratings, lists, reading progress | repo.rs |
| `streaming` | storage trait, scanner, ffprobe, range responses, subtitles, **signed downloads** | storage.rs, scanner.rs, download.rs |
| `playback` | sessions, HLS transcode worker, thumbnails, markers/chapters, handoff | transcode.rs, markers.rs |
| `chat` | conversations/messages + the WS hub & gateway | hub.rs, ws.rs |
| `social` | follows, reviews, comments, reactions | repo.rs |
| `parties` | canonical playback state, event log, sync math | manager.rs, repo.rs |
| `intelligence` | taste DNA, vibe engine, home rails, smart collections | service.rs |
| `recommendations` | affinity recs + similar with Redis/memory caching | service.rs, cache.rs |
| `platform` | API keys, devices, notifications | repo.rs |
| `plugins` | runtime plugin registry + HTTP adapter | http_provider.rs |
| `ratelimit` | fixed-window limiter (Redis→memory fallback) | middleware.rs |
| `telemetry`/`router`/`docs` | logging+metrics init, route assembly, OpenAPI | — |

## Request lifecycle

1. TCP → axum; TraceLayer logs span
2. Rate-limit middleware (per-IP buckets, auth paths stricter)
3. Metrics middleware records duration/route/status after handler
4. Extractors resolve auth (`CurrentUser` from Bearer / cookie / `X-API-Key`)
5. Handler → repo layer (sqlx, parameterized) → JSON or `ApiError`
6. `ApiError → ProblemJson` uniform error body

## Stateful pieces

- **AppState** holds pool, config, storage impl, WS hub, party manager,
  transcode queue+worker, metadata registry (`RwLock`, hot-swappable),
  cache, limiter, recommendation service, cancellation token.
- **Background tasks**: transcode workers (mpsc queue, kill-on-drop children),
  maintenance loop every 10 min (popularity recompute, expired token cleanup,
  stale session sweep), optional scanner scheduler.
- **Graceful shutdown**: SIGTERM/Ctrl-C cancels the tree, kills ffmpeg children,
  closes the pool.

## Key design decisions

- **Opaque hashed tokens** (access+refresh) instead of JWTs: instant revocation,
  rotation on refresh, nothing to leak in payloads.
- **Unified `media` table** for movies/series/books with type-specific columns
  nullable — keeps search/collections/tracking polymorphic.
- **Keyset cursors** on stable sorts (created_at/title/popularity/rating);
  offset cursors only where ranking is volatile (relevance).
- **Party state = DB row + wall-clock projection**; events are append-only so
  reconnects replay deterministically.
- **Plugins over HTTP, not dynamic loading**: process isolation, any language,
  no unsafe ABI; registry swaps live under a RwLock.
- **Dialect portability**: PG-only FTS extras applied opportunistically; query
  paths degrade to ILIKE so CockroachDB works unchanged.

## Extending it

New domain? Add `src/<mod>/{models,repo,handlers,routes}.rs`, merge its router
in `router.rs`, add migrations — everything else (auth, errors, pagination,
metrics, docs skeleton) is already there.
