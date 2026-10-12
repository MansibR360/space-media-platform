# Mood Search & Smart Collections

> [API.md](API.md#intelligence) · [heuristic-engine.md](heuristic-engine.md) · [architecture.md](architecture.md)

SPACE's intelligence module provides natural-language mood matching ("vibe search"), taste profiling, home page rails, and user-created smart collections.

## Vibe Search

Search for media by mood, tone, or feeling rather than keywords.

```bash
GET /api/intelligence/vibe?q=cozy+feel-good&limit=12
```

### How It Works

1. **Query normalization** — lowercase, split on whitespace
2. **Vibe matching** — matches query words against a taxonomy of 12 moods (see below)
3. **Genre union** — collects all genres from matched vibes, deduplicated
4. **Candidate query** — fetches media matching any of those genres, ordered by popularity
5. **Scoring** — each candidate is scored by:
   - Base: `rating_avg + popularity/2000`
   - +1.5 per keyword match in title or overview
   - +0.75 if runtime falls within the vibe's pace band
6. **Ranking** — sort by score descending, return top N

### The 12 Vibes

| Vibe | Label | Genres | Keywords | Pace (min) |
|---|---|---|---|---|
| `Cozy` | Cozy & feel-good | comedy, family, romance, animation | warm, charming, delightful, wholesome | 80–130 |
| `Dark` | Dark & gritty | crime, thriller, drama, mystery | dark, brutal, corrupt, noir, revenge | 95–160 |
| `MindBending` | Mind-bending | science-fiction, sci-fi, fantasy, mystery | time travel, parallel, dream, memory, twist, surreal | 100–170 |
| `Epic` | Epic adventure | adventure, action, fantasy, history | epic, journey, kingdom, war, legend | 110–190 |
| `Tearjerker` | Emotional rollercoaster | drama, romance | loss, grief, love story, illness, farewell | 90–150 |
| `Funny` | Laugh out loud | comedy | hilarious, satire, misfit, wedding | 75–120 |
| `Scary` | Scary as hell | horror, thriller | haunted, demon, supernatural, survive, nightmare | 80–130 |
| `Chill` | Chill & easy | documentary, music, comedy, drama | nature, food, travel, road trip | 60–110 |
| `Nostalgic` | Nostalgia trip | family, animation, adventure, comedy | childhood, summer, coming of age, small town | 85–140 |
| `Intense` | Edge of your seat | thriller, action, crime | race against, hostage, heist, escape, chase | 90–150 |
| `Romantic` | Romance fix | romance, drama, comedy | fall in love, wedding, first kiss, long distance | 85–135 |
| `Cerebral` | Slow-burn cerebral | drama, mystery, science-fiction | philosophical, identity, existential, quiet, isolation | 100–180 |

### Response Format

```json
{
  "matched_vibes": ["Cozy", "Funny"],
  "items": [
    {
      "media_id": "...",
      "title": "Paddington 2",
      "poster_url": "...",
      "score": 8.7,
      "why": "Cozy & feel-good · charming · wholesome · under 130 min"
    }
  ]
}
```

### Listing Available Moods

```bash
GET /api/intelligence/moods
# → [{ "id": "cozy", "label": "Cozy & feel-good" }, ...]
```

## Taste Profile

```bash
GET /api/me/taste
```

Returns a profile built from the user's watch history and list entries:

```json
{
  "top_genres": [
    { "genre": "science-fiction", "weight": 85.2 },
    { "genre": "drama", "weight": 62.1 }
  ],
  "favorite_decades": [2020, 2010, 1990],
  "avg_runtime_minutes": 128,
  "anime_affinity": 32.5,
  "total_watch_hours": 847,
  "completion_rate": 72.4
}
```

**How it's built:**
1. Genre weights from watch history (weighted by count) + list entries (weighted by score, minimum 5.0)
2. Merged, normalized to 0–100, truncated to top 6
3. Favorite decades: top 3 by entry count
4. Anime affinity: percentage of anime genre entries
5. Completion rate: completed / (completed + watching + dropped)

## Home Rails

```bash
GET /api/home
```

Returns 7 curated rails for the home screen:

| Rail | Source | Count |
|---|---|---|
| `continue_watching` | Watch history with resume positions | Up to 12 |
| `trending` | Windowed popularity (week) | Up to 12 |
| `because_you_watched` | Similar items to highest-scored recent watch | Up to 8 |
| `new_episodes` | Episodes airing in next 14 days for user's shows | Up to 8 |
| `friends_activity` | Activity from followed users | Up to 8 |
| `recommended` | Recommendation engine output | Up to 12 |
| `vibe_picks` | Hardcoded `vibe_search("cozy feel-good", 8)` | 8 |

The `because_you_watched` rail picks the user's highest-scored watching/completed entry as the anchor, then fetches similar items.

## Smart Collections

User-created, rule-based media collections that re-evaluate dynamically.

### Creating a Collection

```bash
POST /api/collections
{
  "name": "Hidden Gems",
  "description": "Underrated sci-fi from the 90s",
  "rules": {
    "media_type": "movie",
    "genres": ["science-fiction"],
    "year_from": 1990,
    "year_to": 1999,
    "min_rating": 7.0,
    "is_anime": false,
    "sort": "rating"
  },
  "visibility": "public"
}
```

### Rule Schema

| Field | Type | Description |
|---|---|---|
| `media_type` | `string` | `movie`, `series`, `anime`, `book`, etc. |
| `genres` | `string[]` | Up to 12 genre names |
| `year_from` | `int` | Minimum release year |
| `year_to` | `int` | Maximum release year |
| `min_rating` | `float` | Minimum average rating |
| `is_anime` | `bool` | Filter to anime only |
| `sort` | `string` | `popularity` (default), `rating`, `title`, `created_at` |

### Viewing Collection Items

```bash
GET /api/collections/{id}/items
```

The rules are evaluated against the `media` table at query time — the collection always reflects the current catalog. Visibility is enforced: `public` collections are viewable by anyone, `private` only by the owner.

### Management

```bash
# List your collections
GET /api/me/lists  (or use the intelligence endpoints)

# Delete a collection
DELETE /api/collections/{id}
```

**Constraints:**
- Name: 1–48 characters
- Max 12 genres per rule
- Visibility: `private` (default) or `public`
- Only the owner can delete

## AI Copilot

The AI copilot can automatically enrich the catalog with relationship data:

```bash
# Check status
GET /api/admin/ai/status

# Trigger analysis
POST /api/admin/ai/analyze

# Apply suggested changes
POST /api/admin/ai/apply

# View audit log
GET /api/admin/ai/events
```

The copilot is configured at runtime — no restart needed:

```bash
GET /api/admin/ai/config
PATCH /api/admin/ai/config
```

## Smart Collections vs. Manual Lists

| Feature | Smart Collections | Manual Lists |
|---|---|---|
| Content | Rule-based, auto-populated | Manually curated |
| Updates | Dynamic (re-evaluated on view) | Static (add/remove items) |
| Sharing | Public or private | Shareable via list ID |
| Use case | "Show me all 90s sci-fi rated 7+" | "My favorite movies"
