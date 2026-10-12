use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::pagination::Page;
use crate::state::AppState;
use crate::tracking::models::{
    ContinueWatchingItem, HistoryRow, HistorySettings, HistorySettingsUpdate, ListEntry,
    ListEntryWithTitle, ListStatus, ProgressUpdate, RatingRow, StatusUpdate, UserListItem,
    WatchEntry,
};

// ---------------- reading progress (books) ----------------

#[utoipa::path(put, path = "/api/tracking/reading", tag = "tracking",
    security(("bearer" = [])),
    request_body = crate::tracking::models::ReadingProgressUpdate,
    responses((status = 200, body = crate::tracking::models::ReadingProgress), (status = 400))
)]
pub async fn update_reading_progress(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<crate::tracking::models::ReadingProgressUpdate>,
) -> ApiResult<Json<crate::tracking::models::ReadingProgress>> {
    let progress = crate::tracking::repo::upsert_reading_progress(
        &state.pool,
        auth.id(),
        req.media_id,
        req.position_percent,
        req.page,
    )
    .await?;
    Ok(Json(progress))
}

#[utoipa::path(get, path = "/api/me/media/{media_id}/reading", tag = "tracking", params(MediaIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = crate::tracking::models::ReadingProgress), (status = 404))
)]
pub async fn get_reading_progress(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<Json<crate::tracking::models::ReadingProgress>> {
    Ok(Json(
        crate::tracking::repo::get_reading_progress(&state.pool, auth.id(), path.media_id).await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListIdPath {
    pub list_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListIdMediaPath {
    pub list_id: Uuid,
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct HistoryQuery {
    #[serde(flatten)]
    pub page: crate::pagination::PageQuery,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MyListQuery {
    pub status: Option<ListStatus>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateListRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

fn default_visibility() -> String {
    "private".into()
}

#[utoipa::path(put, path = "/api/tracking/progress", tag = "tracking",
    security(("bearer" = [])),
    request_body = ProgressUpdate,
    responses((status = 200))
)]
pub async fn update_progress(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ProgressUpdate>,
) -> ApiResult<Json<WatchEntry>> {
    if !crate::tracking::repo::history_recording_enabled(&state.pool, auth.id()).await? {
        return Ok(Json(crate::tracking::repo::ephemeral_progress(auth.id(), &req)));
    }
    let entry = crate::tracking::repo::upsert_progress(&state.pool, auth.id(), &req).await?;
    Ok(Json(entry))
}

#[utoipa::path(get, path = "/api/me/continue-watching", tag = "tracking",
    security(("bearer" = [])),
    responses((status = 200, body = [ContinueWatchingItem]))
)]
pub async fn continue_watching(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<ContinueWatchingItem>>> {
    let items = crate::tracking::repo::continue_watching(&state.pool, auth.id(), 20).await?;
    Ok(Json(items))
}

#[utoipa::path(get, path = "/api/me/history", tag = "tracking", params(HistoryQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [HistoryRow]))
)]
pub async fn history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Page<HistoryRow>>> {
    let limit = query.page.clamp_limit();
    let (cursor_ts, cursor_id) = query
        .page
        .cursor
        .as_deref()
        .and_then(|c| crate::pagination::decode_cursor(c).ok())
        .map(|v| {
            (
                v.get("t").and_then(|t| t.as_str()).and_then(|t| {
                    chrono::DateTime::parse_from_rfc3339(t)
                        .ok()
                        .map(|d| d.with_timezone(&chrono::Utc))
                }),
                v.get("i")
                    .and_then(|i| i.as_str())
                    .and_then(|i| Uuid::parse_str(i).ok()),
            )
        })
        .unwrap_or((None, None));

    let (rows, next) =
        crate::tracking::repo::history(&state.pool, auth.id(), limit, cursor_ts, cursor_id).await?;
    let next_cursor = next.map(|(ts, id)| {
        crate::pagination::encode_cursor(&serde_json::json!({
            "t": ts.to_rfc3339(),
            "i": id
        }))
    });
    Ok(Json(Page::with_cursor(rows, next_cursor)))
}

/// Current watch-history recording state for the caller.
#[utoipa::path(get, path = "/api/me/history/settings", tag = "tracking",
    security(("bearer" = [])),
    responses((status = 200, body = HistorySettings))
)]
pub async fn get_history_settings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<HistorySettings>> {
    let enabled = crate::tracking::repo::history_recording_enabled(&state.pool, auth.id()).await?;
    Ok(Json(HistorySettings { enabled }))
}

/// Turns watch-history recording on/off. While off, progress writes are
/// accepted but nothing is persisted; existing rows stay untouched.
#[utoipa::path(put, path = "/api/me/history/settings", tag = "tracking",
    security(("bearer" = [])),
    request_body = HistorySettingsUpdate,
    responses((status = 200, body = HistorySettings))
)]
pub async fn set_history_settings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<HistorySettingsUpdate>,
) -> ApiResult<Json<HistorySettings>> {
    crate::tracking::repo::set_history_recording(&state.pool, auth.id(), req.enabled).await?;
    crate::audit::log_simple(
        &state.pool,
        Some(auth.id()),
        "user.history_recording_changed",
        &format!("enabled={}", req.enabled),
    )
    .await;
    metrics::counter!("history_recording_toggles_total").increment(1);
    Ok(Json(HistorySettings { enabled: req.enabled }))
}

#[utoipa::path(put, path = "/api/me/media/{media_id}/status", tag = "tracking", params(MediaIdPath),
    security(("bearer" = [])),
    request_body = StatusUpdate,
    responses((status = 200, body = ListEntry), (status = 400))
)]
pub async fn set_status(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
    Json(req): Json<StatusUpdate>,
) -> ApiResult<Json<ListEntry>> {
    if let Some(score) = req.score {
        if !(0.0..=10.0).contains(&score) {
            return Err(ApiError::bad_request("score must be between 0 and 10"));
        }
    }
    let entry =
        crate::tracking::repo::set_status(&state.pool, auth.id(), path.media_id, &req).await?;
    if req.status == crate::tracking::models::ListStatus::Completed {
        let pool = state.pool.clone();
        let user_id = auth.id();
        let media_id = path.media_id;
        tokio::spawn(async move {
            crate::heuristic::ingest::record_progress(&pool, user_id, media_id, None, 100.0, 100.0, true).await;
        });
    }
    if req.status == crate::tracking::models::ListStatus::Dropped {
        let pool = state.pool.clone();
        let user_id = auth.id();
        let media_id = path.media_id;
        tokio::spawn(async move {
            crate::heuristic::ingest::record_drop(&pool, user_id, media_id).await;
        });
    }
    Ok(Json(entry))
}

#[utoipa::path(delete, path = "/api/me/media/{media_id}/status", tag = "tracking", params(MediaIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn remove_status(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<StatusCode> {
    crate::tracking::repo::remove_status(&state.pool, auth.id(), path.media_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/me/list", tag = "tracking", params(MyListQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_list(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(query): Query<MyListQuery>,
) -> ApiResult<Json<Vec<ListEntryWithTitle>>> {
    let entries = crate::tracking::repo::my_list(&state.pool, auth.id(), query.status).await?;
    Ok(Json(entries))
}

#[utoipa::path(put, path = "/api/media/{media_id}/rating", tag = "tracking", params(MediaIdPath),
    request_body = f64,
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn rate_media(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<Json<serde_json::Value>> {
    let score = match body {
        Some(Json(value)) => value
            .get("score")
            .and_then(|s| s.as_f64())
            .ok_or_else(|| ApiError::bad_request("body must be {\"score\": <number>}"))?,
        None => return Err(ApiError::bad_request("body required")),
    };
    crate::tracking::repo::rate_media(&state.pool, auth.id(), path.media_id, score).await?;
    {
        let pool = state.pool.clone();
        let user_id = auth.id();
        let media_id = path.media_id;
        tokio::spawn(async move {
            crate::heuristic::ingest::record_rating(&pool, user_id, media_id, score).await;
        });
    }
    Ok(Json(
        serde_json::json!({ "media_id": path.media_id, "score": score }),
    ))
}

#[utoipa::path(delete, path = "/api/media/{media_id}/rating", tag = "tracking", params(MediaIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn delete_rating(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<StatusCode> {
    crate::tracking::repo::delete_rating(&state.pool, auth.id(), path.media_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/me/ratings", tag = "tracking",
    security(("bearer" = [])),
    responses((status = 200, body = [RatingRow]))
)]
pub async fn my_ratings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<RatingRow>>> {
    Ok(Json(
        crate::tracking::repo::my_ratings(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(post, path = "/api/media/{media_id}/favorite", tag = "tracking", params(MediaIdPath),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn toggle_favorite(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let favorite =
        crate::tracking::repo::toggle_favorite(&state.pool, auth.id(), path.media_id).await?;
    if favorite {
        let pool = state.pool.clone();
        let user_id = auth.id();
        let media_id = path.media_id;
        tokio::spawn(async move {
            crate::heuristic::ingest::record_favorite(&pool, user_id, media_id, true).await;
        });
    }
    Ok(Json(serde_json::json!({ "favorite": favorite })))
}

#[utoipa::path(get, path = "/api/me/favorites", tag = "tracking",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn favorites(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<(Uuid, String)>>> {
    Ok(Json(
        crate::tracking::repo::favorites(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(post, path = "/api/me/lists", tag = "tracking",
    security(("bearer" = [])),
    request_body = CreateListRequest,
    responses((status = 201, body = UserListItem))
)]
pub async fn create_list(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreateListRequest>,
) -> ApiResult<(StatusCode, Json<UserListItem>)> {
    if !matches!(req.visibility.as_str(), "private" | "public" | "followers") {
        return Err(ApiError::bad_request(
            "visibility must be private, public or followers",
        ));
    }
    let list = crate::tracking::repo::create_list(
        &state.pool,
        auth.id(),
        &req.name,
        &req.description,
        &req.visibility,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(list)))
}

#[utoipa::path(get, path = "/api/me/lists", tag = "tracking",
    security(("bearer" = [])),
    responses((status = 200, body = [UserListItem]))
)]
pub async fn my_lists(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<UserListItem>>> {
    Ok(Json(
        crate::tracking::repo::my_lists(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(get, path = "/api/lists/{list_id}/items", tag = "tracking", params(ListIdPath),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_items(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ListIdPath>,
) -> ApiResult<Json<Vec<(Uuid, String, i32)>>> {
    let _ = auth;
    Ok(Json(
        crate::tracking::repo::list_items(&state.pool, path.list_id).await?,
    ))
}

#[utoipa::path(post, path = "/api/lists/{list_id}/items/{media_id}", tag = "tracking",
    params(ListIdMediaPath),
    security(("bearer" = [])),
    responses((status = 201))
)]
pub async fn add_list_item(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ListIdMediaPath>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    crate::tracking::repo::add_to_list(&state.pool, auth.id(), path.list_id, path.media_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "added": true })),
    ))
}

#[utoipa::path(delete, path = "/api/lists/{list_id}/items/{media_id}", tag = "tracking",
    params(ListIdMediaPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn remove_list_item(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ListIdMediaPath>,
) -> ApiResult<StatusCode> {
    crate::tracking::repo::remove_from_list(&state.pool, auth.id(), path.list_id, path.media_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
