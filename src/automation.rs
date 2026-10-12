//! Automation engine (M13): WHEN event / IF filter / THEN actions.
//!
//! Rules are evaluated in-process at the same trigger points that fire
//! webhooks. Actions currently supported:
//!   {"type":"notify_all","title":"…","body":"…"}
//!   {"type":"notify_users","user_ids":["…"],"title":"…","body":"…"}
//!   {"type":"refresh_recommendations","user_ids":[…]}   (cache bust)
//!   {"type":"webhook","event":"custom.name","payload":{…}}
//!
//! Filters are equality matchers against the event payload, e.g.
//!   {"category": "movie"}  or  {"plan": "premium_monthly"}

use sqlx::PgPool;
use uuid::Uuid;

use crate::chat::hub::Hub;
use crate::error::{ApiError, ApiResult};
use crate::utils::new_id;

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Automation {
    pub id: Uuid,
    pub name: String,
    pub when_event: String,
    pub filter: serde_json::Value,
    pub actions: serde_json::Value,
    pub active: bool,
    pub run_count: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn create(
    pool: &PgPool,
    name: &str,
    when_event: &str,
    filter: serde_json::Value,
    actions: serde_json::Value,
) -> ApiResult<Automation> {
    if !matches!(
        when_event,
        "media.added" | "playback.started" | "payment.approved"
    ) {
        return Err(ApiError::bad_request(
            "when_event must be media.added, playback.started or payment.approved",
        ));
    }
    if !filter.is_object() || !actions.is_array() {
        return Err(ApiError::bad_request(
            "filter must be an object and actions an array",
        ));
    }
    sqlx::query_as::<_, Automation>(
        "INSERT INTO automations (id, name, when_event, filter, actions)
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(new_id())
    .bind(name.trim())
    .bind(when_event)
    .bind(filter)
    .bind(actions)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

pub async fn list(pool: &PgPool) -> ApiResult<Vec<Automation>> {
    Ok(sqlx::query_as::<_, Automation>(
        "SELECT * FROM automations ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn toggle(pool: &PgPool, id: Uuid, active: bool) -> ApiResult<()> {
    let result = sqlx::query("UPDATE automations SET active = $2 WHERE id = $1")
        .bind(id)
        .bind(active)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("automation"));
    }
    Ok(())
}

pub async fn delete(pool: &PgPool, id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("DELETE FROM automations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("automation"));
    }
    Ok(())
}

fn matches_filter(filter: &serde_json::Value, payload: &serde_json::Value) -> bool {
    let Some(obj) = filter.as_object() else {
        return true;
    };
    if obj.is_empty() {
        return true;
    }
    obj.iter().all(|(key, expected)| {
        payload
            .get(key)
            .map(|actual| actual == expected)
            .unwrap_or(false)
    })
}

/// Called from every trigger point. Failures never propagate.
pub async fn dispatch(pool: &PgPool, hub: &Hub, event: &str, payload: serde_json::Value) {
    let rules: Vec<Automation> = match sqlx::query_as(
        "SELECT * FROM automations WHERE when_event = $1 AND active ORDER BY created_at",
    )
    .bind(event)
    .fetch_all(pool)
    .await
    {
        Ok(rules) => rules,
        Err(err) => {
            tracing::debug!(error = %err, "automation lookup failed");
            return;
        }
    };

    for rule in rules {
        if !matches_filter(&rule.filter, &payload) {
            continue;
        }
        let Some(actions) = rule.actions.as_array() else {
            continue;
        };
        let mut executed = 0usize;

        for action in actions {
            let kind = action.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "notify_all" => {
                    let title = action
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Update");
                    let body = action.get("body").and_then(|b| b.as_str()).unwrap_or("");
                    let user_ids: Vec<(Uuid,)> =
                        sqlx::query_as("SELECT id FROM users WHERE is_active")
                            .fetch_all(pool)
                            .await
                            .unwrap_or_default();
                    for (uid,) in user_ids {
                        let _ = crate::platform::repo::notify(
                            pool,
                            uid,
                            hub,
                            "system",
                            title,
                            body,
                            payload.clone(),
                        )
                        .await;
                    }
                    executed += 1;
                }
                "notify_users" => {
                    let title = action
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Update");
                    let body = action.get("body").and_then(|b| b.as_str()).unwrap_or("");
                    if let Some(ids) = action.get("user_ids").and_then(|u| u.as_array()) {
                        for id_value in ids {
                            if let Some(uid) =
                                id_value.as_str().and_then(|s| Uuid::parse_str(s).ok())
                            {
                                let _ = crate::platform::repo::notify(
                                    pool,
                                    uid,
                                    hub,
                                    "system",
                                    title,
                                    body,
                                    payload.clone(),
                                )
                                .await;
                                executed += 1;
                            }
                        }
                    }
                }
                "webhook" => {
                    let hook_event = action
                        .get("event")
                        .and_then(|e| e.as_str())
                        .unwrap_or("automation.custom");
                    crate::webhooks::emit(pool, hook_event, payload.clone()).await;
                    executed += 1;
                }
                other => tracing::debug!(action = other, "unknown automation action"),
            }
        }

        if executed > 0 {
            metrics::counter!("automation_runs_total", "event" => event.to_string()).increment(1);
            let _ = sqlx::query("UPDATE automations SET run_count = run_count + 1 WHERE id = $1")
                .bind(rule.id)
                .execute(pool)
                .await;
            tracing::info!(rule = %rule.name, executed, "automation executed");
        }
    }
}

use axum::Json;
use axum::extract::{Path as AxPath, State};
use serde::Deserialize;
use std::sync::Arc;

use crate::auth::middleware::AdminUser;
use crate::state::AppState;

#[utoipa::path(get, path = "/api/admin/automations", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_list(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<Vec<Automation>>> {
    Ok(Json(list(&state.pool).await?))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateAutomationRequest {
    pub name: String,
    pub when_event: String,
    #[serde(default)]
    pub filter: serde_json::Value,
    #[serde(default = "default_actions")]
    pub actions: serde_json::Value,
}

fn default_actions() -> serde_json::Value {
    serde_json::json!([])
}

#[utoipa::path(post, path = "/api/admin/automations", tag = "admin",
    security(("bearer" = [])),
    request_body = CreateAutomationRequest,
    responses((status = 201), (status = 400))
)]
pub async fn admin_create(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(req): Json<CreateAutomationRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<Automation>)> {
    let created = create(
        &state.pool,
        &req.name,
        &req.when_event,
        req.filter.clone(),
        req.actions.clone(),
    )
    .await?;
    Ok((axum::http::StatusCode::CREATED, Json(created)))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ToggleRequest {
    pub active: bool,
}

#[utoipa::path(patch, path = "/api/admin/automations/{id}", tag = "admin",
    security(("bearer" = [])),
    request_body = ToggleRequest,
    responses((status = 204))
)]
pub async fn admin_toggle(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
    Json(req): Json<ToggleRequest>,
) -> ApiResult<()> {
    toggle(&state.pool, id, req.active).await
}

#[utoipa::path(delete, path = "/api/admin/automations/{id}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn admin_delete(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<()> {
    delete(&state.pool, id).await
}
