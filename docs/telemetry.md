# Telemetry & Monitoring

> [architecture.md](architecture.md) · [deployment.md](deployment.md) · [API.md](API.md)

SPACE exports Prometheus metrics and structured logs for production observability.

## Enabling Metrics

Set `METRICS_ENABLED=true` in your environment (default: `true`).

The `/metrics` endpoint serves a standard Prometheus text format. Scrape it with your Prometheus instance, Grafana Agent, or any OpenMetrics-compatible collector.

## Structured Logging

| Variable | Default | Effect |
|---|---|---|
| `LOG_LEVEL` | `info` | tracing filter: `debug`, `info`, `warn`, `error` |
| `LOG_JSON` | `false` | `true` → JSON log lines (machine-parseable) |
| `RUST_LOG` | — | Override for any crate |

JSON mode outputs one JSON object per line with fields: `timestamp`, `level`, `target`, `message`, and any structured fields attached via `tracing::info!(field = value, ...)`.

## Prometheus Metrics

All metrics use the `metrics` crate with `metrics-exporter-prometheus`.

### Application Metrics

| Metric | Type | Labels | Description |
|---|---|---|---|
| `http_requests_total` | counter | `method`, `path`, `status` | Total HTTP requests (auto-instrumented by `axum-metrics`) |
| `http_request_duration_seconds` | histogram | `method`, `path` | Request latency distribution |
| `playback_blocked_no_card_total` | counter | — | Playbacks rejected for missing access card |
| `cache_warmed_total` | counter | — | Cache entries warmed per warming tick |
| `cache_warm_duration_seconds` | histogram | — | Wall-clock time of warming tick |
| `automation_runs_total` | counter | `event` | Automation rules executed |
| `webhook_deliveries_total` | counter | `event`, `status` | Webhook delivery attempts |
| `cards_minted_total` | counter | `issued_by` | Cards minted (`admin`, `trial`, `merchant`) |
| `card_activations_total` | counter | `result` | Activation attempts (`success`, `failure`) |

### Infra Metrics (auto-exported by runtime crates)

| Metric | Source | Description |
|---|---|---|
| `sqlx_pool_*` | sqlx | Connection pool: size, idle, wait, timeout |
| `tokio_*` | tokio | Async runtime: threads, tasks, queue depth |

## Cache Warming

A background task runs every 10 minutes (`src/warming.rs`):

1. **User rails**: For up to 50 online users, precomputes recommendation caches (12 items each)
2. **Trending shelves**: For the top 20 trending media of the week, precomputes similar-media caches (8 items each)

Warming is invisible to users — they just see fast rails. The warming loop respects the `CancellationToken` for graceful shutdown.

**Metrics emitted:**
- `cache_warmed_total` — number of cache entries written
- `cache_warm_duration_seconds` — wall-clock time of the tick

## Health Endpoints

| Endpoint | Purpose | Returns |
|---|---|---|
| `GET /health` | Liveness probe | `200 "ok"` |
| `GET /api/ready` | Deep readiness | `200 { "status": "ok", "checks": { "database": true, "redis": true, "ffmpeg": true } }` |
| `GET /api/stats` | Dashboard summary | Session counts, transcode queue, media counts |
| `GET /metrics` | Prometheus scrape | Text format, all registered counters/histograms |
| `GET /api/admin/events` | Event timeline | Platform events with timestamps |
| `GET /api/admin/failures` | Error tracker | Recent 5xx errors, per-route aggregates |

## Alerting Suggestions

### Critical

- `playback_blocked_no_card_total` spike → card system outage
- `http_requests_total{status="503"}` → site lockdown active or upstream down
- `sqlx_pool_*` wait timeout spike → database overloaded

### Warning

- `webhook_deliveries_total{status="network_error"}` → webhook endpoint unreachable
- `cache_warm_duration_seconds` > 30s → recommendation engine slow
- `http_request_duration_seconds` p99 > 5s → general latency degradation

### Info

- `automation_runs_total` → automation activity level
- `cards_minted_total` → merchant/admin card activity

## Grafana Dashboard

A minimal dashboard requires:

1. Prometheus data source pointed at `GET /metrics`
2. Panels for: request rate (`rate(http_requests_total[5m])`), error rate, latency histogram, card blocks, cache warming duration

## Graceful Shutdown

On `SIGTERM`:
1. Stop accepting new connections
2. Drain in-flight HTTP requests (up to 30s)
3. Close all WebSocket connections with `Close` frames
4. Cancel the cache warming loop
5. Close the database pool

The `/health` endpoint returns `503` once shutdown begins, allowing load balancers to stop routing traffic.
