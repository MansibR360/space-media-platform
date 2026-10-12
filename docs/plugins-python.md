# Site Plugins (native)

The site plugins are Rust. They used to be ~500 Python files, each a small HTTP
server on its own port that had to be launched and then registered by hand; all
of them are now compiled into the server binary and register themselves at
startup. The originals were moved out of the tree once converted.

Nothing to launch, no ports, no interpreter, no `register_all.sh`.

## How it works

Three pieces, all in `src/plugins/native/`:

| file | what it is |
|---|---|
| [`sites.rs`](../src/plugins/native/sites.rs) | the 481 sites that reduced to a shared engine: name, label, category, url, route templates |
| [`engines.rs`](../src/plugins/native/engines.rs) | the four behaviours those sites share |
| [`custom/`](../src/plugins/native/custom/) | the 23 plugins that had real code of their own |
| [`mod.rs`](../src/plugins/native/mod.rs) | binds sites to engines, serves them, registers them |

The Python plugins only ever differed in data. Each was a call to one of four
factories in `_plugin_server.py` with constant arguments, so the conversion is
those factories rewritten in Rust plus one table row per site:

| engine | sites | behaviour |
|---|---:|---|
| `Passthrough` | 293 | JS-shell catalogs with no usable search; resolves any known item url on the host |
| `TmdbFrontend` | 124 | TMDB-keyed frontends; deep links follow the site's captured route template |
| `WpCatalog` | 31 | WordPress sites exposing `/wp-json/wp/v2/search` |
| `AnilistFrontend` | 24 | AniList-keyed anime frontends whose watch routes are JS-only |
| `JsonApi` | 8 | sites whose own JSON search endpoint was fingerprinted |
| `Pandora` | 1 | pan.do/ra archives, which answer a documented JSON API |

Counts shift as sites are fingerprinted: `space --detect-passthrough` reads
each remaining `Passthrough` site the way a browser would - fetching its page
and bundles and seeing which service its front-end actually calls - and reports
the engine it could use. 188 sites have been moved off `Passthrough` that way. A fingerprint is only
applied after the endpoint is verified to actually filter - several sites were
detected and then reverted because their endpoint answered the same list for
every query, or was briefly up during the sweep and down on verification.

## Playback

A source has to play, not just list. These sites host no video: a watch page
embeds a third-party player, and that player holds the stream behind its own
API. There are only a handful of those providers behind hundreds of sites, so
playback is solved per provider in
[`custom/embeds.rs`](../src/plugins/native/custom/embeds.rs), not per site.

The chain, all server-side:

```
/api/search?q=          -> results
/watch/{slug}?ep={n}    -> page carries the player id
{provider}/stream/...   -> embed page carries an internal id
/stream/getSources?id=  -> master.m3u8 + subtitle tracks
```

It runs in Rust because those endpoints require `Referer` and
`X-Requested-With`, which a browser cannot set cross-origin. The Python
plugins never attempted this - every one returned a `kind: "page"` handoff
link, several saying stream extraction was out of scope.

Resolved streams are cached for 10 minutes, so a replay, a seek or a second
viewer does not re-walk the chain. Providers are tried best-first and the next
is used if one yields nothing; the watch page is always offered last as a
fallback.

## Search results carry no source

A result is a title and an opaque token - never the backends behind it. Which
source actually plays is decided at resolve time, fastest-first with failover,
so naming sources in results would only go stale the moment one degrades.

## The converted plugins

Twenty-three plugins were not just a factory call - they had their own APIs,
scrapers and player-url schemes. Each is a Rust type in
[`custom/`](../src/plugins/native/custom/), keeping that plugin's ids, labels,
notes and error strings:

| file | plugins |
|---|---|
| `tmdb_player.rs` | cineby, cinebytv, cineplay, cubeflix, fmovies, pstream-cfd, toustream, zstream, rivestream |
| `apis.rs` | kisskh, myanimelist (Jikan), simkl, nasa, radiobrowser, iptv, miruro, animepahe |
| `scrapers.rs` | lunastream, weebcentral, hentaihaven, nyaa, leetx |
| `openmovies.rs` | openmovies |

The nine player sites differed only in which urls they built from a TMDB id, so
they share one engine parameterised by url templates. Their env overrides
(`CINEBY_BASE`, `TOUSTREAM_SERIES_TEMPLATE`, `MIRURO_CATEGORY`,
`SIMKL_CLIENT_ID`, `NYAA_BASE`, `LEETX_BASE`, …) are read exactly as before.

Three files are deliberately not registered:

* `space_plugin.py` - the documentation template. Its demo items point at
  example.com, so it stays a doc rather than becoming a dead source.
* `binge_bang_plugin.py` and `kawaii_anime_plugin.py` - duplicates declaring
  the same source name and site as another file. `plugins.name` is unique, so
  only one of each pair could ever have registered.

One capability was dropped: 1337x could fall back to a FlareSolverr instance
via `FLARESOLVR_URL`. The Rust version fetches directly and reports failure
honestly instead; SPACE's own galaxy integration is the anti-bot path here.

## Registration

At startup the server upserts one `plugins` row per site
(`kind = 'source'`, `config.managed_by = 'native-engine'`) pointing at its own
in-process routes. From that moment every site is an ordinary source: unified
search fan-out with per-source timeouts, circuit breaking, health probing,
benchmark ranking, and the admin console's *Plugins & Sources* list.

`enabled` is only ever set when a row is first created, so a source the admin
console or the AI ops loop disabled stays disabled across restarts. A site that
leaves `sites.rs` has its row disabled rather than deleted.

Two settings, both optional:

| var | default | meaning |
|---|---|---|
| `NATIVE_PLUGINS_ENABLED` | `true` | register the built-in sites at startup |
| `NATIVE_PLUGINS_BASE` | `http://127.0.0.1:{APP_PORT}` | base url the server reaches its own routes on |

Each site still honours the `{NAME}_BASE` override its Python file read, so an
existing override in `.env` keeps working. `sites.rs` carries the exact env key
per site.

## Routes

Every site answers the same contract the Python servers did, under its own
prefix:

| route | response |
|---|---|
| `GET /api/native-plugins` | index of every site and its base url |
| `GET /api/native-plugins/{name}/manifest.json` | `{ name, label, category, kind, transport }` |
| `GET /api/native-plugins/{name}/search?q=&limit=` | `{ results: [SourceItem] }` |
| `GET /api/native-plugins/{name}/details?id=` | `{ title, overview?, year?, poster?, media_type, streams }` |
| `GET /api/native-plugins/{name}/items` | `{ items: [] }` — these sites expose no bulk library |

## Adding or changing a site

Edit [`sites.rs`](../src/plugins/native/sites.rs) — add a `Site { … }` entry
naming one of the four engines — and rebuild. Adding a genuinely new behaviour
means a new `EngineKind` variant and its arm in `engines.rs`.

## Parity with the Python originals

The engines reproduce the Python outputs exactly, down to the stream labels and
the validation error strings. Two checks cover this:

* `cargo test --bin space plugins::` — the table is well-formed and every site
  usable, the shared helpers (slug titles, mirror matching, media-id parsing)
  behave as the Python did, every converted plugin rejects a bad id with its
  original message, and the player url schemes are checked url by url.
* `space --dump-native-parity` — dumps what every site answers for a fixed set
  of offline inputs, for diffing against the originals.

Two deliberate differences from the Python files:

* A U+FEFF that some generated files baked into their site name is stripped, as
  it would fail plugin-name validation. The env key keeps the original spelling.
* 309 of the Python files never ran: they called the factory without assigning
  it, then did `serve(cfg["manifest"], …)`, which raises `NameError` on start.
  The conversion takes the plugin those files describe, so those sites work.

## External HTTP plugins

Unrelated and unchanged: anything can still be registered at runtime over HTTP
in any language — see [plugins.md](plugins.md).
