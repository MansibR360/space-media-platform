# Getting Started

## 0. Prerequisites

- Rust 1.85+ (`rustup`)
- PostgreSQL 14+ **or** CockroachDB — Docker is optional (see "No Docker" below)
- Redis optional — without it, cache/rate-limiting fall back to in-memory
- `ffmpeg` + `ffprobe` on PATH — optional, needed for transcoding, thumbnails,
  subtitle extraction and intro detection

### No Docker (plain localhost)

1. Install Postgres natively:
   - Windows: `winget install PostgreSQL.PostgreSQL.16`
   - macOS: `brew install postgresql@16 && brew services start postgresql@16`
   - Linux: `sudo apt install postgresql`
2. Create the database:
   ```bash
   createdb space        # or: psql -U postgres -c 'CREATE DATABASE space;'
   ```
3. `.env` needs only:
   ```
   DATABASE_URL=postgres://postgres:yourpassword@localhost:5432/space
   ```
4. `cargo run --release` → open http://localhost:8080

Redis can be added later (`REDIS_URL=redis://localhost:6379`) with zero code
changes; everything falls back automatically until then.

## 0b. Infrastructure (Docker route)

```bash
docker compose up -d postgres redis
```

CockroachDB works unchanged — point `DATABASE_URL` at it:

```
postgres://root@localhost:26257/space?sslmode=disable
```

## 2. Configure

```bash
cp .env.example .env
```

Minimum required: `DATABASE_URL`. Recommended for first run:
`ADMIN_EMAIL`, `ADMIN_PASSWORD`, and `TMDB_API_KEY` / `OMDB_API_KEY`
(free keys) so metadata import works.

## 3. Run

```bash
cargo run
```

Migrations apply automatically. The bootstrap admin is created if
`ADMIN_EMAIL`/`ADMIN_PASSWORD` are set.

## 4. First login

```bash
curl -s -X POST localhost:8080/api/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"identifier":"admin@space.local","password":"admin12345"}'
```

Save `access_token` from the response; use it as
`Authorization: Bearer <token>` on every call. Tokens expire in 15 min —
call `/api/auth/refresh` with your `refresh_token` to rotate them.

## 5. Add content

Either import metadata from a provider:

```bash
curl -X POST localhost:8080/api/metadata/import \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"source":"anilist","external_id":"16498"}'
```

…or scan a media folder (creates catalog skeletons matched by filename):

```bash
echo 'LIBRARY_ROOTS=/path/to/media' >> .env   # then restart, or:
curl -X POST localhost:8080/api/admin/library/scan \
  -H "Authorization: Bearer $TOKEN"
```

Probe files to extract tracks/durations:

```bash
curl -X POST localhost:8080/api/admin/files/probe-all \
  -H "Authorization: Bearer $TOKEN"
```

Transcode one file to HLS:

```bash
curl -X POST localhost:8080/api/admin/files/<FILE_ID>/transcode \
  -H "Authorization: Bearer $TOKEN"
```

Then play it in the web client (`/`) or hit
`GET /api/files/<FILE_ID>/master.m3u8` directly with any HLS player.

## 6. Enable downloads for a user

```bash
curl -X POST localhost:8080/api/admin/users/<USER_ID>/permissions \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d '{"permission":"download"}'
```

The user can now mint signed links:
`POST /api/files/{file_id}/download-url {"device_id": "...", "ttl_secs": 1800}`.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `missing required environment variable: DATABASE_URL` | `.env` missing/not readable |
| Playback 409 "file not matched" | run library scan; file has no linked media yet |
| Transcode 503 | ffmpeg not installed or wrong `FFMPEG_BIN` |
| Search returns nothing after fresh CRDB install | expected: FTS extras skipped; ILIKE fallback active |
