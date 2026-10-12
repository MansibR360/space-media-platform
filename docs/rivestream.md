# RiveStream source plugin

[RiveStream](https://www.rivestream.app) support ships as a **runtime source
plugin** — a standalone process speaking the [source-plugin
protocol](plugins.md). Nothing is compiled into the server binary.

RiveStream publishes a public embed API keyed by **TMDB ids** (movies, TV
seasons/episodes, torrent-backed playback, aggregator servers, downloads).
This plugin bridges SPACE onto it:

* `/search?q=` → proxied through **TMDB multi-search** so normal queries work
  (`TMDB_API_KEY` from your `.env` is reused automatically)
* `/details?id=` → returns four `kind:"page"` streams per title:

| Stream | Endpoint |
|---|---|
| Rive · fastest server | `/embed?type=…&id=…` |
| Rive · aggregator servers | `/embed/agg?…` |
| Rive · torrent stream | `/embed/torrent?…` |
| Rive · download | `/download?…` |

> Unlicensed streaming service: what you play is on you. Keep it homelab.

## Run it

```bash
python plugins/python/rivestream_plugin.py --port 8903
```

Stdlib only. Env knobs:

| Var | Meaning |
|---|---|
| `TMDB_API_KEY` | enables search; auto-read from the SPACE `.env` when unset |
| `RIVESTREAM_BASE` | site base override, default `https://www.rivestream.app` |

## Register

```bash
curl -X POST http://localhost:8080/api/admin/sources/register-url \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"url": "http://127.0.0.1:8903"}'
```

or explicitly `POST /api/admin/plugins {"name":"rivestream","kind":"source",
"base_url":"http://127.0.0.1:8903","enabled":true}`.

Afterwards: unified search includes it, tokens resolve to its players, and
health probing skips it when it's down.

## Id format (for scripts / manual calls)

```
movie:{tmdb_id}            e.g. movie:278        (The Dark Knight)
tv:{tmdb_id}               defaults to S1E1      e.g. tv:1396       (Breaking Bad)
tv:{tmdb_id}:{s}:{e}       explicit episode      e.g. tv:1396:2:7
```

Without a TMDB key, `/details` still resolves ids to players — only search
needs the key.
