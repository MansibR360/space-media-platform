use std::sync::Arc;

use chrono::Timelike;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::recommendations::cache::CacheService;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRecommendations {
    pub generated_at: String,
    pub items: Vec<ScoredMedia>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScoredMedia {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContextSignals {
    /// short | long | binge
    #[serde(default)]
    pub session: Option<String>,
    /// device name/category reported by client
    #[serde(default)]
    pub device: Option<String>,
    /// local hour 0-23 (client clock); server falls back to UTC hour
    #[serde(default)]
    pub local_hour: Option<i32>,
}

pub struct RecommendationService {
    pool: PgPool,
    cache: Arc<CacheService>,
}

const USER_TTL: std::time::Duration = std::time::Duration::from_secs(600);
const SIMILAR_TTL: std::time::Duration = std::time::Duration::from_secs(1800);

impl RecommendationService {
    pub fn new(pool: PgPool, cache: Arc<CacheService>) -> Self {
        Self { pool, cache }
    }

    /// Genre-affinity based recommendations for a user.
    pub async fn for_user(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> anyhow::Result<CachedRecommendations> {
        let cache_key = format!("rec:user:{user_id}:{limit}");
        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(parsed) = serde_json::from_value::<CachedRecommendations>(cached) {
                return Ok(parsed);
            }
        }

        let rows: Vec<(Uuid, String, Option<String>, f64)> = sqlx::query_as(
            "WITH affinity AS (
                 SELECT mg.genre_id, COUNT(*)::float8 AS weight
                 FROM list_entries le
                 JOIN media_genres mg ON mg.media_id = le.media_id
                 WHERE le.user_id = $1
                   AND (le.score IS NULL OR le.score >= 6)
                   AND le.status IN ('watching', 'completed')
                 GROUP BY mg.genre_id
             ),
             seen AS (
                 SELECT DISTINCT media_id FROM list_entries WHERE user_id = $1
                 UNION
                 SELECT DISTINCT media_id FROM watch_history WHERE user_id = $1
             )
             SELECT m.id, m.title, m.poster_url,
                    (SUM(aff.weight) + m.popularity::float8 / 1000.0) AS score
             FROM media m
             JOIN media_genres mg ON mg.media_id = m.id
             JOIN affinity aff ON aff.genre_id = mg.genre_id
             WHERE m.id NOT IN (SELECT media_id FROM seen)
               AND m.status <> 'announced'
             GROUP BY m.id
             ORDER BY score DESC, m.popularity DESC
             LIMIT $2",
        )
        .bind(user_id)
        .bind(limit.clamp(1, 50))
        .fetch_all(&self.pool)
        .await?;

        let result = CachedRecommendations {
            generated_at: chrono::Utc::now().to_rfc3339(),
            items: rows
                .into_iter()
                .map(|(media_id, title, poster_url, score)| ScoredMedia {
                    media_id,
                    title,
                    poster_url,
                    score,
                })
                .collect(),
        };

        self.cache
            .set(&cache_key, serde_json::to_value(&result)?, USER_TTL)
            .await;
        Ok(result)
    }

    /// Similar-media via shared genres; cached per source item.
    pub async fn similar(&self, media_id: Uuid, limit: i64) -> anyhow::Result<Vec<ScoredMedia>> {
        let cache_key = format!("rec:similar:{media_id}:{limit}");
        if let Some(cached) = self.cache.get(&cache_key).await {
            if let Ok(parsed) = serde_json::from_value::<Vec<ScoredMedia>>(cached) {
                return Ok(parsed);
            }
        }

        let rows: Vec<(Uuid, String, Option<String>, f64)> = sqlx::query_as(
            "WITH base AS (
                 SELECT genre_id FROM media_genres WHERE media_id = $1
             ),
             base_studios AS (
                 SELECT studio_id FROM media_studios WHERE media_id = $1
             )
             SELECT m.id, m.title, m.poster_url,
                    (COUNT(DISTINCT mg.genre_id)::float8 * 2.0
                     + CASE WHEN EXISTS (
                         SELECT 1 FROM media_studios ms
                         WHERE ms.media_id = m.id AND ms.studio_id IN (SELECT studio_id FROM base_studios)
                       ) THEN 3.0 ELSE 0.0 END) AS score
             FROM media m
             JOIN media_genres mg ON mg.media_id = m.id AND mg.genre_id IN (SELECT genre_id FROM base)
             WHERE m.id <> $1
             GROUP BY m.id
             ORDER BY score DESC, m.popularity DESC
             LIMIT $2",
        )
        .bind(media_id)
        .bind(limit.clamp(1, 50))
        .fetch_all(&self.pool)
        .await?;

        let items: Vec<ScoredMedia> = rows
            .into_iter()
            .map(|(media_id, title, poster_url, score)| ScoredMedia {
                media_id,
                title,
                poster_url,
                score,
            })
            .collect();

        self.cache
            .set(&cache_key, serde_json::to_value(&items)?, SIMILAR_TTL)
            .await;
        Ok(items)
    }

    #[allow(dead_code)]
    pub async fn invalidate_user(&self, user_id: Uuid) {
        self.cache.invalidate(&format!("rec:user:{user_id}")).await;
    }

    pub async fn contextual_for_user(
        &self,
        user_id: Uuid,
        limit: i64,
        ctx: &ContextSignals,
    ) -> anyhow::Result<Vec<ScoredMedia>> {
        let mut items = self.for_user(user_id, limit * 3).await?.items;

        let hour = ctx
            .local_hour
            .unwrap_or_else(|| chrono::Utc::now().hour() as i32);
        let late_night = hour >= 22 || hour < 5;
        let short_session = ctx.session.as_deref() == Some("short");
        let binge = ctx.session.as_deref() == Some("binge");

        if short_session || late_night {
            // Prefer tight runtimes for quick sessions / late nights.
            let runtimes: std::collections::HashMap<Uuid, Option<i32>> =
                sqlx::query_as("SELECT id, runtime_minutes FROM media WHERE id = ANY($1)")
                    .bind(items.iter().map(|i| i.media_id).collect::<Vec<_>>())
                    .fetch_all(&self.pool)
                    .await?
                    .into_iter()
                    .collect();

            let cap = if late_night { 130 } else { 110 };
            items.sort_by_key(|item| {
                let runtime = runtimes.get(&item.media_id).copied().flatten();
                match runtime {
                    Some(rt) if rt as f64 <= cap as f64 => 0,
                    Some(_) => 1,
                    None => 2,
                }
            });
        }

        if binge {
            // Series first when someone settles in.
            items.reverse();
        }

        items.truncate(limit.max(1) as usize);
        Ok(items)
    }
}
