//! IMDb identity and ratings for the catalog.
//!
//! IMDb has no public API, so the data comes through OMDb, which is registered
//! in the metadata registry under the source key `imdb`. What this module adds
//! on top is the part that matters for a catalog assembled from several
//! providers: **the IMDb id is stored on the media row itself**, so a title
//! imported from TMDB, from a plugin, or by hand can all be enriched from IMDb
//! afterwards.
//!
//! Two entry points:
//!   * [`apply_detail`] — called during import, persists whatever the provider
//!     already knew.
//!   * [`sync_one`] / [`sync_stale`] — call OMDb for fresh ratings. Ratings
//!     move, so they are re-fetched on a schedule rather than frozen at import.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::catalog::metadata::ExternalDetail;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// How long a rating is considered fresh. IMDb scores drift slowly; a weekly
/// refresh keeps them current without burning the OMDb daily quota.
const FRESH_FOR_DAYS: i64 = 7;

/// The IMDb facts for one title.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ImdbRatings {
    pub media_id: Uuid,
    pub imdb_id: Option<String>,
    pub imdb_rating: Option<f64>,
    pub imdb_votes: Option<i32>,
    pub metascore: Option<i32>,
    pub content_rating: Option<String>,
    pub imdb_synced_at: Option<DateTime<Utc>>,
}

/// Persist whatever IMDb data an import already carried.
///
/// Every field is written with COALESCE against the incoming value, so a
/// provider that knows only the id cannot blank out ratings fetched earlier.
pub async fn apply_detail(
    pool: &PgPool,
    media_id: Uuid,
    detail: &ExternalDetail,
) -> ApiResult<()> {
    if detail.imdb_id.is_none() && detail.imdb_rating.is_none() {
        return Ok(());
    }

    // Only stamp `imdb_synced_at` when an actual rating arrived; an id alone
    // must leave the row due for a sync.
    sqlx::query(
        "UPDATE media SET
            imdb_id        = COALESCE($2, imdb_id),
            imdb_rating    = COALESCE($3, imdb_rating),
            imdb_votes     = COALESCE($4, imdb_votes),
            metascore      = COALESCE($5, metascore),
            content_rating = COALESCE($6, content_rating),
            imdb_synced_at = CASE WHEN $3 IS NULL THEN imdb_synced_at ELSE now() END,
            updated_at     = now()
         WHERE id = $1",
    )
    .bind(media_id)
    .bind(&detail.imdb_id)
    .bind(detail.imdb_rating)
    .bind(detail.imdb_votes)
    .bind(detail.metascore)
    .bind(&detail.content_rating)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn ratings_for(pool: &PgPool, media_id: Uuid) -> ApiResult<Option<ImdbRatings>> {
    let row: Option<(
        Uuid,
        Option<String>,
        Option<f64>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as(
        "SELECT id, imdb_id, imdb_rating, imdb_votes, metascore, content_rating, imdb_synced_at
         FROM media WHERE id = $1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(media_id, imdb_id, imdb_rating, imdb_votes, metascore, content_rating, imdb_synced_at)| {
            ImdbRatings {
                media_id,
                imdb_id,
                imdb_rating,
                imdb_votes,
                metascore,
                content_rating,
                imdb_synced_at,
            }
        },
    ))
}

/// Fetch fresh IMDb data for one title.
///
/// Resolves the IMDb id from, in order: the media row, an `imdb` external
/// mapping, then a title search. The resolved id is written back, so the next
/// sync is a direct lookup.
pub async fn sync_one(state: &AppState, media_id: Uuid) -> ApiResult<ImdbRatings> {
    let provider = state
        .metadata
        .read()
        .await
        .provider("imdb")
        .ok_or_else(|| ApiError::Unavailable("imdb provider is not configured".into()))?;

    let imdb_id = resolve_id(&state.pool, media_id).await?;
    let detail = provider.details(&imdb_id).await?;

    sqlx::query(
        "UPDATE media SET
            imdb_id        = $2,
            imdb_rating    = $3,
            imdb_votes     = $4,
            metascore      = COALESCE($5, metascore),
            content_rating = COALESCE($6, content_rating),
            imdb_synced_at = now(),
            updated_at     = now()
         WHERE id = $1",
    )
    .bind(media_id)
    // Prefer the id OMDb echoed back: a title search resolves to the canonical
    // tconst, which is what should be stored.
    .bind(detail.imdb_id.clone().unwrap_or(imdb_id))
    .bind(detail.imdb_rating)
    .bind(detail.imdb_votes)
    .bind(detail.metascore)
    .bind(&detail.content_rating)
    .execute(&state.pool)
    .await?;

    ratings_for(&state.pool, media_id)
        .await?
        .ok_or_else(|| ApiError::not_found("media"))
}

/// Find the IMDb id to query for a title.
async fn resolve_id(pool: &PgPool, media_id: Uuid) -> ApiResult<String> {
    let row: Option<(Option<String>, String, Option<chrono::NaiveDate>)> = sqlx::query_as(
        "SELECT m.imdb_id, m.title, m.release_date FROM media m WHERE m.id = $1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    let Some((imdb_id, title, _release)) = row else {
        return Err(ApiError::not_found("media"));
    };

    if let Some(id) = imdb_id.filter(|id| id.starts_with("tt")) {
        return Ok(id);
    }

    // An earlier import may have recorded the mapping without promoting it to
    // the media row.
    let mapped: Option<(String,)> = sqlx::query_as(
        "SELECT external_id FROM external_mappings
         WHERE media_id = $1 AND source = 'imdb' AND external_id LIKE 'tt%'
         LIMIT 1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    if let Some((id,)) = mapped {
        return Ok(id);
    }

    // Last resort: OMDb accepts a title in place of an id, and returns the
    // canonical tconst with it.
    Ok(title)
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SyncReport {
    pub attempted: usize,
    pub updated: usize,
    pub failed: usize,
}

/// Refresh titles whose ratings are missing or stale.
///
/// Titles that already carry an IMDb id come first — those are a direct lookup
/// and always succeed — before falling back to title matching.
pub async fn sync_stale(state: &AppState, limit: i64) -> ApiResult<SyncReport> {
    let cutoff = Utc::now() - Duration::days(FRESH_FOR_DAYS);

    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM media
         WHERE media_type IN ('movie', 'series')
           AND (imdb_synced_at IS NULL OR imdb_synced_at < $2)
         ORDER BY (imdb_id IS NULL), imdb_synced_at NULLS FIRST, popularity DESC
         LIMIT $1",
    )
    .bind(limit.clamp(1, 500))
    .bind(cutoff)
    .fetch_all(&state.pool)
    .await?;

    let attempted = ids.len();
    let mut updated = 0usize;
    let mut failed = 0usize;

    for (media_id,) in ids {
        match sync_one(state, media_id).await {
            Ok(_) => updated += 1,
            Err(e) => {
                // A title with no IMDb match is normal, not an outage. Stamp it
                // so the sweep moves on instead of retrying it every run.
                tracing::debug!(%media_id, error = %e, "imdb sync skipped");
                let _ = sqlx::query("UPDATE media SET imdb_synced_at = now() WHERE id = $1")
                    .bind(media_id)
                    .execute(&state.pool)
                    .await;
                failed += 1;
            }
        }
    }

    Ok(SyncReport {
        attempted,
        updated,
        failed,
    })
}
