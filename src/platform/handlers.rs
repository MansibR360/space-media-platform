use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::ApiResult;
use crate::platform::repo::{self, ApiKeyCreated, ApiKeyView, Device, Notification};
use crate::state::AppState;

// ---------------- API keys ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// comma-separated subset of: read, write, downloads
    #[serde(default = "default_scopes")]
    pub scopes: String,
}

fn default_scopes() -> String {
    "read".into()
}

#[utoipa::path(post, path = "/api/me/keys", tag = "platform",
    security(("bearer" = [])),
    request_body = CreateApiKeyRequest,
    responses((status = 201, body = ApiKeyCreated))
)]
pub async fn create_key(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreateApiKeyRequest>,
) -> ApiResult<(StatusCode, Json<ApiKeyCreated>)> {
    let key = repo::create_api_key(&state.pool, auth.id(), &req.name, &req.scopes).await?;
    Ok((StatusCode::CREATED, Json(key)))
}

#[utoipa::path(get, path = "/api/me/keys", tag = "platform",
    security(("bearer" = [])),
    responses((status = 200, body = [ApiKeyView]))
)]
pub async fn list_keys(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<ApiKeyView>>> {
    Ok(Json(repo::list_keys(&state.pool, auth.id()).await?))
}

#[utoipa::path(delete, path = "/api/me/keys/{key_id}", tag = "platform",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn revoke_key(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(key_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    repo::revoke_key(&state.pool, auth.id(), key_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- devices ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RegisterDeviceRequest {
    #[serde(default)]
    pub device_id: Option<Uuid>,
    #[serde(default = "default_device_name")]
    pub name: String,
    #[serde(default = "default_platform")]
    pub platform: String,
}

fn default_device_name() -> String {
    "Unnamed device".into()
}

fn default_platform() -> String {
    "web".into()
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DeviceIdPath {
    pub device_id: Uuid,
}

#[utoipa::path(post, path = "/api/me/devices", tag = "platform",
    security(("bearer" = [])),
    request_body = RegisterDeviceRequest,
    responses((status = 200, body = Device))
)]
pub async fn register_device(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<RegisterDeviceRequest>,
) -> ApiResult<Json<Device>> {
    let ip = if state.cfg.trust_proxy {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .unwrap_or("unknown")
            .to_string()
    } else {
        addr.ip().to_string()
    };
    let device = repo::register_device(
        &state.pool,
        auth.id(),
        req.device_id,
        &req.name,
        &req.platform,
        &ip,
    )
    .await?;
    Ok(Json(device))
}

#[utoipa::path(get, path = "/api/me/devices", tag = "platform",
    security(("bearer" = [])),
    responses((status = 200, body = [Device]))
)]
pub async fn list_devices(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(repo::list_devices(&state.pool, auth.id()).await?))
}

#[utoipa::path(delete, path = "/api/me/devices/{device_id}", tag = "platform",
    params(DeviceIdPath),
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn revoke_device(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<DeviceIdPath>,
) -> ApiResult<StatusCode> {
    repo::revoke_device(&state.pool, auth.id(), path.device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- search history ----------------

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct SearchHistoryRow {
    pub id: Uuid,
    pub query: String,
    pub result_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(get, path = "/api/me/search-history", tag = "platform",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn search_history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<SearchHistoryRow>>> {
    Ok(Json(
        sqlx::query_as::<_, SearchHistoryRow>(
            "SELECT id, query, result_count, created_at FROM search_history \
             WHERE user_id = $1 ORDER BY created_at DESC LIMIT 30",
        )
        .bind(auth.id())
        .fetch_all(&state.pool)
        .await?,
    ))
}

#[utoipa::path(delete, path = "/api/me/search-history", tag = "platform",
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn clear_search_history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM search_history WHERE user_id = $1")
        .bind(auth.id())
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------- behavior profile (Phase 46 slice) ----------------

#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct GenreCompletion {
    pub genre: String,
    pub completion_rate: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BehaviorProfile {
    pub genre_completion: Vec<GenreCompletion>,
    /// started-but-abandoned-early ratio across all watches
    pub skip_rate: f64,
    /// dropped / total tracked titles
    pub drop_rate: f64,
    pub binge_days_last_7: i64,
    pub avg_daily_minutes: f64,
    pub total_watch_hours: f64,
}

/// How this user actually consumes media — completion per genre, binge
/// cadence, session length. Feeds recommendation tuning clients.
#[utoipa::path(get, path = "/api/me/behavior", tag = "intelligence",
    security(("bearer" = [])),
    responses((status = 200, body = BehaviorProfile))
)]
pub async fn behavior(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<BehaviorProfile>> {
    let genre_completion = sqlx::query_as::<_, GenreCompletion>(
        "SELECT g.name AS genre,
                AVG(CASE WHEN wh.completed THEN 100.0 ELSE 0.0 END)::float8 AS completion_rate
         FROM watch_history wh
         JOIN media_genres mg ON mg.media_id = wh.media_id
         JOIN genres g ON g.id = mg.genre_id
         WHERE wh.user_id = $1
         GROUP BY g.name ORDER BY completion_rate DESC LIMIT 8",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;

    let daily: Vec<(chrono::DateTime<chrono::Utc>, f64)> = sqlx::query_as(
        "SELECT date_trunc('day', updated_at)::timestamptz AS day, \
                COALESCE(SUM(duration_seconds),0)/60.0 AS minutes
         FROM watch_history WHERE user_id = $1 AND updated_at > now() - INTERVAL '7 days'
         GROUP BY day ORDER BY day",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;

    let binge_days = daily.iter().filter(|(_, mins)| *mins >= 180.0).count() as i64;
    let avg_daily = if daily.is_empty() {
        0.0
    } else {
        daily.iter().map(|(_, m)| m).sum::<f64>() / daily.len() as f64
    };

    let total_hours: (f64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(duration_seconds),0)/3600.0 FROM watch_history WHERE user_id = $1",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await?;

    let skip_drop: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE completed = FALSE AND position_seconds < 120)::bigint,
                COUNT(*)::bigint FROM watch_history WHERE user_id = $1",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await?;
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE status = 'dropped')::bigint, COUNT(*)::bigint \
         FROM list_entries WHERE user_id = $1",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await?;
    let skip_rate = if counts.1 > 0 {
        skip_drop.0 as f64 / counts.1 as f64 * 100.0
    } else {
        0.0
    };
    let drop_rate = if counts.1 > 0 {
        counts.0 as f64 / counts.1 as f64 * 100.0
    } else {
        0.0
    };

    Ok(Json(BehaviorProfile {
        genre_completion,
        skip_rate: (skip_rate * 10.0).round() / 10.0,
        drop_rate: (drop_rate * 10.0).round() / 10.0,
        binge_days_last_7: binge_days,
        avg_daily_minutes: (avg_daily * 10.0).round() / 10.0,
        total_watch_hours: (total_hours.0 * 10.0).round() / 10.0,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct NotificationsQuery {
    #[serde(default)]
    pub unread: bool,
    #[serde(default = "default_notif_limit")]
    pub limit: i64,
}

fn default_notif_limit() -> i64 {
    30
}

#[utoipa::path(get, path = "/api/me/notifications", tag = "platform", params(NotificationsQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [Notification]))
)]
pub async fn notifications(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxQuery(query): AxQuery<NotificationsQuery>,
) -> ApiResult<Json<Vec<Notification>>> {
    Ok(Json(
        repo::list_notifications(&state.pool, auth.id(), query.unread, query.limit).await?,
    ))
}

#[utoipa::path(post, path = "/api/me/notifications/{notification_id}/read", tag = "platform",
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn mark_read(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(notification_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    repo::mark_notification_read(&state.pool, auth.id(), notification_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Convenience for clients: unread badge count.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UnreadCount {
    pub unread: i64,
}

#[utoipa::path(get, path = "/api/me/notifications/unread-count", tag = "platform",
    security(("bearer" = [])),
    responses((status = 200, body = UnreadCount))
)]
pub async fn unread_count(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<UnreadCount>> {
    let items = repo::list_notifications(&state.pool, auth.id(), true, 100).await?;
    Ok(Json(UnreadCount {
        unread: items.len() as i64,
    }))
}

