//! Movie sharing + share history (changeset 25).
//!
//! Users push a catalog item into friends' inboxes with an optional note.
//! Every delivery is recorded permanently, so "history share" — everything a
//! user has sent and received — is a first-class query.
//!
//! Privacy is enforced per recipient (`allow_media_shares`: everyone /
//! friends / nobody) plus moderation blocks; disallowed recipients are
//! skipped with a reason instead of failing the whole batch.

use axum::Json;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

const MAX_RECIPIENTS_PER_CALL: usize = 20;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ShareMediaRequest {
    pub media_id: Uuid,
    pub recipient_ids: Vec<Uuid>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ShareView {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub recipient_id: Uuid,
    pub recipient_name: String,
    pub media_id: Uuid,
    pub media_title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub note: String,
    pub viewed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ShareOutcome {
    pub delivered: Vec<Uuid>,
    pub skipped: Vec<SkipReason>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SkipReason {
    pub user_id: Uuid,
    pub reason: String,
}

/// POST /api/share/media
#[utoipa::path(post, path = "/api/share/media", tag = "sharing",
    security(("bearer" = [])),
    request_body = ShareMediaRequest,
    responses((status = 201), (status = 400), (status = 404))
)]
pub async fn share_media(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ShareMediaRequest>,
) -> ApiResult<(StatusCode, Json<ShareOutcome>)> {
    let note = req.note.trim();
    if note.len() > 500 {
        return Err(ApiError::bad_request("note too long (max 500 characters)"));
    }
    // Dedupe while keeping order; cap batch size.
    let mut recipient_ids: Vec<Uuid> = Vec::with_capacity(req.recipient_ids.len());
    for id in &req.recipient_ids {
        if !recipient_ids.contains(id) {
            recipient_ids.push(*id);
        }
    }
    if recipient_ids.is_empty() {
        return Err(ApiError::bad_request("recipient_ids must not be empty"));
    }
    if recipient_ids.len() > MAX_RECIPIENTS_PER_CALL {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_RECIPIENTS_PER_CALL} recipients per share"
        )));
    }

    let media: Option<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT title, poster_url, media_type FROM media WHERE id = $1",
    )
    .bind(req.media_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((title, _poster, _kind)) = media else {
        return Err(ApiError::not_found("media"));
    };

    let mut outcome = ShareOutcome {
        delivered: Vec::new(),
        skipped: Vec::new(),
    };

    for recipient in recipient_ids {
        if recipient == auth.id() {
            outcome.skipped.push(SkipReason {
                user_id: recipient,
                reason: "cannot share with yourself".into(),
            });
            continue;
        }
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND is_active)",
        )
        .bind(recipient)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(false);
        if !active {
            outcome.skipped.push(SkipReason {
                user_id: recipient,
                reason: "user not found".into(),
            });
            continue;
        }
        if let Err(err) =
            crate::privacy::check_media_share_allowed(&state.pool, auth.id(), recipient).await
        {
            outcome.skipped.push(SkipReason {
                user_id: recipient,
                reason: err.to_string(),
            });
            continue;
        }

        let result = sqlx::query(
            "INSERT INTO media_shares (id, sender, recipient, media_id, note) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(crate::utils::new_id())
        .bind(auth.id())
        .bind(recipient)
        .bind(req.media_id)
        .bind(note)
        .execute(&state.pool)
        .await;
        match result {
            Ok(_) => outcome.delivered.push(recipient),
            Err(err) => {
                tracing::warn!(error = %err, %recipient, "media share insert failed");
                outcome.skipped.push(SkipReason {
                    user_id: recipient,
                    reason: "delivery failed".into(),
                });
            }
        }
    }

    if !outcome.delivered.is_empty() {
        metrics::counter!("media_shares_sent_total").increment(outcome.delivered.len() as u64);
        crate::audit::log(
            &state.pool,
            Some(auth.id()),
            "media.shared",
            &req.media_id.to_string(),
            serde_json::json!({ "delivered": outcome.delivered.len(), "skipped": outcome.skipped.len() }),
            "",
        )
        .await;
        for recipient in &outcome.delivered {
            let _ = crate::platform::repo::notify(
                &state.pool,
                *recipient,
                &state.hub,
                "media_share",
                "Movie shared with you",
                &format!("{} shared “{}” with you", auth.user().display_name, title),
                serde_json::json!({ "media_id": req.media_id, "from": auth.id() }),
            )
            .await;
        }
    }

    Ok((StatusCode::CREATED, Json(outcome)))
}

const SHARE_SELECT: &str = "SELECT s.id, s.sender, su.username AS sender_name, \
    s.recipient, ru.username AS recipient_name, m.id AS media_id, m.title AS media_title, \
    m.poster_url, m.media_type, s.note, s.viewed_at, s.created_at \
    FROM media_shares s \
    JOIN media m ON m.id = s.media_id \
    JOIN users su ON su.id = s.sender \
    JOIN users ru ON ru.id = s.recipient";

/// GET /api/me/shares/incoming — movies shared *with* me.
#[utoipa::path(get, path = "/api/me/shares/incoming", tag = "sharing",
    security(("bearer" = [])),
    responses((status = 200, body = [ShareView]))
)]
pub async fn incoming_shares(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<ShareView>>> {
    let rows: Vec<ShareView> = sqlx::query_as(&format!(
        "{SHARE_SELECT} WHERE s.recipient = $1 ORDER BY s.created_at DESC LIMIT 200"
    ))
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

/// GET /api/me/shares/history — everything I have shared (permanent log).
#[utoipa::path(get, path = "/api/me/shares/history", tag = "sharing",
    security(("bearer" = [])),
    responses((status = 200, body = [ShareView]))
)]
pub async fn share_history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<ShareView>>> {
    let rows: Vec<ShareView> = sqlx::query_as(&format!(
        "{SHARE_SELECT} WHERE s.sender = $1 ORDER BY s.created_at DESC LIMIT 200"
    ))
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

/// POST /api/shares/{share_id}/viewed — recipient marks a share opened.
#[utoipa::path(post, path = "/api/shares/{share_id}/viewed", tag = "sharing",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn mark_viewed(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(share_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query(
        "UPDATE media_shares SET viewed_at = now() \
         WHERE id = $1 AND recipient = $2 AND viewed_at IS NULL",
    )
    .bind(share_id)
    .bind(auth.id())
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        // Either not found or already viewed — report current state honestly.
        let exists: Option<(Option<chrono::DateTime<chrono::Utc>>,)> = sqlx::query_as(
            "SELECT viewed_at FROM media_shares WHERE id = $1 AND recipient = $2",
        )
        .bind(share_id)
        .bind(auth.id())
        .fetch_optional(&state.pool)
        .await?;
        let Some((viewed_at,)) = exists else {
            return Err(ApiError::not_found("share"));
        };
        return Ok(Json(serde_json::json!({
            "viewed": true,
            "already_viewed": viewed_at.is_some(),
        })));
    }
    metrics::counter!("media_shares_viewed_total").increment(1);
    Ok(Json(serde_json::json!({ "viewed": true, "already_viewed": false })))
}

/// DELETE /api/shares/{share_id} — recipient clears one from their inbox.
#[utoipa::path(delete, path = "/api/shares/{share_id}", tag = "sharing",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn delete_incoming_share(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(share_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    let result = sqlx::query("DELETE FROM media_shares WHERE id = $1 AND recipient = $2")
        .bind(share_id)
        .bind(auth.id())
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("share"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn sharing_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/share/media", axum::routing::post(share_media))
        .route("/api/me/shares/incoming", axum::routing::get(incoming_shares))
        .route("/api/me/shares/history", axum::routing::get(share_history))
        .route("/api/shares/{share_id}/viewed", axum::routing::post(mark_viewed))
        .route(
            "/api/shares/{share_id}",
            axum::routing::delete(delete_incoming_share),
        )
}
