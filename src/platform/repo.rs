use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, random_hex, sha256_hex};

// ---------------- API keys ----------------

pub const KEY_SCOPES: &[&str] = &["read", "write", "downloads"];

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ApiKeyCreated {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    /// Shown exactly once.
    pub key: String,
    pub scopes: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub scopes: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub struct ApiKeyAuth {
    pub user_id: Uuid,
    pub role: crate::users::models::Role,
    pub scopes: Vec<String>,
    pub key_id: Uuid,
}

pub fn valid_scopes(scopes: &str) -> bool {
    !scopes.is_empty() && scopes.split(',').all(|s| KEY_SCOPES.contains(&s.trim()))
}

/// Creates `sk_<prefix>_<secret>`; only the SHA-256 hash is stored.
pub async fn create_api_key(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    scopes: &str,
) -> ApiResult<ApiKeyCreated> {
    if name.trim().is_empty() || name.len() > 60 {
        return Err(ApiError::bad_request("key name must be 1-60 characters"));
    }
    if !valid_scopes(scopes) {
        return Err(ApiError::bad_request(format!(
            "scopes must be a comma-separated subset of: {}",
            KEY_SCOPES.join(", ")
        )));
    }

    let prefix = random_hex(4);
    let secret = random_hex(24);
    let key = format!("sk_{prefix}_{secret}");

    let row = sqlx::query_as::<_, ApiKeyCreated>(
        "INSERT INTO api_keys (id, user_id, name, key_hash, prefix, scopes)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, name, prefix, $7 AS key, scopes, created_at",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(name.trim())
    .bind(sha256_hex(&secret))
    .bind(&prefix)
    .bind(scopes)
    .bind(&key)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Resolves an incoming `sk_...` key to its owner + scopes.
pub async fn authenticate_api_key(pool: &PgPool, raw: &str) -> ApiResult<Option<ApiKeyAuth>> {
    let Some((_, rest)) = raw.split_once('_') else {
        return Ok(None);
    };
    let Some((prefix, secret)) = rest.split_once('_') else {
        return Ok(None);
    };

    let row: Option<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT k.id, k.user_id, u.role::text as role_text, k.scopes
         FROM api_keys k JOIN users u ON u.id = k.user_id
         WHERE k.prefix = $1 AND k.key_hash = $2 AND k.revoked_at IS NULL",
    )
    .bind(prefix)
    .bind(sha256_hex(secret))
    .fetch_optional(pool)
    .await?;

    let Some((key_id, user_id, role_text, scopes)) = row else {
        // Constant-ish time: even on miss we hash above; fine.
        return Ok(None);
    };

    let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
        .bind(key_id)
        .execute(pool)
        .await;

    let role = match role_text.as_str() {
        "admin" => crate::users::models::Role::Admin,
        "moderator" => crate::users::models::Role::Moderator,
        _ => crate::users::models::Role::User,
    };

    Ok(Some(ApiKeyAuth {
        user_id,
        role,
        scopes: scopes.split(',').map(str::trim).map(String::from).collect(),
        key_id,
    }))
}

pub async fn list_keys(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<ApiKeyView>> {
    Ok(sqlx::query_as::<_, ApiKeyView>(
        "SELECT id, name, prefix, scopes, created_at, last_used_at FROM api_keys \
         WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn revoke_key(pool: &PgPool, user_id: Uuid, key_id: Uuid) -> ApiResult<()> {
    let result = sqlx::query(
        "UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("api key"));
    }
    Ok(())
}

// ---------------- devices ----------------

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub last_ip: Option<String>,
    pub registered_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

pub async fn register_device(
    pool: &PgPool,
    user_id: Uuid,
    device_id: Option<Uuid>,
    name: &str,
    platform: &str,
    ip: &str,
) -> ApiResult<Device> {
    let device = match device_id {
        Some(id) => {
            sqlx::query_as::<_, Device>(
                "UPDATE devices SET name = COALESCE(NULLIF($2,''), name), last_ip = $3, last_seen_at = now()
                 WHERE id = $1 AND user_id = $4 RETURNING *",
            )
            .bind(id)
            .bind(name)
            .bind(ip)
            .bind(user_id)
            .fetch_optional(pool)
            .await?
        }
        None => None,
    };

    if let Some(d) = device {
        return Ok(d);
    }
    let device = sqlx::query_as::<_, Device>(
        "INSERT INTO devices (id, user_id, name, platform, last_ip)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET last_seen_at = now(), last_ip = EXCLUDED.last_ip
         RETURNING *",
    )
    .bind(device_id.unwrap_or_else(new_id))
    .bind(user_id)
    .bind(if name.trim().is_empty() {
        "Unnamed device"
    } else {
        name.trim()
    })
    .bind(platform)
    .bind(ip)
    .fetch_one(pool)
    .await?;
    Ok(device)
}

pub async fn touch_device(pool: &PgPool, user_id: Uuid, device_id: Uuid) {
    let _ = sqlx::query(
        "UPDATE devices SET last_seen_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(device_id)
    .bind(user_id)
    .execute(pool)
    .await;
}

pub async fn list_devices(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<Device>> {
    Ok(sqlx::query_as::<_, Device>(
        "SELECT * FROM devices WHERE user_id = $1 AND revoked_at IS NULL ORDER BY last_seen_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn revoke_device(pool: &PgPool, user_id: Uuid, device_id: Uuid) -> ApiResult<()> {
    let result =
        sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND user_id = $2")
            .bind(device_id)
            .bind(user_id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("device"));
    }
    Ok(())
}

// ---------------- notifications ----------------

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub data: serde_json::Value,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Internal event sink — call from any service trigger.
pub async fn notify(
    pool: &PgPool,
    user_id: Uuid,
    hub: &crate::chat::hub::Hub,
    kind: &str,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> ApiResult<()> {
    let notification = sqlx::query_as::<_, Notification>(
        "INSERT INTO notifications (id, user_id, kind, title, body, data)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(kind)
    .bind(title)
    .bind(body)
    .bind(data)
    .fetch_one(pool)
    .await?;

    let envelope = serde_json::json!({ "type": "notification", "data": notification }).to_string();
    // Global feed for live dashboards…
    hub.publish_or_create(crate::chat::hub::GLOBAL_ROOM, envelope.clone());
    // …plus the recipient's personal room so only they receive it live.
    hub.publish_or_create(&format!("user:{user_id}"), envelope);
    Ok(())
}

pub async fn list_notifications(
    pool: &PgPool,
    user_id: Uuid,
    unread_only: bool,
    limit: i64,
) -> ApiResult<Vec<Notification>> {
    let clause = if unread_only {
        " AND read_at IS NULL"
    } else {
        ""
    };
    Ok(sqlx::query_as::<_, Notification>(&format!(
        "SELECT * FROM notifications WHERE user_id = $1{clause} ORDER BY created_at DESC LIMIT $2"
    ))
    .bind(user_id)
    .bind(limit.clamp(1, 100))
    .fetch_all(pool)
    .await?)
}

pub async fn mark_notification_read(
    pool: &PgPool,
    user_id: Uuid,
    notification_id: Uuid,
) -> ApiResult<()> {
    sqlx::query("UPDATE notifications SET read_at = now() WHERE id = $1 AND user_id = $2")
        .bind(notification_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}
