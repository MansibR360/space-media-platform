use crate::catalog::models::Media;
use crate::catalog::repo::ListFilter;
use crate::error::ApiResult;
use crate::search::models::{SearchHit, TrendWindow};
use chrono::Utc;
use sqlx::PgPool;

pub async fn full_text(
    pool: &PgPool,
    query_text: &str,
    limit: i64,
    offset: i64,
) -> ApiResult<Vec<SearchHit>> {
    let trimmed = query_text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let hits = sqlx::query_as::<_, SearchHit>(
        // Both ts_rank and similarity return `real`, and SearchHit.score is an
        // f64 — without the cast sqlx refuses to decode FLOAT4 as FLOAT8 and
        // every search fails. Same trap as source_ratings.score in 0032.
        "SELECT m.*, GREATEST(
                ts_rank(m.search_vector, websearch_to_tsquery('english', $1)),
                similarity(LOWER(m.title), LOWER($1))
             )::float8 AS score
         FROM media m
         WHERE m.search_vector @@ websearch_to_tsquery('english', $1)
            OR (LENGTH($1) >= 3 AND m.title % $1)
         ORDER BY score DESC, m.popularity DESC, m.id
         LIMIT $2 OFFSET $3",
    )
    .bind(trimmed)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(hits)
}

pub async fn similar(pool: &PgPool, media_id: uuid::Uuid, limit: i64) -> ApiResult<Vec<Media>> {
    let items = sqlx::query_as::<_, Media>(
        "WITH base_genres AS (
             SELECT genre_id FROM media_genres WHERE media_id = $1
         )
         SELECT m.*
         FROM media m
         JOIN media_genres mg ON mg.media_id = m.id AND mg.genre_id IN (SELECT genre_id FROM base_genres)
         WHERE m.id <> $1
         GROUP BY m.id
         ORDER BY COUNT(mg.genre_id) DESC, m.popularity DESC, m.id
         LIMIT $2",
    )
    .bind(media_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(items)
}

pub async fn trending(
    pool: &PgPool,
    window: TrendWindow,
    limit: i64,
) -> ApiResult<Vec<crate::search::models::SearchHit>> {
    let since = (Utc::now()
        - chrono::Duration::days(match window {
            TrendWindow::Day => 1,
            TrendWindow::Week => 7,
            TrendWindow::Month => 30,
        }));
    // Bind the timestamp itself. `.to_rfc3339()` made this a text parameter,
    // and Postgres has no `timestamptz > text` operator.

    let hits = sqlx::query_as::<_, SearchHit>(
        "SELECT m.*, COALESCE(w.watches, 0)::float8 + COALESCE(f.favs, 0)::float8 * 2 AS score
         FROM media m
         LEFT JOIN (
             SELECT media_id, COUNT(*) AS watches
             FROM watch_history WHERE updated_at > $1 GROUP BY media_id
         ) w ON w.media_id = m.id
         LEFT JOIN (
             SELECT media_id, COUNT(*) AS favs FROM favorites GROUP BY media_id
         ) f ON f.media_id = m.id
         WHERE COALESCE(w.watches, 0) + COALESCE(f.favs, 0) > 0
         ORDER BY score DESC, m.popularity DESC, m.id
         LIMIT $2",
    )
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(hits)
}

#[allow(dead_code)]
pub fn default_discover_filter() -> ListFilter {
    ListFilter::default()
}
