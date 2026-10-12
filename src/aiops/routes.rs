//! Admin endpoints for the AI copilot.

use axum::Json;
use axum::Router;
use axum::extract::{Query as AxQuery, State};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

pub fn ai_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/ai/config",
            get(get_config).patch(patch_config),
        )
        .route("/api/admin/ai/analyze", post(analyze))
        .route("/api/admin/ai/apply", post(apply))
        .route("/api/admin/ai/events", get(events))
        .route("/api/admin/ai/status", get(status))
}

use crate::aiops::engine::{self, AiConfig, ConfigPatch};
use crate::aiops::provider::Recommendation;
use crate::auth::middleware::AdminUser;
use crate::error::ApiResult;
use crate::state::AppState;
use uuid::Uuid;

#[utoipa::path(get, path = "/api/admin/ai/config", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = AiConfig))
)]
pub async fn get_config(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<AiConfig>> {
    Ok(Json(engine::load_config(&state.pool).await?))
}

#[utoipa::path(patch, path = "/api/admin/ai/config", tag = "admin",
    security(("bearer" = [])),
    request_body = ConfigPatch,
    responses((status = 200, body = AiConfig), (status = 400))
)]
pub async fn patch_config(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(patch): Json<ConfigPatch>,
) -> ApiResult<Json<AiConfig>> {
    Ok(Json(engine::save_config(&state.pool, &patch).await?))
}

/// Runs the configured brain over the live system snapshot.
#[utoipa::path(post, path = "/api/admin/ai/analyze", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 400))
)]
pub async fn analyze(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let snapshots = state.sources_health.snapshot_all().await;
    let report = state.ai.analyze(serde_json::to_value(snapshots).unwrap_or_default()).await?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ApplyRequest {
    /// Full recommendation object (e.g. from /analyze).
    pub recommendation: Recommendation,
}

/// Admin applies one recommendation. Governance still applies: whitelisted
/// action types only, scope must be granted, security domain respected.
#[utoipa::path(post, path = "/api/admin/ai/apply", tag = "admin",
    security(("bearer" = [])),
    request_body = ApplyRequest,
    responses((status = 200), (status = 403))
)]
pub async fn apply(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(req): Json<ApplyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = state.ai.apply(&req.recommendation, "admin").await?;
    Ok(Json(result))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct AiEvent {
    pub id: Uuid,
    pub kind: String,
    pub summary: String,
    pub payload: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct EventQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[utoipa::path(get, path = "/api/admin/ai/events", tag = "admin",
    security(("bearer" = [])),
    params(EventQuery),
    responses((status = 200))
)]
pub async fn events(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<EventQuery>,
) -> ApiResult<Json<Vec<AiEvent>>> {
    let rows = sqlx::query_as::<_, AiEvent>(
        "SELECT * FROM ai_events ORDER BY created_at DESC LIMIT $1",
    )
    .bind(q.limit.clamp(1, 500))
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

/// Compact status for the admin dashboard header.
#[utoipa::path(get, path = "/api/admin/ai/status", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn status(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let config = engine::load_config(&state.pool).await?;
    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ai_events WHERE created_at > now() - INTERVAL '24 hours'")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);

    Ok(Json(json!({
        "config": config,
        "llm_key_present": state.cfg.ai_api_key.is_some(),
        "events_24h": event_count,
        "actor": config.mode != "off" && config.autonomy == "act",
    })))
}
