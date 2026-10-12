//! Universal Progress Engine (Phase G).
//!
//! One write endpoint and one continue-rail for every media type:
//!   video/music/podcast/audiobook → seconds (stored in `watch_history`)
//!   book/manga                    → page + percent (stored in `reading_progress`)
//!
//! Storage stays specialized; the engine normalizes reads through the
//! `unified_progress` view so "Continue" spans the whole platform.

use axum::Json;
use axum::extract::{Query as AxQuery, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::tracking::models::ProgressUpdate;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UniversalProgressUpdate {
    pub media_id: Uuid,
    #[serde(default)]
    pub episode_id: Option<Uuid>,
    /// seconds form
    #[serde(default)]
    pub position_seconds: Option<f64>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// page/percent form
    #[serde(default)]
    pub position_percent: Option<f64>,
    #[serde(default)]
    pub page: Option<i32>,
    /// optional explicit override — inferred from media_type otherwise
    #[serde(default)]
    pub position_kind: Option<String>,
    #[serde(default)]
    pub completed: bool,
}

/// Single write endpoint for progress on any media type.
#[utoipa::path(put, path = "/api/progress", tag = "tracking",
    security(("bearer" = [])),
    request_body = UniversalProgressUpdate,
    responses((status = 200), (status = 400), (status = 404))
)]
pub async fn update_progress(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<UniversalProgressUpdate>,
) -> ApiResult<Json<serde_json::Value>> {
    let media_type: (String,) = sqlx::query_as("SELECT media_type FROM media WHERE id = $1")
        .bind(req.media_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("media"))?;
    let media_type = media_type.0;

    let kind = req.position_kind.clone().unwrap_or_else(|| match media_type.as_str() {
        "book" | "manga" => "page".to_string(),
        _ => "seconds".to_string(),
    });

    match (kind.as_str(), media_type.as_str()) {
        ("seconds", "book" | "manga") | ("page", "movie" | "series" | "music" | "podcast" | "audiobook") => {
            return Err(ApiError::bad_request(format!(
                "position kind '{kind}' does not fit media_type '{}'",
                media_type
            )));
        }
        ("page", _) => {
            let percent = req.position_percent.unwrap_or(0.0);
            if percent == 0.0 && req.page.is_none() {
                return Err(ApiError::bad_request(
                    "position_percent or page is required for page-kind progress",
                ));
            }
            let progress = crate::tracking::repo::upsert_reading_progress(
                &state.pool,
                auth.id(),
                req.media_id,
                percent.clamp(0.0, 100.0),
                req.page,
            )
            .await?;
            crate::events::emit(
                &state.pool,
                &state.hub,
                "progress.updated",
                serde_json::json!({
                    "media_id": req.media_id,
                    "kind": "page",
                    "percent": progress.position_percent,
                    "completed": progress.completed,
                }),
                Some(auth.id()),
            )
            .await;
            feed_heuristic(
                &state,
                auth.id(),
                req.media_id,
                None,
                progress.position_percent,
                100.0,
                progress.completed,
            );
            Ok(Json(serde_json::json!({ "position_kind": "page", "progress": progress })))
        }
        ("seconds", _) => {
            if !crate::tracking::repo::history_recording_enabled(&state.pool, auth.id()).await? {
                let echo = ProgressUpdate {
                    media_id: req.media_id,
                    episode_id: req.episode_id,
                    position_seconds: req.position_seconds.unwrap_or(0.0),
                    duration_seconds: req.duration_seconds.unwrap_or(0.0),
                    completed: req.completed,
                    session_id: None,
                };
                return Ok(Json(serde_json::json!({
                    "position_kind": "seconds",
                    "recording": false,
                    "progress": crate::tracking::repo::ephemeral_progress(auth.id(), &echo),
                })));
            }
            let update = ProgressUpdate {
                media_id: req.media_id,
                episode_id: req.episode_id,
                position_seconds: req.position_seconds.unwrap_or(0.0),
                duration_seconds: req.duration_seconds.unwrap_or(0.0),
                completed: req.completed,
                session_id: None,
            };
            let entry = crate::tracking::repo::upsert_progress(&state.pool, auth.id(), &update)
                .await?;
            feed_heuristic(
                &state,
                auth.id(),
                req.media_id,
                req.episode_id,
                entry.position_seconds,
                entry.duration_seconds,
                entry.completed,
            );
            crate::events::emit(
                &state.pool,
                &state.hub,
                "progress.updated",
                serde_json::json!({
                    "media_id": req.media_id,
                    "episode_id": req.episode_id,
                    "kind": "seconds",
                    "position": entry.position_seconds,
                    "completed": entry.completed,
                }),
                Some(auth.id()),
            )
            .await;
            Ok(Json(serde_json::json!({ "position_kind": "seconds", "progress": entry })))
        }
        (_, other) => Err(ApiError::bad_request(format!(
            "unknown position kind '{other}'"
        ))),
    }
}

/// Best-effort feed into the heuristic engine; never blocks or fails the
/// tracking write (runs detached, logs at debug on error).
fn feed_heuristic(
    state: &Arc<AppState>,
    user_id: Uuid,
    media_id: Uuid,
    episode_id: Option<Uuid>,
    position_seconds: f64,
    duration_seconds: f64,
    completed: bool,
) {
    let pool = state.pool.clone();
    tokio::spawn(async move {
        crate::heuristic::ingest::record_progress(
            &pool,
            user_id,
            media_id,
            episode_id,
            position_seconds,
            duration_seconds,
            completed,
        )
        .await;
    });
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct UnifiedProgressItem {
    pub media_id: Uuid,
    pub media_type: String,
    pub title: String,
    pub poster_url: Option<String>,
    pub position_kind: String,
    pub episode_id: Option<Uuid>,
    pub position: f64,
    pub total: Option<f64>,
    pub page: Option<i32>,
    pub percent: f64,
    pub completed: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ContinueQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// comma-separated media_type filter, e.g. "movie,manga"
    #[serde(default)]
    pub types: Option<String>,
}

fn default_limit() -> i64 {
    20
}

/// The one rail: Continue Watching / Reading / Listening across every type.
#[utoipa::path(get, path = "/api/continue", tag = "tracking",
    security(("bearer" = [])),
    params(ContinueQuery),
    responses((status = 200))
)]
pub async fn continue_all(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxQuery(q): AxQuery<ContinueQuery>,
) -> ApiResult<Json<Vec<UnifiedProgressItem>>> {
    let types: Vec<String> = q
        .types
        .map(|t| {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let types_ref: Vec<&str> = types.iter().map(String::as_str).collect();

    let rows = if types.is_empty() {
        sqlx::query_as::<_, UnifiedProgressItem>(
            "SELECT media_id, media_type, title, poster_url, position_kind, episode_id,
                    position, total, page, percent, completed, updated_at
             FROM unified_progress
             WHERE user_id = $1 AND completed = FALSE AND percent >= 2 AND percent < 96
             ORDER BY updated_at DESC LIMIT $2",
        )
        .bind(auth.id())
        .bind(q.limit.clamp(1, 60))
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, UnifiedProgressItem>(
            "SELECT media_id, media_type, title, poster_url, position_kind, episode_id,
                    position, total, page, percent, completed, updated_at
             FROM unified_progress
             WHERE user_id = $1 AND completed = FALSE AND percent >= 2 AND percent < 96
               AND media_type = ANY($2)
             ORDER BY updated_at DESC LIMIT $3",
        )
        .bind(auth.id())
        .bind(&types_ref)
        .bind(q.limit.clamp(1, 60))
        .fetch_all(&state.pool)
        .await?
    };

    metrics::counter!("continue_rail_served_total").increment(1);
    Ok(Json(rows))
}
