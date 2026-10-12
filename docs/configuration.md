# Configuration Reference

All configuration is environment-based. Put it in `.env` (git-ignored) or the
process environment; `.env.example` documents every key.

## Server

| Variable | Default | Description |
|---|---|---|
| `APP_HOST` | `0.0.0.0` | Bind address |
| `APP_PORT` | `8080` | HTTP port |
| `LOG_LEVEL` | `info` | tracing filter (`debug`, `space=trace`, …) |
| `LOG_FORMAT` / `LOG_JSON` | `text` | `json` or `LOG_JSON=true` for structured logs |
| `METRICS_ENABLED` | `true` | Prometheus recorder at `/metrics` |
| `CORS_ORIGINS` | *(empty = allow all)* | comma-separated origins, e.g. `https://app.example.com` |
| `TRUST_PROXY` | `false` | honor `X-Forwarded-For` for client IPs (set behind nginx) |

## Database & cache

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | **required** | Postgres DSN. CockroachDB DSN works unchanged. |
| `DB_MAX_CONNECTIONS` | `10` | pool size; scale with vCPU × 2–4 |
| `REDIS_URL` | *(unset)* | enables Redis-backed cache + rate limiting. Unset/unreachable ⇒ in-memory fallbacks automatically |

## Auth & security

| Variable | Default | Description |
|---|---|---|
| `ACCESS_TOKEN_TTL_SECS` | `900` | session token lifetime |
| `REFRESH_TOKEN_TTL_SECS` | `2592000` | refresh lifetime (30 d) |
| `SESSION_COOKIE_NAME` | `space_session` | HttpOnly cookie holding the access token |
| `COOKIE_SECURE` | `false` | set `true` when serving HTTPS |
| `PASSWORD_MIN_LENGTH` | `8` | also enforces letter+digit mix at this length |
| `LOCKOUT_THRESHOLD` | `5` | failed logins before lockout (per identity+IP) |
| `LOCKOUT_WINDOW_SECS` | `900` | failure counting window |
| `LOCKOUT_DURATION_SECS` | `900` | lockout duration after threshold |
| `RESET_LINK_BASE` | `http://localhost:8080/reset` | prefix used in reset links |

## Admin bootstrap

| Variable | Description |
|---|---|
| `ADMIN_EMAIL`, `ADMIN_PASSWORD` | created on startup if absent; role=admin |

## Media & storage

| Variable | Default | Description |
|---|---|---|
| `LIBRARY_ROOTS` | *(unset)* | comma-separated absolute paths to scan |
| `STORAGE_DIR` | `./data/storage` | transcode output, thumbnails, extracted subs |
| `SCANNER_INTERVAL_SECS` | `0` (off) | background rescan period |
| `FFMPEG_BIN` / `FFPROBE_BIN` | `ffmpeg`/`ffprobe` | binary paths |
| `TRANSCODE_HEIGHTS` | `1080,720,480` | HLS ladder (source-capped) |

## Metadata providers & plugins

| Variable | Description |
|---|---|
| `TMDB_API_KEY` | enables built-in `tmdb` source |
| `OMDB_API_KEY` | enables built-in `imdb` source (via OMDb) |
| `ANILIST_ENABLED` | default true; enables `anilist` source |
| `PLUGIN_ALLOW_HTTP` | reserved policy flag for plugin egress |

## Rate limiting

| Variable | Default |
|---|---|
| `RATE_LIMIT_ENABLED` | `true` |
| `RATE_LIMIT_GLOBAL_PER_MIN` | `300` per IP |
| `RATE_LIMIT_AUTH_PER_MIN` | `20` on login/register/refresh/forgot |

## Notes

- Everything degrades gracefully: no Redis → memory cache/limiter; no ffmpeg →
  jobs fail explicitly while streaming keeps working; no provider keys → those
  sources simply don't appear.
- CockroachDB: PG-only FTS extras are applied opportunistically and skipped on
  unsupported engines — search falls back to ILIKE matching.

## Admin security

| Variable | Default | Description |
|---|---|---|
| ADMIN_LOCAL_ONLY | 	rue | /api/admin/* and /admin.html respond only to loopback peers. Set alse for remote administration (auth still applies). Behind a proxy, forwarded headers claiming a remote client are refused while this is on. |
| EMAIL_DEV_ECHO | 	rue | returns verification codes in API responses for localhost testing - disable in production |