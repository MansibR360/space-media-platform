# Script Plugins (JavaScript providers)

Drop a folder into `plugins/js/` and it becomes a source. No rebuild, no
compilation — the server reads the directory at startup.

The contract is [Nuvio's provider API](https://github.com/NuvioMedia/NuvioMobile),
so a provider written for that ecosystem runs here unchanged.

## Why they exist

Almost every site in the built-in catalog only offers a *watch page*, which
means leaving the app to play anything. A provider returns the **stream url
itself**, so playback happens in SPACE's own player.

## Layout

```text
plugins/js/
  my-provider/
    manifest.json
    provider.js
```

### manifest.json

```json
{
  "id": "my-provider",
  "name": "My Provider",
  "version": "1.0.0",
  "author": "you",
  "description": "What it does",
  "supportedTypes": ["movie", "tv"],
  "filename": "provider.js",
  "enabled": true
}
```

Only `id` is required. `supportedTypes` may be omitted to answer for
everything. `enabled: false` keeps a plugin on disk but out of the app.
Both `supportedTypes` and `supported_types` are accepted.

### provider.js

```js
async function getStreams(tmdbId, mediaType, season, episode) {
  const res = await fetch(`https://upstream.example/api/${tmdbId}`)
  const data = await res.json()

  return [
    {
      name: 'My Provider',
      title: '1080p',
      url: data.stream,        // required
      quality: '1080p',
      headers: { Referer: '…' } // optional, sent with the stream request
    },
  ]
}
```

| argument | meaning |
|---|---|
| `tmdbId` | TMDB id as a string, e.g. `"27205"` |
| `mediaType` | `"movie"` or `"tv"` |
| `season` | 1-based, `null` for films |
| `episode` | 1-based, `null` for films |

A row without a `url` is skipped; a row is not required to have anything else.
Returning `[]` means "nothing found" and is not an error.

## What a provider can use

| available | notes |
|---|---|
| `fetch(url, init)` | `method`, `headers`, `body`; returns a Response with `status`, `ok`, `text()`, `json()` |
| `console.log/info/debug/warn/error` | goes to the server log, tagged with the plugin id |
| standard JavaScript | `async`/`await`, Promises, `JSON`, regex, string and array methods |

Not available: Node built-ins (`fs`, `crypto`, `require`), timers, and the DOM.
A provider is a pure function from an id to a list of streams.

`fetch` is synchronous underneath — the host performs the request and hands
back an already-resolved promise. `await` behaves exactly as expected; the
practical effect is that requests inside one provider run in sequence, while
providers themselves run concurrently.

## Limits

| limit | value | why |
|---|---|---|
| provider run | 25 s | a slow or looping script cannot hold a request open |
| single `fetch` | 15 s | one dead upstream should not spend the whole budget |
| response body | 8 MB | large bodies are refused rather than pulled into the engine |

Each run gets a fresh engine and its own thread; a plugin cannot see another
plugin's state or leak into the next request.

## How they appear in the app

At startup each plugin is registered in the `plugins` table as a
`kind = 'source'` row (`config.managed_by = "script-plugin"`) pointing at its
own loopback route, so the aggregator, health prober, benchmark worker and
admin console treat it like any other source.

They are **resolve-only**: a provider answers "what can play this TMDB id",
not "what titles exist", so it supplies streams without adding noise to
search. A plugin removed from disk has its row disabled on the next start.

| route | purpose |
|---|---|
| `GET /api/script-plugins` | what is loaded |
| `GET /api/script-plugins/{id}/manifest.json` | identity |
| `GET /api/script-plugins/{id}/details?id=movie:27205` | run the provider |

Stream kind is inferred from the url — `.m3u8` becomes `hls`, `.mp4` becomes
`mp4` — so the player knows how to open it.

## Getting started

[`plugins/js/example-provider/`](../plugins/js/example-provider/) is a complete,
commented provider that returns a public test stream. Set `enabled: true` in
its manifest and restart to see it end to end, then copy the folder.
