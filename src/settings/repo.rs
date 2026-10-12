use crate::error::{ApiError, ApiResult};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn load_raw(pool: &PgPool, user_id: Uuid) -> ApiResult<Option<serde_json::Value>> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT settings FROM user_settings WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}

pub async fn save(pool: &PgPool, user_id: Uuid, settings: &serde_json::Value) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO user_settings (user_id, settings, updated_at)
         VALUES ($1, $2, now())
         ON CONFLICT (user_id) DO UPDATE SET settings = EXCLUDED.settings, updated_at = now()",
    )
    .bind(user_id)
    .bind(settings)
    .execute(pool)
    .await?;
    Ok(())
}

/// Top-level merge patch: keys present in `patch` replace existing values.
pub fn merge_patch(current: &mut serde_json::Value, patch: serde_json::Value) -> ApiResult<()> {
    let Some(obj) = patch.as_object() else {
        return Err(ApiError::bad_request("settings body must be a JSON object"));
    };
    if !current.is_object() {
        *current = serde_json::json!({});
    }
    let target = current.as_object_mut().expect("checked above");
    for (key, value) in obj {
        if value.is_null() {
            target.remove(key);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub async fn delete_all_for_user(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    sqlx::query("DELETE FROM user_settings WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}
