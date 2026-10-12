use crate::error::{ApiError, ApiResult};
use crate::intelligence::models::SmartCollection;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create_collection(
    pool: &PgPool,
    owner_id: Uuid,
    name: &str,
    description: &str,
    rules: &serde_json::Value,
    visibility: &str,
) -> ApiResult<SmartCollection> {
    if !matches!(visibility, "private" | "public") {
        return Err(ApiError::bad_request(
            "visibility must be private or public",
        ));
    }
    let collection = sqlx::query_as::<_, SmartCollection>(
        "INSERT INTO smart_collections (id, owner_id, name, description, rules, visibility)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING *",
    )
    .bind(crate::utils::new_id())
    .bind(owner_id)
    .bind(name)
    .bind(description)
    .bind(rules)
    .bind(visibility)
    .fetch_one(pool)
    .await?;
    Ok(collection)
}

pub async fn get_collection(
    pool: &PgPool,
    collection_id: Uuid,
) -> ApiResult<Option<SmartCollection>> {
    let collection =
        sqlx::query_as::<_, SmartCollection>("SELECT * FROM smart_collections WHERE id = $1")
            .bind(collection_id)
            .fetch_optional(pool)
            .await?;
    Ok(collection)
}

#[allow(dead_code)]
pub async fn my_collections(pool: &PgPool, owner_id: Uuid) -> ApiResult<Vec<SmartCollection>> {
    Ok(sqlx::query_as::<_, SmartCollection>(
        "SELECT * FROM smart_collections WHERE owner_id = $1 ORDER BY created_at DESC",
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await?)
}

#[allow(dead_code)]
pub async fn public_collections(pool: &PgPool, limit: i64) -> ApiResult<Vec<SmartCollection>> {
    Ok(sqlx::query_as::<_, SmartCollection>(
        "SELECT * FROM smart_collections WHERE visibility = 'public' \
         ORDER BY updated_at DESC LIMIT $1",
    )
    .bind(limit.clamp(1, 50))
    .fetch_all(pool)
    .await?)
}

pub async fn delete_collection(pool: &PgPool, actor: Uuid, collection_id: Uuid) -> ApiResult<()> {
    let result =
        sqlx::query("DELETE FROM smart_collections WHERE id = $1 AND (owner_id = $2 OR $3)")
            .bind(collection_id)
            .bind(actor)
            .bind(false)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("collection"));
    }
    Ok(())
}
