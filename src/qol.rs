//! Quality-of-life APIs (changeset 6): completed works, toplists with
//! plugin/provider fallbacks, random picks, upcoming airings, user summary,
//! data export, history wipe, card snapshot and server metadata.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::routing::{delete as ax_delete, get};
use chrono::Datelike;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ------------------------------------------------------------ completed works

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct CompletedQuery {
    /// movie | series | book | manga | music | podcast | audiobook
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    30
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct MediaCard {
    pub id: Uuid,
    pub title: String,
    pub media_type: String,
    pub poster_url: Option<String>,
    pub release_date: Option<chrono::NaiveDate>,
    pub rating_avg: f64,
    pub overview: String,
}

/// Completed (released/finished) works — the whole catalog minus ongoing noise.
#[utoipa::path(get, path = "/api/completed", tag = "discovery",
    params(CompletedQuery),
    responses((status = 200))
)]
pub async fn completed(
    State(state): State<Arc<AppState>>,
    AxQuery(q): AxQuery<CompletedQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = q.limit.clamp(1, 100);
    let rows = match q.r#type.as_deref() {
        Some(t) => {
            validate_type(t)?;
            sqlx::query_as::<_, MediaCard>(
                "SELECT id, title, media_type, poster_url, release_date, rating_avg, overview \
                 FROM media WHERE status IN ('released', 'finished') AND media_type = $1 \
                 ORDER BY rating_avg DESC, popularity DESC LIMIT $2",
            )
            .bind(t)
            .bind(limit)
            .fetch_all(&state.pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, MediaCard>(
                "SELECT id, title, media_type, poster_url, release_date, rating_avg, overview \
                 FROM media WHERE status IN ('released', 'finished') \
                 ORDER BY rating_avg DESC, popularity DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&state.pool)
            .await?
        }
    };
    Ok(Json(json!({ "items": rows })))
}

fn validate_type(t: &str) -> ApiResult<()> {
    const OK: &[&str] = &[
        "movie", "series", "book", "manga", "music", "podcast", "audiobook",
    ];
    if !OK.contains(&t) {
        return Err(ApiError::bad_request(format!(
            "type must be one of: {}",
            OK.join(", ")
        )));
    }
    Ok(())
}

// ------------------------------------------------------------ toplists

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ToplistQuery {
    /// movies | series | anime | books | all
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_kind() -> String {
    "all".into()
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TopEntry {
    /// local:<uuid> or tmdb:movie:<id> style handle
    pub source: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<f64>,
}

static TOPLIST_CACHE: std::sync::LazyLock<
    dashmap::DashMap<String, (std::time::Instant, Vec<TopEntry>)>,
> = std::sync::LazyLock::new(dashmap::DashMap::new);
const TOPLIST_TTL: std::time::Duration = std::time::Duration::from_secs(1800);

/// Curated toplists. Order of supply: local catalog popularity first, then
/// external providers (TMDB top-rated when TMDB_API_KEY is configured).
/// Everything cached for 30 minutes.
#[utoipa::path(get, path = "/api/toplists", tag = "discovery",
    params(ToplistQuery),
    responses((status = 200))
)]
pub async fn toplists(
    State(state): State<Arc<AppState>>,
    AxQuery(q): AxQuery<ToplistQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let kind = q.kind.to_lowercase();
    if !matches!(kind.as_str(), "movies" | "series" | "anime" | "books" | "all") {
        return Err(ApiError::bad_request("kind must be movies | series | anime | books | all"));
    }
    let cache_key = format!("{}:{}", kind, q.limit);
    if let Some(entry) = TOPLIST_CACHE.get(&cache_key) {
        let (at, items) = entry.value();
        if at.elapsed() < TOPLIST_TTL {
            return Ok(Json(json!({ "kind": kind, "items": items, "cached": true })));
        }
    }

    let mut out: Vec<TopEntry> = Vec::new();
    for part in split_kinds(&kind) {
        out.extend(local_toplist(&state, &part, q.limit).await?);
        out.extend(tmdb_toplist(&state, &part, q.limit).await);
    }

    // The partitions overlap — anime is a flag, not a media type, so an
    // anime series is legitimately in both the "series" and "anime" lists.
    // Without this the same title is returned twice for kind=all.
    let mut seen = std::collections::HashSet::new();
    out.retain(|entry| seen.insert(entry.source.clone()));

    out.truncate(q.limit.clamp(1, 60) as usize * split_kinds(&kind).len());
    TOPLIST_CACHE.insert(cache_key, (std::time::Instant::now(), out.clone()));
    Ok(Json(json!({ "kind": kind, "items": out })))
}

fn split_kinds(kind: &str) -> Vec<String> {
    if kind == "all" {
        vec![
            "movies".into(),
            "series".into(),
            "anime".into(),
            "books".into(),
        ]
    } else {
        vec![kind.to_string()]
    }
}

async fn local_toplist(
    state: &AppState,
    part: &str,
    limit: i64,
) -> ApiResult<Vec<TopEntry>> {
    let (types, anime_only): (Vec<&str>, bool) = match part {
        "movies" => (vec!["movie"], false),
        "series" => (vec!["series"], false),
        "anime" => (vec!["series", "movie"], true),
        "books" => (vec!["book"], false),
        _ => (vec![], false),
    };
    if types.is_empty() {
        return Ok(vec![]);
    }
    let sql = if anime_only {
        "SELECT id, title, poster_url, rating_avg, release_date FROM media \
         WHERE media_type = ANY($1) AND is_anime ORDER BY rating_avg DESC, popularity DESC LIMIT $2"
    } else {
        "SELECT id, title, poster_url, rating_avg, release_date FROM media \
         WHERE media_type = ANY($1) ORDER BY rating_avg DESC, popularity DESC LIMIT $2"
    };
    let rows: Vec<(Uuid, String, Option<String>, f64, Option<chrono::NaiveDate>)> =
        sqlx::query_as(sql)
            .bind(types)
            .bind(limit.clamp(1, 40))
            .fetch_all(&state.pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, title, poster, rating, date)| TopEntry {
            source: format!("local:{id}"),
            title,
            year: date.map(|d| d.year().to_string()),
            poster,
            rating: Some(rating),
        })
        .collect())
}

/// TMDB top-rated — best-effort; silently skipped without a key or on error.
async fn tmdb_toplist(state: &AppState, part: &str, limit: i64) -> Vec<TopEntry> {
    let Some(key) = state.cfg.tmdb_api_key.as_deref().filter(|k| !k.is_empty()) else {
        return vec![];
    };
    let path = match part {
        "movies" => "movie/top_rated",
        "series" => "tv/top_rated",
        _ => return vec![],
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let body: serde_json::Value = match client
        .get(format!("https://api.themoviedb.org/3/{path}"))
        .query(&[("api_key", key)])
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(b) => b,
            Err(_) => return vec![],
        },
        Err(_) => return vec![],
    };

    let mut out = Vec::new();
    if let Some(results) = body.get("results").and_then(|r| r.as_array()) {
        for r in results.iter().take(limit.clamp(1, 20) as usize) {
            let title = r
                .get("title")
                .or_else(|| r.get("name"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() {
                continue;
            }
            let tmdb_id = r.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
            let date = r
                .get("release_date")
                .or_else(|| r.get("first_air_date"))
                .and_then(|d| d.as_str())
                .map(|d| d.chars().take(4).collect::<String>());
            let poster = r
                .get("poster_path")
                .and_then(|p| p.as_str())
                .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"));
            out.push(TopEntry {
                source: format!("tmdb:{tmdb_id}"),
                title,
                year: date.filter(|y| !y.is_empty()),
                poster,
                rating: r.get("vote_average").and_then(|v| v.as_f64()),
            });
        }
    }
    metrics::counter!("toplists_tmdb_served_total").increment(out.len() as u64);
    out
}

// ------------------------------------------------------------ random pick

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct RandomQuery {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub min_rating: Option<f64>,
}

/// Surprise-me: one random catalog pick matching optional filters.
#[utoipa::path(get, path = "/api/random", tag = "discovery",
    params(RandomQuery),
    responses((status = 200), (status = 404))
)]
pub async fn random_pick(
    State(state): State<Arc<AppState>>,
    AxQuery(q): AxQuery<RandomQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(t) = q.r#type.as_deref() {
        validate_type(t)?;
    }
    let min_rating = q.min_rating.unwrap_or(0.0).clamp(0.0, 10.0);
    let row: Option<MediaCard> = match q.r#type.as_deref() {
        Some(t) => sqlx::query_as::<_, MediaCard>(
            "SELECT id, title, media_type, poster_url, release_date, rating_avg, overview \
             FROM media WHERE status <> 'announced' AND media_type = $1 AND rating_avg >= $2 \
             ORDER BY random() LIMIT 1",
        )
        .bind(t)
        .bind(min_rating)
        .fetch_optional(&state.pool)
        .await?,
        None => sqlx::query_as::<_, MediaCard>(
            "SELECT id, title, media_type, poster_url, release_date, rating_avg, overview \
             FROM media WHERE status <> 'announced' AND rating_avg >= $1 \
             ORDER BY random() LIMIT 1",
        )
        .bind(min_rating)
        .fetch_optional(&state.pool)
        .await?,
    };
    let pick = row.ok_or_else(|| ApiError::not_found("matching media"))?;
    Ok(Json(json!({ "pick": pick })))
}

// ------------------------------------------------------------ upcoming

/// Episodes airing within the next 14 days across followed/watched shows.
#[utoipa::path(get, path = "/api/upcoming", tag = "discovery",
    responses((status = 200))
)]
pub async fn upcoming(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    #[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
    struct UpcomingEpisode {
        episode_id: Uuid,
        media_id: Uuid,
        title: String,
        show_title: String,
        number: i32,
        air_date: Option<chrono::NaiveDate>,
        still_url: Option<String>,
    }
    let rows = sqlx::query_as::<_, UpcomingEpisode>(
        "SELECT e.id AS episode_id, m.id AS media_id, e.title, m.title AS show_title, \
                e.number, e.air_date, e.still_url \
         FROM episodes e \
         JOIN seasons s ON s.id = e.season_id \
         JOIN media m ON m.id = s.media_id \
         LEFT JOIN list_entries le ON le.media_id = m.id AND le.user_id = $1 \
         WHERE e.air_date BETWEEN current_date AND current_date + INTERVAL '14 days' \
           AND (le.id IS NOT NULL OR m.popularity > 50) \
         ORDER BY e.air_date ASC LIMIT 30",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "episodes": rows })))
}

// ------------------------------------------------------------ me/summary · export · history wipe

/// Cross-module dashboard snapshot for the signed-in user.
#[utoipa::path(get, path = "/api/me/summary", tag = "users",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn me_summary(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let uid = auth.id();
    let hours: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(duration_seconds), 0) / 3600.0 FROM watch_history \
         WHERE user_id = $1",
    )
    .bind(uid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0.0);
    let watching: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM list_entries WHERE user_id = $1 AND status = 'watching'",
    )
    .bind(uid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let completed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM list_entries WHERE user_id = $1 AND status = 'completed'",
    )
    .bind(uid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let reading: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reading_progress WHERE user_id = $1 AND NOT completed",
    )
    .bind(uid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(uid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let card: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
        "SELECT expires_at FROM access_cards \
         WHERE owner = $1 AND status = 'active' AND expires_at > now() \
         ORDER BY expires_at DESC LIMIT 1",
    )
    .bind(uid)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);

    Ok(Json(json!({
        "watch_hours": (hours * 10.0).round() / 10.0,
        "watching": watching,
        "completed": completed,
        "reading_in_progress": reading,
        "unread_notifications": unread,
        "card": card.map(|(exp,)| json!({
            "active": true,
            "expires_at_unix": exp.timestamp(),
            "days_left": (exp - chrono::Utc::now()).num_days(),
        })).unwrap_or(json!({ "active": false })),
    })))
}

/// Full personal-data export (portability / GDPR-style QoL).
#[utoipa::path(get, path = "/api/me/export", tag = "users",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn me_export(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let uid = auth.id();
    let watch: Vec<serde_json::Value> = sqlx::query_as::<
        _,
        (Uuid, Option<Uuid>, f64, f64, bool, chrono::DateTime<chrono::Utc>),
    >(
        "SELECT media_id, episode_id, position_seconds, duration_seconds, completed, updated_at \
         FROM watch_history WHERE user_id = $1 ORDER BY updated_at DESC LIMIT 5000",
    )
    .bind(uid)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(m, e, p, d, c, u)| {
        json!({ "media_id": m, "episode_id": e, "position": p, "duration": d, "completed": c, "updated_at": u })
    })
    .collect();
    let lists: Vec<serde_json::Value> = sqlx::query_as::<
        _,
        (Uuid, String, Option<f64>, i32),
    >(
        "SELECT media_id, status, score, episodes_watched FROM list_entries \
         WHERE user_id = $1 ORDER BY updated_at DESC LIMIT 5000",
    )
    .bind(uid)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(m, s, sc, ep)| json!({ "media_id": m, "status": s, "score": sc, "episodes_watched": ep }))
    .collect();
    let reading: Vec<serde_json::Value> = sqlx::query_as::<
        _,
        (Uuid, f64, Option<i32>, bool),
    >(
        "SELECT media_id, position_percent, page, completed FROM reading_progress \
         WHERE user_id = $1 LIMIT 5000",
    )
    .bind(uid)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(m, pct, pg, c)| json!({ "media_id": m, "percent": pct, "page": pg, "completed": c }))
    .collect();

    Ok(Json(json!({
        "exported_at": chrono::Utc::now(),
        "user_id": uid,
        "watch_history": watch,
        "lists": lists,
        "reading_progress": reading,
    })))
}

/// Wipes the caller's watch history (positions reset everywhere).
#[utoipa::path(delete, path = "/api/me/history", tag = "users",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn wipe_history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let result =
        sqlx::query("DELETE FROM watch_history WHERE user_id = $1")
            .bind(auth.id())
            .execute(&state.pool)
            .await?;
    crate::audit::log_simple(
        &state.pool,
        Some(auth.id()),
        "user.history_wiped",
        &format!("rows={}", result.rows_affected()),
    )
    .await;
    Ok(Json(json!({ "deleted_rows": result.rows_affected() })))
}

// ------------------------------------------------------------ card snapshot

/// Tiny QoL wrapper: is my access card active and until when?
#[utoipa::path(get, path = "/api/me/card", tag = "billing",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_card_status(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let row: Option<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT card_number, expires_at FROM access_cards \
         WHERE owner = $1 AND status = 'active' AND expires_at > now() \
         ORDER BY expires_at DESC LIMIT 1",
    )
    .bind(auth.id())
    .fetch_optional(&state.pool)
    .await?;
    Ok(Json(match row {
        Some((number, expires)) => json!({
            "active": true,
            "card_number_masked": mask_card(&number),
            "expires_at_unix": expires.timestamp(),
            "days_left": (expires - chrono::Utc::now()).num_days(),
        }),
        None => json!({ "active": false }),
    }))
}

fn mask_card(number: &str) -> String {
    let parts: Vec<&str> = number.split('-').collect();
    if parts.len() == 3 {
        format!("SPC-{}-{}", parts[1], "••••••••")
    } else {
        "SPC-••••".into()
    }
}

// ------------------------------------------------------------ server meta

/// Machine-readable capability sheet for clients.
#[utoipa::path(get, path = "/api/meta", tag = "system",
    responses((status = 200))
)]
pub async fn server_meta(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Reported rather than hardcoded: the Python plugins under plugins/python
    // are standalone HTTP services, not something this binary loads. The
    // capability is "some HTTP source plugin is registered", which was
    // previously asserted as `true` even with none registered at all.
    let http_source_plugins = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM plugins WHERE kind = 'source' AND transport = 'http' AND enabled",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    Json(json!({
        "name": "space",
        "version": env!("CARGO_PKG_VERSION"),
        "features": {
            "unified_resolve": true,
            "access_cards": true,
            "offline_licenses": true,
            "totp_admin": true,
            "hls_encryption": state.cfg.hls_encryption,
            "media_graph": true,
            "ai_copilot": true,
            "http_source_plugins": http_source_plugins,
        },
        "docs": "/docs",
        "spec": "/api-docs/openapi.json",
        "socket": "/ws",
    }))
}

// ------------------------------------------------------------ availability

/// Where can I watch/read this? Matches the catalog title across every
/// healthy external source and counts local files. Returned tokens plug
/// straight into /api/resolve.
#[utoipa::path(post, path = "/api/media/{id}/availability", tag = "discovery",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn availability(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let row: Option<(String, Option<i32>, String)> = sqlx::query_as(
        "SELECT title, release_date, media_type FROM media WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((title, year, media_type)) = row else {
        return Err(ApiError::not_found("media"));
    };

    let local_files: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_files WHERE media_id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let results =
        crate::sources::aggregator::search_everything(&state, &title, 12, None, false).await;
    let key = crate::sources::aggregator::normalize_title(&title);
    let matches: Vec<serde_json::Value> = results
        .iter()
        .filter(|r| {
            crate::sources::aggregator::normalize_title(&r.title) == key
                && r.year.map_or(true, |y| {
                    year.map_or(true, |yy| y == yy)
                })
                && r.category != "book"
                    || media_type == "book" && r.category == "book"
        })
        .map(|r| {
            let mut sources = Vec::with_capacity(1 + r.sources.len());
            sources.push(r.source.clone());
            sources.extend(r.sources.iter().cloned());
            json!({
                "token": r.token,
                "title": r.title,
                "type": r.category,
                "sources": sources,
            })
        })
        .collect();

    Ok(Json(json!({
        "media_id": id,
        "title": title,
        "local_files": local_files,
        "external": matches,
    })))
}

pub fn qol_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/completed", get(completed))
        .route("/api/toplists", get(toplists))
        .route("/api/random", get(random_pick))
        .route("/api/upcoming", get(upcoming))
        .route("/api/meta", get(server_meta))
        .route(
            "/api/media/{id}/availability",
            axum::routing::post(availability),
        )
        .route("/api/me/summary", get(me_summary))
        .route("/api/me/export", get(me_export))
        .route("/api/me/card", get(my_card_status))
        .route("/api/me/history", ax_delete(wipe_history))
}


