# Streaming site plugins

Thirty-four ready-made **runtime source plugins** in `plugins/python/`. Each
is a standalone process speaking the [source-plugin protocol](plugins.md) —
run it, register it, delete it whenever. Nothing is compiled into SPACE.

## Movies & TV (TMDB-keyed)

| Plugin | Port | Streams returned |
|---|---|---|
| `zstream_plugin.py` | 8904 | Z-Stream watch pages (`/media/tmdb-movie-…`) |
| `cineby_plugin.py` | 8905 | VidKing embed players + Cineby page |
| `toustream_plugin.py` | 8906 | TouStream players (`/tou/movies/…`, `/tou/series/…`) |
| `rivestream_plugin.py` | 8903 | RiveStream players ([docs](rivestream.md)) |
| `cubeflix_plugin.py` | 8920 | Cubeflix (P-Stream clone) watch pages · **empty query → TMDB trending** |
| `cinebytv_plugin.py` | 8921 | VidKing player + CinebyTV page · **empty query → TMDB trending** |

## Anime (AniList-keyed)

| Plugin | Port | Streams returned |
|---|---|---|
| `miruro_plugin.py` | 8907 | **real HLS m3u8** per episode + watch page |
| `animepahe_plugin.py` | 8908 | play/all-episode pages |
| `kaa_plugin.py` | 8909 | site handoff links (see capability tiers) |
| `animex_plugin.py` | 8910 | site handoff links |
| `anikoto_plugin.py` | 8911 | site handoff links |
| `reanime_plugin.py` | 8912 | site handoff links |
| `lunastream_plugin.py` | 8922 | info-page streams · **empty query → their trending feed** |

## Asian dramas & encodes

| Plugin | Port | Search | Streams returned |
|---|---|---|---|
| `kissasian_plugin.py` | 8914 | ✅ WP REST API | drama/episode pages |
| `dramanice_plugin.py` | 8916 | ✅ WP REST API | drama pages |
| `pahe_plugin.py` | 8919 | ✅ WP REST API | encode post pages |
| `kisskh_plugin.py` | 8915 | ✅ JSON API | drama page (`/Drama/{slug}?id=`) |
| `goplay_plugin.py` | 8913 | — | passthrough by url |
| `asiaflix_plugin.py` | 8917 | — | passthrough by url |
| `asiancrush_plugin.py` | 8918 | — | passthrough by url |
| `flicker_plugin.py` | 8923 | — | passthrough by url (obfuscated client) |

> These frontends are unlicensed unless noted (AsianCrush is a legit
> ad-funded service) — what you play is on you. Keep them on your homelab.

## Shared behavior

* All speak `/manifest.json`, `/items` (health probe), `/search?q=`,
  `/details?id=` — after registering they join unified search, playback
  resolution with failover, and health monitoring automatically.
* TMDB-keyed sites bridge search through **TMDB** using `TMDB_API_KEY`
  (env or the SPACE `.env`). Ids: `movie:{tmdb}` / `tv:{tmdb}[:s]:[e]`.
* Anime sites key on **AniList** (public GraphQL, no key): `anilist:{id}[:ep]`.
* Three drama sites are WordPress and searched through their own
  `/wp-json/wp/v2/search`; ids are canonical post urls (mirror hosts such as
  `wwv19.kissasian.com.lv` are accepted automatically).

## Source-only mode (recommended for these sites)

By default a registered source also appears in unified search
(`/api/search/all`). For these streaming-site plugins you usually want the
opposite: **clean search, sources asked only at play time.** Register with
`resolve_only` and use the lookup endpoint:

```bash
# 1) register as resolve-only
curl -X POST http://localhost:8080/api/admin/plugins \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"name":"kissasian","kind":"source",
       "base_url":"http://127.0.0.1:8914","enabled":true,
       "config":{"resolve_only":true}}'

# 2) pick something via your normal catalog/metadata search, then:
curl -X POST http://localhost:8080/api/playback/lookup \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"title":"Vincenzo","year":2021,"category":"series"}'
# → {"items":[{"token":"…","source":"kissasian","title":"Vincenzo …"}]}

# 3) play exactly as always:
curl -X POST http://localhost:8080/api/playback/resolve \
  -H "Authorization: Bearer $TOKEN" -d '{"token":"<token from lookup>"}'
```

Resolve-only sources are excluded from `/api/search/all` fan-out entirely but
are fully included in `POST /api/playback/lookup` (title-matched, ±1 year,
category-filtered) and in resolution failover.

## Anime / manga / torrents (direct APIs)

| Plugin | Port | Data source | Streams |
|---|---|---|---|
| `nyaa_plugin.py` | 8924 | Nyaa **RSS feed** (official machine interface) — title/infoHash/seeds/size per entry | **magnet** torrents |
| `mal_plugin.py` | 8925 | Jikan REST (MyAnimeList anime+manga search; no key; rate-limited upstream) | MAL info page |
| `weebcentral_plugin.py` | 8928 | server-rendered `/search/data` fragments | series pages |
| `simkl_plugin.py` | 8927 | official Simkl API (`SIMKL_CLIENT_ID` required — register an app) | simkl app page |
| `rottentomatoes_plugin.py` | 8926 | RT APIs are CF-walled → passthrough | known urls |

## Passthrough tier (resolve known urls)

`comix`(8929) · `mangadot`(8930) · `noxx`(8931) · `bstsrs`(8932) ·
`primewire`(8933) · `hanime`(8934, 18+) · `comicbookplus`(8935) ·
`allcinema`(8936) · `everythingmoe`(8937)

## Search dedupe (two providers, one result)

Unified search already collapses duplicates: results with the same
normalized title + year + category merge into a single item whose `sources`
array lists every contributing provider, keeping the best-ranked token.
Title-based lookup matching (`POST /api/playback/lookup`) uses the same
normalization, so "Vincenzo" from two providers never yields two rows.
Metadata *imports* dedupe separately via external-id mappings.

## Capability tiers (honest limits)

* **Full search + resolve**: zstream, cineby, toustream, rivestream, miruro,
  kissasian, dramanice, pahe, kisskh.
* **Search via AniList + site-handoff streams**: kaa, animex, anikoto,
  reanime — JS-only frontends without stable deep-link formats; resolution
  opens the site's search pre-filled with the exact title (one click). Know a
  deep-link format? Set e.g. `KAA_WATCH_TEMPLATE="{site}/watch/{anilist_id}"`
  and real deep links are emitted instead.
* **Passthrough only**: goplay (origin unreachable at build time), asiaflix &
  asiancrush (app-locked APIs) — resolve known item urls into tokens; search
  reports empty until an upstream endpoint exists.

## Run & register

```bash
python plugins/python/zstream_plugin.py     # repeat per plugin, ports above

curl -X POST http://localhost:8080/api/admin/sources/register-url \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"url": "http://127.0.0.1:8904"}'     # once per running plugin
```

## Env knobs

| Plugin | Vars |
|---|---|
| zstream / cineby / rivestream | `TMDB_API_KEY`, site-base override |
| toustream | `TOUSTREAM_BASE`, `TOUSTREAM_SERIES_TEMPLATE` |
| miruro | `MIRURO_API_BASE`, `MIRURO_CATEGORY` = `sub`\|`dub` |
| animepahe | `ANIMEPAHE_BASE` |
| kaa / animex / anikoto / reanime | `<NAME>_BASE`, `<NAME>_WATCH_TEMPLATE` |
| kissasian / dramanice / pahe / kisskh / goplay / asiaflix / asiancrush | `<NAME>_BASE` mirrors |

## Notes

* Sites change often — every URL template lives in one small file you can
  edit without touching the server.
* For direct files pair with the [1337x torrent plugin](torrents.md); pahe
  posts are download-oriented encodes.
