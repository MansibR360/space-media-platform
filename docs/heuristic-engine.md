# Heuristic Engine

SPACE's built-in learning system — the brain of a Spotify/Netflix-class
platform, running natively on your own data. No external AI service, no
connectors to other companies' apps: the engine silently learns from every
in-app action (progress saves, completions, rewatches, ratings, favorites)
and turns them into a living behavioral profile, Wrapped-style recaps,
deterministic insights and explainable next-up picks.

## What it learns (and how)

| Signal | Technique |
|---|---|
| Genre / decade affinity | engagement weights with **exponential time-decay** (21-day half-life) joined through the catalog |
| Top titles | decayed play-frequency with replay counts |
| Habits | circular-smoothed hour-of-day + weekday histograms → **chronotype** ("night owl", "prime-time loyalist", …), peak windows, weekend shift |
| Sessions | gap-based reconstruction (45 min threshold) with binge detection |
| Replay behavior | replay rate → comfort-replayer vs **explorer index** |
| Completion | percent-played discipline across plays vs completions |
| Streaks & momentum | current/longest active-day streaks, 7-day week-over-week delta |
| Personas | multi-label rule classifier with confidences — Cinephile, Series Binger, Music Devotee, Bookworm, Binge Watcher, Completionist, Endless Explorer, Nocturne … |

Signal weights: `favorite` 3.0 · `complete` 2.0 · `rewatch` 1.5 · `rate` 1.25 · `play` 1.0.

## Automatic feeding

The engine hooks the existing write paths — nothing for clients to call:

* `PUT /api/progress` → play/complete signals (saves inside one hour dedupe to one event)
* `PUT /api/me/media/{id}/status` as completed → completion signal
* `PUT /api/media/{id}/rating` → rating signal
* `POST /api/media/{id}/favorite` → favorite signal

## API

```
GET    /api/heuristic/status              engine liveness + learned-signal count
GET    /api/me/heuristic                  full learned profile
GET    /api/me/heuristic/wrapped?period=  Wrapped recap (week|month|year|all)
GET    /api/me/heuristic/insights         human-readable deterministic insights
GET    /api/me/heuristic/predictions      explainable next-up picks + reasons
DELETE /api/me/heuristic                  wipe all my learned data
```

### Wrapped response highlights

`headline`, total plays/hours, unique titles, new discoveries (first-ever
engagements in the window), top items/genres/decades, biggest single day,
longest session, active days.

### Prediction scoring

`(0.65 × genre affinity + 0.35 × popularity) × freshness`, excluding finished
titles and just-watched items; every pick ships reasons like *"matches your
taste for Thriller & Mystery"*, *"new to you"*, *"back after a while"*.

## Example

```bash
curl http://localhost:8080/api/me/heuristic -H "Authorization: Bearer $TOKEN"
# { "personas": [{"label":"Music Devotee","confidence":0.9}, ...],
#   "habits": {"chronotype":"night owl","peak_hours":[22,23,20], ...},
#   "genre_affinity": [{"key":"Sci-Fi","weight":100.0,"plays":84}, ...] }

curl "http://localhost:8080/api/me/heuristic/wrapped?period=year" -H "Authorization: Bearer $TOKEN"
# { "headline": "This Year was a deep-dive into 2010s Sci-Fi: 212.4 hours ...",
#   "top_items": [...], "new_discoveries": 61, "biggest_day": {...} }
```
