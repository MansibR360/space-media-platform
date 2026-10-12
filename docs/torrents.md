# Torrents — the 1337x source plugin

1337x support ships as a **runtime source plugin** — a standalone process
speaking the [source-plugin protocol](plugins.md). Nothing is added to the
server binary: run the script, register it, delete it whenever you like.

> **Legal.** Torrents are P2P metadata links. 1337x indexes both legal and
> infringing content — what you fetch is on you. Keep this on your homelab,
> respect your local law, and seed responsibly.

## Run it

```bash
python plugins/python/leetx_plugin.py --port 8902
# → serving 1337x · Torrents on http://127.0.0.1:8902
```

Stdlib only — no pip installs. Endpoints: `/manifest.json`, `/items` (health
probe), `/search?q=`, `/details?id=<info-url>` (extracts the **magnet link**).

## Register with SPACE

Either paste-and-probe:

```bash
curl -X POST http://localhost:8080/api/admin/sources/register-url \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"url": "http://127.0.0.1:8902"}'
```

or explicitly:

```bash
curl -X POST http://localhost:8080/api/admin/plugins \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"leetx","kind":"source","base_url":"http://127.0.0.1:8902","enabled":true}'
```

Once registered it joins everything automatically:

* unified search fans out to it (`GET /api/search/all`),
* play tokens resolve through `/details`, returning `kind:"torrent"` streams
  (ranked as last resort below any direct mp4/HLS from other sources),
* health probing keeps it skipped when the site or plugin is down,
* `POST /api/admin/sources/leetx/sync` works too (imports nothing — torrents
  aren't library items).

## Getting blocked? Transport chain

The plugin tries, in order:

1. **direct** HTTPS fetch,
2. **FlareSolverr** when `FLARESOLVR_URL=http://localhost:8191` is set,
3. your local **galaxy** binary (auto-discovered next to the repo, or
   `GALAXY_BIN=/path/to/galaxy.exe`) — set `GALAXY_STRATEGY=auto` to allow its
   stealth headless-browser escalation for hard Cloudflare days.

Other env knobs: `LEETX_BASE=https://mirror.example` for proxies/mirrors.

## Watching results

Resolution hands clients a `magnet:` url. Feed it to whatever you run locally,
e.g. qBittorrent's WebUI (`Add torrent → URL`), or wire download automation:
SPACE's automation rules can react to `media.added` events, and the resolved
stream is available via `POST /api/playback/resolve {token}` for scripts.
