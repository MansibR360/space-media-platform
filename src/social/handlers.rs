use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::pagination::Page;
use crate::social::models::{
    Comment, CreateCommentRequest, CreateReviewRequest, FollowInfo, ReactionCount, Review,
    ToggleReactionRequest,
};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ReviewsPageQuery {
    // NOT #[serde(flatten)]: serde_urlencoded hands flattened fields through as
    // strings, so `?limit=40` fails with "invalid type: string \"40\", expected
    // i64". Same trap as ListMediaQuery. Inlined, with `page()` rebuilding the
    // struct so call sites keep working.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl ReviewsPageQuery {
    pub fn page(&self) -> crate::pagination::PageQuery {
        crate::pagination::PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CommentsPageQuery {
    // Inlined for the same reason as ReviewsPageQuery: a flattened PageQuery
    // arrives as strings and fails to deserialize.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub target_type: String,
    pub target_id: Uuid,
}

impl CommentsPageQuery {
    pub fn page(&self) -> crate::pagination::PageQuery {
        crate::pagination::PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ReactionSummaryQuery {
    pub target_type: String,
    pub target_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FollowToggled {
    pub following: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReactionToggled {
    pub active: bool,
}

fn cursor_before(cursor: Option<&str>) -> ApiResult<Option<chrono::DateTime<chrono::Utc>>> {
    cursor
        .map(|raw| {
            crate::pagination::decode_cursor(raw)?
                .get("t")
                .and_then(|t| t.as_str())
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&chrono::Utc))
                .ok_or_else(|| ApiError::bad_request("invalid cursor"))
        })
        .transpose()
}

#[utoipa::path(get, path = "/api/media/{media_id}/reviews", tag = "social", params(MediaIdPath, ReviewsPageQuery),
    responses((status = 200, body = [Review]))
)]
pub async fn list_reviews(
    State(state): State<Arc<AppState>>,
    maybe_user: Option<CurrentUser>,
    Path(path): Path<MediaIdPath>,
    Query(query): Query<ReviewsPageQuery>,
) -> ApiResult<Json<Vec<Review>>> {
    let is_mod = maybe_user
        .as_ref()
        .map(|u| {
            matches!(
                u.user().role,
                crate::users::models::Role::Moderator | crate::users::models::Role::Admin
            )
        })
        .unwrap_or(false);
    let before = cursor_before(query.page().cursor.as_deref())?;
    let reviews = crate::social::repo::reviews_for_media(
        &state.pool,
        path.media_id,
        is_mod,
        query.page().clamp_limit(),
        before,
    )
    .await?;
    Ok(Json(reviews))
}

#[utoipa::path(post, path = "/api/media/{media_id}/reviews", tag = "social", params(MediaIdPath),
    security(("bearer" = [])),
    request_body = CreateReviewRequest,
    responses((status = 201, body = Review), (status = 400))
)]
pub async fn create_review(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
    Json(req): Json<CreateReviewRequest>,
) -> ApiResult<(StatusCode, Json<Review>)> {
    let review =
        crate::social::repo::create_review(&state.pool, auth.id(), path.media_id, &req).await?;
    state.hub.publish_or_create(
        &crate::chat::hub::GLOBAL_ROOM,
        serde_json::json!({
            "type": "review.new",
            "data": { "user_id": auth.id(), "media_id": path.media_id }
        })
        .to_string(),
    );
    Ok((StatusCode::CREATED, Json(review)))
}

#[utoipa::path(delete, path = "/api/reviews/{review_id}", tag = "social",
    security(("bearer" = [])),
    responses((status = 204), (status = 403))
)]
pub async fn delete_review(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(review_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let is_mod = matches!(
        auth.user().role,
        crate::users::models::Role::Moderator | crate::users::models::Role::Admin
    );
    crate::social::repo::delete_review(&state.pool, auth.id(), is_mod, review_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/social/feed", tag = "social",
    params(ReviewsPageQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [Review]))
)]
pub async fn feed(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(query): Query<ReviewsPageQuery>,
) -> ApiResult<Json<Page<Review>>> {
    let limit = query.page().clamp_limit();
    let before = cursor_before(query.page().cursor.as_deref())?;
    let reviews =
        crate::social::repo::feed_for_user(&state.pool, auth.id(), limit + 1, before).await?;

    let next_cursor = (reviews.len() as i64 > limit)
        .then(|| {
            reviews.get(limit as usize - 1).map(|r| {
                crate::pagination::encode_cursor(
                    &serde_json::json!({ "t": r.created_at.to_rfc3339() }),
                )
            })
        })
        .flatten();
    let mut items = reviews;
    if next_cursor.is_some() {
        items.truncate(limit as usize);
    }
    Ok(Json(Page::with_cursor(items, next_cursor)))
}

#[utoipa::path(get, path = "/api/comments", tag = "social", params(CommentsPageQuery),
    responses((status = 200, body = [Comment]))
)]
pub async fn list_comments(
    State(state): State<Arc<AppState>>,
    maybe_user: Option<CurrentUser>,
    Query(query): Query<CommentsPageQuery>,
) -> ApiResult<Json<Vec<Comment>>> {
    let is_mod = maybe_user
        .as_ref()
        .map(|u| {
            matches!(
                u.user().role,
                crate::users::models::Role::Moderator | crate::users::models::Role::Admin
            )
        })
        .unwrap_or(false);
    let before = cursor_before(query.page().cursor.as_deref())?;
    let comments = crate::social::repo::comments_for_target(
        &state.pool,
        &query.target_type,
        query.target_id,
        is_mod,
        query.page().clamp_limit(),
        before,
    )
    .await?;
    Ok(Json(comments))
}

#[utoipa::path(post, path = "/api/comments", tag = "social",
    security(("bearer" = [])),
    request_body = CreateCommentRequest,
    responses((status = 201, body = Comment), (status = 400))
)]
pub async fn create_comment(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreateCommentRequest>,
) -> ApiResult<(StatusCode, Json<Comment>)> {
    let comment = crate::social::repo::create_comment(&state.pool, auth.id(), &req).await?;
    Ok((StatusCode::CREATED, Json(comment)))
}

#[utoipa::path(delete, path = "/api/comments/{comment_id}", tag = "social",
    security(("bearer" = [])),
    responses((status = 204), (status = 403))
)]
pub async fn delete_comment(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(comment_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let is_mod = matches!(
        auth.user().role,
        crate::users::models::Role::Moderator | crate::users::models::Role::Admin
    );
    crate::social::repo::delete_comment(&state.pool, auth.id(), is_mod, comment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/reactions", tag = "social",
    security(("bearer" = [])),
    request_body = ToggleReactionRequest,
    responses((status = 200, body = ReactionToggled), (status = 400))
)]
pub async fn toggle_reaction(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ToggleReactionRequest>,
) -> ApiResult<Json<ReactionToggled>> {
    let active = crate::social::repo::toggle_reaction(
        &state.pool,
        auth.id(),
        &req.target_type,
        req.target_id,
        &req.emoji,
    )
    .await?;
    Ok(Json(ReactionToggled { active }))
}

#[utoipa::path(get, path = "/api/reactions", tag = "social", params(ReactionSummaryQuery),
    responses((status = 200, body = [ReactionCount]))
)]
pub async fn reactions_summary(
    State(state): State<Arc<AppState>>,
    maybe_user: Option<CurrentUser>,
    Query(query): Query<ReactionSummaryQuery>,
) -> ApiResult<Json<Vec<ReactionCount>>> {
    let summary = crate::social::repo::reactions_summary(
        &state.pool,
        maybe_user.map(|u| u.id()),
        &query.target_type,
        query.target_id,
    )
    .await?;
    Ok(Json(summary))
}

#[utoipa::path(post, path = "/api/users/{user_id}/follow", tag = "social",
    security(("bearer" = [])),
    responses((status = 200, body = FollowToggled))
)]
pub async fn follow_user(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Json<FollowToggled>> {
    let following = crate::social::repo::follow(&state.pool, auth.id(), user_id).await?;

    // Notification trigger: the followed user learns about the new follower.
    if following {
        let _ = crate::platform::repo::notify(
            &state.pool,
            user_id,
            &state.hub,
            "follow",
            "New follower",
            &format!("{} started following you", auth.user().display_name),
            serde_json::json!({ "user_id": auth.id() }),
        )
        .await;
    }

    Ok(Json(FollowToggled { following }))
}

#[utoipa::path(get, path = "/api/me/following", tag = "social",
    security(("bearer" = [])),
    responses((status = 200, body = [FollowInfo]))
)]
pub async fn my_following(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<FollowInfo>>> {
    Ok(Json(
        crate::social::repo::following(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(get, path = "/api/me/followers", tag = "social",
    security(("bearer" = [])),
    responses((status = 200, body = [FollowInfo]))
)]
pub async fn my_followers(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<FollowInfo>>> {
    Ok(Json(
        crate::social::repo::followers(&state.pool, auth.id()).await?,
    ))
}
