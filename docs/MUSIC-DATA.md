# Music data platform & IMDb

Two things landed together in migration `0033_imdb_and_music_data.sql`:
IMDb identity/ratings on the catalog, and the music data platform.

---

## 1. The shape of the music data

The whole design is one split, and everything else follows from it.

| Layer | Tables | Scope | Grows with |
|---|---|---|---|
| **Global — the audio** | `music_analysis` | one row per **track** | every track added |
| **Global — the audience** | `music_track_stats`, `music_track_listeners`, `music_cooccurrence`, `music_similarity` | one row per **track** / **track pair** | every listen, by anyone |
| **Per user** | `music_listens`, `music_affinity` | one row per **user** × thing | that user's own listening |

A track's waveform, key and tempo are facts about the *recording* — computed
once, identical for every listener. Its play count and completion rate are
facts about the *whole audience*. Neither belongs to a user.

Taste is the only per-user layer, and one listener's history never enters
another's recommendations **except** through `music_cooccurrence`, which stores
a pair of track ids and a weight — no identity, ever.

That split is what makes the corpus compound: every new listener improves the
global layer for everyone, while their own profile stays theirs.

---

## 2. The analyzer

`src/music/dsp.rs` (signal processing) and `src/music/analysis.rs` (orchestration).

Audio is decoded **once** — ffmpeg writes mono f32 PCM at 22.05 kHz to a pipe —
and every descriptor comes from that single buffer via one STFT pass
(2048-sample window, 512 hop, Hann).

Three external calls per track:

1. `ffprobe` — container metadata **and embedded tags** (artist, album, year,
   track number, genre). Tags were previously discarded entirely.
2. `ffmpeg` — decode to PCM.
3. `ffmpeg` — `ebur128` for broadcast-standard loudness.

### What comes out

**Container** — duration, sample rate, channels, bitrate, codec
**Tags** — artist, album, album artist, track/disc no., release year, genre
**Level** — integrated LUFS, true peak, loudness range, energy
**Envelopes** — 800-bucket peak envelope + RMS envelope
**Spectral** — centroid, rolloff (85%), flatness, bandwidth, flux, zero-crossing rate, brightness
**Tonal** — 12-bin chroma, musical key + mode, key confidence
  (Krumhansl-Kessler correlation across all 24 rotations)
**Rhythm** — tempo BPM + confidence, onset rate, beat grid
  (autocorrelation of the onset envelope, harmonic-weighted so 120 BPM doesn't
  report as 60)
**Descriptors** — danceability, valence, acousticness, instrumentalness,
  speechiness, liveness
**Vectors** — 13-coefficient MFCC timbre vector, and a 32-dim L2-normalized
  `embedding` used for similarity search
**Structure** — segment boundaries from MFCC novelty

> The six high-level descriptors are **heuristics over measured features**, not
> learned models. They combine the signals that separate each trait in practice
> and clamp to 0..1. They are stable and explainable, which is what the
> recommender and the dataset need — they are not calibrated to match any
> particular vendor's numbers.

The embedding rotates chroma relative to the detected key, so two recordings of
the same song in different keys land near each other rather than 5 semitones
apart.

Bump `ANALYSIS_VERSION` (currently `2`) and `analyze-pending` re-analyzes
everything older, in batches, automatically.

### Requires ffmpeg

**`ffmpeg` and `ffprobe` are not currently installed on this machine.** The
analyzer degrades rather than crashing — it records what it could learn and
writes the reason to `music_analysis.last_error` — but it produces **no
features at all** without them. Install both and put them on `PATH`, or set
`FFMPEG_BIN` / `FFPROBE_BIN`.

---

## 3. Recommendations

`src/music/service.rs`. Four signals, blended by how much history the listener
has:

| Signal | Source | Cold start | Established |
|---|---|---|---|
| Personal taste | `music_affinity` (genre, artist, key, tempo, energy) | 0.0 | 1.0 |
| Collaborative | `music_cooccurrence` — "played together" | 0.0 | 1.2 |
| Content similarity | `music_similarity` — cosine over embeddings | 0.0 | 0.9 |
| Global quality | `music_track_stats` — Wilson score, trend | 1.0 | 0.4 |

Confidence ramps from the first listen to the 12th. A brand-new listener is
recommended what is objectively good; an established one gets what suits them.
Every pick carries a `basis` and human-readable `reasons`.

Quality uses a **Wilson lower bound** on completion rate, not a raw average —
one lucky play cannot outrank a track with a thousand.

Content similarity is computed from the embedding after analysis, so a freshly
uploaded track is recommendable **before anyone has played it**. That is the
cold-start hole that pure collaborative filtering cannot fill.

---

## 4. API

### Global (no auth — same answer for everyone, cacheable)
```
GET  /api/music/charts?limit=20&trending=true
GET  /api/music/{media_id}/analysis
GET  /api/music/{media_id}/stats
GET  /api/music/{media_id}/similar?limit=12
```

### Per user (auth required)
```
POST /api/music/listen
GET  /api/music/taste
GET  /api/music/recommendations?limit=20
GET  /api/music/history?limit=50
```

### Admin
```
POST /api/admin/music/{media_id}/analyze
POST /api/admin/music/analyze-pending?limit=25
GET  /api/admin/music/dataset?limit=500&offset=0
GET  /api/admin/music/dataset/summary
```

`POST /api/music/listen` accepts a **partial** report — the server derives
`completed` and `skipped` from `seconds_played` against `track_seconds`. Send
`previous_media_id` to strengthen the co-play graph.

---

## 5. The dataset export

`GET /api/admin/music/dataset` returns per-track audio features joined to
aggregate engagement.

It is built from the **global tables only**. There is no user column to leak —
that is a property of the query's construction, not a filter that could be
forgotten. The engagement columns are counts across the whole audience.

`GET /api/admin/music/dataset/summary` reports corpus coverage: how much of the
catalog is analyzed, total listens, distinct listeners, hours.

---

## 6. IMDb

The IMDb id now lives **on the media row**, not only in `external_mappings`.
That is the change that matters: a title imported from TMDB, from a plugin, or
by hand can all be enriched from IMDb afterwards.

- `media` gained `imdb_id`, `imdb_rating`, `imdb_votes`, `metascore`,
  `content_rating`, `imdb_synced_at`
- OMDb (registered under the source key `imdb`) already fetched this payload
  and **was discarding all of it** — `ExternalDetail` had nowhere to put it
- TMDB now requests `external_ids` and captures the IMDb id
- Ratings are re-fetched on a 7-day cycle rather than frozen at import

```
GET  /api/media/{id}/imdb
POST /api/admin/media/{id}/imdb/sync
POST /api/admin/imdb/sync?limit=50
```

Requires `OMDB_API_KEY`. Titles with no IMDb match get stamped so the sweep
moves on instead of retrying them forever.

`imdb_rating` is kept separate from `rating_avg` throughout, including in the
UI — they are different numbers from different populations.

---

## 7. Fixes included

- **Migration checksum error** — `sqlx::migrate!` embeds migrations at *compile
  time*. `target/debug/space.exe` predated the renumber that moved
  `0024_source_ratings.sql` to `0029`, so the stale binary carried a duplicate
  v24. The on-disk migrations were always correct. Fixed by rebuilding.
- **Privilege escalation** — `/api/admin/music/analyze` and `analyze-pending`
  took `CurrentUser`, not `AdminUser`. Any signed-in user could spawn up to 200
  ffmpeg processes. Both now require admin.
- **`top_artists` was always empty** — nothing ever wrote the `artist` affinity
  dimension. Now populated from tags (splitting collaborations) with a fallback
  to credited people.
- **Album art broke decoding** — the ffmpeg calls lacked `-vn`, so cover art
  was treated as a video stream.
- **ebur128 parsing** — read labels from the whole stderr stream, including
  per-frame progress lines that share the same labels. Now anchored to the
  trailing `Summary:` block.
- **`mark_failed` silently did nothing** when no analysis row existed yet.

---

## 8. Embedded PostgreSQL

Behind the `embedded-db` cargo feature (off by default). When on and
`DATABASE_URL` is **unset**, SPACE starts its own PostgreSQL from binaries
bundled into the executable — nothing to install. When `DATABASE_URL` **is**
set, that server is used and nothing changes.

```bash
cargo build --release --features embedded-db
```

**Currently blocked on this machine.** The upstream binary repository
(`theseus-rs/postgresql-binaries`) publishes PostgreSQL for
`x86_64-pc-windows-msvc` but **not** `-gnu`, and this toolchain is
`stable-x86_64-pc-windows-gnu`. The `bundled` feature resolves its archive at
build time, so no runtime workaround applies. Switching to the MSVC toolchain
makes the feature flag work as-is — but that needs the MSVC build tools
installed, which is a larger install than PostgreSQL itself.

The code is written and gated, so it becomes a one-flag change if you move to
MSVC. Until then `DATABASE_URL` stays required.
