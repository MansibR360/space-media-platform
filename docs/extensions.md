# Extension system — Stremio addons, 1:1

SPACE speaks the [Stremio Addon SDK](https://github.com/Stremio/addon-sdk)
protocol natively. Install any addon from the stremio ecosystem by its
manifest url and it becomes a first-class source: searchable catalogs join
unified search, `/stream` endpoints join playback resolution with failover,
and subtitles aggregate across all installed addons.

This is a **separate system** from SPACE's plugin/hub/web-source stacks —
different table (`extensions`), different namespace (`/api/extensions/*`,
`/api/admin/extensions`), different manager.

## The protocol (implemented 1:1)

```text
GET {base}/manifest.json
GET {base}/catalog/{type}/{catalogId}/{encoded extras}.json   e.g. search=batman&skip=100
GET {base}/stream/{type}/{id}.json        id: tt1254207 | tt123:1:2 | tmdb:… | mal:…
GET {base}/meta/{type}/{id}.json
GET {base}/subtitles/{type}/{id}.json?extra=…
```

Stream shapes mapped to SPACE kinds: `url` → mp4/hls/page (sniffed),
`infoHash` → **torrent magnet**, `ytId` → YouTube page.

## Install (admin console → Extensions tab, or REST)

```bash
curl -X POST http://localhost:8080/api/admin/extensions \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"url":"https://torrentio.strem.fun/manifest.json"}'
```

The exact Stremio flow works: configure an addon on its website, copy the
resulting manifest url, paste it here. Well-known starting points:
Cinemeta `https://v3-cinemeta.strem.io/manifest.json`,
Torrentio `https://torrentio.strem.fun/manifest.json`.

### Browse community addons

`GET /api/admin/extensions/discover?q=torrent` proxies the central Stremio
index so you can one-click-install from the console tab.

## Management (localhost-only admin surface)

| Endpoint | Purpose |
|---|---|
| `GET/POST /api/admin/extensions` | list / install |
| `PATCH /api/admin/extensions/{id}` | enable/disable |
| `DELETE /api/admin/extensions/{id}` | remove |
| `GET /api/admin/extensions/discover?q=` | central index browser |

## Resource surface (any authenticated user)

1:1 passthrough to any installed addon:

```
GET /api/extensions/{addonId}/catalog/{type}/{catalogId}?search=&genre=&skip=
GET /api/extensions/{addonId}/stream/{type}/{id}
GET /api/extensions/{addonId}/meta/{type}/{id}
GET /api/extensions/subtitles/{type}/{id}      # fans out across all addons
```

## Unified integration

* every catalog with `extraSupported: ["search", …]` becomes a provider in
  `/api/search/all`; results are deduped/merged with everything else;
* every stream-capable addon is consulted during token resolution and
  title-based lookup (including stream-only addons like Torrentio that have
  no catalogs);
* series ids pass through verbatim — clients may resolve `tt123:2:5`
  episode-level tokens directly.

Config: `EXTENSION_TIMEOUT_SECS` (default 15).
