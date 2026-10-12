//! Predictive cache warming (Phase H).
//!
//! Every 10 minutes the backend predicts demand and prepares before anyone
//! asks: recommendation caches for currently-online users, similar-media
//! caches for what's trending this week. Users never see it; they just get
//! fast rails.

use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub fn spawn_loop(
    pool: PgPool,
    recommendations: Arc<crate::recommendations::service::RecommendationService>,
    hub: Arc<crate::chat::hub::Hub>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(600)) => {
                    warm_tick(&pool, &recommendations, &hub).await;
                }
            }
        }
    });
}

async fn warm_tick(
    pool: &PgPool,
    recommendations: &Arc<crate::recommendations::service::RecommendationService>,
    hub: &Arc<crate::chat::hub::Hub>,
) {
    let started = std::time::Instant::now();
    let mut warmed = 0usize;

    // 1. Online users → precompute their personal rails.
    let online = hub.online_users();
    for user_id in online.iter().take(50) {
        if recommendations.for_user(*user_id, 12).await.is_ok() {
            warmed += 1;
        }
    }

    // 2. Trending this week → precompute similar-media shelves.
    let trending: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT media_id FROM watch_history
         WHERE updated_at > now() - INTERVAL '7 days'
         GROUP BY media_id ORDER BY COUNT(*) DESC LIMIT 20",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (media_id,) in trending {
        if recommendations.similar(media_id, 8).await.is_ok() {
            warmed += 1;
        }
    }

    if warmed > 0 {
        metrics::counter!("cache_warmed_total").increment(warmed as u64);
        metrics::histogram!("cache_warm_duration_seconds").record(started.elapsed().as_secs_f64());
        tracing::debug!(warmed, elapsed_ms = started.elapsed().as_millis() as u64, "cache warmed");
    }
}
