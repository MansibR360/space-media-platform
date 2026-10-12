//! Moderation & safety (changeset 5): user reports, blocks/mutes with real
//! enforcement in feeds and DMs, and user presence.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ------------------------------------------------------------ reports

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReportRequest {
    /// media | metadata | comment | chat_message | party_user | user
    pub target_type: String,
    /// id of the offending thing (uuid or stable ref)
    pub target_ref: String,
    /// spam | abuse | offensive | wrong_metadata | other
    pub reason: String,
    #[serde(default)]
    pub details: Option<String>,
}

/// Flags content or a user for moderator review.
#[utoipa::path(post, path = "/api/reports", tag = "moderation",
    security(("bearer" = [])),
    request_body = ReportRequest,
    responses((status = 201), (status = 400))
)]
pub async fn create_report(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ReportRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    const TYPES: &[&str] = &[
        "media",
        "metadata",
        "comment",
        "chat_message",
        "party_user",
        "user",
    ];
    if !TYPES.contains(&req.target_type.as_str()) {
        return Err(ApiError::bad_request(format!(
            "target_type must be one of: {}",
            TYPES.join(", ")
        )));
    }
    if req.reason.trim().is_empty() || req.target_ref.trim().is_empty() {
        return Err(ApiError::bad_request("target_ref and reason are required"));
    }
    // Simple flood guard: max 10 open reports per reporter.
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reports WHERE reporter = $1 AND status IN ('open','reviewing')",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    if open >= 10 {
        return Err(ApiError::bad_request(
            "too many open reports — wait for review",
        ));
    }

    let id = crate::utils::new_id();
    sqlx::query(
        "INSERT INTO reports (id, reporter, target_type, target_ref, reason, details)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(auth.id())
    .bind(&req.target_type)
    .bind(req.target_ref.trim())
    .bind(req.reason.trim())
    .bind(req.details.as_deref().unwrap_or(""))
    .execute(&state.pool)
    .await?;
    metrics::counter!("reports_created_total").increment(1);
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "report_id": id, "status": "open" })),
    ))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct StatusQuery {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ReportRow {
    pub id: Uuid,
    pub reporter: Uuid,
    pub target_type: String,
    pub target_ref: String,
    pub reason: String,
    pub details: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(get, path = "/api/admin/reports", tag = "moderation",
    security(("bearer" = [])),
    params(StatusQuery),
    responses((status = 200))
)]
pub async fn admin_reports(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<StatusQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let status = q.status.unwrap_or_else(|| "open".into());
    let rows = sqlx::query_as::<_, ReportRow>(
        "SELECT * FROM reports WHERE status = $1 ORDER BY created_at DESC LIMIT 200",
    )
    .bind(status)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "reports": rows })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReportStatusUpdate {
    /// reviewing | resolved | dismissed
    pub status: String,
}

#[utoipa::path(patch, path = "/api/admin/reports/{id}", tag = "moderation",
    security(("bearer" = [])),
    request_body = ReportStatusUpdate,
    responses((status = 200), (status = 400))
)]
pub async fn update_report_status(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    AxPath(id): AxPath<Uuid>,
    Json(req): Json<ReportStatusUpdate>,
) -> ApiResult<Json<serde_json::Value>> {
    if !matches!(req.status.as_str(), "reviewing" | "resolved" | "dismissed") {
        return Err(ApiError::bad_request("invalid status"));
    }
    let result = sqlx::query("UPDATE reports SET status = $2 WHERE id = $1")
        .bind(id)
        .bind(&req.status)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("report"));
    }
    crate::audit::log_simple(
        &state.pool,
        Some(auth.0.id()),
        "moderation.report_updated",
        &format!("{id}:{}", req.status),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DecisionNote {
    #[serde(default)]
    pub note: Option<String>,
}

/// Admin agrees with the report — marks it resolved (action taken).
#[utoipa::path(post, path = "/api/admin/reports/{id}/accept", tag = "moderation",
    security(("bearer" = [])),
    request_body = DecisionNote,
    responses((status = 200), (status = 404))
)]
pub async fn accept_report(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    decide_report(state, auth, id, "resolved", body).await
}

/// Admin rejects the report — marks it dismissed (no action needed).
#[utoipa::path(post, path = "/api/admin/reports/{id}/reject", tag = "moderation",
    security(("bearer" = [])),
    request_body = DecisionNote,
    responses((status = 200), (status = 404))
)]
pub async fn reject_report(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    decide_report(state, auth, id, "dismissed", body).await
}

async fn decide_report(
    state: Arc<AppState>,
    auth: AdminUser,
    id: Uuid,
    status: &str,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query("UPDATE reports SET status = $2 WHERE id = $1")
        .bind(id)
        .bind(status)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("report"));
    }
    let note = body
        .and_then(|Json(b)| b.note)
        .map(|n| n.chars().take(500).collect::<String>())
        .unwrap_or_default();
    crate::audit::log_simple(
        &state.pool,
        Some(auth.0.id()),
        if status == "resolved" {
            "moderation.report_accepted"
        } else {
            "moderation.report_rejected"
        },
        &format!("{id}:{status} {note}"),
    )
    .await;
    Ok(Json(json!({ "report_id": id, "status": status })))
}

// ------------------------------------------------------------ blocks & mutes

async fn ensure_not_self(a: Uuid, b: Uuid) -> ApiResult<()> {
    if a == b {
        return Err(ApiError::bad_request("cannot target yourself"));
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/users/{user_id}/block", tag = "moderation",
    security(("bearer" = [])),
    responses((status = 200), (status = 400))
)]
pub async fn block_user(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_not_self(auth.id(), user_id).await?;
    // Two statements — prepared protocol forbids batching with `;`.
    sqlx::query(
        "INSERT INTO user_blocks (blocker, blocked) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(auth.id())
    .bind(user_id)
    .execute(&state.pool)
    .await?;
    sqlx::query("DELETE FROM follows WHERE follower_id = $1 AND followed_id = $2")
        .bind(auth.id())
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    metrics::counter!("users_blocked_total").increment(1);
    Ok(Json(json!({ "blocked": true })))
}

#[utoipa::path(delete, path = "/api/users/{user_id}/block", tag = "moderation",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn unblock_user(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM user_blocks WHERE blocker = $1 AND blocked = $2")
        .bind(auth.id())
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "blocked": false })))
}

#[utoipa::path(get, path = "/api/me/blocks", tag = "moderation",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_blocks(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<Uuid> = sqlx::query_scalar(
        "SELECT blocked FROM user_blocks WHERE blocker = $1 ORDER BY created_at DESC",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "blocked": rows })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MuteRequest {
    /// hours; omit for indefinite
    #[serde(default)]
    pub hours: Option<i64>,
}

/// Mutes a user: their posts/messages disappear from YOUR feeds only.
#[utoipa::path(post, path = "/api/users/{user_id}/mute", tag = "moderation",
    security(("bearer" = [])),
    request_body = MuteRequest,
    responses((status = 200), (status = 400))
)]
pub async fn mute_user(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
    body: Option<Json<MuteRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_not_self(auth.id(), user_id).await?;
    let until = body
        .and_then(|b| b.hours)
        .map(|h| chrono::Utc::now() + chrono::Duration::hours(h.clamp(1, 24 * 365)));
    sqlx::query(
        "INSERT INTO user_mutes (muter, muted, until) VALUES ($1, $2, $3) \
         ON CONFLICT (muter, muted) DO UPDATE SET until = EXCLUDED.until",
    )
    .bind(auth.id())
    .bind(user_id)
    .bind(until)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({ "muted": true, "until": until })))
}

#[utoipa::path(delete, path = "/api/users/{user_id}/mute", tag = "moderation",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn unmute_user(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM user_mutes WHERE muter = $1 AND muted = $2")
        .bind(auth.id())
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "muted": false })))
}

// ------------------------------------------------------------ presence

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PresenceUpdate {
    /// online | idle | watch_party | dnd | invisible
    pub status: String,
    #[serde(default)]
    pub custom_text: Option<String>,
}

/// Updates your own presence; also broadcast live over /ws.
#[utoipa::path(put, path = "/api/me/presence", tag = "social",
    security(("bearer" = [])),
    request_body = PresenceUpdate,
    responses((status = 200), (status = 400))
)]
pub async fn update_my_presence(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<PresenceUpdate>,
) -> ApiResult<Json<serde_json::Value>> {
    const STATUSES: &[&str] = &["online", "idle", "watch_party", "dnd", "invisible"];
    if !STATUSES.contains(&req.status.as_str()) {
        return Err(ApiError::bad_request(format!(
            "status must be one of: {}",
            STATUSES.join(", ")
        )));
    }
    let text = req
        .custom_text
        .as_deref()
        .map(|t| t.chars().take(120).collect::<String>());
    sqlx::query(
        "INSERT INTO presence (user_id, status, custom_text) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id) DO UPDATE SET status = EXCLUDED.status, \
            custom_text = EXCLUDED.custom_text, updated_at = now()",
    )
    .bind(auth.id())
    .bind(&req.status)
    .bind(text.clone().unwrap_or_default())
    .execute(&state.pool)
    .await?;

    state.hub.publish_or_create(
        crate::chat::hub::GLOBAL_ROOM,
        json!({
            "type": "presence.update",
            "data": { "user_id": auth.id(), "status": req.status }
        })
        .to_string(),
    );
    Ok(Json(json!({ "status": req.status, "custom_text": text })))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PresenceRow {
    pub user_id: Uuid,
    pub status: String,
    pub custom_text: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[utoipa::path(get, path = "/api/presence", tag = "social",
    security(("bearer" = [])),
    params(IdListQuery),
    responses((status = 200))
)]
pub async fn batch_presence(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxQuery(q): AxQuery<IdListQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let ids: Vec<Uuid> = q
        .ids
        .split(',')
        .filter_map(|s| Uuid::parse_str(s.trim()).ok())
        .take(100)
        .collect();
    if ids.is_empty() {
        return Err(ApiError::bad_request(
            "ids required (comma-separated uuids)",
        ));
    }
    let rows = sqlx::query_as::<_, PresenceRow>(
        "SELECT p.user_id, p.status, p.custom_text, p.updated_at FROM presence p \
         JOIN users u ON u.id = p.user_id AND u.is_active \
         WHERE p.user_id = ANY($1) AND p.status <> 'invisible'",
    )
    .bind(ids)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "presence": rows })))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct IdListQuery {
    pub ids: String,
}

pub fn moderation_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/reports", post(create_report))
        .route("/api/admin/reports", get(admin_reports))
        .route(
            "/api/admin/reports/{id}",
            axum::routing::patch(update_report_status),
        )
        .route("/api/admin/reports/{id}/accept", post(accept_report))
        .route("/api/admin/reports/{id}/reject", post(reject_report))
        .route("/api/admin/audit-logs", get(admin_audit_logs))
        .route(
            "/api/users/{user_id}/block",
            post(block_user).delete(unblock_user),
        )
        .route(
            "/api/users/{user_id}/mute",
            post(mute_user).delete(unmute_user),
        )
        .route("/api/me/blocks", get(my_blocks))
        .route("/api/me/presence", axum::routing::put(update_my_presence))
        .route("/api/presence", get(batch_presence))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct AuditLogRow {
    pub id: Uuid,
    pub actor: Option<Uuid>,
    pub action: String,
    pub target: String,
    pub details: serde_json::Value,
    pub ip: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AuditQuery {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    150
}

/// Global security/admin audit trail.
#[utoipa::path(get, path = "/api/admin/audit-logs", tag = "admin",
    security(("bearer" = [])),
    params(AuditQuery),
    responses((status = 200))
)]
pub async fn admin_audit_logs(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = match &q.action {
        Some(action) => sqlx::query_as::<_, AuditLogRow>(
            "SELECT * FROM audit_logs WHERE action LIKE $2 ORDER BY created_at DESC LIMIT $1",
        )
        .bind(q.limit.clamp(1, 500))
        .bind(format!("%{}%", action)),
        None => sqlx::query_as::<_, AuditLogRow>(
            "SELECT * FROM audit_logs ORDER BY created_at DESC LIMIT $1",
        )
        .bind(q.limit.clamp(1, 500)),
    };
    let rows = rows.fetch_all(&state.pool).await?;
    Ok(Json(json!({ "logs": rows })))
}
