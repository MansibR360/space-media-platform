/* SPACE feature catalog — each entry maps to shipped code.
   Format: { c: category, n: name, d: description, e: [endpoints/notes] } */

const CATS = [
  "Auth & Identity", "Admin Fortress", "Catalog & Metadata", "Search & Discovery",
  "Sources & Plugins", "Resolution & Playback", "Playback Security",
  "Streaming & Protection", "Tracking & Progress", "Social & Chat",
  "Watch Parties", "Moderation & Safety", "Intelligence", "Media Graph",
  "Realtime", "Card Economy", "Ops & Observability", "Developer Experience",
  "User Platform", "Library & Playlists", "File Management",
];

const FEATURES = [
// ============================================================= Auth & Identity
{ c:"Auth & Identity", n:"Account registration", d:"Username/email/password with policy checks, email verification codes (hashed, expiring), and an automatic 7-day trial access card.", e:["POST /api/auth/register"] },
{ c:"Auth & Identity", n:"Login + lockouts", d:"Identifier accepts username or email. Per (identity, IP) sliding-window lockout after repeated failures.", e:["POST /api/auth/login"] },
{ c:"Auth & Identity", n:"TOTP 2FA (admins)", d:"RFC-6238 implemented in-tree (HMAC-SHA1 + base32). Provisioning URI, ±1 step skew tolerance; enabled admins must send totp_code at login.", e:["POST /api/admin/security/totp/setup","…/enable","…/disable"] },
{ c:"Auth & Identity", n:"Behavioral login baseline", d:"Admin logins record IP-prefix + hour history; first-seen networks are audited as anomalies.", e:["admin_login_history table"] },
{ c:"Auth & Identity", n:"Token refresh rotation", d:"Access tokens 15 min; rotating refresh tokens with hashed storage.", e:["POST /api/auth/refresh"] },
{ c:"Auth & Identity", n:"Sessions management", d:"List active sessions per device; revoke any of them instantly or log out everywhere.", e:["GET /api/auth/sessions","DELETE /api/auth/sessions/{id}","POST /api/auth/logout","POST /api/auth/logout-all"] },
{ c:"Auth & Identity", n:"Password reset flow", d:"Single-use hashed reset tokens with expiry; enumeration-safe (unknown emails return ok).", e:["POST /api/auth/password/forgot","POST /api/auth/password/reset"] },
{ c:"Auth & Identity", n:"Password change", d:"Change password for the authenticated user; current password required.", e:["POST /api/users/me/password"] },
{ c:"Auth & Identity", n:"Email verification", d:"Codes hashed at rest, expire in 15 min, lock after 5 bad tries; dev echo mode for local testing.", e:["POST /api/auth/email/request-code","POST /api/auth/email/verify"] },
{ c:"Auth & Identity", n:"Scoped API keys", d:"X-API-Key credentials with read/write/downloads scopes; mutating requests need write scope.", e:["POST /api/me/keys","GET /api/me/keys","DELETE /api/me/keys/{key_id}"] },
{ c:"Auth & Identity", n:"Permission grants", d:"Admin grants fine-grained capabilities: download, manage_library, moderate_content.", e:["POST /api/admin/users/{id}/permissions","GET /api/admin/users/{id}/permissions","DELETE /api/admin/users/{id}/permissions/{perm}"] },
{ c:"Auth & Identity", n:"User profiles", d:"Public profiles by username; authenticated users can view and update their own profile.", e:["GET /api/users/{username}","GET /api/users/me","PATCH /api/users/me"] },

// ============================================================= Admin Fortress
{ c:"Admin Fortress", n:"Loopback-only admin surface", d:"/api/admin/* is localhost-only unless ADMIN_LOCAL_ONLY=false; proxied forwarded headers are refused.", e:["admin_gate middleware"] },
{ c:"Admin Fortress", n:"Watchdog attack detector", d:"Sliding window over admin failures (≥12/min per IP or ≥40/min distributed) trips a 15-min localhost-only remote lockout. Localhost always passes.", e:["in-memory · admin_gate integration"] },
{ c:"Admin Fortress", n:"Honeypot traps", d:"Hidden form fields + fake dump-secrets route. Any touch = permanent ban (IP-hash + user-hash), full session wipe, watchdog trip.", e:["GET|POST /api/admin/debug/dump-secrets"] },
{ c:"Admin Fortress", n:"Escalated admin lockout bans", d:"≥5 failed logins against an admin identity ban the username-hash AND address-hash from the whole admin surface for 4× the lockout window.", e:["admin_bans table"] },
{ c:"Admin Fortress", n:"Site-wide lockdown", d:"One toggle blocks every public route with 503 while admin APIs + health stay up. Message is customizable.", e:["GET|POST /api/admin/security/lockdown"] },
{ c:"Admin Fortress", n:"Server-gated console page", d:"The console HTML is only served at /console after session verification (role-checked). Raw asset paths always 404 — markup never leaks unauthenticated.", e:["GET /console → gated"] },
{ c:"Admin Fortress", n:"Global audit trail", d:"Every sensitive action (policy changes, keys, bans, cards, lockdown) persisted with actor + IP; filterable timeline.", e:["GET /api/admin/audit-logs?action=&limit="] },
{ c:"Admin Fortress", n:"Admin user management", d:"Full user listing with pagination; grant, list, and revoke per-user permissions.", e:["GET /api/admin/users"] },

// ============================================================= Catalog & Metadata
{ c:"Catalog & Metadata", n:"Media CRUD", d:"Full lifecycle for movie/series/book/music/manga/podcast/audiobook entries with status, ratings, popularity.", e:["GET|POST /api/media","GET /api/media/{id}"] },
{ c:"Catalog & Metadata", n:"Admin media management", d:"Create, update, and delete media entries through admin endpoints.", e:["POST /api/admin/media","PATCH /api/admin/media/{id}","DELETE /api/admin/media/{id}"] },
{ c:"Catalog & Metadata", n:"Seasons & episodes", d:"Series structure with numbering, air dates, stills, runtime. Episodes listed per media.", e:["POST /api/admin/media/{id}/seasons","POST …/seasons/{sid}/episodes","GET /api/media/{id}/episodes"] },
{ c:"Catalog & Metadata", n:"People & credits graph", d:"Genres, studios, people and credits linked to media for affinity queries.", e:["media_genres · media_studios · credits tables"] },
{ c:"Catalog & Metadata", n:"External mappings + confidence", d:"Every item tracks provider IDs with confidence scores feeding quality-conflict review.", e:["external_mappings table"] },
{ c:"Catalog & Metadata", n:"Metadata providers", d:"TMDB, AniList, OMDb built-ins plus hot-swappable HTTP plugins behind one registry.", e:["GET /api/metadata/sources","POST /api/metadata/import","GET /api/metadata/search"] },
{ c:"Catalog & Metadata", n:"Scheduled metadata refresh", d:"Background re-sync of stale external mappings with change tracking. Quality conflict review.", e:["POST /api/admin/metadata/refresh","GET /api/admin/quality/conflicts"] },
{ c:"Catalog & Metadata", n:"Quality duplicate detection", d:"Fuzzy matching across catalog to find duplicate entries; auto-resolve merges weaker duplicates into the stronger one.", e:["GET /api/admin/quality/duplicates","POST /api/admin/quality/duplicates/auto-resolve"] },
{ c:"Catalog & Metadata", n:"Storage analytics", d:"Disk usage breakdown per media type and top consumers across all library roots.", e:["GET /api/admin/storage/analytics"] },

// ============================================================= Search & Discovery
{ c:"Search & Discovery", n:"Full-text search", d:"Postgres tsvector ranking with ILIKE fallback for engines lacking extensions (CockroachDB-safe).", e:["GET /api/search?q="] },
{ c:"Search & Discovery", n:"Query parser", d:"Structured query parser extracting filters, operators and natural-language tokens.", e:["GET /api/search/parse?q="] },
{ c:"Search & Discovery", n:"Unified cross-source search", d:"Parallel fan-out across local catalog + every healthy source in one call.", e:["GET /api/search/all?q="] },
{ c:"Search & Discovery", n:"Discover", d:"Curated discovery feed by filters.", e:["GET /api/discover"] },
{ c:"Search & Discovery", n:"Trending", d:"Windowed popularity from real watch activity.", e:["GET /api/trending?window=week"] },
{ c:"Search & Discovery", n:"Similar media", d:"Shared-genre + shared-studio scoring, cached per item.", e:["GET /api/media/{id}/similar"] },
{ c:"Search & Discovery", n:"Typo-tolerant suggestions", d:"Fast type-ahead suggestions endpoint.", e:["GET /api/search/suggest?q="] },
{ c:"Search & Discovery", n:"Vibe / mood search", d:"Natural-language mood matching ('cozy feel-good') over genres, keywords and pacing — zero external AI required.", e:["GET /api/intelligence/vibe?q="] },
{ c:"Search & Discovery", n:"Completed works", d:"Finished/released catalog filtered per type — movies, books, manga, series, music.", e:["GET /api/completed?type=movie"] },
{ c:"Search & Discovery", n:"Toplists with fallbacks", d:"Curated top charts: local popularity first, TMDB top-rated when a key is configured; 30-min cache.", e:["GET /api/toplists?kind=movies|series|anime|books|all"] },
{ c:"Search & Discovery", n:"Random pick", d:"Surprise-me endpoint with type and minimum-rating filters.", e:["GET /api/random?type=&min_rating="] },
{ c:"Search & Discovery", n:"Upcoming airings", d:"Episodes airing in the next 14 days across your lists plus popular shows.", e:["GET /api/upcoming"] },
{ c:"Search & Discovery", n:"Availability lookup", d:"Where can I play this? Matches the title across every healthy source + counts local files; tokens plug into /api/resolve.", e:["POST /api/media/{id}/availability"] },

// ============================================================= Sources & Plugins
{ c:"Sources & Plugins", n:"Native compiled sources", d:"Internet Archive films, IA audio, Project Gutenberg, MangaDex, Audius music, podcasts — in-process, zero extra hops.", e:["6 built-in providers"] },
{ c:"Sources & Plugins", n:"HTTP plugin auto-registration", d:"Paste a base URL; SPACE probes manifest.json → /items → /search and registers whichever protocol it speaks.", e:["POST /api/admin/sources/register-url"] },
{ c:"Sources & Plugins", n:"Python source plugins", d:"Stdlib-only template server implementing the plugin contract; register like any HTTP source. Documented guide included.", e:["plugins/python/space_plugin.py","docs/plugins-python.md"] },
{ c:"Sources & Plugins", n:"Circuit breaker", d:"3 consecutive failures trip a source open — skipped entirely until the half-open prober recovers it.", e:["health_loop every 60 s"] },
{ c:"Sources & Plugins", n:"Source telemetry", d:"Per-source latency, search/resolve success rates, active-request gauge and circuit state drive resolver preference.", e:["GET /api/sources","GET /api/sources/status"] },
{ c:"Sources & Plugins", n:"Browse + sync-to-catalog", d:"Native browse hook (fallback empty-query search); import source items into the local catalog with counts.", e:["GET /api/sources/{name}/browse","POST /api/admin/sources/{name}/sync"] },
{ c:"Sources & Plugins", n:"Source benchmark rankings", d:"Head-to-head latency and success-rate comparison across all registered sources.", e:["GET /api/sources/rankings"] },
{ c:"Sources & Plugins", n:"Source debug inspection", d:"Admin-only debug search and single-item fetch for any registered source.", e:["GET /api/admin/sources/{name}/debug-search","GET /api/admin/sources/{name}/debug-item/{id}"] },
{ c:"Sources & Plugins", n:"MangaDex chapter & page inspection", d:"Admin endpoints for inspecting MangaDex chapters and pages directly.", e:["GET /api/admin/mangadex/chapters/{manga_id}","GET /api/admin/mangadex/pages/{chapter_id}"] },
{ c:"Sources & Plugins", n:"Web sources (self-learning scrapers)", d:"Admin CRUD for web sources with automatic recipe learning from page structures; test endpoint validates scraping rules.", e:["GET|POST /api/admin/websources","PUT|DELETE /api/admin/websources/{id}","POST …/relearn","POST …/test","GET …/recipe"] },
{ c:"Sources & Plugins", n:"Hub federation", d:"Admin CRUD for hub nodes with bidirectional push/pull sync and connectivity tests.", e:["GET|POST /api/admin/hubs","PUT|DELETE /api/admin/hubs/{id}","POST …/pull","POST …/push","POST …/test"] },
{ c:"Sources & Plugins", n:"Hub public server API", d:"Public hub endpoints for item discovery, search, and source listing across federated nodes.", e:["GET /api/hub/find","GET /api/hub/item","GET|POST /api/hub/sources"] },
{ c:"Sources & Plugins", n:"YouTube source healing", d:"Admin status view and automated healing for the YouTube source provider.", e:["GET /api/admin/youtube/status","POST /api/admin/youtube/heal"] },
{ c:"Sources & Plugins", n:"Plugin management & testing", d:"Full lifecycle for metadata/streaming plugins: install, configure, test, remove.", e:["GET|POST /api/admin/plugins","PATCH|DELETE /api/admin/plugins/{id}","POST …/test","GET /api/metadata/sources"] },
{ c:"Sources & Plugins", n:"Plugin source sync", d:"Trigger a sync from a registered plugin source into the local catalog.", e:["POST /api/admin/plugins/{id}/sync"] },

// ============================================================= Resolution & Playback
{ c:"Resolution & Playback", n:"Central resolver pipeline", d:"11 stages: identify → sources → health → candidates → rank → quality → language → subtitles → availability → select → stream.", e:["POST /api/resolve"] },
{ c:"Resolution & Playback", n:"Preference ranking", d:"Quality distance (exact > close > known-far > unknown), language hints, subtitle hints, kind weight, source health bonus — pure unit-tested scoring.", e:["preferences.quality/language/subtitles"] },
{ c:"Resolution & Playback", n:"Automatic failover", d:"Ranked same-source fallbacks returned upfront; on failure the resolver retries other providers title-first.", e:["resolution.fallbacks[]"] },
{ c:"Resolution & Playback", n:"Legacy resolve alias", d:"Old contract preserved for existing clients.", e:["POST /api/playback/resolve"] },
{ c:"Resolution & Playback", n:"Manga reading flows", d:"Chapters listing, page extraction, and one-shot open (detail + chapters + first pages).", e:["POST /api/playback/manga/chapters|pages|open"] },
{ c:"Resolution & Playback", n:"Resume intelligence", d:"Best-file selection by height cap, stored position, remembered audio/subtitle track preferences.", e:["GET /api/playback/resume/{media_id}","GET /api/playback/resume/episode/{episode_id}"] },
{ c:"Resolution & Playback", n:"Playback lookup", d:"Availability-first resolver: returns where a title can be played without triggering full resolve.", e:["POST /api/playback/lookup"] },
{ c:"Resolution & Playback", n:"Playback handoff (cross-device)", d:"Create a one-time handoff token on one device; redeem it on another to resume from the same position.", e:["POST /api/playback/handoff","POST /api/playback/handoff/redeem"] },
{ c:"Resolution & Playback", n:"Next-in-queue hint", d:"Suggests the next episode or related media based on current playback context.", e:["GET /api/playback/next-hint"] },

// ============================================================= Playback Security
{ c:"Playback Security", n:"Playback controller entry", d:"Authenticate → risk gate → short-lived signed grant in one call.", e:["POST /api/playback/session"] },
{ c:"Playback Security", n:"Risk engine", d:"Weighted signals: concurrent sessions, distinct devices/24h, resolves/hour, prior flags, distinct client contexts. Transparent thresholds admins tune.", e:["GET|PATCH /api/admin/security/policy"] },
{ c:"Playback Security", n:"Fingerprint-bound grants", d:"Edge JA4 or client fingerprint hashed into the grant; later fetches must present the same context or get 403 mismatch.", e:["X-JA4 / X-Client-Fingerprint headers"] },
{ c:"Playback Security", n:"Segment-scoped child grants", d:"Parent grant mints ≤120 s tickets scoped to seg:N inside the signature — initial access authorizes one segment, not the file.", e:["POST /api/playback/grant/segment"] },
{ c:"Playback Security", n:"Instant revocation", d:"Revoke one grant or every grant of a user in one statement; future segment/key access dies immediately.", e:["POST /api/admin/security/grants/{id}/revoke","POST …/users/{uid}/revoke-grants"] },
{ c:"Playback Security", n:"Decision audit", d:"Every challenge/revoke decision persisted with signals snapshot; live push to the user's personal WS room.", e:["GET /api/admin/security/events"] },
{ c:"Playback Security", n:"Grant inventory", d:"Admin view of all active grants with filtering and mass-revoke capabilities.", e:["GET /api/admin/security/grants"] },
{ c:"Playback Security", n:"Offline key lifecycle", d:"Issue, validate, and revoke offline licenses bound to user+media+device with 24-hour TTL.", e:["POST /api/playback/offline-keys","GET /api/playback/offline-keys","GET …/{id}/validate","DELETE …/{id}"] },

// ============================================================= Streaming & Protection
{ c:"Streaming & Protection", n:"Library scanner", d:"Recursive scan of LIBRARY_ROOTS with filename parsing (series/S01E01/movie+year), probe scheduling and self-healing job retries.", e:["POST /api/admin/library/scan","POST /api/admin/files/probe-all"] },
{ c:"Streaming & Protection", n:"Ranged streaming", d:"HTTP Range support for seeking without full downloads.", e:["GET /api/files/{id}/raw"] },
{ c:"Streaming & Protection", n:"Subtitle pipeline", d:"Extraction + format conversion served per file.", e:["GET /api/files/{id}/subtitles"] },
{ c:"Streaming & Protection", n:"HLS adaptive transcoding", d:"Per-title ladder capped at source height, live progress jobs with poll/cancel, dynamic master playlist.", e:["POST /api/admin/files/{id}/transcode","GET /api/transcode-jobs/{id}","DELETE /api/transcode-jobs/{id}"] },
{ c:"Streaming & Protection", n:"Hardware acceleration", d:"nvenc / qsv / vaapi encoder selection via HWACCEL.", e:["encoder_args(cfg)"] },
{ c:"Streaming & Protection", n:"Key service (HKDF + AES-GCM)", d:"Master secret → purpose KEKs → AES-256-GCM-wrapped per-media keys. Rotation keeps old versions decryptable; revocation is instant and irreversible.", e:["POST /api/admin/security/keys/{ref}/rotate|revoke"] },
{ c:"Streaming & Protection", n:"HLS AES-128 encryption", d:"HLS_ENCRYPTION=true stamps EXT-X-KEY into playlists; content keys released only against valid playback grants.", e:["GET /api/playback/keys/{file_id}"] },
{ c:"Streaming & Protection", n:"Signed download links", d:"HMAC-SHA256 URLs with hard expiry, device binding and nonce hit-limits; audit row per grant.", e:["POST /api/files/{id}/download-url","GET /api/dl/{token}"] },
{ c:"Streaming & Protection", n:"Forensic watermarking", d:"Every resolved URL carries wm=<16-hex> binding user+session+hour — leaked copies trace to one authorized session.", e:["resolve response .watermark"] },
{ c:"Streaming & Protection", n:"Private-origin gate", d:"ORIGIN_INTERNAL_TOKEN makes key/download endpoints require the proxy-injected X-Origin-Token header; direct internet hits rejected.", e:["nginx pattern in docs/deployment.md §8"] },
{ c:"Streaming & Protection", n:"Offline licenses (24 h)", d:"Content key delivered once over TLS bound to user+media+device; idempotent within TTL; revocable; rotation kills re-delivery after expiry.", e:["POST /api/playback/offline-keys","GET|DELETE /api/playback/offline-keys"] },
{ c:"Streaming & Protection", n:"Lyrics pipeline", d:"Fetch synchronized lyrics for any media by query; aggregated provider list; media-bound lyrics lookup.", e:["GET /api/lyrics?q=","GET /api/lyrics/providers","GET /api/media/{id}/lyrics"] },

// ============================================================= Tracking & Progress
{ c:"Tracking & Progress", n:"Video progress engine", d:"Position/duration upserts with auto-complete at 92% and session sync.", e:["PUT /api/tracking/progress"] },
{ c:"Tracking & Progress", n:"Reading progress", d:"Percent + page for books/manga with completion inference.", e:["PUT /api/tracking/reading","GET /api/me/media/{id}/reading"] },
{ c:"Tracking & Progress", n:"Universal progress router", d:"One write endpoint routing seconds vs page by media_type.", e:["PUT /api/progress"] },
{ c:"Tracking & Progress", n:"Continue rail (all types)", d:"Watching + reading + listening merged via unified_progress view; filterable by types.", e:["GET /api/continue?types=movie,manga","GET /api/me/continue-watching"] },
{ c:"Tracking & Progress", n:"Lists / ratings / favorites", d:"watching/completed/on_hold/dropped/planned with scores, notes, episode counts, rewatch tracking.", e:["PUT /api/me/media/{id}/status","DELETE …/status","PUT /api/media/{id}/rating","DELETE …/rating","POST /api/media/{id}/favorite"] },
{ c:"Tracking & Progress", n:"Paginated history", d:"Cursor-based complete watch/reading history with configurable settings.", e:["GET /api/me/history","GET|PUT /api/me/history/settings","DELETE /api/me/history"] },
{ c:"Tracking & Progress", n:"Custom lists", d:"User-created named lists with full item management and ordering.", e:["POST|GET /api/me/lists","GET|POST /api/lists/{id}/items","DELETE /api/lists/{id}/items/{mid}","GET /api/me/list"] },
{ c:"Tracking & Progress", n:"Ratings & favorites view", d:"Quick access to personal ratings and favorited media.", e:["GET /api/me/ratings","GET /api/me/favorites"] },
{ c:"Tracking & Progress", n:"Me summary", d:"One dashboard call: watch hours, watching/completed counts, reading progress, unread notifications, card status.", e:["GET /api/me/summary"] },
{ c:"Tracking & Progress", n:"Data export", d:"Full personal-data export (watch history, lists, reading) as JSON.", e:["GET /api/me/export"] },
{ c:"Tracking & Progress", n:"Playback session tracking", d:"Create, list, heartbeat, and end playback sessions for concurrent-device monitoring and analytics.", e:["POST /api/playback/sessions","GET /api/playback/sessions","POST …/{id}/heartbeat","DELETE …/{id}"] },

// ============================================================= Social & Chat
{ c:"Social & Chat", n:"Reviews", d:"Published-status reviews with ratings feeding aggregates.", e:["GET|POST /api/media/{id}/reviews","DELETE /api/reviews/{id}"] },
{ c:"Social & Chat", n:"Comments + reactions", d:"Threaded discussion and emoji reactions on media.", e:["GET|POST /api/comments","DELETE /api/comments/{id}","POST|GET /api/reactions"] },
{ c:"Social & Chat", n:"Follows + activity feed", d:"Follow users; feed blends own + followed activity with block/mute filtering enforced.", e:["POST /api/users/{id}/follow","GET /api/me/following","GET /api/me/followers","GET /api/social/feed"] },
{ c:"Social & Chat", n:"Conversations & DMs", d:"Direct/group conversations, membership checks, read receipts.", e:["GET|POST /api/chat/conversations","GET …/messages","POST …/messages","POST …/members/{id}","DELETE …/members/me","POST …/read"] },
{ c:"Social & Chat", n:"Message edit & delete", d:"Owner edits (edited_at marker, live message.edited event); owner/staff soft-delete keeps audit history.", e:["PATCH|DELETE /api/chat/messages/{id}"] },
{ c:"Social & Chat", n:"Block enforcement in DMs", d:"Blocked pairs cannot exchange messages in either direction.", e:["user_blocks check on send"] },
{ c:"Social & Chat", n:"Friends system", d:"Send, accept, decline, cancel, and remove friend requests. List friends and pending requests.", e:["POST /api/friends/{id}/request","DELETE /api/friends/{id}","POST /api/friends/requests/{id}/accept","POST /api/friends/requests/{id}/decline","DELETE /api/friends/requests/{id}","GET /api/friends/requests","GET /api/friends"] },
{ c:"Social & Chat", n:"Media sharing", d:"Share media with other users via direct share; view incoming shares, history, mark viewed, or delete.", e:["POST /api/share/media","GET /api/me/shares/incoming","GET /api/me/shares/history","POST /api/shares/{id}/viewed","DELETE /api/shares/{id}"] },

// ============================================================= Watch Parties
{ c:"Watch Parties", n:"Party lifecycle", d:"Create, join by id or short code, leave; membership-authorised rooms.", e:["POST /api/parties","POST /api/parties/join-by-code","DELETE /api/parties/{id}","GET /api/me/parties"] },
{ c:"Watch Parties", n:"Host sync projection", d:"Server projects playhead from host actions with wall-clock correction fields for drift-free clients.", e:["state.sync frames · POST /api/parties/{id}/latency"] },
{ c:"Watch Parties", n:"Timestamped reactions + heatmap", d:"Emoji at second offsets with aggregate heatmaps.", e:["POST /api/parties/{id}/reactions","GET …/reactions/heatmap"] },
{ c:"Watch Parties", n:"Presence-aware rooms", d:"Join/leave events, party-ended broadcast, presence status 'watch_party'.", e:["join.event · leave.event · party.ended","POST /api/parties/{id}/join","POST /api/parties/{id}/leave"] },
{ c:"Watch Parties", n:"Host transfer & co-host promotion", d:"Transfer host role or promote a member to co-host for shared control.", e:["POST /api/parties/{id}/transfer","POST /api/parties/{id}/promote"] },
{ c:"Watch Parties", n:"Real-time host controls", d:"Play/pause/seek commands broadcast from host to all members.", e:["POST /api/parties/{id}/control","GET /api/parties/{id}/state","GET /api/parties/{id}/events"] },
{ c:"Watch Parties", n:"Party members & info", d:"List all members in a party; view party details.", e:["GET /api/parties/{id}/members","GET /api/parties/{id}"] },

// ============================================================= Moderation & Safety
{ c:"Moderation & Safety", n:"User reports", d:"Flag media, metadata, comments, chat messages, party users or users with reasons; flood-guarded (10 open max).", e:["POST /api/reports","GET /api/admin/reports?status=","PATCH /api/admin/reports/{id}"] },
{ c:"Moderation & Safety", n:"Report resolution workflow", d:"Admin accept/reject actions on reports with outcome tracking.", e:["POST /api/admin/reports/{id}/accept","POST /api/admin/reports/{id}/reject"] },
{ c:"Moderation & Safety", n:"Blocks", d:"Block removes follows both directions; blocked pairs cannot DM; feeds exclude them.", e:["POST|DELETE /api/users/{id}/block","GET /api/me/blocks"] },
{ c:"Moderation & Safety", n:"Mutes", d:"Optional duration; muted authors disappear from YOUR feeds and chat reads only.", e:["POST|DELETE /api/users/{id}/mute"] },
{ c:"Moderation & Safety", n:"User appeals", d:"Users submit appeals for moderation decisions; admin review with accept/reject.", e:["POST /api/appeals","GET /api/me/appeals","GET /api/admin/appeals","POST …/{id}/accept","POST …/{id}/reject"] },
{ c:"Moderation & Safety", n:"Privacy controls", d:"Per-user privacy settings for profile visibility, activity sharing, and data collection.", e:["GET|PATCH /api/me/privacy"] },

// ============================================================= Intelligence
{ c:"Intelligence", n:"Taste profile", d:"Genre weights, favorite decades, avg runtime, anime affinity, completion rate.", e:["GET /api/me/taste"] },
{ c:"Intelligence", n:"Home rails", d:"Composite: continue, trending, because-you-watched, new episodes, friends activity, recommendations, vibe picks.", e:["GET /api/home"] },
{ c:"Intelligence", n:"Recommendations engine", d:"Genre-affinity scored + cached; contextual variant adapts to hour, session length, device.", e:["GET /api/recommendations","GET /api/media/{id}/similar-scores"] },
{ c:"Intelligence", n:"Personal media twin", d:"Behavioral model: formats, genre weights, completion pace, creators, discovery ratio, current interests — daily snapshots for drift.", e:["GET /api/me/twin"] },
{ c:"Intelligence", n:"Cross-media journeys", d:"Adaptation chains reached from finished anchors, excluding what you consumed.", e:["GET /api/discover/journeys"] },
{ c:"Intelligence", n:"AI copilot", d:"Pluggable brain (heuristic rules | OpenAI-compatible LLM) with observe/suggest/act autonomy, scope gates, whitelisted actions, full audit, background actor loop.", e:["GET|PATCH /api/admin/ai/config","POST /api/admin/ai/analyze","POST /api/admin/ai/apply","GET /api/admin/ai/events","GET /api/admin/ai/status"] },
{ c:"Intelligence", n:"Mood catalog", d:"Browse available moods and match media to natural-language emotional descriptions.", e:["GET /api/intelligence/moods","GET /api/intelligence/vibe?q="] },
{ c:"Intelligence", n:"Smart collections", d:"User-created curated collections of media with full CRUD and item browsing.", e:["POST /api/collections","GET /api/collections/{id}/items","DELETE /api/collections/{id}"] },
{ c:"Intelligence", n:"Heuristic engine", d:"On-device behavioral engine with status, personal profile, wrapped year-in-review, deep insights, predictions, and per-item similarity scores.", e:["GET /api/heuristic/status","GET /api/me/heuristic","DELETE /api/me/heuristic","GET …/wrapped","GET …/insights","GET …/predictions","GET …/similar/{id}"] },

// ============================================================= Media Graph
{ c:"Media Graph", n:"12 typed relationships", d:"adaptation_of, sequel_of, spin_off_of, same_universe, remake_of… linking any formats.", e:["POST /api/media/{id}/relationships","GET /api/media/{id}/relationships","DELETE /api/media/{id}/relationships/{to}/{rel}"] },
{ c:"Media Graph", n:"Related neighborhood", d:"One hop both directions grouped by relation.", e:["GET /api/media/{id}/related"] },
{ c:"Media Graph", n:"Franchise traversal", d:"BFS ≤3 hops across all edges producing ordered journey nodes.", e:["GET /api/media/{id}/journey"] },
{ c:"Media Graph", n:"AI-maintained graph", d:"Grant the copilot the graph scope and it may apply add_media_relationship actions — audited.", e:["ai action add_media_relationship"] },

// ============================================================= Realtime
{ c:"Realtime", n:"WebSocket gateway", d:"Token-authenticated upgrade, hello handshake, room subscribe/unsubscribe protocol.", e:["GET /ws?token=…"] },
{ c:"Realtime", n:"Room authorization", d:"global · user:{self} · conv: membership · party: membership — validated per frame.", e:["subscribe / unsubscribe frames"] },
{ c:"Realtime", n:"Heartbeat + reaping", d:"Server pings every 30 s; silent connections reaped after 75 s. Client ping/pong also counted.", e:["protocol-level Ping/Pong"] },
{ c:"Realtime", n:"Event bus bridge", d:"Platform events persisted then enveloped onto the global room; queryable history.", e:["GET /api/admin/events"] },
{ c:"Realtime", n:"Presence system", d:"REST update + batch query + WS presence.update envelopes; invisible stays hidden.", e:["PUT /api/me/presence","GET /api/presence?ids="] },
{ c:"Realtime", n:"Live chat & party frames", d:"typing indicators, message.new, reactions, party state.sync — all through one socket.", e:["docs/websocket.md"] },

// ============================================================= Card Economy
{ c:"Card Economy", n:"Cryptographic card numbers", d:"SPC-{12hex}-{8hmac}: checksum group authenticates the body offline; forgery requires the signing secret.", e:["access_cards.card_number"] },
{ c:"Card Economy", n:"Hard playback gate", d:"No active card = nothing plays — resolve, sessions, offline keys, local streaming, admins included. No bypass path exists.", e:["ensure_card_access()"] },
{ c:"Card Economy", n:"7-day trial, abuse-proofed", d:"Auto-granted at registration; partial unique index makes double-issuance impossible even under races.", e:["one_trial_per_user index"] },
{ c:"Card Economy", n:"Activation = one month", d:"unused→active for duration_days; activating another extends your ceiling.", e:["POST /api/cards/activate"] },
{ c:"Card Economy", n:"Merchant economy", d:"Self-service registration; banned merchants rejected everywhere; totals tracked.", e:["POST /api/merchant/register","GET /api/merchant/inventory","POST /api/admin/merchants/{id}/ban","POST …/lock","GET /api/admin/merchants"] },
{ c:"Card Economy", n:"Crypto purchases + approval", d:"Merchant orders N cards at admin-set price → pending queue → approval mints inventory. Rejection supported.", e:["POST /api/merchant/cards/purchase","GET /api/admin/card-purchases","POST …/{id}/approve|reject"] },
{ c:"Card Economy", n:"Give & transfer codes", d:"Merchants give directly by username, or buyers publish a 20-minute one-time code the merchant redeems for N cards.", e:["POST /api/merchant/cards/give","POST /api/cards/transfer-code","POST /api/merchant/cards/redeem-transfer"] },
{ c:"Card Economy", n:"Instant hold + chain visibility", d:"Hold/release any card immediately; merchant→holder aggregation shows where every card went.", e:["POST /api/admin/cards/{number}/hold","GET /api/admin/cards/chain"] },
{ c:"Card Economy", n:"Pricing configuration", d:"Unit price, currency, crypto payout address — admin editable, audited.", e:["GET|PATCH /api/admin/billing/config"] },
{ c:"Card Economy", n:"My cards & summary", d:"Quick card status check (active/inactive, masked number, days remaining) and full card inventory.", e:["GET /api/me/card","GET /api/me/cards","GET /api/me/cards/summary"] },
{ c:"Card Economy", n:"Admin card minting & search", d:"Mint new cards in bulk and search/filter all issued cards.", e:["POST /api/admin/cards/mint","GET /api/admin/cards","POST /api/admin/cards/grant"] },

// ============================================================= Ops & Observability
{ c:"Ops & Observability", n:"Automations (WHEN/IF/THEN)", d:"Event-triggered rules with payload filters executing notify_all/notify_users/webhook actions.", e:["GET|POST /api/admin/automations","PATCH|DELETE /{id}"] },
{ c:"Ops & Observability", n:"Signed webhooks", d:"HMAC-SHA256 signed deliveries with event filters and test-fire endpoint.", e:["GET|POST /api/admin/webhooks","DELETE …/{id}","POST …/test"] },
{ c:"Ops & Observability", n:"Failure analysis", d:"5xx recorder middleware with route + duration; top-offender aggregation; AI raises notifications on spikes.", e:["GET /api/admin/failures"] },
{ c:"Ops & Observability", n:"Adaptive privacy", d:"Events older than 30 d roll into daily aggregates; raw rows deleted past 90 d; aged security payloads scrubbed. Personal data untouched.", e:["maintenance loop · event_rollups"] },
{ c:"Ops & Observability", n:"Predictive cache warmer", d:"Every 10 min: recommendation rails for online users, similar-shelves for weekly trending.", e:["warming loop · cache_warmed_total"] },
{ c:"Ops & Observability", n:"Notifications", d:"Persisted notifications pushed to global feed AND the recipient's personal WS room.", e:["platform notifications table"] },
{ c:"Ops & Observability", n:"Prometheus metrics", d:"HTTP histograms/counters, business counters (cards, risk decisions, warming), scrape-ready endpoint.", e:["GET /metrics"] },
{ c:"Ops & Observability", n:"Health & readiness", d:"Liveness, deep readiness (db/redis/ffmpeg) and rich stats for dashboards.", e:["GET /api/health","/api/ready","/api/stats"] },
{ c:"Ops & Observability", n:"Extension management", d:"Admin install, configure, discover, update, and remove platform extensions.", e:["GET|POST /api/admin/extensions","PATCH|DELETE /api/admin/extensions/{id}","GET …/discover"] },
{ c:"Ops & Observability", n:"Billing request queue", d:"User-submitted billing requests with admin approval/rejection workflow.", e:["POST|GET /api/billing/requests","GET /api/admin/billing","POST …/{id}/approve","POST …/{id}/reject"] },
{ c:"Ops & Observability", n:"YouTube source health", d:"Real-time status and automated healing for the YouTube content source.", e:["GET /api/admin/youtube/status","POST /api/admin/youtube/heal"] },

// ============================================================= Developer Experience
{ c:"Developer Experience", n:"OpenAPI + Swagger UI", d:"Machine-readable spec covering the API; interactive docs hosted at /docs.", e:["GET /api-docs/openapi.json","GET /docs"] },
{ c:"Developer Experience", n:"Uniform problem errors", d:"Every error is { code, message, trace_id } with correct status + Retry-After semantics.", e:["error.rs ProblemJson"] },
{ c:"Developer Experience", n:"Cursor pagination", d:"Opaque cursors, limit clamps, next_cursor passthrough convention.", e:["?limit=&cursor= everywhere"] },
{ c:"Developer Experience", n:"Rate limiting", d:"Global per-IP 300/min, auth endpoints 20/min, configurable, Redis-backed when present.", e:["429 rate_limited + Retry-After"] },
{ c:"Developer Experience", n:"API versioning + v1 compat", d:"X-API-Version on every response; legacy /v1/* rewritten with deprecation tag.", e:["v1_rewrite middleware"] },
{ c:"Developer Experience", n:"WS schema documentation", d:"Complete protocol: auth, framing, rooms, heartbeat, every envelope shape with payloads.", e:["docs/websocket.md"] },
{ c:"Developer Experience", n:"Deployment guides", d:"Docker/compose prod stack, TLS, backups/PITR, scaling, zero-downtime upgrades, CDN origin pattern.", e:["docker-compose.prod.yml","docs/deployment.md"] },
{ c:"Developer Experience", n:"Server meta", d:"Machine-readable capability sheet: version, feature flags, doc/socket pointers.", e:["GET /api/meta"] },

// ============================================================= User Platform
{ c:"User Platform", n:"User settings", d:"Per-user preferences for playback, UI, notifications — read, replace, or partial patch.", e:["GET|PUT|PATCH /api/users/me/settings"] },
{ c:"User Platform", n:"Device registration", d:"Register, list, and revoke client devices for playback binding and session tracking.", e:["POST|GET /api/me/devices","DELETE /api/me/devices/{id}"] },
{ c:"User Platform", n:"Notifications", d:"Paginated notification feed with unread count and per-item read marking.", e:["GET /api/me/notifications","GET …/unread-count","POST …/{id}/read"] },
{ c:"User Platform", n:"Search history", d:"Track and clear personal search query history.", e:["GET|DELETE /api/me/search-history"] },
{ c:"User Platform", n:"Behavior analytics", d:"Personal consumption analytics: peak hours, preferred genres, device usage breakdown.", e:["GET /api/me/behavior"] },
{ c:"User Platform", n:"Session management", d:"List all active sessions; revoke individual sessions or wipe all.", e:["GET /api/auth/sessions","DELETE /api/auth/sessions/{id}","POST /api/auth/logout","POST /api/auth/logout-all"] },
{ c:"User Platform", n:"YouTube watch history", d:"Record, list, clear, and individually remove YouTube video watch history entries.", e:["PUT /api/youtube/history","GET /api/youtube/history","DELETE /api/youtube/history","DELETE /api/youtube/history/{video_id}"] },

// ============================================================= Library & Playlists
{ c:"Library & Playlists", n:"Playlist CRUD", d:"Create, read, update, delete named playlists with descriptions and metadata.", e:["POST|GET /api/library/playlists","GET|PATCH|DELETE /api/library/playlists/{id}"] },
{ c:"Library & Playlists", n:"Playlist items", d:"Add, remove, replace, and list items within a playlist with ordering support.", e:["GET|POST|PUT /api/library/playlists/{id}/items","DELETE …/items/{mid}"] },
{ c:"Library & Playlists", n:"Playlist ordering", d:"Drag-and-drop reorder items within a playlist via explicit position indices.", e:["PUT /api/library/playlists/{id}/order"] },
{ c:"Library & Playlists", n:"Storage info", d:"Disk usage overview across library roots with per-type breakdown.", e:["GET /api/library/storage"] },

// ============================================================= File Management
{ c:"File Management", n:"File listing & metadata", d:"Browse and inspect all registered media files with probe data, sizes, codecs.", e:["GET /api/files","GET /api/files/{id}"] },
{ c:"File Management", n:"File probe & events", d:"On-demand FFprobe of individual files; audit trail of all file lifecycle events.", e:["POST /api/admin/files/{id}/probe","GET /api/admin/files/{id}/events"] },
{ c:"File Management", n:"HLS master playlist", d:"Dynamic master.m3u8 assembled from running/completed transcode ladders.", e:["GET /api/files/{id}/master.m3u8","GET /api/hls/{job}/{*asset}"] },
{ c:"File Management", n:"Transcode job management", d:"Enqueue transcode, poll progress, and cancel running jobs.", e:["POST /api/admin/files/{id}/transcode","GET|DELETE /api/transcode-jobs/{id}"] },
{ c:"File Management", n:"Thumbnail generation", d:"Enqueue thumbnail extraction from video files at configurable intervals.", e:["POST /api/admin/files/{id}/thumbnails"] },
{ c:"File Management", n:"Content markers & chapters", d:"Read and write scene markers (intro, outro, chapter); auto-detect markers from video analysis.", e:["GET|PUT /api/files/{id}/markers","POST /api/admin/files/{id}/markers/detect","GET /api/files/{id}/chapters"] },
{ c:"File Management", n:"Multi-format subtitle aggregation", d:"Unified subtitle endpoint across all registered extensions.", e:["GET /api/extensions/subtitles/{stype}/{sid}"] },
{ c:"File Management", n:"Extension resource routing", d:"Stream, catalog, and metadata resources served through installed extensions.", e:["GET /api/extensions/{id}/catalog/{stype}/{cid}","GET …/stream/{stype}/{sid}","GET …/meta/{stype}/{mid}"] },
];
