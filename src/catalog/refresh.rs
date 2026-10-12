//! Metadata refresh scheduler + change history + confidence scoring (M3).

use axum::Json;
use axum::extract::State;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::state::AppState;

/// 0..1 — title similarity plus presence bonuses.
pub fn compute_confidence(
    local_title: &str,
    provided_title: &str,
    has_overview: bool,
    has_poster: bool,
) -> f32 {
    let a = local_title.trim().to_lowercase();
    let b = provided_title.trim().to_lowercase();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let max_len = a.chars().count().max(b.chars().count()) as f32;
    let distance = crate::search::suggest::levenshtein(&a, &b, 12) as f32;
    let similarity = 1.0 - (distance / max_len.max(1.0));
    let mut score = similarity * 0.6;
    if has_overview {
        score += 0.15;
    }
    if has_poster {
        score += 0.25;
    }
    score.clamp(0.0, 1.0)
}

pub async fn record_change(
    pool: &PgPool,
    media_id: Uuid,
    field: &str,
    old: Option<&str>,
    new: Option<&str>,
    source: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO metadata_changes (id, media_id, field, old_value, new_value, source)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(crate::utils::new_id())
    .bind(media_id)
    .bind(field)
    .bind(old)
    .bind(new)
    .bind(source)
    .execute(pool)
    .await;
}

/// Refreshes up to `limit` mappings older than 7 days via their provider's
/// details; records changed fields and updates confidence scores.
pub async fn refresh_stale(state: &AppState, limit: i64) -> ApiResult<usize> {
    let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "SELECT e.id, e.media_id, e.source, e.external_id FROM external_mappings e \
         WHERE e.synced_at < now() - INTERVAL '7 days' LIMIT $1",
    )
    .bind(limit.clamp(1, 100))
    .fetch_all(&state.pool)
    .await?;

    let mut refreshed = 0usize;
    for (mapping_id, media_id, source, external_id) in rows {
        let provider = match state.metadata.read().await.provider(&source) {
            Some(p) => p,
            None => continue,
        };
        let detail = match provider.details(&external_id).await {
            Ok(d) => d,
            Err(err) => {
                tracing::debug!(%source, %external_id, error = %err, "refresh skipped");
                continue;
            }
        };

        let current_title: Option<String> =
            sqlx::query_scalar("SELECT title FROM media WHERE id = $1")
                .bind(media_id)
                .fetch_optional(&state.pool)
                .await?
                .flatten();

        if let Some(old_title) = &current_title {
            if *old_title != detail.title {
                record_change(
                    &state.pool,
                    media_id,
                    "title",
                    Some(old_title),
                    Some(detail.title.as_str()),
                    &provider.source(),
                )
                .await;
            }
        }

        sqlx::query(
            "UPDATE media SET overview = COALESCE(NULLIF($2,''), overview), \
             poster_url = COALESCE($3, poster_url), updated_at = now() WHERE id = $1",
        )
        .bind(media_id)
        .bind(&detail.overview)
        .bind(&detail.poster)
        .execute(&state.pool)
        .await?;

        let confidence = compute_confidence(
            current_title.as_deref().unwrap_or(""),
            &detail.title,
            !detail.overview.is_empty(),
            detail.poster.is_some(),
        );
        sqlx::query(
            "UPDATE external_mappings SET confidence = $2, synced_at = now() WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(confidence)
        .execute(&state.pool)
        .await?;

        refreshed += 1;
    }
    metrics::counter!("metadata_refresh_total").increment(refreshed as u64);
    Ok(refreshed)
}

pub async fn spawn_scheduler(state: std::sync::Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            if let Err(err) = refresh_stale(&state, 25).await {
                tracing::debug!(error = %err, "metadata refresh pass failed");
            }
        }
    });
}

#[utoipa::path(post, path = "/api/admin/metadata/refresh", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn refresh_now(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::middleware::AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let refreshed = refresh_stale(&state, 25).await?;
    Ok(Json(json!({ "refreshed": refreshed })))
}

// ---------------- conflict detection (Phase 45 slice) ----------------

#[derive(Debug, serde::Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ConflictRow {
    pub media_id: Uuid,
    pub title: String,
    pub mapping_count: i64,
    pub min_confidence: f32,
}

#[utoipa::path(get, path = "/api/admin/quality/conflicts", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn quality_conflicts(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::middleware::AdminUser,
) -> ApiResult<Json<Vec<ConflictRow>>> {
    let rows = sqlx::query_as::<_, ConflictRow>(
        "SELECT m.id AS media_id, m.title, COUNT(e.id)::bigint AS mapping_count, \
                COALESCE(MIN(e.confidence), 0)::real AS min_confidence
         FROM media m JOIN external_mappings e ON e.media_id = m.id
         GROUP BY m.id HAVING COUNT(e.id) > 1 OR MIN(e.confidence) < 0.5
         ORDER BY mapping_count DESC, min_confidence ASC LIMIT 50",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}
