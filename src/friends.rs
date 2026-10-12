//! Friend system (changeset 25).
//!
//! Follows exist for one-way feeds; friendship is the mutual, consent-gated
//! relationship that powers sharing and privacy scopes:
//!
//! * Requests are explicit: send -> accept/decline, with auto-accept when the
//!   target has already requested you (two clicks become zero).
//! * The target's `allow_friend_requests` privacy policy is enforced here —
//!   `everyone`, `friends_of_friends`, or `nobody`.
//! * Blocks (moderation) are checked in both directions and reported
//!   generically so blockers are never revealed.
//! * Friendship rows are stored symmetrically in one transaction.

use axum::Json;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct FriendUser {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub friends_since: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct FriendRequestView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RequestsView {
    pub incoming: Vec<FriendRequestView>,
    pub outgoing: Vec<FriendRequestView>,
}

async fn ensure_target(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT is_active FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((true,)) => Ok(()),
        Some((false,)) | None => Err(ApiError::not_found("user")),
    }
}

/// POST /api/friends/{user_id}/request
#[utoipa::path(post, path = "/api/friends/{user_id}/request", tag = "friends",
    security(("bearer" = [])),
    responses((status = 201), (status = 400), (status = 403), (status = 404))
)]
pub async fn send_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if user_id == auth.id() {
        return Err(ApiError::bad_request("you cannot befriend yourself"));
    }
    ensure_target(&state.pool, user_id).await?;
    crate::privacy::ensure_friend_requests_allowed(&state.pool, auth.id(), user_id).await?;

    let existing: bool = crate::privacy::are_friends(&state.pool, auth.id(), user_id).await?;
    if existing {
        return Err(ApiError::bad_request("you are already friends"));
    }

    // Reverse pending request? Accept it on the sender's behalf.
    let reverse: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM friend_requests \
         WHERE requester = $2 AND addressee = $1 AND status = 'pending'",
    )
    .bind(auth.id())
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(request_id) = reverse {
        accept_internal(&state.pool, request_id, auth.id()).await?;
        metrics::counter!("friend_requests_autoaccepted_total").increment(1);
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "friends",
                "message": "they had already requested you — you are now friends",
            })),
        ));
    }

    // Upsert: a declined/cancelled request may be re-sent; a pending one cannot.
    let result = sqlx::query(
        "INSERT INTO friend_requests (id, requester, addressee) VALUES ($1, $2, $3)
         ON CONFLICT (requester, addressee) DO UPDATE SET
             status = 'pending', created_at = now(), responded_at = NULL
         WHERE friend_requests.status IN ('declined', 'cancelled')",
    )
    .bind(crate::utils::new_id())
    .bind(auth.id())
    .bind(user_id)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        let still_pending: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM friend_requests WHERE requester = $1 AND addressee = $2",
        )
        .bind(auth.id())
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await?;
        if still_pending.map(|(s,)| s == "pending").unwrap_or(false) {
            return Err(ApiError::Conflict("friend request already pending".into()));
        }
        return Err(ApiError::Conflict("friend request cannot be re-sent".into()));
    }

    metrics::counter!("friend_requests_sent_total").increment(1);
    crate::audit::log_simple(&state.pool, Some(auth.id()), "friend.request_sent", &user_id.to_string()).await;

    let _ = crate::platform::repo::notify(
        &state.pool,
        user_id,
        &state.hub,
        "friend_request",
        "New friend request",
        &format!("{} wants to be your friend", auth.user().display_name),
        serde_json::json!({ "user_id": auth.id() }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "status": "pending" }))))
}

/// Marks a request accepted + creates both friendship rows atomically.
/// `acceptor` must be the addressee.
async fn accept_internal(pool: &PgPool, request_id: Uuid, acceptor: Uuid) -> ApiResult<(Uuid, Uuid)> {
    let mut tx = pool.begin().await?;
    let updated: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE friend_requests SET status = 'accepted', responded_at = now() \
         WHERE id = $1 AND addressee = $2 AND status = 'pending' \
         RETURNING requester, addressee",
    )
    .bind(request_id)
    .bind(acceptor)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((requester, addressee)) = updated else {
        return Err(ApiError::not_found("pending friend request"));
    };
    for (a, b) in [(requester, addressee), (addressee, requester)] {
        sqlx::query("INSERT INTO friendships (user_id, friend_id) VALUES ($1, $2)")
            .bind(a)
            .bind(b)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok((requester, addressee))
}

/// POST /api/friends/requests/{request_id}/accept
#[utoipa::path(post, path = "/api/friends/requests/{request_id}/accept", tag = "friends",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn accept_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(request_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let (requester, _me) = accept_internal(&state.pool, request_id, auth.id()).await?;

    metrics::counter!("friend_requests_accepted_total").increment(1);
    crate::audit::log_simple(&state.pool, Some(auth.id()), "friend.accepted", &requester.to_string()).await;

    let _ = crate::platform::repo::notify(
        &state.pool,
        requester,
        &state.hub,
        "friend_accepted",
        "Friend request accepted",
        &format!("{} accepted your friend request", auth.user().display_name),
        serde_json::json!({ "user_id": auth.id() }),
    )
    .await;

    Ok(Json(serde_json::json!({ "status": "friends" })))
}

/// POST /api/friends/requests/{request_id}/decline
#[utoipa::path(post, path = "/api/friends/requests/{request_id}/decline", tag = "friends",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn decline_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(request_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = sqlx::query(
        "UPDATE friend_requests SET status = 'declined', responded_at = now() \
         WHERE id = $1 AND addressee = $2 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(auth.id())
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("pending friend request"));
    }
    Ok(Json(serde_json::json!({ "status": "declined" })))
}

/// DELETE /api/friends/requests/{request_id} — cancel an outgoing request.
#[utoipa::path(delete, path = "/api/friends/requests/{request_id}", tag = "friends",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn cancel_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(request_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    let result = sqlx::query(
        "UPDATE friend_requests SET status = 'cancelled', responded_at = now() \
         WHERE id = $1 AND requester = $2 AND status = 'pending'",
    )
    .bind(request_id)
    .bind(auth.id())
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("pending friend request"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// GET /api/friends/requests — pending incoming and outgoing.
#[utoipa::path(get, path = "/api/friends/requests", tag = "friends",
    security(("bearer" = [])),
    responses((status = 200, body = RequestsView))
)]
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<RequestsView>> {
    const SELECT: &str = "SELECT r.id, u.id AS user_id, u.username, u.display_name, \
        u.avatar_url, r.created_at FROM friend_requests r JOIN users u ON u.id = ";
    let incoming: Vec<FriendRequestView> = sqlx::query_as(&format!(
        "{SELECT} u.id WHERE r.addressee = $1 AND r.status = 'pending' ORDER BY r.created_at DESC LIMIT 100"
    ))
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    let outgoing: Vec<FriendRequestView> = sqlx::query_as(&format!(
        "{SELECT} u.id WHERE r.requester = $1 AND r.status = 'pending' ORDER BY r.created_at DESC LIMIT 100"
    ))
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(RequestsView { incoming, outgoing }))
}

/// GET /api/friends
#[utoipa::path(get, path = "/api/friends", tag = "friends",
    security(("bearer" = [])),
    responses((status = 200, body = [FriendUser]))
)]
pub async fn list_friends(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<FriendUser>>> {
    let friends: Vec<FriendUser> = sqlx::query_as(
        "SELECT u.id AS user_id, u.username, u.display_name, u.avatar_url, f.created_at AS friends_since \
         FROM friendships f JOIN users u ON u.id = f.friend_id \
         WHERE f.user_id = $1 AND u.is_active ORDER BY u.display_name ASC LIMIT 500",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(friends))
}

/// DELETE /api/friends/{user_id} — unfriend.
#[utoipa::path(delete, path = "/api/friends/{user_id}", tag = "friends",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn remove_friend(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    let mut tx = state.pool.begin().await?;
    let removed = sqlx::query(
        "DELETE FROM friendships WHERE (user_id = $1 AND friend_id = $2) \
         OR (user_id = $2 AND friend_id = $1)",
    )
    .bind(auth.id())
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Also clear any lingering requests between the two.
    sqlx::query(
        "DELETE FROM friend_requests \
         WHERE (requester = $1 AND addressee = $2) OR (requester = $2 AND addressee = $1)",
    )
    .bind(auth.id())
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if removed == 0 {
        return Err(ApiError::not_found("friendship"));
    }
    crate::audit::log_simple(&state.pool, Some(auth.id()), "friend.removed", &user_id.to_string()).await;
    Ok(StatusCode::NO_CONTENT)
}

pub fn friends_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/friends/{user_id}/request", axum::routing::post(send_request))
        .route("/api/friends/{user_id}", axum::routing::delete(remove_friend))
        .route(
            "/api/friends/requests/{request_id}/accept",
            axum::routing::post(accept_request),
        )
        .route(
            "/api/friends/requests/{request_id}/decline",
            axum::routing::post(decline_request),
        )
        .route("/api/friends/requests", axum::routing::get(list_requests))
        .route(
            "/api/friends/requests/{request_id}",
            axum::routing::delete(cancel_request),
        )
        .route("/api/friends", axum::routing::get(list_friends))
}
