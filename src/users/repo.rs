use crate::error::{ApiError, ApiResult};
use crate::users::models::{Role, UpdateProfileRequest, User};
use sqlx::PgPool;
use uuid::Uuid;

const USER_COLS: &str = "id, username, email, display_name, password_hash, role, avatar_url, bio, is_active, email_verified, created_at, updated_at";

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> ApiResult<Option<User>> {
    let user = sqlx::query_as::<_, User>(&format!("SELECT {USER_COLS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> ApiResult<Option<User>> {
    let user = sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLS} FROM users WHERE LOWER(email) = LOWER($1)"
    ))
    .bind(email)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> ApiResult<Option<User>> {
    let user = sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLS} FROM users WHERE LOWER(username) = LOWER($1)"
    ))
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}

pub struct NewUser<'a> {
    pub id: Uuid,
    pub username: &'a str,
    pub email: &'a str,
    pub display_name: &'a str,
    pub password_hash: &'a str,
    pub role: Role,
}

pub async fn insert(pool: &PgPool, new: NewUser<'_>) -> ApiResult<User> {
    let user = sqlx::query_as::<_, User>(&format!(
        "INSERT INTO users (id, username, email, display_name, password_hash, role)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING {USER_COLS}"
    ))
    .bind(new.id)
    .bind(new.username)
    .bind(new.email)
    .bind(new.display_name)
    .bind(new.password_hash)
    .bind(new.role)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::Conflict("username or email already taken".into())
        }
        other => ApiError::from(other),
    })?;
    Ok(user)
}

pub async fn update_profile(
    pool: &PgPool,
    id: Uuid,
    req: &UpdateProfileRequest,
) -> ApiResult<User> {
    let user = sqlx::query_as::<_, User>(&format!(
        "UPDATE users
         SET display_name = COALESCE($2, display_name),
             bio = COALESCE($3, bio),
             avatar_url = CASE WHEN $4 = '' THEN NULL ELSE COALESCE($4, avatar_url) END,
             updated_at = now()
         WHERE id = $1
         RETURNING {USER_COLS}"
    ))
    .bind(id)
    .bind(
        req.display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(req.bio.as_deref())
    .bind(req.avatar_url.as_deref().unwrap_or(""))
    .fetch_one(pool)
    .await?;
    Ok(user)
}

pub async fn update_password(pool: &PgPool, id: Uuid, password_hash: &str) -> ApiResult<()> {
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list(pool: &PgPool, limit: i64, offset: i64) -> ApiResult<Vec<User>> {
    let users = sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLS} FROM users ORDER BY created_at DESC LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(users)
}

#[allow(dead_code)]
pub async fn grant_permission(pool: &PgPool, user_id: Uuid, permission: &str) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO user_permissions (user_id, permission) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(permission)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn permissions_of(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT permission FROM user_permissions WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
