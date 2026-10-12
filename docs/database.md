# Database

PostgreSQL dialect (CockroachDB wire-compatible; see portability notes).
Migrations live in `migrations/` and apply automatically on boot.

| Migration | Domain |
|---|---|
| `0001_core` | users, user_permissions, sessions |
| `0002_catalog` | media (+FTS vector), seasons, episodes, genres, people, studios, credits, external_mappings |
| `0003_tracking` | watch_history, list_entries, favorites, user_lists(+items) |
| `0004_streaming` | library_paths, media_files, media_streams, playback_sessions, transcode_jobs, thumbnails |
| `0005_social` | follows, reviews, comments, reactions, parties(+members/events), conversations, messages, receipts |
| `0006_security_plugins` | user_settings, password_resets, login_lockouts, plugins |
| `0007_intelligence` | media_markers, playback_handoffs, party_reactions, smart_collections |
| `0008_platform` | book columns on media, reading_progress, api_keys, devices, notifications, signing_secrets, download_grants |
| `0009_download_permission` | extends permission CHECK with `download` |

## Entity relationships (core)

```
users ─┬─< sessions
       ├─< list_entries >─ media        media ─┬─< seasons ─< episodes
       ├─< watch_history >─ media              ├─< media_genres >─ genres
       ├─< reviews      >─ media               ├─< media_studios >─ studios
       ├─< favorites    >─ media               ├─< credits >─ people
       ├─< reading_progress                    └─< external_mappings
       ├─< api_keys   ├─< devices
       └─< notifications

library_paths ─< media_files ─< media_streams
             media_files ─< playback_sessions, transcode_jobs, thumbnails,
                            media_markers, download_grants

parties ─< party_members / party_events / party_reactions
conversations ─< conversation_members, messages ─< message_receipts
smart_collections (rules JSONB → executed against media at read time)
```

## Conventions

- **UUID v7 primary keys** — time-ordered, index-friendly, cursor-safe
- **Enums as TEXT + CHECK** — portable across PG-wire databases; mapped in Rust
  via `#[derive(sqlx::Type)] #[sqlx(rename_all = "snake_case")]`
- **Tokens/secrets stored hashed** (SHA-256): session tokens, refresh tokens,
  reset tokens, API-key secrets, download-signing secret (server-only)
- Watch history uniqueness via functional unique index on
  `(user_id, media_id, COALESCE(episode_id, nil))`; upserts handled app-side
  with `FOR UPDATE` to stay portable
- Party sync state is denormalized onto the `parties` row (`is_playing`,
  `position_seconds`, `state_updated_at`, `last_event_seq`) with an append-only
  `party_events` log for reconnect replay

## Portability: CockroachDB & TiDB

- **CockroachDB**: point `DATABASE_URL` at it. On first migrate, the optional
  layer (`migrations-optional/pg_search.sql`) applies statement-by-statement;
  unsupported statements (pg_trgm) are skipped with one info log. FTS queries
  detect the missing vector and fall back to ILIKE automatically.
- **TiDB**: speaks MySQL wire protocol — requires swapping sqlx driver types.
  Not recommended until a dedicated milestone; CRDB delivers distributed SQL
  with zero application changes.

## Ops cheat-sheet

```sql
-- biggest libraries by storage
SELECT lp.root_path, pg_size_pretty(SUM(f.file_size)::bigint)
FROM media_files f JOIN library_paths lp ON lp.id=f.library_path_id
GROUP BY 1 ORDER BY 2 DESC;

-- most watched (14d)
SELECT m.title, COUNT(*) FROM watch_history wh
JOIN media m ON m.id=wh.media_id
WHERE wh.updated_at > now()-interval '14 days' GROUP BY 1 ORDER BY 2 DESC LIMIT 10;

-- stuck transcodes
UPDATE transcode_jobs SET status='failed', error='stuck'
WHERE status='running' AND started_at < now()-interval '6 hours';
```
