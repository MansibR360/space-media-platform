# SPACE — Complete API Reference

> **Base URL:** `http://localhost:8080`
> **Interactive docs:** `GET /docs` (Swagger UI) · `GET /api-docs/openapi.json` (OpenAPI spec)
> **Version header:** Every response includes `X-API-Version: space/<version>`
> **v1 compatibility:** Legacy `/v1/*` paths are auto-rewritten to `/api/*` with deprecation tag

## Table of Contents

- [Authentication](#authentication)
- [Error Format](#error-format)
- [Pagination](#pagination)
- [Rate Limiting](#rate-limiting)
- [Auth & Identity](#auth--identity)
- [User Platform](#user-platform)
- [Admin Fortress](#admin-fortress)
- [Catalog & Metadata](#catalog--metadata)
- [Search & Discovery](#search--discovery)
- [Sources & Plugins](#sources--plugins)
- [Resolution & Playback](#resolution--playback)
- [Playback Security](#playback-security)
- [Streaming & Protection](#streaming--protection)
- [Tracking & Progress](#tracking--progress)
- [Social & Chat](#social--chat)
- [Watch Parties](#watch-parties)
- [Moderation & Safety](#moderation--safety)
- [Intelligence](#intelligence)
- [Media Graph](#media-graph)
- [Realtime (WebSocket)](#realtime-websocket)
- [Card Economy](#card-economy)
- [Library & Playlists](#library--playlists)
- [File Management](#file-management)
- [Ops & Observability](#ops--observability)
- [Developer Experience](#developer-experience)

---

## Authentication

SPACE supports three authentication methods:

### Bearer Token (primary)
```bash
# Login to get tokens
curl -X POST /api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"identifier":"admin","password":"secret"}'
# → { "access_token": "...", "refresh_token": "...", "user": {...} }

# Use the access token
curl -H "Authorization: Bearer <access_token>" /api/users/me
```

### API Keys (machine-to-machine)
```bash
# Create a scoped API key
curl -X POST /api/me/keys -H "Authorization: Bearer $TOKEN" \
  -d '{"name":"ci-pipeline","scopes":["read","downloads"]}'

# Use it
curl -H "X-API-Key: sk_..." /api/search?q=test
```
Scopes: `read` (GET), `write` (mutations), `downloads` (file access).

### Session Cookies (browser)
HttpOnly, SameSite=Lax cookies are set on login/refresh. Works with the SPA.

**Token lifecycle:**
- Access token: 15 min (configurable via `ACCESS_TOKEN_TTL_SECS`)
- Refresh token: 30 days, rotating on use, hashed at rest
- Single-use password reset tokens: 30 min TTL
- All tokens are SHA-256 hashed in the database

---

## Error Format

Every error returns a uniform `ProblemJson` body:

```json
{
  "code": "validation_error",
  "message": "email is required",
  "trace_id": "a1b2c3d4e5f6"
}
```

| Status | `code` | Meaning |
|--------|--------|---------|
| 400 | `validation_error` | Bad request / missing fields |
| 401 | `unauthorized` | No auth or invalid token |
| 403 | `forbidden` | Insufficient permissions |
| 404 | `not_found` | Resource doesn't exist |
| 409 | `conflict` | Duplicate resource |
| 429 | `rate_limited` | Too many requests (includes `Retry-After` header) |
| 503 | `unavailable` | Service temporarily down |
| 500 | `internal_error` | Server bug |

---

## Pagination

Most list endpoints use cursor-based pagination:

```bash
GET /api/media?limit=20&cursor=<opaque>
```

- `limit`: items per page (clamped 1–100)
- `cursor`: opaque string from `next_cursor` in response; pass it back as `cursor=`
- Response always includes `next_cursor: null | "..."` when exhausted

---

## Rate Limiting

| Scope | Limit | Window |
|-------|-------|--------|
| Global (per-IP) | 300 req/min | sliding |
| Auth endpoints | 20 req/min | sliding |
| Login (nginx edge) | 10 req/min burst 5 | fixed |

Returns `429` with `Retry-After: 60` header. Redis-backed when available, falls back to in-memory.

---

## Auth & Identity

### POST /api/auth/register

Create a new account. Automatically grants a 7-day trial access card.

```bash
curl -X POST /api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"alice","email":"alice@example.com","password":"Secure123!"}'
```

**Body:** `{ username, email, password }`  
**Response:** `{ access_token, refresh_token, user: { id, username, email, role } }`

---

### POST /api/auth/login

Login with username or email. Supports TOTP 2FA for admins.

```bash
curl -X POST /api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"identifier":"alice","password":"Secure123!"}'
# Admin with 2FA:
# -d '{"identifier":"admin","password":"...","totp_code":"123456"}'
```

**Body:** `{ identifier, password, totp_code? }`  
**Response:** `{ access_token, refresh_token, user }`  
**Lockout:** 5 failed attempts per (identity, IP) → 15-minute lockout

---

### POST /api/auth/refresh

Rotate to a new token pair. Old refresh token is invalidated.

```bash
curl -X POST /api/auth/refresh \
  -H "Content-Type: application/json" \
  -d '{"refresh_token":"..."}'
```

---

### POST /api/auth/logout

Invalidate the current session.

### POST /api/auth/logout-all

Invalidate every session for the user.

---

### POST /api/auth/password/forgot

Send a password reset email. Enumeration-safe (always returns OK).

### POST /api/auth/password/reset

Consume the reset token and set a new password.

```bash
curl -X POST /api/auth/password/reset \
  -H "Content-Type: application/json" \
  -d '{"token":"...","new_password":"NewSecure123!"}'
```

---

### POST /api/users/me/password

Change password for the authenticated user (requires current password).

```bash
curl -X POST /api/users/me/password \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"current_password":"old","new_password":"new"}'
```

---

### POST /api/auth/email/request-code

Request an email verification code. Hashed at rest, expires in 15 min, locks after 5 bad tries.

### POST /api/auth/email/verify

Verify the code. In dev mode (`EMAIL_DEV_ECHO=true`), the code is echoed in the API response.

---

### GET /api/auth/sessions

List all active sessions for the current user.

### DELETE /api/auth/sessions/{session_id}

Revoke a specific session.

---

### POST /api/me/keys

Create a scoped API key.

```bash
curl -X POST /api/me/keys -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"my-script","scopes":["read"]}'
```

**Response:** `{ key_id, key: "sk_...", name, scopes, created_at }` — the `key` is shown only once.

### GET /api/me/keys

List all API keys (secrets masked).

### DELETE /api/me/keys/{key_id}

Revoke an API key.

---

### POST /api/admin/users/{user_id}/permissions

Grant a fine-grained permission.

```bash
curl -X POST /api/admin/users/$UID/permissions \
  -H "Authorization: Bearer $ADMIN" \
  -H "Content-Type: application/json" \
  -d '{"permission":"download"}'
```

Valid permissions: `download`, `manage_library`, `moderate_content`

### GET /api/admin/users/{user_id}/permissions

List permissions for a user.

### DELETE /api/admin/users/{user_id}/permissions/{permission}

Revoke a permission.

---

### GET /api/users/{username}

Public profile for any user.

### GET /api/users/me

Current user's profile.

### PATCH /api/users/me

Update current user's profile.

---

## User Platform

### GET|PUT|PATCH /api/users/me/settings

Per-user preferences (playback, UI, notifications). Supports full replace (PUT) or partial patch (PATCH).

---

### POST /api/me/devices

Register a client device.

```bash
curl -X POST /api/me/devices -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"Living Room TV","platform":"android-tv"}'
```

### GET /api/me/devices

List registered devices.

### DELETE /api/me/devices/{device_id}

Revoke a device (unbinds from download links).

---

### GET /api/me/notifications

Paginated notification feed.

### GET /api/me/notifications/unread-count

Returns `{ "count": 3 }`.

### POST /api/me/notifications/{notification_id}/read

Mark a notification as read.

---

### GET /api/me/search-history

List recent search queries.

### DELETE /api/me/search-history

Clear search history.

---

### GET /api/me/behavior

Personal consumption analytics: peak hours, preferred genres, device usage.

---

### PUT /api/me/presence

Update your online status.

### GET /api/presence?ids=uuid1,uuid2

Batch presence lookup.

---

## Admin Fortress

> **Security:** All `/api/admin/*` endpoints are localhost-only by default (`ADMIN_LOCAL_ONLY=true`). Set to `false` to allow remote access.

### GET|POST /api/admin/security/lockdown

Toggle site-wide lockdown (503 on all public routes). Customizable message.

### GET /api/admin/audit-logs?action=&limit=

Filterable audit trail of all sensitive actions with actor IP and timestamp.

### GET /api/admin/users

Paginated user listing (admin-only).

### GET|POST /api/admin/debug/dump-secrets

**Honeypot trap.** Any touch = permanent ban + session wipe + watchdog trip.

### POST /api/admin/security/totp/setup

Generate TOTP provisioning URI for an admin.

### POST /api/admin/security/totp/enable

Enable TOTP 2FA for an admin.

### POST /api/admin/security/totp/disable

Disable TOTP 2FA.

### GET|PATCH /api/admin/security/policy

View and tune the playback risk engine thresholds.

### GET /api/admin/security/events

Decision audit log: every challenge/revoke with signals snapshot.

### GET /api/admin/security/grants

List all active playback grants.

### POST /api/admin/security/grants/{id}/revoke

Revoke a single grant.

### POST /api/admin/security/users/{user_id}/revoke-grants

Revoke all grants for a user.

### POST /api/admin/security/keys/{media_ref}/rotate

Rotate encryption keys for a media item.

### POST /api/admin/security/keys/{media_ref}/revoke

Irreversibly revoke all keys for a media item.

---

## Catalog & Metadata

### GET /api/media

List media with filters.

```bash
GET /api/media?media_type=movie&sort=rating&limit=20&cursor=...
```

Filters: `media_type`, `genre`, `year_from`, `year_to`, `sort` (title/rating/popularity/created_at).

### POST /api/media

Create a media entry (admin).

### GET /api/media/{id}

Get full media detail with genres, studios, credits, external mappings.

### PATCH /api/media/{id}

Update a media entry (admin).

### DELETE /api/media/{id}

Delete a media entry (admin).

---

### GET /api/media/{id}/episodes

List episodes for a series.

### POST /api/admin/media/{id}/seasons

Create a season (admin).

### POST /api/admin/media/{id}/seasons/{season_id}/episodes

Create an episode (admin).

---

### GET /api/metadata/search

Search external metadata providers.

```bash
GET /api/metadata/search?q=cowboy+bebop&provider=tmdb
```

### GET /api/metadata/sources

List available metadata providers (built-in + plugins).

### POST /api/metadata/import

Import metadata from an external provider.

```bash
POST /api/metadata/import
{ "source": "tmdb", "external_id": "1234" }
# → Full MediaDetail with imported data
```

---

### POST /api/admin/metadata/refresh

Trigger metadata refresh for stale external mappings.

### GET /api/admin/quality/conflicts

Review metadata conflicts from multiple sources.

### GET /api/admin/quality/duplicates

Find potential duplicate catalog entries.

### POST /api/admin/quality/duplicates/auto-resolve

Merge weaker duplicates into the stronger entry.

### GET /api/admin/storage/analytics

Disk usage breakdown per media type.

---

## Search & Discovery

### GET /api/search?q=

Full-text search with tsvector ranking (ILIKE fallback on CockroachDB).

```bash
GET /api/search?q=action+2020-2025+1080p&limit=20
```

### GET /api/search/parse?q=

Parse a natural-language query into structured filters.

```bash
GET /api/search/parse?q=action 2020-2025 1080p english
# → { genres: ["action"], year_from: 2020, year_to: 2025, quality: "1080p", language: "english" }
```

### GET /api/search/all?q=

Unified cross-source search: parallel fan-out across local catalog + every healthy source.

### GET /api/search/suggest?q=

Type-ahead suggestions (fast, truncated).

---

### GET /api/discover

Curated discovery feed with filter parameters.

### GET /api/trending?window=week

Windowed popularity from real watch activity. Windows: `day`, `week`, `month`.

### GET /api/media/{id}/similar

Similar media based on shared genres + studios, cached per item.

### GET /api/media/{id}/similar-scores

Detailed similarity scores.

---

### GET /api/intelligence/vibe?q=

Natural-language mood matching ("cozy feel-good") over genres, keywords, and pacing.

### GET /api/completed?type=movie

Finished/released catalog filtered by type.

### GET /api/toplists?kind=movies|series|anime|books|all

Curated top charts. Local popularity first, TMDB top-rated fallback. 30-min cache.

### GET /api/random?type=&min_rating=

Random pick with type and minimum-rating filters.

### GET /api/upcoming

Episodes airing in the next 14 days.

### POST /api/media/{id}/availability

Where can I play this? Matches across every healthy source + counts local files.

---

## Sources & Plugins

### Built-in Sources

| Source | Category | Content |
|--------|----------|---------|
| ia-films | movies | Internet Archive public-domain films |
| ia-audio | audiobooks | IA audio collections |
| gutenberg | books | 75k+ Project Gutenberg ebooks |
| mangadex | manga | Licensed manga with chapters/pages |
| audius | music | Artist-uploaded music tracks |
| librivox | audiobooks | Free public-domain audiobooks |

### GET /api/sources

List all registered sources.

### GET /api/sources/status

Per-source health: latency, success rates, circuit state.

### GET /api/sources/rankings

Head-to-head benchmark scores (daily worker).

### GET /api/sources/{name}/browse

Browse trending/top rails for a source.

### POST /api/admin/sources/{name}/sync

Import source items into the local catalog.

### POST /api/admin/sources/register-url

Register an HTTP source by base URL. Probes `manifest.json → /items → /search`.

---

### Source Debug (admin)

```bash
GET /api/admin/sources/{name}/debug-search?q=...
GET /api/admin/sources/{name}/debug-item/{external_id}
```

---

### Plugin Management

```bash
# List plugins
GET /api/admin/plugins

# Install a plugin
POST /api/admin/plugins
{ "name": "my-metadata", "kind": "metadata", "base_url": "https://...", "enabled": true }

# Update / Delete
PATCH /api/admin/plugins/{id}
DELETE /api/admin/plugins/{id}

# Test a plugin
POST /api/admin/plugins/{id}/test

# Sync source plugin into catalog
POST /api/admin/plugins/{id}/sync
```

### GET /api/metadata/sources

List all enabled metadata providers (built-ins + plugins).

---

### Web Sources (Galaxy Engine)

Self-learning web scrapers. Paste a site URL and SPACE learns its structure.

```bash
# Create
POST /api/admin/websources
{ "site": "https://movies.example", "category": "movie" }

# List / Update / Delete
GET /api/admin/websources
PUT /api/admin/websources/{id}
DELETE /api/admin/websources/{id}

# Test scraping
POST /api/admin/websources/{id}/test

# Re-learn after site redesign
POST /api/admin/websources/{id}/relearn

# View learned recipe
GET /api/admin/websources/{id}/recipe
```

---

### Hub Federation

Connect SPACE instances as fallback providers.

```bash
# Manage hubs
GET /api/admin/hubs
POST /api/admin/hubs
{ "url": "http://192.168.1.50:8080" }
PUT /api/admin/hubs/{id}
DELETE /api/admin/hubs/{id}

# Sync recipes
POST /api/admin/hubs/{id}/pull
POST /api/admin/hubs/{id}/push

# Test connectivity
POST /api/admin/hubs/{id}/test
```

### Hub Public API (server-side)

```bash
GET /api/hub/find?q=...        # Search across hub
GET /api/hub/item?id=...       # Get item details
GET /api/hub/sources           # List hub's sources
POST /api/hub/sources          # Upload source recipe
```

Requires `SOURCE_HUB_TOKEN` header on the hub server.

---

### Stremio Extensions

Install community addons by manifest URL. Catalogs join unified search; streams join playback resolution.

```bash
# Install
POST /api/admin/extensions
{ "url": "https://torrentio.strem.fun/manifest.json" }

# Manage
GET /api/admin/extensions
PATCH /api/admin/extensions/{id}
DELETE /api/admin/extensions/{id}

# Discover community addons
GET /api/admin/extensions/discover

# Raw resource access (1:1 Stremio SDK)
GET /api/extensions/{id}/catalog/{type}/{catalog_id}
GET /api/extensions/{id}/stream/{type}/{id}
GET /api/extensions/{id}/meta/{type}/{id}
GET /api/extensions/subtitles/{type}/{id}
```

---

## Resolution & Playback

### POST /api/resolve

Central resolver pipeline (11 stages). The primary way to get a playable stream.

```bash
POST /api/resolve
{
  "title": "Cowboy Bebop",
  "media_type": "anime",
  "year": 1998,
  "preferences": {
    "quality": "1080p",
    "language": "japanese",
    "subtitles": "english"
  }
}
```

**Response:**
```json
{
  "stream_url": "https://...",
  "quality": "1080p",
  "source": "miruro",
  "format": "hls",
  "fallbacks": [...],
  "watermark": "a1b2c3d4e5f6g7h8"
}
```

Every URL carries `wm=<16-hex>` forensic watermark binding user + session + hour.

---

### POST /api/playback/resolve

Legacy resolve endpoint (preserved for existing clients).

### POST /api/playback/lookup

Availability-first resolver: returns where a title can be played without triggering full resolve.

```bash
POST /api/playback/lookup
{ "title": "Arrival", "media_type": "movie" }
# → { "available": true, "sources": [...], "local_files": 2 }
```

---

### POST /api/playback/manga/chapters

List chapters for a manga title.

### POST /api/playback/manga/pages

Get page images for a chapter.

### POST /api/playback/manga/open

One-shot open: detail + chapters + first pages.

---

### GET /api/playback/resume/{media_id}

Resume info for a media item (best file, stored position, track preferences).

### GET /api/playback/resume/episode/{episode_id}

Resume info for a specific episode.

### GET /api/playback/next-hint

Suggests the next episode or related media.

---

### POST /api/playback/handoff

Create a one-time cross-device handoff token.

### POST /api/playback/handoff/redeem

Redeem a handoff token on another device to resume playback.

---

## Playback Security

### POST /api/playback/session

Authenticate → risk gate → short-lived signed grant. Entry point for all playback.

```bash
POST /api/playback/session
{ "media_id": "...", "file_id": "...", "client_fingerprint": "..." }
# → { "grant": "eyJ...", "expires_in": 600 }
```

---

### POST /api/playback/grant/segment

Mint a child grant scoped to a specific HLS segment (≤120s TTL).

### POST /api/playback/offline-keys

Issue an offline playback license (24h TTL, bound to user+media+device).

### GET /api/playback/offline-keys

List your offline licenses.

### GET /api/playback/offline-keys/{license_id}/validate

Validate an offline license.

### DELETE /api/playback/offline-keys/{license_id}

Revoke an offline license.

---

## Streaming & Protection

### GET /api/files/{id}/raw

HTTP Range streaming for seeking. Supports `Range: bytes=0-1023` headers.

### GET /api/files/{id}/subtitles

Extracted + converted subtitles (SRT/ASS → VTT).

### POST /api/files/{id}/download-url

Generate a signed download URL (HMAC-SHA256, device-bound, expiring).

```bash
POST /api/files/{id}/download-url
{ "device_id": "..." }
# → { "url": "https://.../dl/eyJ...", "expires_at": "..." }
```

### GET /api/dl/{token}

Redeem a signed download URL. Returns the file with `Content-Disposition: attachment`.

---

### POST /api/admin/files/{id}/probe

On-demand FFprobe of a file.

### POST /api/admin/files/probe-all

Probe all unprobed files.

### POST /api/admin/library/scan

Scan configured `LIBRARY_ROOTS` for new media.

---

### GET /api/lyrics?q=

Search for synchronized lyrics by query.

### GET /api/lyrics/providers

List available lyrics providers.

### GET /api/media/{id}/lyrics

Get lyrics for a specific media item.

---

## Tracking & Progress

### PUT /api/tracking/progress

Report video progress (auto-completes at 92%).

```bash
PUT /api/tracking/progress
{ "media_id": "...", "position_seconds": 1234, "duration_seconds": 5400 }
```

### PUT /api/tracking/reading

Report reading progress for books/manga.

```bash
PUT /api/tracking/reading
{ "media_id": "...", "position_percent": 45.2, "page": 120 }
```

### PUT /api/progress

Universal progress router (auto-routes by media_type).

### GET /api/me/media/{id}/reading

Get reading progress for a media item.

---

### GET /api/continue?types=movie,manga

Continue rail: merged watching + reading + listening progress, filterable by type.

### GET /api/me/continue-watching

Continue watching (video only).

---

### PUT /api/me/media/{id}/status

Set your list status for a media item.

```bash
PUT /api/me/media/{id}/status
{ "status": "watching" }
# Values: watching, completed, on_hold, dropped, planned
```

### DELETE /api/me/media/{id}/status

Remove your status.

### PUT /api/media/{id}/rating

Rate a media item (1–10).

### DELETE /api/media/{id}/rating

Remove your rating.

### POST /api/media/{id}/favorite

Toggle favorite.

---

### GET /api/me/history

Paginated watch/reading history (cursor-based).

### GET /api/me/history/settings

Get history display/filter settings.

### PUT /api/me/history/settings

Update history settings.

### DELETE /api/me/history

Wipe entire watch history. Audited.

---

### GET /api/me/ratings

List your ratings.

### GET /api/me/favorites

List your favorites.

### GET /api/me/list

Get your combined list (all statuses).

---

### POST /api/me/lists

Create a custom named list.

### GET /api/me/lists

List your custom lists.

### GET /api/lists/{id}/items

List items in a custom list.

### POST /api/lists/{id}/items/{media_id}

Add an item to a list.

### DELETE /api/lists/{id}/items/{media_id}

Remove an item from a list.

---

### GET /api/me/summary

Dashboard summary: watch hours, counts, reading progress, notifications, card status.

### GET /api/me/export

Full personal-data export as JSON.

---

### Playback Session Tracking

```bash
POST /api/playback/sessions         # Create a session
GET /api/playback/sessions           # List my sessions
POST /api/playback/sessions/{id}/heartbeat  # Keep alive
DELETE /api/playback/sessions/{id}   # End session
```

---

## Social & Chat

### Reviews

```bash
GET /api/media/{id}/reviews          # List reviews for media
POST /api/media/{id}/reviews         # Create a review
DELETE /api/reviews/{id}             # Delete your review
```

### Comments & Reactions

```bash
GET /api/comments?media_id=...       # List comments
POST /api/comments                   # Create a comment
DELETE /api/comments/{id}            # Delete your comment
POST /api/reactions                  # Toggle a reaction
GET /api/reactions?media_id=...      # Get reaction summary
```

### Follows & Activity Feed

```bash
POST /api/users/{id}/follow          # Follow a user
GET /api/me/following                # Who you follow
GET /api/me/followers                # Who follows you
GET /api/social/feed                 # Activity feed (own + followed)
```

### Conversations & DMs

```bash
GET /api/chat/conversations                  # List conversations
POST /api/chat/conversations                 # Create a conversation
GET /api/chat/conversations/{id}/messages    # List messages
POST /api/chat/conversations/{id}/messages    # Send a message
POST /api/chat/conversations/{id}/members/{user_id}  # Add member
DELETE /api/chat/conversations/{id}/members/me        # Leave
POST /api/chat/conversations/{id}/read        # Mark read
```

### Message Edit & Delete

```bash
PATCH /api/chat/messages/{id}        # Edit (adds edited_at marker)
DELETE /api/chat/messages/{id}       # Soft-delete (keeps audit)
```

### Friends System

```bash
POST /api/friends/{id}/request       # Send friend request
DELETE /api/friends/{id}             # Remove friend
POST /api/friends/requests/{id}/accept   # Accept request
POST /api/friends/requests/{id}/decline  # Decline request
DELETE /api/friends/requests/{id}    # Cancel outgoing request
GET /api/friends/requests            # List pending requests
GET /api/friends                     # List friends
```

### Media Sharing

```bash
POST /api/share/media                # Share media with a user
GET /api/me/shares/incoming          # Incoming shares
GET /api/me/shares/history           # Share history
POST /api/shares/{id}/viewed         # Mark as viewed
DELETE /api/shares/{id}              # Delete a share
```

---

## Watch Parties

### Party Lifecycle

```bash
POST /api/parties                    # Create a party
POST /api/parties/join-by-code       # Join by short code
POST /api/parties/{id}/join          # Join by ID
POST /api/parties/{id}/leave         # Leave
DELETE /api/parties/{id}             # End (host only)
GET /api/me/parties                  # List your parties
GET /api/parties/{id}                # Party details
GET /api/parties/{id}/members        # List members
```

### Host Controls

```bash
POST /api/parties/{id}/control       # Play/pause/seek commands
GET /api/parties/{id}/state          # Current sync state
GET /api/parties/{id}/events         # Event history (reconnect replay)
POST /api/parties/{id}/latency       # Report latency for drift correction
```

### Reactions & Transfers

```bash
POST /api/parties/{id}/reactions         # Add emoji reaction
GET /api/parties/{id}/reactions/heatmap  # Aggregate heatmap
POST /api/parties/{id}/transfer          # Transfer host role
POST /api/parties/{id}/promote           # Promote to co-host
```

---

## Moderation & Safety

### User Reports

```bash
POST /api/reports                    # Submit a report
GET /api/admin/reports?status=       # List reports (admin)
PATCH /api/admin/reports/{id}        # Update status
POST /api/admin/reports/{id}/accept  # Accept report
POST /api/admin/reports/{id}/reject  # Reject report
```

Report targets: `media`, `metadata`, `comment`, `chat_message`, `party_user`, `user`.

### Blocks & Mutes

```bash
POST /api/users/{id}/block           # Block a user
DELETE /api/users/{id}/block         # Unblock
GET /api/me/blocks                   # List blocked users
POST /api/users/{id}/mute            # Mute (optional duration)
DELETE /api/users/{id}/mute          # Unmute
```

### User Appeals

```bash
POST /api/appeals                    # Submit an appeal
GET /api/me/appeals                  # Your appeals
GET /api/admin/appeals               # All appeals (admin)
POST /api/admin/appeals/{id}/accept  # Accept appeal
POST /api/admin/appeals/{id}/reject  # Reject appeal
```

### Privacy Controls

```bash
GET /api/me/privacy                  # Get privacy settings
PATCH /api/me/privacy                # Update privacy settings
```

---

## Intelligence

### Taste Profile

```bash
GET /api/me/taste
# → { genre_weights: {...}, favorite_decades: [...], avg_runtime: 120, anime_affinity: 0.3, completion_rate: 0.72 }
```

### Home Rails

```bash
GET /api/home
# → { continue: [...], trending: [...], because_you_watched: [...], new_episodes: [...], friends: [...], recommendations: [...], vibe: [...] }
```

### Recommendations

```bash
GET /api/recommendations              # Personalized recommendations
GET /api/media/{id}/similar-scores    # Detailed similarity scores
```

### Personal Media Twin

```bash
GET /api/me/twin
# → Behavioral model: format preferences, genre weights, completion pace, creators, discovery ratio, current interests
```

### Cross-Media Journeys

```bash
GET /api/discover/journeys
# → Adaptation chains: book → movie → series → remake...
```

### AI Copilot

```bash
GET /api/admin/ai/config              # View AI configuration
PATCH /api/admin/ai/config            # Update AI configuration
POST /api/admin/ai/analyze            # Trigger analysis
POST /api/admin/ai/apply              # Apply suggested changes
GET /api/admin/ai/events              # AI action audit log
GET /api/admin/ai/status              # Engine status
```

### Mood Catalog

```bash
GET /api/intelligence/moods           # List available moods
GET /api/intelligence/vibe?q=cozy     # Match media to mood
```

### Smart Collections

```bash
POST /api/collections                 # Create a collection
GET /api/collections/{id}/items       # List items
DELETE /api/collections/{id}          # Delete collection
```

### Heuristic Engine

```bash
GET /api/heuristic/status             # Engine status
GET /api/me/heuristic                 # Your behavioral profile
DELETE /api/me/heuristic              # Wipe your data
GET /api/me/heuristic/wrapped         # Year-in-review recap
GET /api/me/heuristic/insights        # Deep consumption insights
GET /api/me/heuristic/predictions     # Next-up predictions
GET /api/me/heuristic/similar/{id}    # Per-item similarity scores
```

---

## Media Graph

### Relationships

12 typed relationships: `adaptation_of`, `sequel_of`, `spin_off_of`, `same_universe`, `remake_of`, `prequel_of`, `parent_story`, `side_story`, `alternative_version`, `shared_character`, `crossover`, `inspired_by`.

```bash
POST /api/media/{id}/relationships        # Create a relationship
GET /api/media/{id}/relationships         # List relationships
DELETE /api/media/{id}/relationships/{to}/{rel}  # Remove
```

### Traversal

```bash
GET /api/media/{id}/related      # One-hop neighborhood, grouped by relation
GET /api/media/{id}/journey      # BFS ≤3 hops across all edges
```

---

## Realtime (WebSocket)

**Endpoint:** `GET /ws?token=<access_token>`

### Connection Flow
1. Connect with a valid access token as query param
2. Receive `hello` frame with `user_id` and `online_users[]`
3. Subscribe to rooms: `subscribe` frame with `room` field

### Rooms

| Room Pattern | Purpose | Auth |
|---|---|---|
| `__global__` | Presence changes, platform events | Auto-subscribed |
| `user:<uuid>` | Notifications, security flags | Auto-subscribed, self-only |
| `conv:<uuid>` | Chat messages, typing indicators | Membership required |
| `party:<uuid>` | Playback sync, reactions | Membership required |

### Client → Server Frames

```json
{ "type": "ping" }
{ "type": "subscribe",   "room": "conv:<uuid>" }
{ "type": "unsubscribe", "room": "conv:<uuid>" }
{ "type": "typing",      "room": "conv:<uuid>" }
{ "type": "message.send", "room": "conv:<uuid>", "body": "hello" }
{ "type": "read",         "room": "conv:<uuid>" }
{ "type": "reaction",     "room": "party:<uuid>", "emoji": "😱", "second_offset": 812 }
```

### Server → Client Events

| Event | Room | Description |
|---|---|---|
| `hello` | — | Connection established |
| `presence` | global | User online/offline status |
| `platform.event` | global | Event bus envelope |
| `notification` | global + user | New notification |
| `security.flag` | user | Challenge/revoke decision |
| `typing` | conv/party | Typing indicator |
| `message.new` | conv | New message |
| `message.edited` | conv | Message was edited |
| `state.sync` | party | Playback sync state |
| `reaction` | party | Emoji reaction |
| `join.event` / `leave.event` | party | Member joined/left |
| `party.ended` | party | Party was ended |
| `pong` | — | Heartbeat response |

### PartySyncState

```json
{
  "seq": 42,
  "playing": true,
  "position_seconds": 611.3,
  "server_time_ms": 1767225600123,
  "server_position_seconds": 623.9
}
```

Clients render from `server_position_seconds` and correct drift using `server_time_ms`.

### Heartbeat

Server pings every 30s; connections silent for 75s are reaped.

---

## Card Economy

### Card Basics

Cards are cryptographic tokens (`SPC-{12hex}-{8hmac}`) that gate playback.

```bash
POST /api/cards/activate              # Activate a card
GET /api/me/card                      # Quick status check
GET /api/me/cards                     # Full card inventory
GET /api/me/cards/summary             # Summary with days remaining
```

### Transfer

```bash
POST /api/cards/transfer-code         # Generate a 20-min one-time code
```

### Merchant Operations

```bash
POST /api/merchant/register           # Register as a merchant
GET /api/merchant/inventory           # View inventory
POST /api/merchant/cards/purchase     # Purchase cards
POST /api/merchant/cards/give         # Give cards by username
POST /api/merchant/cards/redeem-transfer  # Redeem a transfer code
```

### Admin Card Operations

```bash
POST /api/admin/cards/mint            # Mint cards in bulk
GET /api/admin/cards                  # Search/filter all cards
POST /api/admin/cards/grant           # Grant cards to a user
POST /api/admin/cards/{number}/hold   # Instant hold/release
GET /api/admin/cards/chain            # Full card chain visibility
GET /api/admin/billing/config         # View pricing config
PATCH /api/admin/billing/config       # Update pricing
GET /api/admin/card-purchases         # Purchase queue
POST /api/admin/card-purchases/{id}/approve  # Approve purchase
POST /api/admin/card-purchases/{id}/reject   # Reject purchase
GET /api/admin/merchants              # List merchants
POST /api/admin/merchants/{id}/lock   # Lock merchant
POST /api/admin/merchants/{id}/ban    # Ban merchant
```

---

## Library & Playlists

### Playlist CRUD

```bash
POST /api/library/playlists           # Create a playlist
GET /api/library/playlists            # List your playlists
GET /api/library/playlists/{id}       # Get playlist details
PATCH /api/library/playlists/{id}     # Update playlist
DELETE /api/library/playlists/{id}    # Delete playlist
```

### Playlist Items

```bash
GET /api/library/playlists/{id}/items          # List items
POST /api/library/playlists/{id}/items          # Add item
PUT /api/library/playlists/{id}/items           # Replace all items
DELETE /api/library/playlists/{id}/items/{media_id}  # Remove item
PUT /api/library/playlists/{id}/order           # Reorder items
```

### Storage

```bash
GET /api/library/storage              # Disk usage overview
```

---

## File Management

### File Listing

```bash
GET /api/files                        # List all files
GET /api/files/{id}                   # File details with probe data
```

### Transcoding

```bash
POST /api/admin/files/{id}/transcode     # Enqueue transcode
GET /api/transcode-jobs/{id}              # Job status/progress
DELETE /api/transcode-jobs/{id}           # Cancel job
POST /api/admin/files/{id}/thumbnails     # Enqueue thumbnail extraction
```

### HLS Streaming

```bash
GET /api/files/{id}/master.m3u8       # Dynamic master playlist
GET /api/hls/{job_id}/{*asset}        # HLS segment/playlist assets
```

### Content Markers

```bash
GET /api/files/{id}/markers           # Read markers
PUT /api/files/{id}/markers           # Set markers (intro, outro, chapter)
POST /api/admin/files/{id}/markers/detect  # Auto-detect markers
GET /api/files/{id}/chapters          # Chapter list
```

### Extension Resources

```bash
GET /api/extensions/{id}/catalog/{type}/{catalog_id}
GET /api/extensions/{id}/stream/{type}/{sid}
GET /api/extensions/{id}/meta/{type}/{mid}
GET /api/extensions/subtitles/{type}/{sid}
```

---

## Ops & Observability

### Health & Monitoring

```bash
GET /health                           # Liveness (plain "ok")
GET /api/ready                        # Deep readiness (DB/Redis/FFmpeg)
GET /api/stats                        # Dashboard: counts, sessions, transcodes
GET /metrics                          # Prometheus scrape endpoint
```

### Automation Rules

```bash
GET /api/admin/automations            # List rules
POST /api/admin/automations           # Create rule
{ "event": "user.registered", "filter": {}, "action": "notify_all" }
PATCH /api/admin/automations/{id}     # Toggle enabled/disabled
DELETE /api/admin/automations/{id}    # Delete rule
```

### Webhooks

```bash
GET /api/admin/webhooks               # List webhooks
POST /api/admin/webhooks              # Create webhook
DELETE /api/admin/webhooks/{id}       # Delete webhook
POST /api/admin/webhooks/{id}/test    # Test-fire webhook
```

### Failure Analysis

```bash
GET /api/admin/failures               # Recent 5xx + per-route aggregates
```

### Event Timeline

```bash
GET /api/admin/events?event=&limit=   # Platform events timeline
```

### Notifications

Notifications are pushed to the global WS feed AND the recipient's personal `user:<id>` room.

### Billing Requests

```bash
POST /api/billing/requests            # Submit billing request
GET /api/billing/requests             # My requests
GET /api/admin/billing                # Admin queue
POST /api/admin/billing/{id}/approve  # Approve
POST /api/admin/billing/{id}/reject   # Reject
```

---

## Developer Experience

### API Versioning

Every response includes `X-API-Version: space/<version>`. Legacy `/v1/*` paths auto-rewrite to `/api/*`.

### OpenAPI

```bash
GET /api-docs/openapi.json            # Machine-readable spec
GET /docs                             # Swagger UI (interactive)
```

### Error Format

All errors return `{ code, message, trace_id }` with correct HTTP status.

### Pagination Convention

All list endpoints support `?limit=N&cursor=<opaque>`. Response includes `next_cursor`.

### Rate Limiting Headers

When rate-limited: `429` + `Retry-After: 60`.

### Server Meta

```bash
GET /api/meta
# → { version: "0.1.0", features: [...], docs: "/docs", websocket: "/ws" }
```

### Deployment

- `docker-compose.prod.yml` — Production stack (app + Postgres + Redis + nginx)
- `ops/nginx.conf` — TLS-ready reverse proxy config
- `ops/backup.sh` — Nightly backup with 14-day retention
- `ops/restore.sh` — Restore with verification steps
- Graceful shutdown: SIGTERM drains in-flight requests and WS connections
