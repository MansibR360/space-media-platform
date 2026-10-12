use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::music::models::{
    AnalysisView, DatasetRow, MusicListen, MusicRecommendation, MusicTaste, RecordListen,
    TrackStats,
};
use crate::music::{analysis, repo, service};
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct LimitQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Report one play. Called on track end, skip, or when the player unmounts.
#[utoipa::path(post, path = "/api/music/listen", tag = "music",
    request_body = RecordListen, security(("bearer" = [])),
    responses((status = 200, body = MusicListen)))]
pub async fn record_listen(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(body): Json<RecordListen>,
) -> ApiResult<Json<MusicListen>> {
    let listen = service::record_listen(&state.pool, auth.id(), body).await?;
    Ok(Json(listen))
}

/// The learned music profile: genres, artists, tempo and energy preferences,
/// habits. Personal to the caller.
#[utoipa::path(get, path = "/api/music/taste", tag = "music",
    security(("bearer" = [])), responses((status = 200, body = MusicTaste)))]
pub async fn taste(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<MusicTaste>> {
    Ok(Json(service::taste(&state.pool, auth.id()).await?))
}

/// Personalized picks. Sharpens as the listen log grows.
#[utoipa::path(get, path = "/api/music/recommendations", tag = "music",
    params(LimitQuery), security(("bearer" = [])),
    responses((status = 200, body = [MusicRecommendation])))]
pub async fn recommendations(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<MusicRecommendation>>> {
    let items = service::recommendations(&state.pool, auth.id(), q.limit.unwrap_or(20)).await?;
    Ok(Json(items))
}

/// Recent plays for this user.
#[utoipa::path(get, path = "/api/music/history", tag = "music",
    params(LimitQuery), security(("bearer" = [])),
    responses((status = 200, body = [MusicListen])))]
pub async fn history(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<MusicListen>>> {
    let rows =
        repo::recent_listens(&state.pool, auth.id(), q.limit.unwrap_or(50).clamp(1, 200)).await?;
    Ok(Json(rows))
}

/// Audio analysis for one track: waveform, key, tempo, descriptors.
///
/// Global data — the same response for every caller, so it is cacheable and
/// needs no authentication.
#[utoipa::path(get, path = "/api/music/{media_id}/analysis", tag = "music",
    responses((status = 200, body = AnalysisView), (status = 404)))]
pub async fn get_analysis(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<AnalysisView>> {
    repo::get_analysis(&state.pool, media_id)
        .await?
        .map(|a| Json(AnalysisView::from(a)))
        .ok_or_else(|| ApiError::not_found("analysis"))
}

/// Global engagement for one track, across every listener.
#[utoipa::path(get, path = "/api/music/{media_id}/stats", tag = "music",
    responses((status = 200, body = TrackStats), (status = 404)))]
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<TrackStats>> {
    repo::track_stats(&state.pool, media_id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("track stats"))
}

/// Tracks that sound like this one, from the content embedding. Works on a
/// brand new upload, before anybody has played it.
#[utoipa::path(get, path = "/api/music/{media_id}/similar", tag = "music",
    params(LimitQuery), responses((status = 200, body = [MusicRecommendation])))]
pub async fn similar(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<Uuid>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<MusicRecommendation>>> {
    let limit = q.limit.unwrap_or(12).clamp(1, 50);
    let items = service::similar_to(&state.pool, media_id, limit).await?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ChartQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// `true` ranks by 7-day momentum instead of all-time quality.
    #[serde(default)]
    pub trending: bool,
}

/// The global charts. Identical for every caller — this is the cross-user
/// layer, and what a listener with no history is shown.
#[utoipa::path(get, path = "/api/music/charts", tag = "music",
    params(ChartQuery), responses((status = 200, body = [MusicRecommendation])))]
pub async fn charts(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ChartQuery>,
) -> ApiResult<Json<Vec<MusicRecommendation>>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let items = service::charts(&state.pool, limit, q.trending).await?;
    Ok(Json(items))
}

// ------------------------------------------------------------------ admin

/// Analyze one track now. Runs inline so the caller learns whether ffmpeg is
/// actually available, rather than silently queueing into a void.
#[utoipa::path(post, path = "/api/admin/music/{media_id}/analyze", tag = "music",
    security(("bearer" = [])), responses((status = 200, body = AnalysisView)))]
pub async fn analyze_one(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<AnalysisView>> {
    let Some((file_id, path)) = repo::audio_file_for(&state.pool, media_id).await? else {
        return Err(ApiError::not_found("audio file for this track"));
    };

    let result = analysis::analyze_file(
        media_id,
        Some(file_id),
        std::path::Path::new(&path),
        &state.cfg.ffprobe_bin,
        &state.cfg.ffmpeg_bin,
    )
    .await;

    repo::save_analysis(&state.pool, &result).await?;
    tracing::info!(%media_id, summary = %analysis::describe(&result), "analyzed track");

    if let Err(e) = repo::refresh_similarity(&state.pool, media_id, 50).await {
        tracing::warn!(error = %e, %media_id, "similarity refresh failed");
    }

    Ok(Json(AnalysisView::from(result)))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct BatchResult {
    pub claimed: usize,
    pub analyzed: usize,
    pub failed: usize,
}

/// Analyze a batch of tracks that are new or were analyzed by an older
/// version. Safe to call repeatedly; each call claims only tracks no other run
/// has taken.
#[utoipa::path(post, path = "/api/admin/music/analyze-pending", tag = "music",
    params(LimitQuery), security(("bearer" = [])),
    responses((status = 200, body = BatchResult)))]
pub async fn analyze_pending(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<BatchResult>> {
    let limit = q.limit.unwrap_or(25).clamp(1, 200);
    let ids = repo::claim_unanalyzed(&state.pool, limit, analysis::ANALYSIS_VERSION).await?;
    let claimed = ids.len();
    let mut analyzed = 0usize;
    let mut failed = 0usize;

    for media_id in ids {
        match repo::audio_file_for(&state.pool, media_id).await? {
            Some((file_id, path)) => {
                let result = analysis::analyze_file(
                    media_id,
                    Some(file_id),
                    std::path::Path::new(&path),
                    &state.cfg.ffprobe_bin,
                    &state.cfg.ffmpeg_bin,
                )
                .await;
                let ok = result.waveform.is_some() || result.duration_seconds.is_some();
                repo::save_analysis(&state.pool, &result).await?;
                if ok {
                    analyzed += 1;
                    if let Err(e) = repo::refresh_similarity(&state.pool, media_id, 50).await {
                        tracing::warn!(error = %e, %media_id, "similarity refresh failed");
                    }
                } else {
                    failed += 1;
                }
            }
            None => {
                repo::mark_failed(&state.pool, media_id, "no audio file on disk").await?;
                failed += 1;
            }
        }
    }

    Ok(Json(BatchResult {
        claimed,
        analyzed,
        failed,
    }))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct DatasetQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct DatasetPage {
    pub rows: Vec<DatasetRow>,
    pub limit: i64,
    pub offset: i64,
    pub returned: usize,
    pub analysis_version: i32,
}

/// Export the music dataset: per-track features plus aggregate engagement.
///
/// Built from the global tables only, so it carries no user identifiers — the
/// engagement columns are counts across the whole audience, never rows per
/// person. Paginated because the corpus is meant to outgrow one response.
#[utoipa::path(get, path = "/api/admin/music/dataset", tag = "music",
    params(DatasetQuery), security(("bearer" = [])),
    responses((status = 200, body = DatasetPage)))]
pub async fn dataset(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Query(q): Query<DatasetQuery>,
) -> ApiResult<Json<DatasetPage>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let offset = q.offset.unwrap_or(0).max(0);
    let rows = repo::dataset(&state.pool, limit, offset).await?;
    Ok(Json(DatasetPage {
        returned: rows.len(),
        rows,
        limit,
        offset,
        analysis_version: analysis::ANALYSIS_VERSION,
    }))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct DatasetSummary {
    pub music_items: i64,
    pub analyzed: i64,
    pub total_listens: i64,
    pub listeners: i64,
    pub hours_listened: f64,
    pub analysis_version: i32,
    /// Fraction of the music catalog that has been through the analyzer.
    pub coverage: f64,
}

/// Corpus counters: how much of the catalog is analyzed, and how much
/// listening has accumulated behind it.
#[utoipa::path(get, path = "/api/admin/music/dataset/summary", tag = "music",
    security(("bearer" = [])), responses((status = 200, body = DatasetSummary)))]
pub async fn dataset_summary(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<DatasetSummary>> {
    let (music_items, analyzed, total_listens, listeners, hours) =
        repo::dataset_summary(&state.pool).await?;
    Ok(Json(DatasetSummary {
        music_items,
        analyzed,
        total_listens,
        listeners,
        hours_listened: hours,
        analysis_version: analysis::ANALYSIS_VERSION,
        coverage: if music_items > 0 {
            analyzed as f64 / music_items as f64
        } else {
            0.0
        },
    }))
}
