//! Internal signal recording: every meaningful in-app action becomes one
//! heuristic event. Callers are fire-and-forget — tracking writes must never
//! block or fail because of the learning layer.

use crate::heuristic::models::action_weight;
use crate::heuristic::repo::PlaySignal;
use sha1::{Digest, Sha1};

fn hour_bucket_key(user_id: uuid::Uuid, media_id: uuid::Uuid, bucket: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(user_id.as_bytes());
    hasher.update(media_id.as_bytes());
    hasher.update(bucket.as_bytes());
    hex::encode(&hasher.finalize()[..10])
}

/// A progress save / playback tick: counts as a play (or a completion past the
/// finish line). Saves inside the same hour collapse into one event so
/// aggressive clients cannot inflate play counts.
#[allow(clippy::too_many_arguments)]
pub async fn record_progress(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    media_id: uuid::Uuid,
    episode_id: Option<uuid::Uuid>,
    position_seconds: f64,
    duration_seconds: f64,
    explicitly_completed: bool,
) {
    let percent = if duration_seconds > 0.0 {
        Some((position_seconds / duration_seconds * 100.0).clamp(0.0, 100.0))
    } else {
        None
    };
    let completed = explicitly_completed || percent.map(|p| p >= 92.0).unwrap_or(false);
    let action = if completed { "complete" } else { "play" };
    let effective = if completed && duration_seconds > 0.0 {
        duration_seconds as i32
    } else {
        position_seconds.min(i32::MAX as f64) as i32
    };
    let now = chrono::Utc::now();
    let signal = PlaySignal {
        user_id,
        media_id,
        episode_id,
        action,
        percent,
        rating: None,
        effective_seconds: effective.max(0),
        dedupe_key: Some(hour_bucket_key(
            user_id,
            media_id,
            &format!("{}:{action}", now.format("%Y-%m-%dT%H")),
        )),
    };
    if let Err(err) = crate::heuristic::repo::record(pool, &signal).await {
        tracing::debug!(error = %err, "heuristic progress signal skipped");
    }
}

/// A star rating — a strong taste statement regardless of watch state.
pub async fn record_rating(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    media_id: uuid::Uuid,
    score: f64,
) {
    let signal = PlaySignal {
        user_id,
        media_id,
        episode_id: None,
        action: "rate",
        percent: None,
        rating: Some(score.clamp(0.0, 10.0)),
        effective_seconds: 0,
        dedupe_key: None,
    };
    if let Err(err) = crate::heuristic::repo::record(pool, &signal).await {
        tracing::debug!(error = %err, "heuristic rating signal skipped");
    }
}

/// Favoriting is the loudest positive signal the engine accepts.
pub async fn record_favorite(pool: &sqlx::PgPool, user_id: uuid::Uuid, media_id: uuid::Uuid, favorited: bool) {
    if !favorited {
        return;
    }
    let signal = PlaySignal {
        user_id,
        media_id,
        episode_id: None,
        action: "favorite",
        percent: None,
        rating: None,
        effective_seconds: 0,
        dedupe_key: None,
    };
    if let Err(err) = crate::heuristic::repo::record(pool, &signal).await {
        tracing::debug!(error = %err, "heuristic favorite signal skipped");
    }
}

/// Marking something "dropped" is a negative vote — it suppresses similar
/// picks in predictions without erasing the watch history.
pub async fn record_drop(pool: &sqlx::PgPool, user_id: uuid::Uuid, media_id: uuid::Uuid) {
    let signal = PlaySignal {
        user_id,
        media_id,
        episode_id: None,
        action: "drop",
        percent: None,
        rating: None,
        effective_seconds: 0,
        dedupe_key: None,
    };
    if let Err(err) = crate::heuristic::repo::record(pool, &signal).await {
        tracing::debug!(error = %err, "heuristic drop signal skipped");
    }
}

#[allow(dead_code)]
fn _weight_reference() -> f64 {
    action_weight("play")
}
