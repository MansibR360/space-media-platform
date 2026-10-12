//! Personal Media Twin (Phase H).
//!
//! A private, evolving model of how a person consumes media — not just what.
//! Computed from behavioral signals (formats, completion behavior, pace,
//! creators, discovery vs rewatch balance, current interests) and snapshotted
//! at most once per day so drift over time is visible.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct FormatAffinity {
    pub media_type: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct GenreWeight {
    pub genre: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct CreatorAffinity {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MediaTwin {
    /// What they consume: movie/series/book/manga/music…
    pub formats: Vec<FormatAffinity>,
    /// What they like: genre weights normalized to the top genre = 100.
    pub genres: Vec<GenreWeight>,
    /// Share of started items actually finished (0–100).
    pub completion_rate: f64,
    /// Preferred pace in minutes; None when unknowable yet.
    pub preferred_runtime_minutes: Option<f64>,
    /// Studios behind the media they keep finishing.
    pub creators: Vec<CreatorAffinity>,
    /// New items started in the last 30 days vs total consumed — discovery appetite.
    pub discovery_ratio_30d: f64,
    /// Genres of activity from the last two weeks ("what's hot right now").
    pub current_interests: Vec<GenreWeight>,
    pub computed_at: chrono::DateTime<chrono::Utc>,
}

pub async fn compute(pool: &PgPool, user_id: Uuid) -> ApiResult<MediaTwin> {
    let formats: Vec<FormatAffinity> = sqlx::query_as(
        "SELECT m.media_type, COUNT(DISTINCT m.id)::int8 AS count
         FROM (SELECT media_id FROM watch_history WHERE user_id = $1
               UNION SELECT media_id FROM list_entries WHERE user_id = $1
               UNION SELECT media_id FROM reading_progress WHERE user_id = $1) seen
         JOIN media m ON m.id = seen.media_id
         GROUP BY m.media_type ORDER BY count DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let genres: Vec<GenreWeight> = sqlx::query_as(
        "SELECT g.name, COUNT(*)::float8 AS weight
         FROM (SELECT media_id FROM watch_history WHERE user_id = $1
               UNION SELECT media_id FROM list_entries WHERE user_id = $2
               UNION SELECT media_id FROM reading_progress WHERE user_id = $1) seen
         JOIN media_genres mg ON mg.media_id = seen.media_id
         JOIN genres g ON g.id = mg.genre_id
         GROUP BY g.name ORDER BY weight DESC LIMIT 8",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let max_genre = genres.first().map(|g| g.weight).unwrap_or(1.0).max(1.0);
    let genres: Vec<GenreWeight> = genres
        .into_iter()
        .map(|g| GenreWeight {
            genre: g.genre,
            weight: (g.weight / max_genre * 100.0).round(),
        })
        .collect();

    // Aggregate-without-GROUP-BY always returns exactly one row.
    let completion: (f64, f64) = sqlx::query_as(
        "WITH started AS (
             SELECT DISTINCT media_id FROM watch_history WHERE user_id = $1
             UNION SELECT media_id FROM list_entries WHERE user_id = $1 AND status IN ('watching','completed','on_hold')
             UNION SELECT media_id FROM reading_progress WHERE user_id = $1
         ),
         finished AS (
             SELECT media_id FROM watch_history WHERE user_id = $1 AND completed
             UNION SELECT media_id FROM list_entries WHERE user_id = $1 AND status = 'completed'
             UNION SELECT media_id FROM reading_progress WHERE user_id = $1 AND completed
         )
         SELECT (SELECT COUNT(*) FROM finished)::float8 AS done,
                GREATEST((SELECT COUNT(*) FROM started), 1)::float8 AS all_started",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    let completion_rate = (completion.0 / completion.1.max(1.0) * 100.0).round();

    let runtime: (Option<f64>,) = sqlx::query_as(
        "SELECT AVG(m.runtime_minutes)::float8
         FROM list_entries le JOIN media m ON m.id = le.media_id
         WHERE le.user_id = $1 AND m.runtime_minutes IS NOT NULL",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let creators: Vec<CreatorAffinity> = sqlx::query_as(
        "SELECT s.name, COUNT(DISTINCT ms.media_id)::int8 AS count
         FROM (SELECT media_id FROM list_entries WHERE user_id = $1 AND status IN ('watching','completed')
               UNION SELECT media_id FROM watch_history WHERE user_id = $1 AND completed) liked
         JOIN media_studios ms ON ms.media_id = liked.media_id
         JOIN studios s ON s.id = ms.studio_id
         GROUP BY s.name ORDER BY count DESC LIMIT 5",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let discovery: (f64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT media_id)::float8 FROM (
             SELECT media_id, MIN(created_at) AS first_seen FROM (
                 SELECT media_id, created_at FROM watch_history WHERE user_id = $1
                 UNION ALL SELECT media_id, created_at FROM list_entries WHERE user_id = $1
                 UNION ALL SELECT media_id, now() FROM reading_progress WHERE user_id = $1
             ) all_activity GROUP BY media_id
         ) firsts WHERE first_seen > now() - INTERVAL '30 days'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    let total_formats: f64 = formats.iter().map(|f| f.count as f64).sum();
    let discovery_ratio_30d = if total_formats > 0.0 {
        (discovery.0 / total_formats * 100.0).round()
    } else {
        0.0
    };

    let mut interests: Vec<GenreWeight> = sqlx::query_as(
        "SELECT g.name, COUNT(*)::float8 AS weight
         FROM (SELECT media_id, updated_at FROM watch_history WHERE user_id = $1
               UNION ALL SELECT media_id, updated_at FROM reading_progress WHERE user_id = $1) recent
         JOIN media_genres mg ON mg.media_id = recent.media_id
         JOIN genres g ON g.id = mg.genre_id
         WHERE recent.updated_at > now() - INTERVAL '14 days'
         GROUP BY g.name ORDER BY weight DESC LIMIT 4",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let max_interest = interests.first().map(|i| i.weight).unwrap_or(1.0).max(1.0);
    for w in &mut interests {
        w.weight = (w.weight / max_interest * 100.0).round();
    }

    Ok(MediaTwin {
        formats,
        genres,
        completion_rate,
        preferred_runtime_minutes: runtime.0,
        creators,
        discovery_ratio_30d,
        current_interests: interests,
        computed_at: chrono::Utc::now(),
    })
}

/// Compute + persist a daily snapshot. Snapshot writes are best-effort.
pub async fn twin_with_snapshot(pool: &PgPool, user_id: Uuid) -> ApiResult<MediaTwin> {
    let twin = compute(pool, user_id).await?;
    let payload = serde_json::to_value(&twin)
        .unwrap_or_else(|_| serde_json::json!({}));
    let _ = sqlx::query(
        "INSERT INTO twin_snapshots (id, user_id, snapshot_date, twin)
         VALUES ($1, $2, CURRENT_DATE, $3)
         ON CONFLICT (user_id, snapshot_date) DO UPDATE SET twin = EXCLUDED.twin",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(&payload)
    .execute(pool)
    .await;
    Ok(twin)
}
