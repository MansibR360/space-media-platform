//! Unified, source-anonymous playback surface.
//!
//! The public contract: search returns tokens; `resolve` turns a token into a
//! playable URL with automatic best-stream picking and cross-source failover.
//! Clients never see — and never choose — which backing source is used.

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::IntoParams;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::sources::aggregator;
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams, utoipa::ToSchema)]
pub struct EverythingQuery {
    #[serde(default = "default_q")]
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// optional client-side category filter: movie|series|book|music|manga
    #[serde(default)]
    pub category: Option<String>,
}

fn default_q() -> String {
    String::new()
}

fn default_limit() -> i64 {
    30
}

/// The source-anonymous shape clients consume: a title and an opaque token,
/// never the backends behind it.
fn unified_items(results: &[aggregator::UnifiedResult]) -> Vec<serde_json::Value> {
    results
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.token,
                "title": r.title,
                "type": r.category,
                "year": r.year,
                "poster": r.poster,
                "overview": r.overview,
                "relevance": r.relevance,
            })
        })
        .collect()
}

#[utoipa::path(get, path = "/api/search/all", tag = "search",
    params(EverythingQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn search_everything(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    ConnectInfo(_peer): ConnectInfo<std::net::SocketAddr>,
    axum::extract::Query(query): axum::extract::Query<EverythingQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let started = std::time::Instant::now();

    // A search that has been answered before is answered from the database.
    // The fan-out costs seconds and hundreds of upstream requests, and the
    // answer is the same for everyone, so the first caller pays for it once
    // and every caller after is served from here.
    let cache_key = crate::search::cache::key(
        &query.q,
        query.category.as_deref(),
        query.limit,
    );
    let cacheable = !query.q.trim().is_empty();

    if cacheable
        && let Some(cached) = crate::search::cache::get(&state.pool, &cache_key).await
    {
        metrics::counter!("unified_search_cache_hits_total").increment(1);
        metrics::histogram!("unified_search_duration_seconds")
            .record(started.elapsed().as_secs_f64());

        // An old entry is still served immediately; the refresh happens behind
        // the caller so the *next* one gets fresher results.
        if cached.is_stale() {
            spawn_refresh(state.clone(), query.q.clone(), query.category.clone(), query.limit, cache_key.clone());
        }

        let pool = state.pool.clone();
        let key = cache_key.clone();
        tokio::spawn(async move {
            crate::search::cache::record_hit(&pool, &key).await;
        });

        return Ok(Json(cached.payload));
    }

    let results = aggregator::search_everything(
        &state,
        &query.q,
        query.limit as i32,
        query.category.as_deref(),
        true,
    )
    .await;
    metrics::counter!("unified_search_cache_misses_total").increment(1);
    metrics::histogram!("unified_search_duration_seconds").record(started.elapsed().as_secs_f64());

    // Search history (Phase 48): recorded fire-and-forget, never blocks.
    if !query.q.trim().is_empty() {
        let pool = state.pool.clone();
        let q = query.q.trim().to_string();
        let user_id = auth.id();
        let count = results.len();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO search_history (id, user_id, query, result_count) VALUES ($1, $2, $3, $4)",
            )
            .bind(crate::utils::new_id())
            .bind(user_id)
            .bind(&q)
            .bind(count as i32)
            .execute(&pool)
            .await;
        });
    }

    // Results are source-anonymous: a title carries an opaque token, never the
    // backends behind it. Which source actually plays is decided at resolve
    // time, fastest-first with failover, so naming them here would only leak
    // where the media comes from and go stale the moment a source degrades.
    let items = unified_items(&results);

    let payload = serde_json::json!({
        "items": items,
        "results": results,
    });

    // Keep it, so nobody pays for this query again.
    if cacheable {
        let pool = state.pool.clone();
        let key = cache_key.clone();
        let q = query.q.clone();
        let category = query.category.clone();
        let limit = query.limit;
        let stored = payload.clone();
        let count = results.len();
        tokio::spawn(async move {
            crate::search::cache::put(
                &pool,
                &key,
                &q,
                category.as_deref(),
                limit,
                &stored,
                count,
            )
            .await;
        });
    }

    Ok(Json(payload))
}

/// Re-run a stale search behind the caller and write the fresher answer back.
fn spawn_refresh(
    state: Arc<AppState>,
    query: String,
    category: Option<String>,
    limit: i64,
    key: String,
) {
    tokio::spawn(async move {
        let results = aggregator::search_everything(
            &state,
            &query,
            limit as i32,
            category.as_deref(),
            true,
        )
        .await;
        if results.is_empty() {
            // A momentary upstream outage must not overwrite a good answer
            // with an empty one.
            return;
        }
        let items = unified_items(&results);
        let payload = serde_json::json!({ "items": items, "results": results });
        crate::search::cache::put(
            &state.pool,
            &key,
            &query,
            category.as_deref(),
            limit,
            &payload,
            results.len(),
        )
        .await;
    });
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ResolveRequest {
    /// token from any aggregated result
    pub token: String,
    /// enables title-based failover when the primary source has no stream
    #[serde(default)]
    pub title_hint: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ResolvedPlayback {
    pub url: String,
    /// mp4 | hls | audio | page | file
    pub kind: String,
    pub title: String,
}

/// Auto-selects the best playable stream for an item. Never exposes sources.
#[utoipa::path(post, path = "/api/playback/resolve", tag = "playback",
    security(("bearer" = [])),
    request_body = ResolveRequest,
    responses((status = 200, body = ResolvedPlayback), (status = 503))
)]
pub async fn resolve_playback(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    ConnectInfo(_peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<ResolveRequest>,
) -> ApiResult<Json<ResolvedPlayback>> {
    let _ = auth;
    let resolved = aggregator::resolve(
        &state,
        req.token.trim(),
        req.title_hint.as_deref(),
        true,
    )
    .await?;
    Ok(Json(ResolvedPlayback {
        url: resolved.url,
        kind: resolved.kind,
        title: resolved.title,
    }))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct WatchLookupRequest {
    /// exact title to match across sources (from your catalog/metadata search)
    pub title: String,
    /// optional year disambiguation (±1 tolerance)
    #[serde(default)]
    pub year: Option<i32>,
    /// optional category filter: movie|series|book|music|manga
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WatchLookupItem {
    pub token: String,
    pub source: String,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub category: &'static str,
}

/// Where-can-I-watch-this: asks every healthy source — including
/// resolve-only ones that never appear in search — for a title match and
/// returns minted play tokens. Feed a token into `POST /api/playback/resolve`
/// as usual to actually play.
#[utoipa::path(post, path = "/api/playback/lookup", tag = "playback",
    security(("bearer" = [])),
    request_body = WatchLookupRequest,
    responses((status = 200), (status = 503))
)]
pub async fn watch_lookup(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    ConnectInfo(_peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<WatchLookupRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let started = std::time::Instant::now();
    let hits = aggregator::lookup_title(
        &state,
        req.title.trim(),
        req.year,
        req.category.as_deref(),
        true,
    )
    .await;
    metrics::histogram!("watch_lookup_duration_seconds").record(started.elapsed().as_secs_f64());
    metrics::counter!("watch_lookups_total").increment(1);

    let items: Vec<WatchLookupItem> = hits
        .into_iter()
        .map(|h| WatchLookupItem {
            token: h.token,
            source: h.source,
            title: h.title,
            year: h.year,
            poster: h.poster,
            category: h.category,
        })
        .collect();
    Ok(Json(serde_json::json!({
        "title": req.title.trim(),
        "count": items.len(),
        "elapsed_ms": started.elapsed().as_millis(),
        "items": items,
        "next": "POST /api/playback/resolve {token} to play",
    })))
}

// ---------------- first-class resolver (POST /api/resolve) ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ResolveV2Request {
    /// opaque id from /api/search/all (`items[].id`)
    pub token: String,
    /// optional local-library episode hint for future hybrid resolution
    #[serde(default)]
    #[allow(dead_code)]
    pub episode_id: Option<String>,
    #[serde(default)]
    pub title_hint: Option<String>,
    #[serde(default)]
    pub preferences: aggregator::ResolvePreferences,
    /// accepts a risk-layer challenge (logged when used)
    #[serde(default)]
    pub acknowledge_challenge: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct FallbackDto {
    pub source: String,
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ResolveResponse {
    pub resolution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// mp4 | hls | audio | page | file
    pub kind: String,
    pub url: String,
    pub title: String,
    pub quality: String,
    pub audio: String,
    pub subtitles: Vec<String>,
    /// unix seconds — authorization and URL validity window
    pub expires_at_unix: i64,
    /// signed short-lived playback authorization (verify with the security controller)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<FallbackDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<serde_json::Value>,
    /// Forensic watermark code bound to user+session+hour (leak tracing).
    pub watermark: String,
}

/// Central resolution pipeline: identify → healthy sources → candidates →
/// rank(preferences) → select → failover list → short-lived authorization.
///
/// When the admin-enabled risk layer flags the session, resolution is
/// challenged (403 `challenge_required`) or blocked outright.
#[utoipa::path(post, path = "/api/resolve", tag = "playback",
    security(("bearer" = [])),
    request_body = ResolveV2Request,
    responses((status = 200, body = ResolveResponse), (status = 403), (status = 503))
)]
pub async fn resolve_v2(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    auth: CurrentUser,
    ConnectInfo(_peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<ResolveV2Request>,
) -> ApiResult<Json<ResolveResponse>> {
    let include_web = true;
    // Client-context fingerprint (edge JA4 preferred) — bound into the grant
    // so stolen authorizations can't be replayed from a different stack.
    let fingerprint = crate::security::presented_fingerprint(&headers, None);
    let device_hash = fingerprint
        .as_deref()
        .map(|fp| format!("ja4:{}", crate::security::hash_fingerprint(fp)))
        .unwrap_or_default();

    // ---- access-card gate: nothing plays without one (admins included) ----
    crate::cards::ensure_card_access(&state.pool, auth.id()).await?;

    // ---- security layer (no-op while disabled by policy) ----
    let assessment = crate::security::risk::assess(&state.pool, auth.id()).await?;
    match assessment.decision {
        crate::security::risk::Decision::Revoke => {
            crate::security::risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_revoke",
                "critical",
                assessment.score,
                crate::security::risk::Decision::Revoke,
                serde_json::json!({ "endpoint": "/api/resolve", "token": req.token }),
            )
            .await;
            metrics::counter!("risk_decisions_total", "decision" => "revoke").increment(1);
            return Err(ApiError::forbidden("playback blocked by risk policy"));
        }
        crate::security::risk::Decision::Challenge if !req.acknowledge_challenge => {
            crate::security::risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_challenge",
                "warning",
                assessment.score,
                crate::security::risk::Decision::Challenge,
                serde_json::json!({ "endpoint": "/api/resolve", "token": req.token }),
            )
            .await;
            metrics::counter!("risk_decisions_total", "decision" => "challenge").increment(1);
            return Err(ApiError::forbidden(
                "challenge_required: re-send with acknowledge_challenge=true",
            ));
        }
        _ => {}
    }

    // ---- central resolver ----
    let set = aggregator::resolve_with_preferences(
        &state,
        req.token.trim(),
        req.title_hint.as_deref(),
        &req.preferences,
        include_web,
    )
    .await?;

    // ---- short-lived signed playback grant ----
    let ticket = crate::security::grants::issue(
        &state.pool,
        auth.id(),
        req.token.trim(),
        &set.quality,
        None,
        &device_hash,
        &set.source,
        state.cfg.playback_grant_ttl,
    )
    .await?;
    metrics::counter!("resolution_issued_total", "kind" => set.kind.clone()).increment(1);

    // Forensic watermark (Phase J): every resolved URL carries a per-session
    // mark so a leaked copy traces back to this user/session/hour.
    let wm_secret =
        crate::security::grants::signing_secret_for(&state.pool, "playback").await?;
    let watermark = crate::security::watermark::watermark_code(
        &wm_secret,
        &auth.id(),
        &ticket.grant_id.to_string(),
        chrono::Utc::now(),
    );
    let stamped_url = crate::security::watermark::stamp_url(&set.url, &watermark);

    crate::events::emit(
        &state.pool,
        &state.hub,
        "resolution.issued",
        serde_json::json!({
            "resolution_id": ticket.grant_id,
            "source": set.source.clone(),
            "kind": set.kind.clone(),
            "quality": set.quality.clone(),
        }),
        Some(auth.id()),
    )
    .await;

    Ok(Json(ResolveResponse {
        resolution_id: ticket.grant_id.to_string(),
        source: Some(set.source),
        kind: set.kind,
        url: stamped_url,
        title: set.title,
        quality: set.quality,
        audio: set.audio,
        subtitles: set.subtitles,
        expires_at_unix: ticket.expires_at_unix,
        authorization: Some(ticket.authorization),
        fallbacks: set
            .fallbacks
            .into_iter()
            .map(|f| FallbackDto {
                source: f.source,
                kind: f.kind,
                url: f.url,
            })
            .collect(),
        risk: if assessment.enabled {
            Some(serde_json::json!({
                "score": assessment.score,
                "decision": assessment.decision,
            }))
        } else {
            None
        },
        watermark,
    }))
}

// ---------------- manga via token ----------------

#[utoipa::path(post, path = "/api/playback/manga/chapters", tag = "playback",
    security(("bearer" = [])),
    request_body = ResolveRequest,
    responses((status = 200))
)]
pub async fn manga_chapters(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ResolveRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let (_, chapters) = aggregator::manga_chapters_for_token(&state, req.token.trim()).await?;
    Ok(Json(serde_json::json!({ "chapters": chapters })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MangaPagesRequest {
    pub token: String,
    pub chapter_id: String,
}

#[utoipa::path(post, path = "/api/playback/manga/pages", tag = "playback",
    security(("bearer" = [])),
    request_body = MangaPagesRequest,
    responses((status = 200), (status = 400))
)]
pub async fn manga_pages(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<MangaPagesRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let pages =
        aggregator::manga_pages_for_token(&state, req.token.trim(), &req.chapter_id).await?;
    Ok(Json(serde_json::json!({ "pages": pages })))
}

/// Convenience: full manga open in one call — detail + chapters + first pages.
#[utoipa::path(post, path = "/api/playback/manga/open", tag = "playback",
    security(("bearer" = [])),
    request_body = ResolveRequest,
    responses((status = 200))
)]
pub async fn manga_open(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ResolveRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let (_, chapters) = aggregator::manga_chapters_for_token(&state, req.token.trim()).await?;
    let first_pages = match chapters.first() {
        Some(first) => aggregator::manga_pages_for_token(&state, req.token.trim(), &first.id)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(Json(serde_json::json!({
        "chapters": chapters,
        "first_chapter_id": chapters.first().map(|c| c.id.clone()),
        "pages": first_pages,
    })))
}

// keep handler referenced even if unused elsewhere
#[allow(dead_code)]
fn _unused_status(_: StatusCode) {}


#[derive(Debug, Deserialize, IntoParams, utoipa::ToSchema)]
pub struct EpisodesQuery {
    /// Play token of the series, from any search result.
    pub token: String,
    /// Season to list. Defaults to the one already being watched, else the
    /// first real season.
    #[serde(default)]
    pub season: Option<i64>,
}

/// Seasons and episodes for a series found through search.
///
/// A source hit has no catalog row, so there was nothing to list and the play
/// page could only ever play the one thing it resolved. Each episode comes back
/// with its own token addressed to the same source.
#[utoipa::path(get, path = "/api/playback/episodes", tag = "playback",
    params(EpisodesQuery),
    security(("bearer" = [])),
    responses((status = 200), (status = 400))
)]
pub async fn source_episodes(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    axum::extract::Query(query): axum::extract::Query<EpisodesQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = auth;
    let api_key = state
        .cfg
        .tmdb_api_key
        .as_deref()
        .ok_or_else(|| ApiError::Unavailable("TMDB_API_KEY is not configured".into()))?;

    let (source, external_id) = aggregator::token_parts(&query.token)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ApiError::Unavailable(format!("http client: {e}")))?;

    let listing = crate::sources::episodes::listing(
        &client,
        api_key,
        &source,
        &external_id,
        query.season,
    )
    .await?;

    Ok(Json(serde_json::to_value(listing).unwrap_or_default()))
}
