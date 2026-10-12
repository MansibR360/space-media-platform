//! Per-user privacy settings (changeset 25).
//!
//! Every account controls who sees its profile/activity and who may send it
//! friend requests or shared movies. Rows are optional in the database — a
//! missing row means the platform defaults below — so gating helpers never
//! require a prior write.
//!
//! Gates are enforced server-side at every entry point that exposes user
//! data or delivers social traffic; clients cannot opt out of them.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const PROFILE_VISIBILITIES: &[&str] = &["everyone", "friends", "private"];
pub const FRIEND_REQUEST_POLICIES: &[&str] = &["everyone", "friends_of_friends", "nobody"];
pub const MEDIA_SHARE_POLICIES: &[&str] = &["everyone", "friends", "nobody"];

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PrivacySettings {
    /// everyone | friends | private
    pub profile_visibility: String,
    pub show_watch_history: bool,
    pub show_activity_feed: bool,
    /// everyone | friends_of_friends | nobody
    pub allow_friend_requests: String,
    /// everyone | friends | nobody
    pub allow_media_shares: String,
    pub show_online_status: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            profile_visibility: "everyone".into(),
            show_watch_history: true,
            show_activity_feed: true,
            allow_friend_requests: "everyone".into(),
            allow_media_shares: "friends".into(),
            show_online_status: true,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PrivacyPatch {
    #[serde(default)]
    pub profile_visibility: Option<String>,
    #[serde(default)]
    pub show_watch_history: Option<bool>,
    #[serde(default)]
    pub show_activity_feed: Option<bool>,
    #[serde(default)]
    pub allow_friend_requests: Option<String>,
    #[serde(default)]
    pub allow_media_shares: Option<String>,
    #[serde(default)]
    pub show_online_status: Option<bool>,
}

fn validate(patch: &PrivacyPatch) -> ApiResult<()> {
    if let Some(v) = &patch.profile_visibility {
        if !PROFILE_VISIBILITIES.contains(&v.as_str()) {
            return Err(ApiError::bad_request(format!(
                "profile_visibility must be one of: {}",
                PROFILE_VISIBILITIES.join(", ")
            )));
        }
    }
    if let Some(v) = &patch.allow_friend_requests {
        if !FRIEND_REQUEST_POLICIES.contains(&v.as_str()) {
            return Err(ApiError::bad_request(format!(
                "allow_friend_requests must be one of: {}",
                FRIEND_REQUEST_POLICIES.join(", ")
            )));
        }
    }
    if let Some(v) = &patch.allow_media_shares {
        if !MEDIA_SHARE_POLICIES.contains(&v.as_str()) {
            return Err(ApiError::bad_request(format!(
                "allow_media_shares must be one of: {}",
                MEDIA_SHARE_POLICIES.join(", ")
            )));
        }
    }
    Ok(())
}

/// Loads the target's privacy settings; missing row == defaults.
pub async fn get(pool: &PgPool, user_id: Uuid) -> ApiResult<PrivacySettings> {
    let row: Option<PrivacySettings> = sqlx::query_as(
        "SELECT profile_visibility, show_watch_history, show_activity_feed, \
                allow_friend_requests, allow_media_shares, show_online_status \
         FROM privacy_settings WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or_default())
}

// ------------------------------------------------------------ gates

pub async fn is_blocked_between(pool: &PgPool, a: Uuid, b: Uuid) -> ApiResult<bool> {
    let blocked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_blocks WHERE (blocker = $1 AND blocked = $2) \
         OR (blocker = $2 AND blocked = $1))",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    Ok(blocked)
}

pub async fn are_friends(pool: &PgPool, a: Uuid, b: Uuid) -> ApiResult<bool> {
    let friends: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM friendships WHERE user_id = $1 AND friend_id = $2)",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    Ok(friends)
}

async fn has_mutual_friend(pool: &PgPool, a: Uuid, b: Uuid) -> ApiResult<bool> {
    let mutual: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM friendships f1 JOIN friendships f2 ON f1.friend_id = f2.friend_id \
         WHERE f1.user_id = $1 AND f2.user_id = $2)",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    Ok(mutual)
}

/// Whether `viewer` may see `target`'s profile. Admins bypass (handled by the
/// caller); anonymous viewers only see 'everyone' profiles.
pub async fn can_view_profile(pool: &PgPool, viewer: Option<Uuid>, target: Uuid) -> ApiResult<bool> {
    if viewer == Some(target) {
        return Ok(true);
    }
    let settings = get(pool, target).await?;
    match settings.profile_visibility.as_str() {
        "everyone" => Ok(true),
        "private" => Ok(false),
        "friends" => match viewer {
            Some(v) => are_friends(pool, v, target).await,
            None => Ok(false),
        },
        _ => Ok(true),
    }
}

/// Friend-request policy gate for `sender -> target`.
pub async fn ensure_friend_requests_allowed(
    pool: &PgPool,
    sender: Uuid,
    target: Uuid,
) -> ApiResult<()> {
    if is_blocked_between(pool, sender, target).await? {
        // Deliberately generic: do not reveal the block to the sender.
        return Err(ApiError::forbidden("friend request cannot be delivered"));
    }
    let settings = get(pool, target).await?;
    match settings.allow_friend_requests.as_str() {
        "nobody" => Err(ApiError::forbidden(
            "this user is not accepting friend requests",
        )),
        "friends_of_friends" => {
            if has_mutual_friend(pool, sender, target).await? {
                Ok(())
            } else {
                Err(ApiError::forbidden(
                    "this user only accepts friend requests from friends-of-friends",
                ))
            }
        }
        _ => Ok(()),
    }
}

/// Media-share gate for `sender -> recipient`. Returns Ok(()) when allowed,
/// otherwise an Err describing why (callers batching multiple recipients can
/// downgrade this to a per-recipient skip).
pub async fn check_media_share_allowed(
    pool: &PgPool,
    sender: Uuid,
    recipient: Uuid,
) -> ApiResult<()> {
    if is_blocked_between(pool, sender, recipient).await? {
        return Err(ApiError::forbidden("cannot share with this user"));
    }
    let settings = get(pool, recipient).await?;
    match settings.allow_media_shares.as_str() {
        "nobody" => Err(ApiError::forbidden("this user does not accept shared media")),
        "friends" => {
            if are_friends(pool, sender, recipient).await? {
                Ok(())
            } else {
                Err(ApiError::forbidden("only friends can share media with this user"))
            }
        }
        _ => Ok(()),
    }
}

// ------------------------------------------------------------ endpoints

#[utoipa::path(get, path = "/api/me/privacy", tag = "users",
    security(("bearer" = [])),
    responses((status = 200, body = PrivacySettings))
)]
pub async fn my_privacy(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<PrivacySettings>> {
    Ok(Json(get(&state.pool, auth.id()).await?))
}

#[utoipa::path(patch, path = "/api/me/privacy", tag = "users",
    security(("bearer" = [])),
    request_body = PrivacyPatch,
    responses((status = 200, body = PrivacySettings), (status = 400))
)]
pub async fn patch_my_privacy(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(patch): Json<PrivacyPatch>,
) -> ApiResult<Json<PrivacySettings>> {
    validate(&patch)?;
    let current = get(&state.pool, auth.id()).await?;
    let next = PrivacySettings {
        profile_visibility: patch.profile_visibility.unwrap_or(current.profile_visibility),
        show_watch_history: patch.show_watch_history.unwrap_or(current.show_watch_history),
        show_activity_feed: patch.show_activity_feed.unwrap_or(current.show_activity_feed),
        allow_friend_requests: patch
            .allow_friend_requests
            .unwrap_or(current.allow_friend_requests),
        allow_media_shares: patch.allow_media_shares.unwrap_or(current.allow_media_shares),
        show_online_status: patch.show_online_status.unwrap_or(current.show_online_status),
    };

    sqlx::query(
        "INSERT INTO privacy_settings (user_id, profile_visibility, show_watch_history, \
            show_activity_feed, allow_friend_requests, allow_media_shares, show_online_status)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (user_id) DO UPDATE SET
            profile_visibility = EXCLUDED.profile_visibility,
            show_watch_history = EXCLUDED.show_watch_history,
            show_activity_feed = EXCLUDED.show_activity_feed,
            allow_friend_requests = EXCLUDED.allow_friend_requests,
            allow_media_shares = EXCLUDED.allow_media_shares,
            show_online_status = EXCLUDED.show_online_status,
            updated_at = now()",
    )
    .bind(auth.id())
    .bind(&next.profile_visibility)
    .bind(next.show_watch_history)
    .bind(next.show_activity_feed)
    .bind(&next.allow_friend_requests)
    .bind(&next.allow_media_shares)
    .bind(next.show_online_status)
    .execute(&state.pool)
    .await?;

    crate::audit::log_simple(&state.pool, Some(auth.id()), "privacy.updated", "").await;
    Ok(Json(next))
}

pub fn privacy_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/me/privacy",
            axum::routing::get(my_privacy).patch(patch_my_privacy),
        )
}
