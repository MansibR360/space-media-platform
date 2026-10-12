# Web Sources — turn any site into a source with one field

Web sources are **learned** catalogs. You paste a site url, SPACE asks the
bundled [galaxy](../README.md) scraper to figure out how the site's search
works and what an item page looks like, stores the resulting *recipe*, and the
site immediately joins unified search (`/api/search/all`), playback resolution
with failover, health monitoring and admin sync — exactly like compiled-in
sources.

## Quick start

```bash
cargo build --release            # space server
cd ../galaxy && cargo build --release   # galaxy binary (sibling checkout)
```

SPACE finds `galaxy.exe` automatically in `./`, `./target/release/`,
`../galaxy/target/release/`; or set `GALAXY_BIN` explicitly.

### 1. Teach it a site (all fields except `site` optional)

```bash
curl -X POST http://localhost:8080/api/admin/websources \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{ "site": "https://movies.example.com", "category": "movie" }'
```

That's it. Under the hood galaxy:

1. sniffs the site's homepage for a GET search form (falls back to probing
   common patterns `/search?q=`, `/?s=`, …);
2. infers repeating result-card structure (row selector + title/link/poster/
   overview fields) or falls back to a URL-shape pattern;
3. fetches one sample item page (yours via `sample_item`, else the first hit)
   and learns detail selectors + stream probes (video tags, embeds iframes,
   direct media links);
4. validates everything against the live pages before saving.

Optional overrides: `name`, `label`, `search_url`
(`"https://site/search?q={query}"` — must contain `{query}`), `sample_item`,
`category` (`movie|series|book|music|manga`).

### 2. Use it like any other source

* `GET /api/search/all?q=…` — learned sites fan out with everything else;
  tokens work on `POST /api/playback/resolve` and `POST /api/resolve`.
* `GET /api/sources/{name}/browse` — empty-query "latest" feed.
* `POST /api/admin/sources/{name}/sync` — import items into your catalog.
* Health circuit-breaker: dead sites are skipped automatically.

### 3. Manage

| Endpoint | Purpose |
|---|---|
| `GET /api/admin/websources` | list + liveness |
| `PATCH /api/admin/websources/{id}` | `{label?, category?, enabled?}` |
| `DELETE /api/admin/websources/{id}` | remove |
| `POST /api/admin/websources/{id}/relearn` | re-run learning (sites change!) |
| `POST /api/admin/websources/{id}/test` | live search + first-item resolve |
| `GET /api/admin/websources/{id}/recipe` | raw recipe JSON |

The admin console has a **Web Sources** tab covering all of the above.

## How it works at runtime

Recipes replay deterministically through the local galaxy binary
(`galaxy source search|item --recipe -`, JSON over stdin/stdout). No AI is
involved after learning; failures surface as `503 unavailable` and feed the
source circuit breaker. Recipes are plain JSON — inspect them with
`galaxy source test` or fix them by hand in the DB if a site redesigns.

## Source hubs — connect servers together

Any SPACE instance can act as a **hub** for others: it serves `/api/hub/*`
from its learned sources, accepts uploaded recipes, and answers find/item
queries when a peer comes up empty. This is the "server asks that API" tier:

```
your laptop SPACE ──search/resolve fallback──▶ hub SPACE ──▶ learned sites
        ▲                                            │
        └──────────── push / pull recipes ───────────┘
```

* `GET  /api/hub/find?q=&category=&limit=` — search across the hub's learned sources
* `GET  /api/hub/item?source=&id=` — detail + streams (the "give it to me to watch" hop)
* `GET  /api/hub/sources` — recipe catalog
* `POST /api/hub/sources` — import a recipe into the hub

**Auth:** set `SOURCE_HUB_TOKEN` on the hub and send the same value as the
`X-Hub-Token` header. With no token configured, `/api/hub/*` answers
**loopback peers only**.

### Client side (admin, localhost-managed like web sources)

| Endpoint | Purpose |
|---|---|
| `POST /api/admin/hubs` | `{url, token?}` — probes and registers |
| `GET /api/admin/hubs` | list + server-side status |
| `PATCH /api/admin/hubs/{id}` | `{url?, token?, enabled?}` |
| `DELETE /api/admin/hubs/{id}` | disconnect |
| `POST /api/admin/hubs/{id}/pull` | import the hub's recipes locally |
| `POST /api/admin/hubs/{id}/push` | upload your recipes to the hub |
| `POST /api/admin/hubs/{id}/test` | live find query |

Once registered, a hub automatically joins unified search **and resolution
failover**: when every local provider fails to produce a playable stream, the
resolver asks your hubs before giving up — results become watchable tokens
like everything else. Dead hubs are skipped via the source circuit breaker.

## Configuration

| Env | Default | Meaning |
|---|---|---|
| `WEB_SOURCES_ENABLED` | `true` | master switch |
| `GALAXY_BIN` | auto-discover | path to the galaxy binary |
| `GALAXY_TIMEOUT_SECS` | `25` | per-invocation subprocess timeout (5–120) |
| `SOURCE_HUB_TOKEN` | unset | inbound auth for `/api/hub/*`; unset = loopback-only hub |
| `HUB_TIMEOUT_SECS` | `15` | outbound timeout when querying registered hubs |

## CLI equivalents (galaxy side)

```bash
galaxy source learn  --site https://site.example [--search-url ..] [--sample-item ..] [--category movie]
galaxy source search --recipe recipe.json "query" --limit 20
galaxy source item   --recipe - https://site.example/movie/x   # recipe on stdin
galaxy source test   --recipe recipe.json
```

## Legal & etiquette

Learned sources scrape third-party sites: respect their terms and local law,
and prefer official APIs/plugins when they exist.
