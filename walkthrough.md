# SPACE â€” Complete Source & Extension Walkthrough

Every way to feed SPACE content, how to install each, what's already wired,
and honest limits. Nothing here requires code changes â€” everything is
runtime-installable from the console (`/console`) or curl.

---

## 0. Boot the server

```bash
docker compose up -d postgres redis     # infrastructure (once)
cp .env.example .env                    # set DATABASE_URL, ADMIN_EMAIL/PASSWORD
cargo run --release                     # â†’ http://localhost:8080
```

Login at `/admin.html`, open `/console`. All admin routes below need a
bearer token (or your session cookie).

---

## 1. Built-in sources (zero setup)

Compiled in, always on, auto-failover, circuit-breaker protected:

| Source | Content | Streams |
|---|---|---|
| ia-films | Internet Archive public-domain films | direct MP4 + torrent webseeds |
| gutenberg | Project Gutenberg ebooks | read / EPUB |
| librivox Â· ia-audio | free audiobooks & audio | audio |
| mangadex | licensed manga | chapters/pages reader |
| audius | artist-uploaded music | audio streams |
| podcasts | podcasts | audio |

Unified endpoints you get for free:

```
GET  /api/search/all?q=&category=      merged, deduped, ranked results w/ tokens
POST /api/playback/resolve {token}     best stream (+failover across ALL sources)
POST /api/playback/lookup {title,â€¦}    "where can I watch this?" title-match
GET  /api/sources/{name}/browse        trending/top rails per source
POST /api/admin/sources/{name}/sync    import into local catalog
```

---

## 2. Web Sources â€” learn any site (galaxy engine)

Paste a site url; SPACE's bundled galaxy scraper learns its search page,
result cards and item pages, then replays deterministically forever.

```bash
curl -X POST :8080/api/admin/websources -H "$AUTH" \
  -d '{"site":"https://movies.example","category":"movie"}'
```

* Re-learn when a site redesigns; test endpoint included.
â†’ Full guide: [docs/web-sources.md](docs/web-sources.md)

---

## 3. Extension system â€” Stremio addons (1:1 SDK)

Separate stack (`extensions` table, `/api/extensions/*`). Install by
**manifest url** exactly like Stremio:

```bash
curl -X POST :8080/api/admin/extensions -H "$AUTH" \
  -d '{"url":"https://torrentio.strem.fun/manifest.json"}'   # streams/torrents
curl -X POST :8080/api/admin/extensions \
  -d '{"url":"https://v3-cinemeta.strem.io/manifest.json"}'  # catalogs+meta
```

* Every searchable addon catalog joins unified search automatically.
* Every stream-capable addon is consulted during resolution (torrent
  infoHashes become magnets; m3u8/mp4 pass through; ytId â†’ YouTube).
* Browse/install thousands more via **Extensions â†’ community addons**
  (proxies the central Stremio index).
* Raw 1:1 resource surface for any client:
  `/api/extensions/{addonId}/stream/{type}/{id}` etc.

Verified live against Cinemeta (50 search hits) and Torrentio (61 magnets for
one movie). â†’ [docs/extensions.md](docs/extensions.md)

---

## 4. Hubs â€” connect your servers together

Any SPACE instance can be a fallback provider + recipe store for another.

```bash
curl -X POST :8080/api/admin/hubs -H "$AUTH" \
  -d '{"url":"http://192.168.1.50:8080"}'
POST /api/admin/hubs/{id}/pull    # import its learned recipes
POST /api/admin/hubs/{id}/push    # share yours
```

When local providers can't play something, hubs are asked automatically.
Set `SOURCE_HUB_TOKEN` on the hub (else loopback-only).

---

## 5. Python source plugins (~40 ready-made)

All live in [`plugins/python/`](plugins/python), stdlib-only, one process per
source. Register pattern for every one of them:

```bash
python plugins/python/<name>_plugin.py            # default port below
curl -X POST :8080/api/admin/sources/register-url -H "$AUTH" \
  -d '{"url":"http://127.0.0.1:<PORT>"}'
```

### Movies & TV (TMDB-keyed)
| Plugin | Port | Notes |
|---|---|---|
| zstream | 8904 | P-Stream family watch pages Â· empty query = TMDB trending |
| cineby | 8905 | VidKing player + site page |
| toustream | 8906 | movies/series players |
| cubeflix | 8920 | P-Stream clone Â· trending |
| cinebytv | 8921 | VidKing player + page Â· trending |

### Anime & manga
| Plugin | Port | Notes |
|---|---|---|
| miruro | 8907 | **real HLS per episode** (sub/dub via env) |
| animepahe | 8908 | play pages |
| kaa / animex / anikoto / reanime | 8909â€“8912 | AniList search + site handoff |
| lunastream | 8922 | multi-source; **empty query = their trending** |
| weebcentral | 8928 | server-rendered search fragments |
| nyaa | 8924 | **RSS â†’ magnets** (anime torrents, seeders/size) |
| myanimelist | 8925 | Jikan API anime+manga search |

### Asian dramas & encodes
kissasian(8914) Â· dramanice(8916) Â· pahe(8919) â€” WordPress REST search Â·
kisskh(8915) JSON API

### Multimedia extras
| Plugin | Port | Content |
|---|---|---|
| iptv | 8940 | **thousands of live TV channels** (iptv-org JSON; HLS) |
| radiobrowser | 8941 | 50k radio stations (audio) |
| nasa | 8942 | documentaries, direct MP4 |
| hentaihaven | 8943 | 18+ anime Â· trending browse + episode MP4s |
| movy / cineplay / fmovies / popcornmovies / bingebox | 8944â€“8948 | TMDB-keyed movie & TV frontends (per-site fingerprinted routes) |
| cinejoy / flixer / hexa | 8949â€“8951 | cineby-family/TMDB handoff + passthrough tiers |
| m67nl / m67net | 8952â€“8953 | 67Movies mirrors Â· TMDB search |
| showsst | 8954 | shows.st Â· TMDB, captured `/watch/tv/{id}` route |
| phantomflix / ravenflix / meowtv | 8955â€“8958 | TMDB handoff tier |
| hentai20 | 8973 | **18+ manga Â· live WordPress REST search** |
| bcine / flickystream / coreflix / shuttletv-su / shuttletv-pk / movienig | 8957â€“8963 | passthrough |
| movish / moonflix-web / moviebite / cinegram / shiopa / willowmovies | 8974â€“8981 (subset) | TMDB handoff tier |
| latestmovies | 8975 | movie route `/movie/{id}`; series â†’ handoff |
| chillflix (routed) / cinemove (`/watch/movie/{id}`) / stigstream (`/movie/watch/{id}`) | 8983â€“8987 | captured deep routes |
| vidplay (embed host) / streamo / hivex / flystream / overlook / cineapse / allyoucanwatch / framemovie / tonkacine.watch / tonkacine.xyz | 8976â€“8993 | passthrough tier |
| arrowtv / cinezo | 8964 / 8966 | TMDB with captured `/watch/movie|tv/{id}` routes |
| neonflix / flikhub / opstream | 8965 / 8967 / 8972 | passthrough |
| moovie (vidking family) / spencerdevs (movie route) / cinetaro / vuflix | 8968â€“8971 | TMDB handoff tier |
| hentai20 | 8973 | **18+ manga Â· live WordPress REST search** |
| zxcprime | 8997 | TMDB handoff (filter rails /tv?network= /movie?company=) |
| nextbox-uno / nextbox-su | 9000 / 9001 | TMDB, captured `/player/movie/{id}` route (.uno/.su mirrors) |
| xullys / novahd | 9005 / 9011 | TMDB handoff tier |
| vivarium / flixtrz / cinebolt / cinepro-fstream / cineflix-fstream / nextwork / cinema-army / smashystream / bingebang / nomorflix-cc / diostream / nomorflix-com / novera | 8994â€“9010 rest | passthrough (smovies.co & ww1-fmovies.com dead at build time â€” skipped) |
| netplayz â†’ uniquestream | 9012â€“9058 | **46 more sites** fingerprinted & added: screenscape/apexmovies/kdesa/bingr/peestream/vaultplayer/rreelstream = TMDB handoff; pstream.cfd P-Stream routes; **uniquestream = live WordPress REST search**; netplayz/cinelove/dulo/watchsurface/mappltv/watchott/emnexmovies/streamvaults/nxsha/vegetatv/playtorrioÃ—2/aetherÃ—2/basementxÃ—2/cinevaro/icefy/ee3/rips/cinestream-kje/nepuÃ—5/rar/streamingunity/cinemacity/ridomovies/filmo/azmovies/onionplay/showbox-media = passthrough |

### Passthrough tier
Resolve known item urls into tokens (search reports honestly empty):
rottentomatoes(8926) Â· simkl(8927, needs your client id) Â· comix(8929) Â·
mangadot(8930) Â· noxx(8931) Â· bstsrs(8932) Â· primewire(8933) Â· hanime(8934,18+) Â·
comicbookplus(8935) Â· allcinema(8936) Â· everythingmoe(8937) Â· flicker(8923) Â·
goplay(8913) Â· asiaflix(8917) Â· asiancrush(8918)

### Mirror overrides
Most plugins take `<NAME>_BASE` to run any mirror domain of the same network,
e.g. `MIRURO_API_BASE`, `KISSKH_BASE=kisskh.fr`, `ZSTREAM_BASE`, `CINEBY_BASE`,
`TOUSTREAM_BASE`, `ANIMEPAHE_BASE`, `LEETX_BASE`, `NYAA_BASE`.

---

## 6. The 390-site network â€” every site, its own script

Your mega-list was fingerprinted site-by-site (what stack/data-source each
one actually uses) and **each of the 390 reachable domains got its own
dedicated plugin script** in [`plugins/python/`](plugins/python):

| Fingerprinted family | Count | Engine | Search |
|---|---|---|---|
| WordPress | 21 | `wp_catalog_plugin` | âœ… live REST search |
| AniList frontends | 8 | `anilist_frontend_plugin` | âœ… AniList GraphQL |
| Cineby/VidKing network | 4 | `tmdb_frontend_plugin` | âœ… TMDB |
| P-Stream/TMDB Next.js (+captured routes) | ~36 | `tmdb_frontend_plugin` | âœ… TMDB trending+search |
| JS shells / obfuscated / unreachable | ~321 | `passthrough_plugin` | resolve known urls |

Ports are **stable**: `9000 + catalog index`; full mapping in
`sites_ports.txt`. Start any of them:

```bash
python plugins/python/<slug>_plugin.py            # port is in its docstring
plugins/python/register_all.ps1                   # or .sh â€” registers every running one
```

Regenerate everything after editing fingerprints:
`python plugins/python/gen_site_scripts.py`.

Not converted (by design): t.me/rentry/github/reddit/status/CSE links; giant
legal platforms (Tubi, Pluto, Plex, BBC iPlayer, Arte, Filmzie, NFBâ€¦) flagged
for future dedicated API integrations. archive.org already built-in.

---

## 7. Clean-search mode & daily ratings (fastest-server auto-selection)

**Daily benchmarks:** ~30 s after boot, then every 24 h, every registered
source gets a real end-to-end test â€” timed search â†’ resolve first hit â†’
ranged throughput probe (â‰¤256 KB) on direct media. Results are blended into
a 0â€“100 score and persisted (`source_ratings`).

**Fastest-first everywhere:** `gather_providers` sorts providers by that
score, so fan-out, failover AND title-lookup always consult the best sources
first; `stream_score` still prefers direct mp4/hls over torrents/pages.

Leaderboard:

```bash
curl http://localhost:8080/api/sources/rankings
# [{ "source":"â€¦","score":93.5,"latency_ms":410,"speed_kbps":2871,â€¦ }]
```

Duplicates never appear twice: unified search merges identical
normalized-title+year+category hits into ONE item listing every contributing
source (unit-tested); imports dedupe via external-id mappings.

---

## 8. Troubleshooting

* **503 from a plugin** â†’ upstream blocked/down; check its stderr, try env
  mirror override, or FlareSolverr (`leetx`: `FLARESOLVR_URL`).
* **Jikan/MAL 504** â†’ MAL upstream flakiness; plugin retries, recovers later.
* **Source skipped in search** â†’ check `GET /api/sources/status`
  (circuit breaker) and re-enable after fixing.
* **Extension install fails validation** â†’ url must end in `/manifest.json`
  (or contain it); configurable addons: paste the *configured* manifest url.

## 9. Legal

Several integrated frontends host unlicensed content â€” keep this on your
homelab, mind your jurisdiction, prefer the built-in public-domain/legal
sources and official APIs where they exist.
