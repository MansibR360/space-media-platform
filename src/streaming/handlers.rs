use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::pagination::Page;
use crate::state::AppState;
use crate::streaming::probe;
use crate::streaming::ranged::range_response;

#[derive(Debug, sqlx::FromRow, Serialize, ToSchema)]
pub struct FileRow {
    pub id: Uuid,
    pub media_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    pub media_title: Option<String>,
    pub file_name: String,
    pub relative_path: String,
    pub file_size: i64,
    pub container: Option<String>,
    pub duration_seconds: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub probed_at: Option<DateTime<Utc>>,
}

pub(crate) struct FileWithRoot {
    pub(crate) row: FileRow,
    pub(crate) library_root: Option<String>,
}

pub(crate) async fn load_file_public(state: &AppState, file_id: Uuid) -> ApiResult<FileWithRoot> {
    load_file(state, file_id).await
}

/// Bulk-probe up to 200 unprobed files (admin maintenance action).
/// Missing-file review queue (Phase M6): flagged by the scanner sweep.
#[utoipa::path(get, path = "/api/admin/files/missing", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_missing(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT f.id, f.relative_path, COALESCE(m.title, '') FROM media_files f \
         LEFT JOIN media m ON m.id = f.media_id \
         WHERE f.missing ORDER BY f.last_seen_at DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({
        "count": rows.len(),
        "files": rows.into_iter().map(|(id, path, title)| {
            serde_json::json!({ "id": id, "path": path, "title": title })
        }).collect::<Vec<_>>(),
    })))
}

pub async fn probe_all_unprobed(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT f.id FROM media_files f \
         WHERE f.probed_at IS NULL LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;

    let mut probed = 0usize;
    for (file_id,) in rows {
        if probe_file(&state, file_id).await.is_ok() {
            probed += 1;
        }
    }
    Ok(Json(serde_json::json!({ "probed": probed })))
}

pub(crate) fn absolute_path_public(file: &FileWithRoot) -> ApiResult<PathBuf> {
    absolute_path(file)
}

async fn load_file(state: &AppState, file_id: Uuid) -> ApiResult<FileWithRoot> {
    let row = sqlx::query_as::<_, FileRow>(
        "SELECT f.id, f.media_id, f.episode_id, m.title AS media_title, f.file_name, \
         f.relative_path, f.file_size, f.container, f.duration_seconds, f.width, f.height, \
         f.video_codec, f.audio_codec, f.probed_at
         FROM media_files f
         LEFT JOIN media m ON m.id = f.media_id
         WHERE f.id = $1",
    )
    .bind(file_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("file"))?;

    let library_root: Option<String> =
        sqlx::query_scalar("SELECT lp.root_path FROM media_files f JOIN library_paths lp ON lp.id = f.library_path_id WHERE f.id = $1")
            .bind(file_id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();

    Ok(FileWithRoot { row, library_root })
}

/// Loads a file enforcing restricted-library visibility for non-librarians.
async fn load_file_checked(
    state: &AppState,
    file_id: Uuid,
    viewer_can_see_restricted: bool,
) -> ApiResult<FileWithRoot> {
    let restricted: Option<(bool,)> = sqlx::query_as(
        "SELECT COALESCE(lp.restricted, FALSE) FROM media_files f \
         LEFT JOIN library_paths lp ON lp.id = f.library_path_id WHERE f.id = $1",
    )
    .bind(file_id)
    .fetch_optional(&state.pool)
    .await?;

    if let Some((is_restricted,)) = restricted {
        if is_restricted && !viewer_can_see_restricted {
            return Err(ApiError::forbidden("this library is restricted"));
        }
    }

    load_file(state, file_id).await
}

fn absolute_path(file: &FileWithRoot) -> ApiResult<PathBuf> {
    let root = file
        .library_root
        .as_deref()
        .ok_or_else(|| ApiError::Unavailable("file is not attached to a library path".into()))?;
    let root_path = PathBuf::from(root);
    let full = root_path.join(&file.row.relative_path);
    if !full.starts_with(&root_path) {
        return Err(ApiError::forbidden("path traversal blocked"));
    }
    Ok(full)
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListFilesQuery {
    #[serde(flatten)]
    pub page: crate::pagination::PageQuery,
    pub media_id: Option<Uuid>,
}

#[utoipa::path(get, path = "/api/files", tag = "streaming", params(ListFilesQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [FileRow]))
)]
pub async fn list_files(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(query): Query<ListFilesQuery>,
) -> ApiResult<Json<Page<FileRow>>> {
    crate::auth::permissions::require(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    )?;

    let limit = query.page.clamp_limit();
    let offset = query
        .page
        .cursor
        .as_deref()
        .and_then(|c| crate::pagination::decode_cursor(c).ok())
        .and_then(|v| v.get("o").and_then(|o| o.as_i64()))
        .unwrap_or(0);

    let rows = match query.media_id {
        Some(media_id) => {
            sqlx::query_as::<_, FileRow>(
                "SELECT f.id, f.media_id, f.episode_id, m.title AS media_title, f.file_name, \
                 f.relative_path, f.file_size, f.container, f.duration_seconds, f.width, f.height, \
                 f.video_codec, f.audio_codec, f.probed_at
                 FROM media_files f LEFT JOIN media m ON m.id = f.media_id
                 WHERE f.media_id = $3 ORDER BY f.created_at DESC LIMIT $1 OFFSET $2",
            )
            .bind(limit)
            .bind(offset)
            .bind(media_id)
            .fetch_all(&state.pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, FileRow>(
                "SELECT f.id, f.media_id, f.episode_id, m.title AS media_title, f.file_name, \
                 f.relative_path, f.file_size, f.container, f.duration_seconds, f.width, f.height, \
                 f.video_codec, f.audio_codec, f.probed_at
                 FROM media_files f LEFT JOIN media m ON m.id = f.media_id
                 ORDER BY f.created_at DESC LIMIT $1 OFFSET $2",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&state.pool)
            .await?
        }
    };

    let next_cursor = (rows.len() as i64 == limit)
        .then(|| crate::pagination::encode_cursor(&serde_json::json!({ "o": offset + limit })));
    Ok(Json(Page::with_cursor(rows, next_cursor)))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct FileIdPath {
    pub file_id: Uuid,
}

#[utoipa::path(get, path = "/api/files/{file_id}", tag = "streaming", params(FileIdPath),
    responses((status = 200, body = FileDetail), (status = 404))
)]
pub async fn get_file(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<FileIdPath>,
) -> ApiResult<Json<FileDetail>> {
    let can_restricted = crate::auth::permissions::has(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    );
    let file = load_file_checked(&state, path.file_id, can_restricted)
        .await?
        .row;

    type TrackTuple = (
        Uuid,
        String,
        i32,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i32>,
        bool,
        Option<String>,
    );
    let tracks: Vec<TrackTuple> = sqlx::query_as(
        "SELECT id, stream_kind, stream_index, codec, language, title, channels, is_default, external_path \
         FROM media_streams WHERE file_id = $1 ORDER BY stream_index",
    )
    .bind(path.file_id)
    .fetch_all(&state.pool)
    .await?;

    let mut audio_tracks = Vec::new();
    let mut subtitle_tracks = Vec::new();
    for track in tracks {
        let info = TrackInfo {
            id: track.0,
            codec: track.3,
            language: track.4,
            title: track.5,
            channels: track.6,
            is_default: track.7,
            external_path: track.8,
        };
        match track.1.as_str() {
            "audio" => audio_tracks.push(info),
            "subtitle" => subtitle_tracks.push(info),
            _ => {}
        }
    }

    Ok(Json(FileDetail {
        file,
        audio_tracks,
        subtitle_tracks,
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TrackInfo {
    pub id: Uuid,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub channels: Option<i32>,
    pub is_default: bool,
    pub external_path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileDetail {
    #[serde(flatten)]
    pub file: FileRow,
    pub audio_tracks: Vec<TrackInfo>,
    pub subtitle_tracks: Vec<TrackInfo>,
}


/// Optional `?grant=` on media endpoints.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct GrantQuery {
    #[serde(default)]
    pub grant: Option<String>,
}

/// Resolve the caller for a media byte stream.
///
/// Bearer/cookie auth wins when present. Otherwise a signed playback grant is
/// accepted, which is the only mechanism available to a `<video>`/`<audio>`
/// element: it cannot set headers, and the session cookie is not sent
/// cross-origin. The grant is verified against its fingerprint binding, so a
/// token lifted from one client will not replay from another.
pub async fn authorize_media(
    state: &AppState,
    auth: Option<&CurrentUser>,
    grant: Option<&str>,
    headers: &HeaderMap,
) -> ApiResult<uuid::Uuid> {
    if let Some(user) = auth {
        return Ok(user.id());
    }

    // No session and no grant: nothing to authorize against.
    let Some(token) = grant.filter(|g| !g.trim().is_empty()) else {
        return Err(ApiError::Unauthorized);
    };

    let presented = crate::security::presented_fingerprint(headers, None);
    let verified = crate::security::grants::verify_with_fingerprint(
        &state.pool,
        token,
        presented.as_deref(),
    )
    .await?;

    Ok(verified.user_id)
}

#[utoipa::path(get, path = "/api/files/{file_id}/raw", tag = "streaming", params(FileIdPath),
    security(("bearer" = [])),
    responses((status = 200, description = "video bytes"), (status = 404))
)]
pub async fn stream_file(
    State(state): State<Arc<AppState>>,
    auth: Option<CurrentUser>,
    Path(path): Path<FileIdPath>,
    axum::extract::Query(q): axum::extract::Query<GrantQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // A media element cannot send an Authorization header, and the session
    // cookie does not ride along cross-origin. The signed playback grant is
    // what covers that case: it is short-lived, bound to one media ref, and
    // optionally to a device fingerprint.
    let user_id = authorize_media(&state, auth.as_ref(), q.grant.as_deref(), &headers).await?;

    // Access-card gate (changeset 5): no card, no playback — everyone.
    crate::cards::ensure_card_access(&state.pool, user_id).await?;
    let can_restricted = auth.as_ref().is_some_and(|a| {
        crate::auth::permissions::has(
            a.ctx.as_ref(),
            crate::auth::permissions::Permission::ManageLibrary,
        )
    });
    let file = load_file_checked(&state, path.file_id, can_restricted).await?;
    let full_path = absolute_path(&file)?;

    if !state.storage.exists(&full_path).await {
        return Err(ApiError::not_found("file on disk"));
    }

    let stat = state
        .storage
        .stat(&full_path)
        .await
        .map_err(ApiError::from)?;
    let range_spec = header_str(&headers, "range");
    let (start, end_inclusive) = range_spec
        .and_then(|spec| parse_simple_range(spec, stat.size))
        .map(|r| (r.0, Some(r.1)))
        .unwrap_or((0, None));

    let stream = state
        .storage
        .stream_range(&full_path, start, end_inclusive)
        .await
        .map_err(ApiError::from)?;

    range_response(&full_path, &stat, range_spec, stream)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.trim().is_empty())
}

fn parse_simple_range(spec: &str, size: u64) -> Option<(u64, u64)> {
    let body = spec.trim().strip_prefix("bytes=")?;
    let (start_raw, end_raw) = body.split_once('-')?;
    let start: u64 = start_raw.trim().parse().ok()?;
    let end = end_raw
        .trim()
        .parse::<u64>()
        .unwrap_or(size - 1)
        .min(size - 1);
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

#[utoipa::path(post, path = "/api/admin/files/{file_id}/probe", tag = "admin", params(FileIdPath),
    security(("bearer" = [])),
    responses((status = 200), (status = 503))
)]
pub async fn probe_file_endpoint(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<FileIdPath>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    crate::streaming::handlers::probe_file(&state, path.file_id).await?;
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "probed": true, "file_id": path.file_id })),
    ))
}

pub async fn probe_file(state: &AppState, file_id: Uuid) -> ApiResult<()> {
    match probe_file_inner(state, file_id).await {
        Ok(()) => {
            sqlx::query("UPDATE media_files SET probe_failed = FALSE WHERE id = $1")
                .bind(file_id)
                .execute(&state.pool)
                .await?;
            record_event(&state.pool, file_id, "probe_ok", serde_json::json!({})).await;
            Ok(())
        }
        Err(err) => {
            sqlx::query("UPDATE media_files SET probe_failed = TRUE WHERE id = $1")
                .bind(file_id)
                .execute(&state.pool)
                .await
                .ok();
            record_event(
                &state.pool,
                file_id,
                "probe_failed",
                serde_json::json!({ "error": err.to_string() }),
            )
            .await;
            metrics::counter!("probe_failures_total").increment(1);
            Err(err)
        }
    }
}

async fn record_event(pool: &sqlx::PgPool, file_id: Uuid, kind: &str, detail: serde_json::Value) {
    let _ =
        sqlx::query("INSERT INTO file_events (id, file_id, kind, detail) VALUES ($1, $2, $3, $4)")
            .bind(crate::utils::new_id())
            .bind(file_id)
            .bind(kind)
            .bind(detail)
            .execute(pool)
            .await;
}

async fn probe_file_inner(state: &AppState, file_id: Uuid) -> ApiResult<()> {
    let file = load_file(state, file_id).await?;
    let full_path = absolute_path(&file)?;

    let result = probe::probe(&state.cfg.ffprobe_bin, &full_path).await?;

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE media_files SET duration_seconds = $2, width = $3, height = $4, video_codec = $5, \
         audio_codec = $6, bitrate = $7, hdr = $8, replaygain_db = $9, probed_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(file_id)
    .bind((result.duration_seconds > 0.0).then_some(result.duration_seconds))
    .bind(result.width)
    .bind(result.height)
    .bind(result.video_codec.clone())
    .bind(result.audio_codec.clone())
    .bind(result.bitrate)
    .bind(result.hdr)
    .bind(result.replaygain_db)
    .execute(&mut *tx)
    .await?;

    let defaults_present = result.audio_tracks.iter().any(|t| t.is_default);

    for track in &result.audio_tracks {
        insert_stream(
            &mut tx,
            file_id,
            "audio",
            track.index,
            track.codec.as_deref(),
            track.language.as_deref(),
            track.title.as_deref(),
            track.channels,
            track.is_default || !defaults_present,
        )
        .await?;
    }
    for track in &result.subtitle_tracks {
        insert_stream(
            &mut tx,
            file_id,
            "subtitle",
            track.index,
            track.codec.as_deref(),
            track.language.as_deref(),
            track.title.as_deref(),
            None,
            false,
        )
        .await?;
    }

    scan_external_subtitles(state, &mut tx, &file).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_stream(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    file_id: Uuid,
    kind: &str,
    index: i32,
    codec: Option<&str>,
    language: Option<&str>,
    title: Option<&str>,
    channels: Option<i32>,
    is_default: bool,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO media_streams (id, file_id, stream_kind, stream_index, codec, language, title, channels, is_default)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (file_id, stream_index) DO UPDATE SET
            stream_kind = EXCLUDED.stream_kind, codec = EXCLUDED.codec, language = EXCLUDED.language,
            title = EXCLUDED.title, channels = EXCLUDED.channels, is_default = EXCLUDED.is_default",
    )
    .bind(crate::utils::new_id())
    .bind(file_id)
    .bind(kind)
    .bind(index)
    .bind(codec)
    .bind(language)
    .bind(title)
    .bind(channels)
    .bind(is_default)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn scan_external_subtitles(
    state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    file: &FileWithRoot,
) -> ApiResult<()> {
    let full_path = absolute_path(file)?;
    let dir = match full_path.parent() {
        Some(dir) => dir.to_path_buf(),
        None => return Ok(()),
    };
    let root_path = PathBuf::from(file.library_root.clone().unwrap_or_default());

    let mut entries = tokio::fs::read_dir(&dir).await?;
    let stem_lower = FsPath::new(&file.row.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut next_index = 9000i32;

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        let Some(_) = ext
            .as_deref()
            .and_then(crate::streaming::subtitles::SubtitleFormat::from_extension)
        else {
            continue;
        };

        let sub_stem = FsPath::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !sub_stem.starts_with(&stem_lower) {
            continue;
        }

        let language = sub_stem
            .strip_prefix(&stem_lower)
            .and_then(|rest| rest.split('.').find(|seg| !seg.is_empty()))
            .map(str::trim)
            .filter(|seg| {
                seg.len() >= 2 && seg.len() <= 5 && seg.chars().all(|c| c.is_ascii_alphanumeric())
            })
            .map(str::to_string);

        let relative = entry
            .path()
            .strip_prefix(root_path.as_path())
            .unwrap_or(entry.path().as_path())
            .to_string_lossy()
            .replace('\\', "/");

        sqlx::query(
            "DELETE FROM media_streams WHERE file_id = $1 AND stream_kind = 'subtitle' AND external_path IS NOT NULL AND title = $2",
        )
        .bind(file.row.id)
        .bind(name.clone())
        .execute(&mut **tx)
        .await?;

        sqlx::query(
            "INSERT INTO media_streams (id, file_id, stream_kind, stream_index, codec, language, title, external_path)
             VALUES ($1, $2, 'subtitle', $3, $4, $5, $6, $7)",
        )
        .bind(crate::utils::new_id())
        .bind(file.row.id)
        .bind(next_index)
        .bind(ext.clone())
        .bind(language)
        .bind(name.clone())
        .bind(relative)
        .execute(&mut **tx)
        .await?;

        next_index += 1;
    }
    let _ = state;
    Ok(())
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SubtitleQuery {
    /// UUID of the media_streams row
    pub track: String,
    /// keep original ass format instead of converting to vtt
    #[serde(default)]
    pub raw: bool,
}

#[utoipa::path(get, path = "/api/files/{file_id}/subtitles", tag = "streaming", params(FileIdPath, SubtitleQuery),
    security(("bearer" = [])),
    responses((status = 200, description = "webvtt/ass text"), (status = 404))
)]
pub async fn get_subtitle(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<FileIdPath>,
    Query(query): Query<SubtitleQuery>,
) -> ApiResult<Response> {
    let can_restricted = crate::auth::permissions::has(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    );
    let _ = can_restricted; // subtitle tracks inherit file visibility via load below
    let track_id = Uuid::parse_str(query.track.trim())
        .map_err(|_| ApiError::bad_request("track must be a uuid"))?;

    let track: Option<(i32, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT stream_index, codec, external_path FROM media_streams \
         WHERE id = $1 AND file_id = $2 AND stream_kind = 'subtitle'",
    )
    .bind(track_id)
    .bind(path.file_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some((stream_index, codec, external)) = track else {
        return Err(ApiError::not_found("subtitle track"));
    };

    let is_ass = codec.as_deref() == Some("ass") || codec.as_deref() == Some("ssa");

    if let Some(rel) = external {
        let file = load_file_checked(&state, path.file_id, can_restricted).await?;
        let root = file
            .library_root
            .ok_or_else(|| ApiError::Unavailable("no library root".into()))?;
        let sub_path = PathBuf::from(root).join(rel);
        let content = tokio::fs::read_to_string(&sub_path)
            .await
            .map_err(ApiError::from)?;
        if query.raw || is_ass {
            return ass_response(content);
        }
        return vtt_response(content);
    }

    let cache_key = format!("{}_{}", path.file_id.simple(), stream_index);
    let cached = state
        .cfg
        .storage_dir
        .join("subs")
        .join(format!("{cache_key}.vtt"));

    if state.storage.exists(&cached).await {
        let content = tokio::fs::read_to_string(&cached)
            .await
            .map_err(ApiError::from)?;
        return vtt_response(content);
    }

    let file = load_file_checked(&state, path.file_id, can_restricted).await?;
    let video_path = absolute_path(&file)?;
    crate::streaming::storage::ensure_parent_dir(&cached).map_err(ApiError::from)?;
    extract_subtitle(&state.cfg.ffmpeg_bin, &video_path, stream_index, &cached).await?;
    let content = tokio::fs::read_to_string(&cached)
        .await
        .map_err(ApiError::from)?;
    vtt_response(content)
}

async fn extract_subtitle(
    ffmpeg_bin: &str,
    video: &FsPath,
    stream_index: i32,
    out: &FsPath,
) -> ApiResult<()> {
    let output = tokio::process::Command::new(ffmpeg_bin)
        .args(["-y", "-v", "error", "-i"])
        .arg(video)
        .arg("-map")
        .arg(format!("0:s:{stream_index}"))
        .arg("-c:s")
        .arg("webvtt")
        .arg(out)
        .output()
        .await
        .map_err(|e| ApiError::Unavailable(format!("ffmpeg unavailable: {e}")))?;

    if !output.status.success() || !out.exists() {
        return Err(ApiError::Unavailable(format!(
            "ffmpeg could not extract subtitle {}: {}",
            stream_index,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("")
        )));
    }
    Ok(())
}

fn vtt_response(content: String) -> ApiResult<Response> {
    let converted = crate::streaming::subtitles::convert(
        &content,
        crate::streaming::subtitles::SubtitleFormat::Srt,
    )?;
    Ok((
        [(header::CONTENT_TYPE, "text/vtt; charset=utf-8")],
        converted,
    )
        .into_response())
}

fn ass_response(content: String) -> ApiResult<Response> {
    Ok((
        [(header::CONTENT_TYPE, "text/x-ssa; charset=utf-8")],
        content,
    )
        .into_response())
}
