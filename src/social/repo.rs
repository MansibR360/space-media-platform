use crate::error::{ApiError, ApiResult};
use crate::social::models::{
    Comment, CreateCommentRequest, CreateReviewRequest, FollowInfo, ReactionCount, Review,
};
use crate::utils::{new_id, now};
use sqlx::PgPool;
use uuid::Uuid;

const REVIEW_SELECT: &str = "SELECT r.id, r.user_id, u.username, u.avatar_url, r.media_id, m.title AS media_title, \
     COALESCE(r.title, '') AS title, r.body, r.score, r.status::text AS status, r.created_at, r.updated_at \
     FROM reviews r JOIN users u ON u.id = r.user_id JOIN media m ON m.id = r.media_id";

pub async fn create_review(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    req: &CreateReviewRequest,
) -> ApiResult<Review> {
    if req.body.trim().is_empty() || req.body.len() > 20000 {
        return Err(ApiError::bad_request(
            "review body must be 1-20000 characters",
        ));
    }
    if let Some(score) = req.score {
        if !(0.5..=10.0).contains(&score) {
            return Err(ApiError::bad_request("score must be between 0.5 and 10"));
        }
    }

    let review_id = new_id();
    sqlx::query(
        "INSERT INTO reviews (id, user_id, media_id, title, body, score)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, media_id) DO UPDATE SET
            title = EXCLUDED.title, body = EXCLUDED.body, score = EXCLUDED.score,
            status = 'published', updated_at = now()",
    )
    .bind(review_id)
    .bind(user_id)
    .bind(media_id)
    .bind(req.title.as_deref().unwrap_or("").trim())
    .bind(req.body.trim())
    .bind(req.score)
    .execute(pool)
    .await?;

    crate::catalog::repo::recompute_rating(pool, media_id).await?;
    my_review(pool, user_id, media_id)
        .await?
        .ok_or_else(|| ApiError::internal_msg("review vanished after insert"))
}

pub async fn my_review(pool: &PgPool, user_id: Uuid, media_id: Uuid) -> ApiResult<Option<Review>> {
    let review = sqlx::query_as::<_, Review>(&format!(
        "{REVIEW_SELECT} WHERE r.user_id = $1 AND r.media_id = $2"
    ))
    .bind(user_id)
    .bind(media_id)
    .fetch_optional(pool)
    .await?;
    Ok(review)
}

pub async fn reviews_for_media(
    pool: &PgPool,
    media_id: Uuid,
    include_hidden: bool,
    limit: i64,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> ApiResult<Vec<Review>> {
    let hidden_clause = if include_hidden {
        ""
    } else {
        " AND r.status = 'published'"
    };
    let sql = format!(
        "{REVIEW_SELECT} WHERE r.media_id = $1{hidden_clause} \
         AND (($2 IS NULL) OR (r.created_at, r.id) < ($2, 'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid)) \
         ORDER BY r.created_at DESC LIMIT $3"
    );
    Ok(sqlx::query_as::<_, Review>(&sql)
        .bind(media_id)
        .bind(before)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

pub async fn feed_for_user(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> ApiResult<Vec<Review>> {
    let sql = format!(
        "{REVIEW_SELECT}
         WHERE r.status = 'published'
           AND (r.user_id = $1 OR r.user_id IN (SELECT followed_id FROM follows WHERE follower_id = $1))
           AND r.user_id NOT IN (SELECT blocked FROM user_blocks WHERE blocker = $1)
           AND r.user_id NOT IN (SELECT muted FROM user_mutes WHERE muter = $1 AND (until IS NULL OR until > now()))
           AND (($2 IS NULL) OR (r.created_at, r.id) < ($2, 'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid))
         ORDER BY r.created_at DESC LIMIT $3"
    );
    Ok(sqlx::query_as::<_, Review>(&sql)
        .bind(user_id)
        .bind(before)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

pub async fn delete_review(
    pool: &PgPool,
    actor: Uuid,
    is_moderator: bool,
    review_id: Uuid,
) -> ApiResult<()> {
    let owner: (Uuid,) = sqlx::query_as("SELECT user_id FROM reviews WHERE id = $1")
        .bind(review_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("review"))?;

    if owner.0 != actor && !is_moderator {
        return Err(ApiError::forbidden("not your review"));
    }
    sqlx::query("DELETE FROM reviews WHERE id = $1")
        .bind(review_id)
        .execute(pool)
        .await?;
    Ok(())
}

const COMMENT_SELECT: &str = "SELECT c.id, c.author_id, u.display_name AS author_name, c.target_type, c.target_id, \
     c.parent_id, c.body, c.status::text AS status, c.created_at \
     FROM comments c JOIN users u ON u.id = c.author_id";

pub async fn comments_for_target(
    pool: &PgPool,
    target_type: &str,
    target_id: Uuid,
    include_hidden: bool,
    limit: i64,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> ApiResult<Vec<Comment>> {
    let hidden_clause = if include_hidden {
        ""
    } else {
        " AND c.status = 'published'"
    };
    let sql = format!(
        "{COMMENT_SELECT}
         WHERE c.target_type = $1 AND c.target_id = $2{hidden_clause}
           AND (($3 IS NULL) OR (c.created_at, c.id) < ($3, 'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid))
         ORDER BY c.created_at ASC LIMIT $4"
    );
    Ok(sqlx::query_as::<_, Comment>(&sql)
        .bind(target_type)
        .bind(target_id)
        .bind(before)
        .bind(limit)
        .fetch_all(pool)
        .await?)
}

pub async fn create_comment(
    pool: &PgPool,
    author_id: Uuid,
    req: &CreateCommentRequest,
) -> ApiResult<Comment> {
    if !matches!(req.target_type.as_str(), "review" | "media" | "party") {
        return Err(ApiError::bad_request(
            "target_type must be review, media or party",
        ));
    }
    let trimmed = req.body.trim();
    if trimmed.is_empty() || trimmed.len() > 4000 {
        return Err(ApiError::bad_request(
            "comment body must be 1-4000 characters",
        ));
    }

    let comment_id = new_id();
    let comment = sqlx::query_as::<_, Comment>(&format!(
        "WITH inserted AS (
            INSERT INTO comments (id, author_id, target_type, target_id, parent_id, body)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
         )
         SELECT i.id, i.author_id, u.display_name AS author_name, i.target_type, i.target_id, \
                i.parent_id, i.body, i.status::text AS status, i.created_at
         FROM inserted i JOIN users u ON u.id = i.author_id"
    ))
    .bind(comment_id)
    .bind(author_id)
    .bind(&req.target_type)
    .bind(req.target_id)
    .bind(req.parent_id)
    .bind(trimmed)
    .fetch_one(pool)
    .await?;
    Ok(comment)
}

pub async fn delete_comment(
    pool: &PgPool,
    actor: Uuid,
    is_moderator: bool,
    comment_id: Uuid,
) -> ApiResult<()> {
    let owner: (Uuid,) = sqlx::query_as("SELECT author_id FROM comments WHERE id = $1")
        .bind(comment_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("comment"))?;
    if owner.0 != actor && !is_moderator {
        return Err(ApiError::forbidden("not your comment"));
    }
    sqlx::query("DELETE FROM comments WHERE id = $1")
        .bind(comment_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn toggle_reaction(
    pool: &PgPool,
    user_id: Uuid,
    target_type: &str,
    target_id: Uuid,
    emoji: &str,
) -> ApiResult<bool> {
    if !matches!(target_type, "review" | "comment" | "message") {
        return Err(ApiError::bad_request(
            "target_type must be review, comment or message",
        ));
    }
    if emoji.is_empty() || emoji.chars().count() > 8 {
        return Err(ApiError::bad_request("emoji must be 1-8 characters"));
    }

    let deleted = sqlx::query(
        "DELETE FROM reactions WHERE user_id = $1 AND target_type = $2 AND target_id = $3 AND emoji = $4",
    )
    .bind(user_id)
    .bind(target_type)
    .bind(target_id)
    .bind(emoji)
    .execute(pool)
    .await?
    .rows_affected()
        > 0;

    if deleted {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO reactions (user_id, target_type, target_id, emoji) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(target_type)
    .bind(target_id)
    .bind(emoji)
    .execute(pool)
    .await?;
    Ok(true)
}

pub async fn reactions_summary(
    pool: &PgPool,
    viewer: Option<Uuid>,
    target_type: &str,
    target_id: Uuid,
) -> ApiResult<Vec<ReactionCount>> {
    let rows = sqlx::query_as::<_, ReactionCount>(
        "SELECT emoji, COUNT(*)::bigint AS count, BOOL_OR(user_id = $1) AS mine
         FROM reactions WHERE target_type = $2 AND target_id = $3
         GROUP BY emoji ORDER BY count DESC, emoji",
    )
    .bind(viewer.unwrap_or(Uuid::nil()))
    .bind(target_type)
    .bind(target_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn follow(pool: &PgPool, follower: Uuid, followed: Uuid) -> ApiResult<bool> {
    if follower == followed {
        return Err(ApiError::bad_request("cannot follow yourself"));
    }
    let exists: Option<(bool,)> =
        sqlx::query_as("SELECT TRUE FROM follows WHERE follower_id = $1 AND followed_id = $2")
            .bind(follower)
            .bind(followed)
            .fetch_optional(pool)
            .await?;

    if exists.is_some() {
        sqlx::query("DELETE FROM follows WHERE follower_id = $1 AND followed_id = $2")
            .bind(follower)
            .bind(followed)
            .execute(pool)
            .await?;
        return Ok(false);
    }

    let target_active: Option<(bool,)> =
        sqlx::query_as("SELECT is_active FROM users WHERE id = $1")
            .bind(followed)
            .fetch_optional(pool)
            .await?;
    if target_active.is_none() {
        return Err(ApiError::not_found("user"));
    }

    sqlx::query(
        "INSERT INTO follows (follower_id, followed_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(follower)
    .bind(followed)
    .execute(pool)
    .await?;
    Ok(true)
}

const FOLLOW_SELECT: &str = "SELECT f.followed_id AS user_id, u.username, u.display_name, u.avatar_url, f.created_at AS followed_at";

pub async fn following(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<FollowInfo>> {
    Ok(sqlx::query_as::<_, FollowInfo>(&format!(
        "{FOLLOW_SELECT} FROM follows f JOIN users u ON u.id = f.followed_id \
         WHERE f.follower_id = $1 ORDER BY f.created_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn followers(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<FollowInfo>> {
    Ok(sqlx::query_as::<_, FollowInfo>(&format!(
        "SELECT f.follower_id AS user_id, u.username, u.display_name, u.avatar_url, f.created_at AS followed_at \
         FROM follows f JOIN users u ON u.id = f.follower_id \
         WHERE f.followed_id = $1 ORDER BY f.created_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

#[allow(dead_code)]
pub fn touch_marker() -> chrono::DateTime<chrono::Utc> {
    now()
}
