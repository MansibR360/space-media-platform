use super::models::PluginRecord;
use crate::error::{ApiError, ApiResult};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn list(pool: &PgPool) -> ApiResult<Vec<PluginRecord>> {
    Ok(
        sqlx::query_as::<_, PluginRecord>("SELECT * FROM plugins ORDER BY created_at")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn get(pool: &PgPool, plugin_id: Uuid) -> ApiResult<PluginRecord> {
    sqlx::query_as::<_, PluginRecord>("SELECT * FROM plugins WHERE id = $1")
        .bind(plugin_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("plugin"))
}

pub async fn find_by_name(pool: &PgPool, name: &str) -> ApiResult<Option<PluginRecord>> {
    let plugin =
        sqlx::query_as::<_, PluginRecord>("SELECT * FROM plugins WHERE LOWER(name) = LOWER($1)")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(plugin)
}

pub async fn insert(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    kind: &str,
    base_url: &str,
    config: serde_json::Value,
    enabled: bool,
    created_by: Option<Uuid>,
) -> ApiResult<PluginRecord> {
    let plugin = sqlx::query_as::<_, PluginRecord>(
        "INSERT INTO plugins (id, name, kind, transport, base_url, config, enabled, created_by)
         VALUES ($1, $2, $3, 'http', $4, $5, $6, $7)
         RETURNING *",
    )
    .bind(id)
    .bind(name)
    .bind(kind)
    .bind(base_url)
    .bind(config)
    .bind(enabled)
    .bind(created_by)
    .fetch_one(pool)
    .await?;
    Ok(plugin)
}

pub async fn update(
    pool: &PgPool,
    plugin_id: Uuid,
    enabled: Option<bool>,
    base_url: Option<&str>,
    config: Option<serde_json::Value>,
) -> ApiResult<PluginRecord> {
    let plugin = sqlx::query_as::<_, PluginRecord>(
        "UPDATE plugins SET
            enabled = COALESCE($2, enabled),
            base_url = COALESCE($3, base_url),
            config = COALESCE($4, config),
            updated_at = now()
         WHERE id = $1
         RETURNING *",
    )
    .bind(plugin_id)
    .bind(enabled)
    .bind(base_url)
    .bind(config)
    .fetch_one(pool)
    .await?;
    Ok(plugin)
}

/// Insert or refresh a row owned by a code-side registry.
///
/// `enabled` is deliberately only set on insert: the AI ops loop and the admin
/// console disable failing sources, and a startup sweep that re-enabled every
/// row would undo that on each restart. `config` is merged rather than
/// replaced so an operator's `auth_token` / `resolve_only` survive.
pub async fn upsert_managed(
    pool: &PgPool,
    id: Uuid,
    name: &str,
    kind: &str,
    base_url: &str,
    config: serde_json::Value,
) -> ApiResult<PluginRecord> {
    let plugin = sqlx::query_as::<_, PluginRecord>(
        "INSERT INTO plugins (id, name, kind, transport, base_url, config, enabled)
         VALUES ($1, $2, $3, 'http', $4, $5, TRUE)
         ON CONFLICT (name) DO UPDATE SET
            kind = EXCLUDED.kind,
            transport = 'http',
            base_url = EXCLUDED.base_url,
            config = plugins.config || EXCLUDED.config,
            updated_at = now()
         RETURNING *",
    )
    .bind(id)
    .bind(name)
    .bind(kind)
    .bind(base_url)
    .bind(config)
    .fetch_one(pool)
    .await?;
    Ok(plugin)
}

/// Retire managed rows whose source left the registry, leaving hand-registered
/// plugins alone.
pub async fn disable_missing_managed(
    pool: &PgPool,
    managed_by: &str,
    keep: &[String],
) -> ApiResult<u64> {
    let result = sqlx::query(
        "UPDATE plugins SET enabled = FALSE, updated_at = now()
         WHERE config->>'managed_by' = $1 AND enabled AND NOT (name = ANY($2))",
    )
    .bind(managed_by)
    .bind(keep)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn delete(pool: &PgPool, plugin_id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(plugin_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("plugin"));
    }
    Ok(())
}
