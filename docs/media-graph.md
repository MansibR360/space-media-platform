# Media Graph

> [API.md](API.md#media-graph) · [architecture.md](architecture.md)

The media graph is a typed, directed relationship network connecting any media items — regardless of type. An anime can link to its manga source, a novel to its film adaptation, a movie to its sequel, and so on.

## Relationship Types

| Relation | Meaning | Example |
|---|---|---|
| `adaptation_of` | This media is adapted from the other | Movie → Book (the book came first) |
| `based_on` | Inspired by or loosely adapted from | Film → True story |
| `sequel_of` | Direct continuation | Terminator 2 → Terminator |
| `prequel_of` | Preceding story | Star Wars Ep I → Ep IV |
| `spin_off_of` | Side story or derivative | Better Call Saul → Breaking Bad |
| `remake_of` | New version of older work | 2010 Karate Kid → 1984 Karate Kid |
| `side_story_of` | Parallel narrative | Animated spinoff → Main series |
| `parent_of` | Parent work that spawned children | Series → Spinoff |
| `same_universe` | Shared fictional universe | Iron Man → Captain America |
| `same_franchise` | Same franchise, different stories | Final Fantasy VII → FFXVI |
| `alternate_version_of` | Different telling of same story | Manga → Anime |
| `related_to` | Thematically or contextually related | Documentary → Subject film |

### Directionality

Relationships are **directed**: `adaptation_of` means the *from* item was adapted *from* the *to* item. The graph API resolves both directions when traversing — a query for "related" items returns both outgoing and incoming edges.

### Cross-Media Relations

Five relations carry story across media formats (the "backbone" of cross-media journeys):

```
adaptation_of, based_on, spin_off_of, remake_of, alternate_version_of
```

These are used by the journey recommendation engine to suggest "the same story continues in another format."

## Graph Operations

### One-Hop Neighborhood

```bash
GET /api/media/{id}/related
```

Returns all directly connected media, grouped by relation direction (`out` = this media points at the other; `in` = the other points here). Ordered by `release_date`.

### Franchise Traversal (BFS)

```bash
GET /api/media/{id}/journey
```

Breadth-first traversal up to 3 hops deep. Returns a flat list of `JourneyNode` objects, each with:
- `media_id`, `title`, `media_type`, `poster_url`, `release_date`
- `via_relation` — the edge that led to this node (null for root)
- `depth` — hops from root (0 = the root item)

The traversal:
1. Starts at the root media item
2. Explores all edges (both directions) at each depth level
3. Uses a `visited` set to prevent cycles
4. Limits to 4 hops maximum (clamped server-side)
5. Issues one SQL round-trip per depth level (bounded query count)

### Cross-Media Journeys (User-Scoped)

```bash
GET /api/discover/journeys
```

Recommends story continuations the user hasn't consumed yet. Algorithm:

1. Find the user's **anchors**: recently completed or highly-rated items (score ≥ 7)
2. For each anchor, follow adaptation-style edges (`adaptation_of`, `based_on`, `spin_off_of`, `remake_of`, `alternate_version_of`) in both directions
3. Filter out items the user has already watched or read
4. Return up to `limit` journeys with the "reason" string: *"you finished {title} — the same story continues in another format"*

### CRUD

```bash
# Create a relationship (admin)
POST /api/media/{id}/relationships
{ "to_media_id": "...", "relation": "adaptation_of", "source": "tmdb" }

# List all relationships for a media item
GET /api/media/{id}/relationships
# → { "outgoing": [...], "incoming": [...] }

# Delete a relationship (admin)
DELETE /api/media/{id}/relationships/{to_media_id}/{relation}
```

## Database Schema

```sql
CREATE TABLE media_relationships (
    id          UUID PRIMARY KEY,
    from_media_id UUID NOT NULL REFERENCES media(id),
    to_media_id   UUID NOT NULL REFERENCES media(id),
    relation    TEXT NOT NULL,  -- CHECK: one of the 12 relation types
    source      TEXT NOT NULL DEFAULT 'manual',
    confidence  DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (from_media_id, to_media_id, relation)
);
```

- `source` tracks who created the relationship: `manual`, `tmdb`, `ai_copilot`, `plugin`, etc.
- `confidence` defaults to 1.0 (manual); AI-sourced links may have lower confidence
- Upsert semantics: re-creating the same `(from, to, relation)` updates the `source`

## Population

Relationships are populated from:

1. **Manual admin entry** — via the API
2. **TMDB metadata import** — related titles, franchises, collections
3. **AI Copilot** — automatic graph enrichment (whitelisted action)
4. **Plugin imports** — some metadata plugins supply relationship data

## Metrics

Graph operations don't emit dedicated metrics. Query performance is tracked via the general `http_request_duration_seconds` histogram.

## Limitations

- Maximum traversal depth: 4 hops (hardcoded clamp)
- No weighted edges (confidence exists but isn't used in traversal ranking)
- Cycle-safe via visited set (no infinite loops)
- No graph-wide analytics (pagerank, clustering) — this is a homelab server, not a social network
