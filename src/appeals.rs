//! Appeals (changeset 27): users contest moderation decisions, admins review
//! and accept or reject each appeal with an optional note.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::moderation::DecisionNote;
use crate::state::AppState;

// ------------------------------------------------------------ user surface

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AppealRequest {
    /// report | ban | suspension | content_removal | other
    pub target_type: String,
    /// id of the decision/thing being appealed
    pub target_ref: String,
    #[serde(default)]
    pub reason: String,
}

/// Files an appeal against a moderation decision.
#[utoipa::path(post, path = "/api/appeals", tag = "moderation",
    security(("bearer" = [])),
    request_body = AppealRequest,
    responses((status = 201), (status = 400))
)]
pub async fn create_appeal(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<AppealRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    const TYPES: &[&str] = &["report", "ban", "suspension", "content_removal", "other"];
    if !TYPES.contains(&req.target_type.as_str()) {
        return Err(ApiError::bad_request(format!(
            "target_type must be one of: {}",
            TYPES.join(", ")
        )));
    }
    if req.reason.trim().is_empty() || req.target_ref.trim().is_empty() {
        return Err(ApiError::bad_request("target_ref and reason are required"));
    }
    // Flood guard: max 5 pending appeals per user.
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appeals WHERE appellant = $1 AND status = 'pending'",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    if open >= 5 {
        return Err(ApiError::bad_request(
            "too many pending appeals — wait for review",
        ));
    }

    let id = crate::utils::new_id();
    sqlx::query(
        "INSERT INTO appeals (id, appellant, target_type, target_ref, reason)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(auth.id())
    .bind(req.target_type.trim())
    .bind(req.target_ref.trim())
    .bind(req.reason.trim().chars().take(2000).collect::<String>())
    .execute(&state.pool)
    .await?;
    metrics::counter!("appeals_created_total").increment(1);
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "appeal_id": id, "status": "pending" })),
    ))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct AppealRow {
    pub id: Uuid,
    pub appellant: Uuid,
    pub target_type: String,
    pub target_ref: String,
    pub reason: String,
    pub status: String,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decision_note: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Lists your own appeals and their outcomes.
#[utoipa::path(get, path = "/api/me/appeals", tag = "moderation",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_appeals(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query_as::<_, AppealRow>(
        "SELECT * FROM appeals WHERE appellant = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "appeals": rows })))
}

// ------------------------------------------------------------ admin surface

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AppealStatusQuery {
    #[serde(default)]
    pub status: Option<String>,
}

/// Admin queue of appeals, newest first.
#[utoipa::path(get, path = "/api/admin/appeals", tag = "moderation",
    security(("bearer" = [])),
    params(AppealStatusQuery),
    responses((status = 200))
)]
pub async fn admin_appeals(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<AppealStatusQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let status = q.status.unwrap_or_else(|| "pending".into());
    let rows = sqlx::query_as::<_, AppealRow>(
        "SELECT * FROM appeals WHERE status = $1 ORDER BY created_at DESC LIMIT 200",
    )
    .bind(status)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(json!({ "appeals": rows })))
}

/// Accepts an appeal — the contested decision is overturned.
#[utoipa::path(post, path = "/api/admin/appeals/{id}/accept", tag = "moderation",
    security(("bearer" = [])),
    request_body = DecisionNote,
    responses((status = 200), (status = 404))
)]
pub async fn accept_appeal(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    decide_appeal(state, admin, id, "accepted", body).await
}

/// Rejects an appeal — the original decision stands.
#[utoipa::path(post, path = "/api/admin/appeals/{id}/reject", tag = "moderation",
    security(("bearer" = [])),
    request_body = DecisionNote,
    responses((status = 200), (status = 404))
)]
pub async fn reject_appeal(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    decide_appeal(state, admin, id, "rejected", body).await
}

async fn decide_appeal(
    state: Arc<AppState>,
    admin: AdminUser,
    id: Uuid,
    status: &str,
    body: Option<Json<DecisionNote>>,
) -> ApiResult<Json<serde_json::Value>> {
    let note = body
        .and_then(|Json(b)| b.note)
        .map(|n| n.chars().take(500).collect::<String>())
        .unwrap_or_default();
    let result = sqlx::query(
        "UPDATE appeals SET status = $2, decided_by = $3, decided_at = now(), \
         decision_note = $4 WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(status)
    .bind(admin.0.id())
    .bind(&note)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("pending appeal"));
    }
    crate::audit::log_simple(
        &state.pool,
        Some(admin.0.id()),
        if status == "accepted" {
            "moderation.appeal_accepted"
        } else {
            "moderation.appeal_rejected"
        },
        &format!("{id}:{status} {note}"),
    )
    .await;
    Ok(Json(json!({ "appeal_id": id, "status": status })))
}

pub fn appeals_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/appeals", post(create_appeal))
        .route("/api/me/appeals", get(my_appeals))
        .route("/api/admin/appeals", get(admin_appeals))
        .route("/api/admin/appeals/{id}/accept", post(accept_appeal))
        .route("/api/admin/appeals/{id}/reject", post(reject_appeal))
}
