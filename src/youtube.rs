//! YouTube playback history — deliberately a **separate trail**.
//!
//! YouTube-sourced tracks have no catalog row, so they can never join
//! `watch_history` / `unified_progress` without polluting the library rails
//! (continue-watching, popularity, taste signals). Instead this module keeps
//! an isolated table with its own API surface:
//!
//! - `PUT /api/youtube/history`            record/refresh playback
//! - `GET /api/youtube/history`            list (cursor-paginated)
//! - `DELETE /api/youtube/history/{video_id}`  remove one
//! - `DELETE /api/youtube/history`         clear all
//!
//! Nothing here writes to tracking tables and nothing reads from them.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::routing::{delete, put};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct YoutubeHistoryEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub video_id: String,
    pub title: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    pub duration_seconds: f64,
    pub position_seconds: f64,
    pub completed: bool,
    pub plays: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct YoutubeHistoryUpdate {
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    /// youtube url accepted too — normalized server-side
    #[serde(default)]
    pub video_url: Option<String>,
    #[serde(default)]
    pub duration_seconds: f64,
    #[serde(default)]
    pub position_seconds: f64,
    #[serde(default)]
    pub completed: bool,
    /// true when starting fresh playback (increments play count)
    #[serde(default)]
    pub started: bool,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HistoryQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub before: Option<chrono::DateTime<chrono::Utc>>,
    /// optional substring filter on title/channel
    #[serde(default)]
    pub q: Option<String>,
}

fn default_limit() -> i64 {
    50
}

/// Records or refreshes one YouTube playback entry. Isolated by design:
/// no watch_history row, no unified_progress write, no heuristic ingest.
#[utoipa::path(put, path = "/api/youtube/history", tag = "youtube",
    security(("bearer" = [])),
    request_body = YoutubeHistoryUpdate,
    responses((status = 200, body = YoutubeHistoryEntry), (status = 400))
)]
pub async fn record(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<YoutubeHistoryUpdate>,
) -> ApiResult<Json<YoutubeHistoryEntry>> {
    let video_id = normalize_video_id(&req.video_id, req.video_url.as_deref())?;
    if req.position_seconds < 0.0 || req.duration_seconds < 0.0 {
        return Err(ApiError::bad_request("seconds must be non-negative"));
    }
    let completed =
        req.completed || (req.duration_seconds > 60.0 && req.position_seconds >= req.duration_seconds * 0.92);

    let entry = sqlx::query_as::<_, YoutubeHistoryEntry>(
        "INSERT INTO youtube_history
            (id, user_id, video_id, title, channel, thumbnail,
             duration_seconds, position_seconds, completed, plays)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CASE WHEN $10 THEN 1 ELSE 0 END)
         ON CONFLICT (user_id, video_id) DO UPDATE SET
            title = CASE WHEN EXCLUDED.title <> '' THEN EXCLUDED.title ELSE youtube_history.title END,
            channel = CASE WHEN EXCLUDED.channel <> '' THEN EXCLUDED.channel ELSE youtube_history.channel END,
            thumbnail = COALESCE(EXCLUDED.thumbnail, youtube_history.thumbnail),
            duration_seconds = GREATEST(EXCLUDED.duration_seconds, youtube_history.duration_seconds),
            position_seconds = EXCLUDED.position_seconds,
            completed = youtube_history.completed OR EXCLUDED.completed,
            plays = youtube_history.plays + CASE WHEN $10 THEN 1 ELSE 0 END,
            updated_at = now()
         RETURNING *",
    )
    .bind(new_id())
    .bind(auth.id())
    .bind(&video_id)
    .bind(req.title.trim())
    .bind(req.channel.trim())
    .bind(req.thumbnail)
    .bind(req.duration_seconds)
    .bind(req.position_seconds)
    .bind(completed)
    .bind(req.started)
    .fetch_one(&state.pool)
    .await?;

    metrics::counter!("youtube_history_records_total").increment(1);
    Ok(Json(entry))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct YoutubeHistoryPage {
    pub items: Vec<YoutubeHistoryEntry>,
    pub next_before: Option<chrono::DateTime<chrono::Utc>>,
}

/// Lists the caller's YouTube-only history. Never mixed into `/api/history`.
#[utoipa::path(get, path = "/api/youtube/history", tag = "youtube",
    security(("bearer" = [])),
    params(HistoryQuery),
    responses((status = 200, body = YoutubeHistoryPage))
)]
pub async fn list(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxQuery(q): AxQuery<HistoryQuery>,
) -> ApiResult<Json<YoutubeHistoryPage>> {
    let limit = q.limit.clamp(1, 200);
    let filter = format!("%{}%", q.q.as_deref().unwrap_or("").trim().to_lowercase());

    let rows = sqlx::query_as::<_, YoutubeHistoryEntry>(
        "SELECT * FROM youtube_history
         WHERE user_id = $1
           AND (($2 IS NULL) OR updated_at < $2)
           AND ($3 = '%%' OR LOWER(title) LIKE $3 OR LOWER(channel) LIKE $3)
         ORDER BY updated_at DESC
         LIMIT $4",
    )
    .bind(auth.id())
    .bind(q.before)
    .bind(filter)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;

    let mut items = rows;
    let mut next_before = None;
    if items.len() as i64 > limit {
        next_before = items.pop().map(|e| e.updated_at);
    }
    Ok(Json(YoutubeHistoryPage { items, next_before }))
}

#[utoipa::path(delete, path = "/api/youtube/history/{video_id}", tag = "youtube",
    security(("bearer" = [])),
    params(("video_id" = String, description = "bare video id")),
    responses((status = 200), (status = 404))
)]
pub async fn remove_one(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(video_id): AxPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query("DELETE FROM youtube_history WHERE user_id = $1 AND video_id = $2")
        .bind(auth.id())
        .bind(video_id.trim())
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("youtube history entry"));
    }
    Ok(Json(json!({ "removed": video_id })))
}

#[utoipa::path(delete, path = "/api/youtube/history", tag = "youtube",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn clear_all(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query("DELETE FROM youtube_history WHERE user_id = $1")
        .bind(auth.id())
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "cleared": result.rows_affected() })))
}

/// Accepts bare ids and full urls; returns the bare 11-char id.
fn normalize_video_id(video_id: &str, video_url: Option<&str>) -> ApiResult<String> {
    for candidate in [Some(video_id), video_url].into_iter().flatten() {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        if candidate.len() == 11 && !candidate.contains(['/', '?', '&', '=', '.']) {
            return Ok(candidate.to_string());
        }
        for prefix in ["watch?v=", "&v=", "/shorts/", "/embed/", "/live/", "youtu.be/"] {
            if let Some(idx) = candidate.find(prefix) {
                let rest = &candidate[idx + prefix.len()..];
                let id: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .take(11)
                    .collect();
                if id.len() == 11 {
                    return Ok(id);
                }
            }
        }
    }
    Err(ApiError::bad_request("valid video_id or video_url required"))
}

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/youtube/history", put(record).get(list).delete(clear_all))
        .route("/api/youtube/history/{video_id}", delete(remove_one))
}
