# Deployment

## 1. Production stack (Docker)

```bash
cp .env.example .env    # set POSTGRES_PASSWORD, ADMIN_*, CORS_ORIGINS, provider keys
docker compose -f docker-compose.prod.yml up -d --build
```

Services: `app` (this image) · `postgres:16` · `redis:7` (AOF persistence) ·
`nginx` edge proxy. Media and storage live in named volumes
(`media`, `storage`); Postgres data in `pgdata`.

## 2. TLS

`ops/nginx.conf` ships HTTP-ready with a commented 443 server block:

```bash
mkdir -p ops/certs && cp fullchain.pem privkey.pem ops/certs/
# uncomment the TLS server block, then:
docker compose -f docker-compose.prod.yml restart proxy
```

For public deployments prefer certbot/Let's Encrypt mounted into the same path.
Set `TRUST_PROXY=true` and `COOKIE_SECURE=true` in `.env` when terminating TLS.

## 3. Reverse-proxy behavior (already configured)

- `/ws` → upgrade headers + 1 h read timeout
- `/api/(files|hls|transcodes)` → `proxy_buffering off`, Range passthrough,
  long read timeouts (streaming-friendly)
- `/api/auth/login` → edge rate limit (`10r/m` burst 5), defense-in-depth on
  top of the app limiter
- Everything else → standard proxy headers

## 4. Backups & recovery

Nightly logical backup + 14-day retention + gzip verification:

```bash
./ops/backup.sh                     # BACKUP_DIR/CONTAINER overridable
```

Restore **rehearsal** (do this quarterly — an untested backup is a hope):

```bash
./ops/restore.sh ops/backups/space-20260101T000000Z.sql.gz
curl -fsS localhost:8080/api/ready   # must be {"ready":true,...}
```

Point-in-time recovery: enable Postgres WAL archiving
(`archive_command = 'cp %p /backups/wal/%f'`) and use `pgBackRest`/`wal-g`;
the restore script's flow stays the same.

Media files are outside the DB by design — snapshot the `media`/`storage`
volumes (or your object store bucket) alongside DB dumps.

## 5. Monitoring

| Signal | Source |
|---|---|
| Liveness | `GET /health` |
| Readiness | `GET /api/ready` (DB ping; wire to LB health checks) |
| Metrics | `GET /metrics` — HTTP latency histogram, request counters, ws gauges, playback sessions, transcode activity, download counters |
| Business stats | `GET /api/stats` |

Scrape example (prometheus.yml):

```yaml
scrape_configs:
  - job_name: space
    static_configs: [{ targets: ["app:8080"] }]
```

Suggested alerts: p95 latency > 500 ms for 5 m · error-rate > 2% ·
`rate_limit_rejected_total` spike · transcode failure ratio > 20% ·
`up == 0` on readiness.

## 6. Scaling notes

- App is stateless except WS presence — run N replicas behind nginx
  (`ip_hash` or sticky cookies) until Redis pub/sub fan-out lands (ROADMAP M9)
- Postgres: raise `DB_MAX_CONNECTIONS` with pooler (PgBouncer) in front;
  read replicas work as-is for search-heavy traffic
- Transcodes are CPU-bound: scale workers horizontally by running extra app
  instances pointed at a shared `STORAGE_DIR` (NFS/object storage), or pin
  jobs per node
- HLS segments are immutable → put a CDN in front of `/api/hls/*` and
  `/api/files/*/master.m3u8` with auth-aware caching rules

## 7. Zero-downtime upgrades

Migrations are additive-first; deploys follow blue/green:
start new version → wait `/api/ready` → shift proxy weight → drain old
(with_graceful_shutdown drains in-flight requests and WS upgrades).

---

## 8. Private origin behind a CDN (media protection)

Goal: the internet talks to your edge; only the edge talks to SPACE.

```
Internet --? CDN / reverse proxy (TLS, caching)
                 �  injects X-Origin-Token: <ORIGIN_INTERNAL_TOKEN>
                 ?
             SPACE origin (not publicly routable)
```

1. Set a long random `ORIGIN_INTERNAL_TOKEN` on the origin.
2. Configure the proxy to add the header on `/api/playback/keys/*` and
   `/api/dl/*` (nginx):

```nginx
location ~ ^/api/(playback/keys|dl)/ {
    proxy_set_header X-Origin-Token "the-long-random-token";
    proxy_pass http://space-origin:8080;
}
```

Direct hits without the header get `403 origin token required`. Cache media at
the edge but never cache the key endpoint (`Cache-Control: no-store` is set
automatically for it).

3. Optional hardening: `HLS_ENCRYPTION=true` encrypts transcode outputs with
AES-128; segment keys are released only against valid short-lived playback
grants (`POST /api/playback/session`, then pass the grant to
`GET /api/playback/keys/{file_id}?grant=...`). Rotate or revoke per-media keys
via the admin security endpoints � revocation stops future decryption even for
copies already downloaded in pieces.

4. Every `POST /api/resolve` URL carries `wm=<code>` � a forensic watermark
binding user + resolution + hour. Keep request logs at the edge and a leaked
copy's manifest traces back to one session.
