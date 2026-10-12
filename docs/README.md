# SPACE Documentation

Everything you need to run, understand, and build against the platform.

## Core

| Document | What's inside |
|---|---|
| [Getting Started](getting-started.md) | Run it locally in 5 minutes; first admin; first scan |
| [Architecture](architecture.md) | Modules, data flow, key design decisions |
| [Configuration](configuration.md) | Every environment variable explained |
| [Database](database.md) | Schema map per domain + CockroachDB/TiDB notes |
| [Deployment](deployment.md) | Production Docker, nginx/TLS, backups, monitoring |

## API & Protocol

| Document | What's inside |
|---|---|
| [API Reference](API.md) | **Every endpoint** with auth, bodies, and curl examples |
| [WebSocket Protocol](websocket.md) | Real-time events: chat, presence, party sync, notifications |
| [Media Graph](media-graph.md) | Typed relationships, BFS traversal, cross-media journeys |
| [Security Model](security-model.md) | Card gate → risk engine → grants → watermarks → offline keys |

## Features

| Document | What's inside |
|---|---|
| [Mood Search & Smart Collections](mood-search.md) | Vibe matching taxonomy, taste profiles, home rails, rule-based collections |
| [Card Economy](card-economy.md) | Card lifecycle, merchant workflow, activation hardening, HMAC checksums |
| [Automation & Webhooks](automation.md) | WHEN/IF/THEN rules, event types, HMAC-signed outbound webhooks |

## Extension System

| Document | What's inside |
|---|---|
| [Plugin Guide](plugins.md) | Write metadata & source plugins in any language |
| [Python Plugins](plugins-python.md) | Quickstart for Python source plugins |
| [Streaming Plugins](streaming-plugins.md) | 34+ ready-made streaming plugin catalog |
| [Stremio Extensions](extensions.md) | Install community addons (1:1 Stremio protocol) |
| [Web Sources](web-sources.md) | Galaxy engine: paste a site URL, SPACE learns its structure |
| [Adding a Source](adding-a-source.md) | Two paths: Rust NativeSource or HTTP plugin |
| [Hub Federation](web-sources.md#source-hubs) | Connect SPACE instances as fallback providers |

## Operations

| Document | What's inside |
|---|---|
| [Telemetry & Monitoring](telemetry.md) | Prometheus metrics, cache warming, health endpoints, alerting |
| [Heuristic Engine](heuristic-engine.md) | Behavioral learning, Wrapped recap, prediction scoring |
| [Torrents](torrents.md) | 1337x torrent plugin setup |
| [RiveStream](rivestream.md) | RiveStream plugin setup |
| [Security Audit](../SECURITY-AUDIT.md) | Findings, threat model, crypto inventory, test coverage |
| [Security Posture](../SECURITY.md) | Auth, authorization, abuse resistance summary |
| [Roadmap](../ops/ROADMAP.md) | M1–M13 milestones with shipped/planned status |
| [PITR](../ops/pitr.md) | Point-in-time recovery with WAL archiving |
| [Walkthrough](../walkthrough.md) | Complete content source walkthrough (40+ plugins) |

## Quick Links (once running)

- Web client: `http://localhost:8080/`
- Swagger UI: `http://localhost:8080/docs`
- Plugin console: `http://localhost:8080/admin.html`
- Health: `GET /health` · Readiness: `GET /api/ready` · Metrics: `GET /metrics`
