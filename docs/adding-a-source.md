# Adding a Source

SPACE treats every catalog identically: search fans out to all healthy
sources, results merge, and the central resolver picks streams at play time.
There are **two ways** to plug into that, depending on where your code runs.

---

## Way 1 — Compiled-in source (Rust, `NativeSource`)

Best for: sources you maintain and want zero-hop in-process speed.
Reference implementation: `src/sources/librivox.rs` (added as the worked
example alongside this guide).

The whole contract is four required + three optional methods:

```rust
#[async_trait]
impl NativeSource for MySource {
    fn name(&self) -> &'static str { "mysource" }        // registry key
    fn label(&self) -> &'static str { "My Source" }      // display
    fn category(&self) -> &'static str { "book" }        // movie|series|book|manga|music|podcast|audiobook

    async fn search(&self, query: &str, limit: i32)
        -> ApiResult<Vec<SourceItem>>;                   // cards for /api/search/all
    async fn item(&self, external_id: &str)
        -> ApiResult<SourceDetail>;                      // detail + playable StreamLinks

    // optional overrides:
    fn capabilities(&self) -> SourceCapabilities { .. }  // tells the resolver what you support
    fn manifest(&self) -> serde_json::Value { .. }       // self-description for /api/sources
    async fn browse(&self, page: i32) -> Option<Vec<SourceItem>> { .. }
}
```

`StreamLink` is `{ label, url, kind }` where `kind` is one of `mp4 | hls |
audio | page | file`. URLs must be directly fetchable — that is what the
resolver hands clients.

### Registration (one line)

```rust
// src/sources/mod.rs → SourceRegistry::new()
providers: vec![
    Box::new(archive::ArchiveFilms::default()),
    Box::new(librivox::LibriVox::default()),   // ← yours
    …
]
```

That single registration wires you into everything automatically:

* `/api/search/all` fan-out with per-source timeouts and duplicate merging
* circuit breaker (3 failures → skipped) + 60 s half-open prober
* telemetry on `/api/sources/status` (latency, success rates, active requests)
* resolver preference ranking + title-based failover
* admin sync-to-catalog imports and debug endpoints

## Way 2 — HTTP plugin (any language)

Best for: third-party or dynamically deployed catalogs. SPACE speaks a tiny
JSON protocol:

| Route | Response |
|---|---|
| `GET /manifest.json` | `{ name, label, category, transport }` |
| `GET /items?limit=` | `{ items: [SourceItem] }` |
| `GET /search?q=&limit=` | `{ results: [SourceItem] }` |
| `GET /details?id=` | `{ title, overview?, year?, poster?, media_type, streams: [{label,url,kind}], chapters? }` |

Register without touching the server binary:

```http
POST /api/admin/sources/register-url
{ "url": "https://my-plugin.example.com" }
```

SPACE probes the three routes and registers whichever protocol answers.

**Python?** A runnable stdlib-only template lives at
`plugins/python/space_plugin.py` — see `docs/plugins-python.md`.

---

## Rules that keep a source in good standing

1. **Legality**: catalogs must be public-domain, openly licensed, or
   authorized. Sources serving unlicensed copies of copyrighted works are not
   accepted — this is a hard project rule, not a style preference.
2. **Direct URLs**: every `StreamLink.url` must be playable without extra
   scraping hops at request time.
3. **Timeouts matter**: fan-out allows 9 s per provider; slow sources get
   dropped from results and their success rate suffers.
4. **Failures are cheap**: return `Err` early rather than empty success —
   the breaker uses outcomes to protect the rest of the platform.
