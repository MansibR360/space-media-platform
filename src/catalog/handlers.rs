use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::catalog::models::{
    CreateEpisodeRequest, CreateMediaRequest, CreateSeasonRequest, Episode, Media, MediaDetail,
    Season, UpdateMediaRequest,
};
use crate::catalog::repo;
use crate::error::{ApiError, ApiResult};
use crate::pagination::Page;
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
#[allow(dead_code)]
pub struct SeasonIdPath {
    pub media_id: Uuid,
    pub season_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams, utoipa::ToSchema)]
pub struct ListMediaQuery {
    // NOT #[serde(flatten)]: serde_urlencoded hands flattened fields through as
    // strings, so `?limit=3` failed with "invalid type: string \"3\", expected i64".
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub media_type: Option<crate::catalog::models::MediaType>,
    #[serde(default)]
    pub anime: Option<bool>,
    pub status: Option<crate::catalog::models::MediaStatus>,
    #[serde(default)]
    pub genre: Option<String>,
    pub studio: Option<String>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub min_rating: Option<f64>,
    pub q: Option<String>,
    pub person: Option<String>,
    /// ISO code (en, ja, …)
    pub language: Option<String>,
    pub min_height: Option<i32>,
    pub codec: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
}

#[utoipa::path(get, path = "/api/media", tag = "catalog", params(ListMediaQuery),
    responses((status = 200, body = [Media]))
)]
pub async fn list_media(
    State(state): State<Arc<AppState>>,
    _maybe_user: Option<CurrentUser>,
    Query(query): Query<ListMediaQuery>,
) -> ApiResult<Json<Page<Media>>> {
    let filter = repo::ListFilter {
        media_type: query.media_type,
        is_anime: query.anime,
        status: query.status,
        genres: query
            .genre
            .as_deref()
            .map(|g| {
                g.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        studio: query.studio,
        year_from: query.year_from,
        year_to: query.year_to,
        min_rating: query.min_rating,
        q: query.q.clone(),
        person: query.person.clone(),
        language: query.language.clone(),
        min_height: query.min_height,
        codec: query.codec.clone(),
        sort: query
            .sort
            .as_deref()
            .and_then(repo::Sort::parse)
            .unwrap_or_default(),
    };

    let (items, next_cursor) = repo::list(
        &state.pool,
        &filter,
        query.limit.unwrap_or(20).clamp(1, 100),
        query.cursor.as_deref(),
    )
    .await?;
    Ok(Json(Page::with_cursor(items, next_cursor)))
}

#[utoipa::path(get, path = "/api/media/{media_id}", tag = "catalog", params(MediaIdPath),
    responses((status = 200, body = MediaDetail), (status = 404))
)]
pub async fn get_media(
    State(state): State<Arc<AppState>>,
    Path(path): Path<MediaIdPath>,
    maybe_user: Option<CurrentUser>,
) -> ApiResult<Json<MediaDetail>> {
    let media = repo::find_by_id(&state.pool, path.media_id)
        .await?
        .ok_or_else(|| ApiError::not_found("media"))?;
    let detail = repo::detail_for_user(&state.pool, media, maybe_user.map(|u| u.id())).await?;
    Ok(Json(detail))
}

#[utoipa::path(post, path = "/api/admin/media", tag = "admin",
    security(("bearer" = [])),
    request_body = CreateMediaRequest,
    responses((status = 201, body = Media))
)]
pub async fn create_media(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(req): Json<CreateMediaRequest>,
) -> ApiResult<(StatusCode, Json<Media>)> {
    let media = repo::create(&state.pool, &req).await?;
    Ok((StatusCode::CREATED, Json(media)))
}

#[utoipa::path(patch, path = "/api/admin/media/{media_id}", tag = "admin",
    security(("bearer" = [])), params(MediaIdPath),
    request_body = UpdateMediaRequest,
    responses((status = 200, body = Media), (status = 404))
)]
pub async fn update_media(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<MediaIdPath>,
    Json(req): Json<UpdateMediaRequest>,
) -> ApiResult<Json<Media>> {
    let media = repo::update(&state.pool, path.media_id, &req).await?;
    Ok(Json(media))
}

#[utoipa::path(delete, path = "/api/admin/media/{media_id}", tag = "admin",
    security(("bearer" = [])), params(MediaIdPath),
    responses((status = 204))
)]
pub async fn delete_media(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<StatusCode> {
    repo::delete(&state.pool, path.media_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/admin/media/{media_id}/seasons", tag = "admin",
    security(("bearer" = [])), params(MediaIdPath),
    request_body = CreateSeasonRequest,
    responses((status = 201, body = Season), (status = 404))
)]
pub async fn create_season(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<MediaIdPath>,
    Json(req): Json<CreateSeasonRequest>,
) -> ApiResult<(StatusCode, Json<Season>)> {
    if repo::find_by_id(&state.pool, path.media_id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("media"));
    }
    let season = repo::create_season(&state.pool, path.media_id, &req).await?;
    Ok((StatusCode::CREATED, Json(season)))
}

#[utoipa::path(get, path = "/api/media/{media_id}/episodes", tag = "catalog", params(MediaIdPath),
    responses((status = 200, body = [Episode]), (status = 404))
)]
pub async fn list_episodes(
    State(state): State<Arc<AppState>>,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<Json<Vec<Episode>>> {
    let episodes = repo::episodes_of_media(&state.pool, path.media_id).await?;
    Ok(Json(episodes))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CreateEpisodePath {
    pub media_id: Uuid,
    pub season_id: Uuid,
}

#[utoipa::path(post, path = "/api/admin/media/{media_id}/seasons/{season_id}/episodes", tag = "admin",
    security(("bearer" = [])), params(CreateEpisodePath),
    request_body = CreateEpisodeRequest,
    responses((status = 201, body = Episode), (status = 404))
)]
pub async fn create_episode(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<CreateEpisodePath>,
    Json(req): Json<CreateEpisodeRequest>,
) -> ApiResult<(StatusCode, Json<Episode>)> {
    if repo::find_by_id(&state.pool, path.media_id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("media"));
    }
    let episode = repo::create_episode(&state.pool, path.media_id, path.season_id, &req).await?;
    Ok((StatusCode::CREATED, Json(episode)))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MetadataSearchQuery {
    pub source: String,
    pub q: String,
}

async fn resolve_provider(
    state: &AppState,
    source: &str,
) -> ApiResult<Arc<dyn crate::catalog::metadata::MetadataProvider>> {
    state
        .metadata
        .read()
        .await
        .provider(source)
        .ok_or_else(|| ApiError::not_found(format!("metadata source '{source}'")))
}

#[utoipa::path(get, path = "/api/metadata/search", tag = "metadata", params(MetadataSearchQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn metadata_search(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(query): Query<MetadataSearchQuery>,
) -> ApiResult<Json<Vec<crate::catalog::metadata::ExternalSearchResult>>> {
    crate::auth::permissions::require(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    )?;
    let provider = resolve_provider(&state, &query.source).await?;
    let results = provider.search(query.q.trim()).await?;
    Ok(Json(results))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ImportMetadataRequest {
    pub source: String,
    pub external_id: String,
}

#[utoipa::path(post, path = "/api/metadata/import", tag = "metadata",
    security(("bearer" = [])),
    request_body = ImportMetadataRequest,
    responses((status = 201, body = MediaDetail))
)]
pub async fn import_metadata(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ImportMetadataRequest>,
) -> ApiResult<(StatusCode, Json<MediaDetail>)> {
    crate::auth::permissions::require(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    )?;

    let provider = resolve_provider(&state, &req.source).await?;
    let detail = provider.details(&req.external_id).await?;

    let existing = repo::media_for_external(&state.pool, &req.source, &req.external_id).await?;

    let media = match existing {
        Some(media) => {
            let update = UpdateMediaRequest {
                title: None,
                overview: Some(detail.overview.clone()),
                release_date: None,
                runtime_minutes: detail.runtime_minutes,
                status: detail
                    .status
                    .as_deref()
                    .and_then(crate::catalog::models::MediaStatus::parse),
                poster_url: detail.poster.clone(),
                backdrop_url: detail.backdrop.clone(),
                trailer_url: None,
                is_anime: Some(detail.is_anime),
            };
            repo::update(&state.pool, media.id, &update).await?
        }
        None => {
            let create = CreateMediaRequest {
                media_type: match detail.media_type.as_deref() {
                    Some("movie") => crate::catalog::models::MediaType::Movie,
                    _ => crate::catalog::models::MediaType::Series,
                },
                title: detail.title.clone(),
                is_anime: detail.is_anime,
                original_title: detail.original_title.clone(),
                overview: Some(detail.overview.clone()),
                release_date: detail.release_date,
                runtime_minutes: detail.runtime_minutes,
                status: detail
                    .status
                    .as_deref()
                    .and_then(crate::catalog::models::MediaStatus::parse),
                poster_url: detail.poster.clone(),
                backdrop_url: detail.backdrop.clone(),
                genres: Vec::new(),
                studios: Vec::new(),
            };
            repo::create(&state.pool, &create).await?
        }
    };

    for genre in &detail.genres {
        repo::attach_genre_by_name(&state.pool, media.id, genre).await?;
    }
    for studio in &detail.studios {
        repo::attach_studio_by_name(&state.pool, media.id, studio).await?;
    }
    for (person, character, order) in &detail.cast {
        repo::add_credit(
            &state.pool,
            media.id,
            person,
            "cast",
            character.as_deref(),
            *order,
        )
        .await?;
    }

    repo::upsert_external_mapping(
        &state.pool,
        media.id,
        provider.source().as_str(),
        &req.external_id,
        serde_json::json!({ "title": detail.title }),
    )
    .await?;

    // Persist whatever IMDb data the provider carried. TMDB reports the IMDb
    // id, OMDb reports the ratings too, so a title is enrichable from IMDb
    // afterwards regardless of where it was imported from.
    crate::catalog::imdb::apply_detail(&state.pool, media.id, &detail).await?;

    let refreshed = repo::find_by_id(&state.pool, media.id)
        .await?
        .ok_or_else(|| ApiError::not_found("media"))?;
    let full = repo::detail_for_user(&state.pool, refreshed, Some(auth.id())).await?;
    Ok((StatusCode::CREATED, Json(full)))
}

// ---------------- storage analytics & file events (M6) ----------------

#[derive(Debug, serde::Serialize, sqlx::FromRow, ToSchema)]
pub struct LibraryUsage {
    pub library: String,
    pub files: i64,
    pub total_bytes: i64,
}

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct StorageAnalytics {
    pub total_files: i64,
    pub total_bytes: i64,
    pub missing_files: i64,
    pub probe_failures: i64,
    pub per_library: Vec<LibraryUsage>,
    pub largest: Vec<serde_json::Value>,
}

#[utoipa::path(get, path = "/api/admin/storage/analytics", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn storage_analytics(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<StorageAnalytics>> {
    let totals: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*)::bigint, COALESCE(SUM(file_size),0)::bigint FROM media_files",
    )
    .fetch_one(&state.pool)
    .await?;

    let flags: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE missing)::bigint, \
                COUNT(*) FILTER (WHERE probe_failed)::bigint FROM media_files",
    )
    .fetch_one(&state.pool)
    .await?;

    let per_library = sqlx::query_as::<_, LibraryUsage>(
        "SELECT COALESCE(lp.label, lp.root_path) AS library, COUNT(f.id)::bigint AS files, \
                COALESCE(SUM(f.file_size),0)::bigint AS total_bytes
         FROM media_files f JOIN library_paths lp ON lp.id = f.library_path_id
         GROUP BY 1 ORDER BY total_bytes DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let largest: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT json_build_object('id', f.id, 'name', f.file_name, 'bytes', f.file_size, \
         'title', m.title)
         FROM media_files f LEFT JOIN media m ON m.id = f.media_id
         ORDER BY f.file_size DESC LIMIT 10",
    )
    .fetch_all(&state.pool)
    .await?;

    metrics::gauge!("library_total_bytes").set(totals.1 as f64);
    Ok(Json(StorageAnalytics {
        total_files: totals.0,
        total_bytes: totals.1,
        missing_files: flags.0,
        probe_failures: flags.1,
        per_library,
        largest: largest.into_iter().map(|(v,)| v).collect(),
    }))
}

#[utoipa::path(get, path = "/api/admin/files/{file_id}/events", tag = "admin",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn file_events(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    axum::extract::Path(path): axum::extract::Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<Json<Vec<serde_json::Value>>> {
    let rows: Vec<(
        Uuid,
        String,
        chrono::DateTime<chrono::Utc>,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT id, kind::text AS kind, created_at, detail FROM file_events \
             WHERE file_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(path.file_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, kind, created_at, detail)| {
                json!({ "id": id, "kind": kind, "created_at": created_at, "detail": detail })
            })
            .collect(),
    ))
}

#[derive(Debug, serde::Serialize, sqlx::FromRow, ToSchema)]
pub struct DuplicateGroup {
    pub norm_title: String,
    pub count: i64,
    pub media_ids: String,
}

/// Flags catalog titles that appear multiple times after normalization —
/// candidates for merging (admin review queue).
#[utoipa::path(get, path = "/api/admin/quality/duplicates", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn quality_duplicates(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<Vec<DuplicateGroup>>> {
    let rows = sqlx::query_as::<_, DuplicateGroup>(
        "SELECT LOWER(TRIM(title)) AS norm_title, COUNT(*)::bigint AS count,
                STRING_AGG(id::text, ',' ORDER BY created_at) AS media_ids
         FROM media GROUP BY 1 HAVING COUNT(*) > 1 ORDER BY count DESC LIMIT 50",
    )
    .fetch_all(&state.pool)
    .await?;
    metrics::gauge!("quality_duplicate_groups").set(rows.len() as f64);
    Ok(Json(rows))
}

/// Self-organizing library (changeset 3): merges duplicate catalog entries
/// automatically. The keeper is the entry with the most external mappings
/// (oldest wins ties). Files, tracking and social rows are re-pointed; a
/// duplicate's seasons move over only when the keeper has none — conflicting
/// series groups are skipped and reported instead of guessed at.
#[utoipa::path(post, path = "/api/admin/quality/duplicates/auto-resolve", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn auto_resolve_duplicates(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let groups = sqlx::query_as::<_, DuplicateGroup>(
        "SELECT LOWER(TRIM(title)) AS norm_title, COUNT(*)::bigint AS count,
                STRING_AGG(id::text, ',' ORDER BY created_at) AS media_ids
         FROM media GROUP BY 1 HAVING COUNT(*) > 1",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut merged_total: u64 = 0;
    let mut moved_files: u64 = 0;
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    let mut merged_titles: Vec<String> = Vec::new();

    for group in &groups {
        let ids: Vec<Uuid> = group
            .media_ids
            .split(',')
            .filter_map(|s| Uuid::parse_str(s.trim()).ok())
            .collect();
        if ids.len() < 2 {
            continue;
        }

        // Keeper: most external mappings, then oldest.
        let keeper: Option<(Uuid,)> = sqlx::query_as(
            "SELECT m.id FROM media m
             LEFT JOIN external_mappings e ON e.media_id = m.id
             WHERE m.id = ANY($1)
             GROUP BY m.id
             ORDER BY COUNT(e.id) DESC, m.created_at ASC LIMIT 1",
        )
        .bind(&ids)
        .fetch_optional(&state.pool)
        .await?;
        let Some((keeper_id,)) = keeper else { continue };

        for dup in ids.iter().filter(|id| **id != keeper_id) {
            let keeper_has_seasons: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM seasons WHERE media_id = $1)",
            )
            .bind(keeper_id)
            .fetch_one(&state.pool)
            .await?;
            let dup_has_seasons: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM seasons WHERE media_id = $1)",
            )
            .bind(dup)
            .fetch_one(&state.pool)
            .await?;

            if keeper_has_seasons && dup_has_seasons {
                // Episode lattices can collide (S1E1 on both sides) — needs a human.
                skipped.push(serde_json::json!({
                    "title": group.norm_title,
                    "reason": "both entries have seasons; manual merge required",
                    "keeper": keeper_id,
                    "duplicate": dup,
                }));
                continue;
            }

            let mut tx = match state.pool.begin().await {
                Ok(tx) => tx,
                Err(_) => continue,
            };

            // Media files first (their FK is SET NULL — re-point or lose them).
            let files = sqlx::query(
                "UPDATE media_files SET media_id = $2 WHERE media_id = $1",
            )
            .bind(dup)
            .bind(keeper_id)
            .execute(&mut *tx)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            moved_files += files as u64;

            if !keeper_has_seasons && dup_has_seasons {
                sqlx::query("UPDATE seasons SET media_id = $2 WHERE media_id = $1")
                    .bind(dup)
                    .bind(keeper_id)
                    .execute(&mut *tx)
                    .await
                    .ok();
            }

            // User-scoped rows: only where the user has no row on the keeper.
            for table in [
                "watch_history",
                "list_entries",
                "reading_progress",
                "favorites",
            ] {
                let _ = sqlx::query(&format!(
                    "UPDATE {table} SET media_id = $2 \
                     WHERE media_id = $1 AND user_id NOT IN \
                       (SELECT user_id FROM {table} WHERE media_id = $2)"
                ))
                .bind(dup)
                .bind(keeper_id)
                .execute(&mut *tx)
                .await;
            }
            let _ = sqlx::query("UPDATE reviews SET media_id = $2 WHERE media_id = $1")
                .bind(dup)
                .bind(keeper_id)
                .execute(&mut *tx)
                .await;

            // Cascade clears genres/studios/mappings/relationships/episodes.
            if let Ok(result) = sqlx::query("DELETE FROM media WHERE id = $1")
                .bind(dup)
                .execute(&mut *tx)
                .await
            {
                if result.rows_affected() > 0 {
                    merged_total += 1;
                }
            }
            if tx.commit().await.is_ok() {
                merged_titles.push(group.norm_title.clone());
            }
        }
    }

    metrics::counter!("duplicates_auto_merged_total").increment(merged_total);
    tracing::info!(merged = merged_total, skipped = skipped.len(), "auto-resolve complete");
    Ok(Json(serde_json::json!({
        "groups_scanned": groups.len(),
        "entries_merged": merged_total,
        "files_repointed": moved_files,
        "skipped": skipped,
    })))
}

// ---------------------------- IMDb ----------------------------

/// IMDb identity and ratings for one title.
#[utoipa::path(get, path = "/api/media/{media_id}/imdb", tag = "metadata",
    responses((status = 200, body = crate::catalog::imdb::ImdbRatings), (status = 404)))]
pub async fn imdb_ratings(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<crate::catalog::imdb::ImdbRatings>> {
    crate::catalog::imdb::ratings_for(&state.pool, media_id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("media"))
}

/// Pull fresh IMDb data for one title now.
#[utoipa::path(post, path = "/api/admin/media/{media_id}/imdb/sync", tag = "metadata",
    security(("bearer" = [])),
    responses((status = 200, body = crate::catalog::imdb::ImdbRatings)))]
pub async fn imdb_sync_one(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(media_id): Path<Uuid>,
) -> ApiResult<Json<crate::catalog::imdb::ImdbRatings>> {
    let ratings = crate::catalog::imdb::sync_one(&state, media_id).await?;
    Ok(Json(ratings))
}

/// Backfill IMDb ratings for titles that are missing or stale. Safe to run on
/// a schedule; each call takes the oldest slice.
#[utoipa::path(post, path = "/api/admin/imdb/sync", tag = "metadata",
    params(("limit" = Option<i64>, Query, description = "titles to refresh, max 500")),
    security(("bearer" = [])),
    responses((status = 200, body = crate::catalog::imdb::SyncReport)))]
pub async fn imdb_sync_stale(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Query(q): Query<ImdbSyncQuery>,
) -> ApiResult<Json<crate::catalog::imdb::SyncReport>> {
    let report = crate::catalog::imdb::sync_stale(&state, q.limit.unwrap_or(50)).await?;
    Ok(Json(report))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ImdbSyncQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}
