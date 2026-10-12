# Plugin Guide

SPACE is extensible at runtime through **HTTP plugins**: small services that
speak a JSON protocol. Write them in any language (Node, Python, Go, Rust…),
host them anywhere, register them from the admin console (`/admin.html`) or
the API — no server restart.

Two kinds:

| kind | Purpose | Protocol |
|---|---|---|
| `metadata` | Search + detail lookups (like built-in TMDB/IMDb/AniList) | `/search`, `/details` |
| `source` | Import an entire external library into the catalog | `/items` |

## Registering

```bash
curl -X POST localhost:8080/api/admin/plugins \
  -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' \
  -d '{ "name":"my-anime-db", "kind":"metadata",
        "base_url":"https://plugins.example.com/mydb", "enabled":true }'
```

Rules: `name` = unique slug `[a-zA-Z0-9_-]{1,48}` and becomes the source key;
`base_url` must be absolute http(s), no trailing slash. Registered plugins are
hot-loaded — they appear instantly in `GET /api/metadata/sources`.

## Metadata plugin protocol

### `GET {base}/search?q=<query>`

```json
{ "results": [
  { "id": "42",
    "title": "Cowboy Bebop",
    "overview": "…",
    "year": 1998,
    "poster": "https://…/poster.jpg",
    "type": "series" }
]}
```
- `id`: your plugin's stable external id (string)
- `type`: `movie` | `tv` | `anime` (hint only)

### `GET {base}/details?id=<id>`

```json
{
  "title": "Cowboy Bebop",
  "original_title": "カウボーイビバップ",
  "overview": "…",
  "release_date": "1998-04-03",
  "runtime_minutes": 24,
  "status": "finished",
  "media_type": "anime",
  "poster": "https://…", "backdrop": "https://…",
  "genres": ["action", "sci-fi", "western"],
  "studios": ["Sunrise"],
  "cast": [ { "name": "Steve Blum", "character": "Spike" } ]
}
```
Only `title` is mandatory; everything else degrades gracefully.
`media_type:"anime"` sets the anime flag on import. Statuses map to:
`announced releasing released ongoing finished cancelled`.

### What import does

`POST /api/metadata/import {"source":"my-anime-db","external_id":"42"}` will
create or update the catalog item, attach genres/studios/cast, store the
mapping `(source=my-anime-db, external_id=42)` so re-imports update in place,
and return the full MediaDetail.

## Source plugin protocol

A source exposes its whole library; sync pulls it into the catalog:

### `GET {base}/items`

```json
{ "items": [
  { "id": "book-123", "title": "Dune", "type": "book",
    "year": 1965, "overview": "…", "poster": "https://…" },
  { "id": "mov-9",    "title": "Arrival", "type": "movie", … }
]}
```

Then trigger (or schedule) the sync:

```bash
curl -X POST localhost:8080/api/admin/plugins/<PLUGIN_ID>/sync \
     -H "Authorization: Bearer $ADMIN"
→ { "imported": 128, "skipped": 12 }
```

Skips items already mapped (dedupe by `(plugin name, id)`). Re-running is
idempotent; new items get added.

## Minimal Node.js metadata plugin (~30 lines)

```js
import express from "express";
const app = express();
const db = [ // your data layer here
  { id: "1", title: "Night Raid 1931", type: "anime", year: 2010,
    overview": "…", genres: ["action","thriller"] }
];

app.get("/search", (req, res) => {
  const q = String(req.query.q ?? "").toLowerCase();
  res.json({ results: db.filter(x => x.title.toLowerCase().includes(q)) });
});

app.get("/details", (req, res) => {
  const hit = db.find(x => x.id === req.query.id);
  if (!hit) return res.status(404).json({ error: "not found" });
  res.json({ ...hit, release_date: `${hit.year}-01-01` });
});

app.listen(7001);
console.log("plugin on :7001");
```

Register with `{"name":"night-raid","kind":"metadata","base_url":"http://127.0.0.1:7001"}`.

## Lifecycle & ops notes

- Timeouts: 10 s per request (metadata), 30 s for source sync pages
- Redirects are not followed; respond directly
- Plugin failures are isolated: errors surface as `503 unavailable` on the
  specific call and never take down core routes
- Disable without deleting via `PATCH … {"enabled":false}`
- `POST …/{id}/test` runs a live search probe and reports counts
