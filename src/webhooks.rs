//! Outbound webhooks (M13 core): admin-registered endpoints receiving
//! HMAC-SHA256 signed JSON events with bounded retries.

use axum::Json;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::AdminUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::utils::{new_id, random_hex};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Webhook {
    pub id: Uuid,
    pub url: String,
    pub events: String,
    pub active: bool,
    #[serde(skip_serializing)]
    pub secret: String,
    /// shown once at creation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_secret: Option<String>,
    pub last_status: Option<String>,
    pub last_fired_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub const WEBHOOK_EVENTS: &[&str] = &["media.added", "playback.started", "payment.approved", "*"];

#[utoipa::path(get, path = "/api/admin/webhooks", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<Vec<Webhook>>> {
    let mut rows = sqlx::query_as::<_, Webhook>(
        "SELECT id, url, events, active, secret, NULL AS display_secret, last_status, last_fired_at, created_at \
         FROM webhooks ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;
    for row in &mut rows {
        row.secret = String::new();
    }
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/webhooks", tag = "admin",
    security(("bearer" = [])),
    request_body = CreateWebhookRequest,
    responses((status = 201), (status = 400))
)]
pub async fn create(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(req): Json<CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if !req.url.starts_with("https://")
        && !req.url.starts_with("http://localhost")
        && !req.url.starts_with("http://127.0.0.1")
    {
        return Err(ApiError::bad_request(
            "webhook urls must be https (loopback http allowed for testing)",
        ));
    }
    if req.events != "*" && !WEBHOOK_EVENTS.contains(&req.events.as_str()) {
        return Err(ApiError::bad_request(format!(
            "events must be * or one of: {}",
            WEBHOOK_EVENTS.join(", ")
        )));
    }

    let secret = random_hex(24);
    let id = new_id();
    sqlx::query(
        "INSERT INTO webhooks (id, user_id, url, secret, events) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(_admin.0.id())
    .bind(req.url.trim())
    .bind(&secret)
    .bind(&req.events)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({ "id": id, "secret": secret, "note": "shown once; sign header is X-Signature (hmac-sha256 of raw body)" }),
        ),
    ))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateWebhookRequest {
    pub url: String,
    /// * | media.added | playback.started | payment.approved
    #[serde(default = "default_events")]
    pub events: String,
}

fn default_events() -> String {
    "*".into()
}

#[utoipa::path(delete, path = "/api/admin/webhooks/{webhook_id}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn delete(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(webhook_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    let result = sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(webhook_id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("webhook"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Fire-and-forget dispatcher: spawns a task posting to every matching
/// active webhook with X-Signature = HMAC-SHA256(secret, body). 3 retries.
pub async fn emit(pool: &sqlx::PgPool, event: &str, payload: serde_json::Value) {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;

    let hooks: Vec<(Uuid, String, String)> = match sqlx::query_as(
        "SELECT id, url, secret FROM webhooks WHERE active AND (events = '*' OR events = $1)",
    )
    .bind(event)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(error = %err, "webhook lookup failed");
            return;
        }
    };
    if hooks.is_empty() {
        return;
    }

    let body = serde_json::json!({
        "event": event,
        "ts": chrono::Utc::now().to_rfc3339(),
        "data": payload,
    })
    .to_string();

    for (id, url, secret) in hooks {
        let pool = pool.clone();
        let body = body.clone();
        let event = event.to_string();
        tokio::spawn(async move {
           
            let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
                .expect("hmac accepts any key length");
            mac.update(body.as_bytes());
            let sig = hex::encode(mac.finalize().into_bytes());

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .build()
                .expect("reqwest client");

            let mut last_status = String::from("error");
            for attempt in 0..3 {
                let attempt_result = client
                    .post(&url)
                    .header("X-Signature", &sig)
                    .header("X-Event", &event)
                    .header("Content-Type", "application/json")
                    .body(body.clone())
                    .send()
                    .await;
                match attempt_result {
                    Ok(resp) => {
                        last_status = resp.status().as_u16().to_string();
                        if resp.status().is_success() {
                            break;
                        }
                    }
                    Err(_) => last_status = "network_error".into(),
                }
                tokio::time::sleep(std::time::Duration::from_millis(800 * (attempt + 1))).await;
            }
            metrics::counter!("webhook_deliveries_total", "event" => event, "status" => last_status.clone())
                .increment(1);
            let _ = sqlx::query(
                "UPDATE webhooks SET last_status = $2, last_fired_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(last_status)
            .execute(&pool)
            .await;
        });
    }
}

#[utoipa::path(post, path = "/api/admin/webhooks/{webhook_id}/test", tag = "admin",
    security(("bearer" = [])),
    responses((status = 202))
)]
pub async fn test_fire(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(webhook_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    let exists: Option<(bool,)> =
        sqlx::query_as("SELECT TRUE FROM webhooks WHERE id = $1 AND active")
            .bind(webhook_id)
            .fetch_optional(&state.pool)
            .await?;
    if exists.is_none() {
        return Err(ApiError::not_found("active webhook"));
    }
    // Temporarily scope a single-event fire by reusing emit with '*'.
    crate::webhooks::emit(&state.pool, "*", serde_json::json!({ "test": true })).await;
    Ok(StatusCode::ACCEPTED)
}

