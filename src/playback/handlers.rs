use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::playback::sessions;
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePlaybackRequest {
    pub file_id: Uuid,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub client: String,
    pub audio_stream_id: Option<Uuid>,
    pub subtitle_stream_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SessionIdPath {
    pub session_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct HeartbeatRequest {
    pub position_seconds: f64,
    pub audio_stream_id: Option<Uuid>,
    pub subtitle_stream_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TranscodeAccepted {
    pub job_id: Uuid,
    pub status: &'static str,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct JobIdPath {
    pub job_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct JobAssetPath {
    pub job_id: Uuid,
    pub asset: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct EpisodeResumePath {
    pub episode_id: Uuid,
}

#[utoipa::path(get, path = "/api/playback/resume/episode/{episode_id}", tag = "playback",
    params(EpisodeResumePath),
    security(("bearer" = [])),
    responses((status = 200, body = crate::playback::sessions::ResumeInfo), (status = 404))
)]
pub async fn resume_episode(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<EpisodeResumePath>,
) -> ApiResult<Json<sessions::ResumeInfo>> {
    let info = sessions::resume_for_episode(&state.pool, auth.id(), path.episode_id, 1080).await?;
    Ok(Json(info))
}

#[utoipa::path(post, path = "/api/playback/sessions", tag = "playback",
    security(("bearer" = [])),
    request_body = CreatePlaybackRequest,
    responses((status = 201, body = crate::playback::sessions::PlaybackSession), (status = 404), (status = 409))
)]
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreatePlaybackRequest>,
) -> ApiResult<(StatusCode, Json<sessions::PlaybackSession>)> {
    let created = sessions::create(
        &state.pool,
        auth.id(),
        &sessions::CreateSession {
            file_id: req.file_id,
            device_name: req.device_name.chars().take(120).collect(),
            client: req.client.chars().take(60).collect(),
            audio_stream_id: req.audio_stream_id,
            subtitle_stream_id: req.subtitle_stream_id,
        },
    )
    .await?;
    metrics::gauge!("playback_active_sessions").increment(1.0);
    crate::automation::dispatch(
        &state.pool,
        &state.hub,
        "playback.started",
        serde_json::json!({ "user_id": auth.id(), "file_id": req.file_id, "device": created.device_name }),
    )
    .await;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(post, path = "/api/playback/sessions/{session_id}/heartbeat", tag = "playback",
    params(SessionIdPath),
    security(("bearer" = [])),
    request_body = HeartbeatRequest,
    responses((status = 204), (status = 404))
)]
pub async fn heartbeat(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<SessionIdPath>,
    Json(req): Json<HeartbeatRequest>,
) -> ApiResult<StatusCode> {
    sessions::heartbeat(
        &state.pool,
        auth.id(),
        path.session_id,
        req.position_seconds,
    )
    .await?;
    if let Some(audio) = req.audio_stream_id {
        let _ = sqlx::query("UPDATE playback_sessions SET audio_stream_id = $2 WHERE id = $1")
            .bind(path.session_id)
            .bind(audio)
            .execute(&state.pool)
            .await;
    }
    if let Some(subs) = req.subtitle_stream_id {
        let _ = sqlx::query("UPDATE playback_sessions SET subtitle_stream_id = $2 WHERE id = $1")
            .bind(path.session_id)
            .bind(subs)
            .execute(&state.pool)
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/playback/sessions/{session_id}", tag = "playback",
    params(SessionIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn end_session(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<SessionIdPath>,
) -> ApiResult<StatusCode> {
    sessions::end(&state.pool, auth.id(), path.session_id).await?;
    metrics::gauge!("playback_active_sessions").decrement(1.0);
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/playback/sessions", tag = "playback",
    security(("bearer" = [])),
    responses((status = 200, body = [crate::playback::sessions::PlaybackSession]))
)]
pub async fn my_sessions(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<sessions::PlaybackSession>>> {
    Ok(Json(sessions::my_active(&state.pool, auth.id()).await?))
}

#[utoipa::path(get, path = "/api/playback/resume/{media_id}", tag = "playback", params(MediaIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = crate::playback::sessions::ResumeInfo), (status = 404))
)]
pub async fn resume(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<MediaIdPath>,
) -> ApiResult<Json<sessions::ResumeInfo>> {
    let info = sessions::resume_for_media(&state.pool, auth.id(), path.media_id, 1080).await?;
    Ok(Json(info))
}

#[utoipa::path(post, path = "/api/admin/files/{file_id}/transcode", tag = "admin",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 202, body = TranscodeAccepted), (status = 503))
)]
pub async fn enqueue_transcode(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<(StatusCode, Json<TranscodeAccepted>)> {
    let job_id = state.transcode.enqueue_hls(path.file_id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(TranscodeAccepted {
            job_id,
            status: "queued",
        }),
    ))
}

#[utoipa::path(post, path = "/api/admin/files/{file_id}/thumbnails", tag = "admin",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 202, body = TranscodeAccepted), (status = 503))
)]
pub async fn enqueue_thumbnails(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<(StatusCode, Json<TranscodeAccepted>)> {
    let job_id = state.transcode.enqueue_thumbnails(path.file_id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(TranscodeAccepted {
            job_id,
            status: "queued",
        }),
    ))
}

#[utoipa::path(get, path = "/api/transcode-jobs/{job_id}", tag = "playback", params(JobIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = crate::playback::transcode::TranscodeJob), (status = 404))
)]
pub async fn job_status(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path(path): Path<JobIdPath>,
) -> ApiResult<Json<crate::playback::transcode::TranscodeJob>> {
    Ok(Json(state.transcode.job_status(path.job_id).await?))
}

#[utoipa::path(delete, path = "/api/transcode-jobs/{job_id}", tag = "playback", params(JobIdPath),
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn cancel_job(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<JobIdPath>,
) -> ApiResult<StatusCode> {
    state.transcode.cancel_job(path.job_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/files/{file_id}/master.m3u8", tag = "playback",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 200, description = "hls master playlist"), (status = 404))
)]
pub async fn hls_master(
    State(state): State<Arc<AppState>>,
    auth: Option<CurrentUser>,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
    axum::extract::Query(q): axum::extract::Query<crate::streaming::handlers::GrantQuery>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    // Same reasoning as the raw stream: hls.js fetches the playlist without an
    // Authorization header, so the signed grant is the only credential it has.
    let user_id = crate::streaming::handlers::authorize_media(
        &state,
        auth.as_ref(),
        q.grant.as_deref(),
        &headers,
    )
    .await?;
    crate::cards::ensure_card_access(&state.pool, user_id).await?;
    let dir = state
        .transcode
        .latest_completed_hls_dir(path.file_id)
        .await?;
    let master = dir.join("master.m3u8");
    if !tokio::fs::try_exists(&master).await.unwrap_or(false) {
        // No ladder has been built for this file — the client should stream the
        // original bytes instead. 404 says that clearly; the player falls back
        // to /raw rather than sitting on a dead <video> source.
        return Err(ApiError::not_found(
            "no HLS ladder for this file - stream the raw file instead",
        ));
    }
    let body = tokio::fs::read_to_string(&master)
        .await
        .map_err(ApiError::from)?;
    Ok((
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    )
        .into_response())
}

#[utoipa::path(get, path = "/api/hls/{job_id}/{*asset}", tag = "playback",
    params(JobIdPath),
    security(("bearer" = [])),
    responses((status = 200, description = "playlist or segment"), (status = 404))
)]
pub async fn hls_asset(
    State(state): State<Arc<AppState>>,
    auth: Option<CurrentUser>,
    Path(path): Path<JobAssetPath>,
    axum::extract::Query(q): axum::extract::Query<crate::streaming::handlers::GrantQuery>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    // Segment requests inherit the grant from the playlist's own URLs.
    let user_id = crate::streaming::handlers::authorize_media(
        &state,
        auth.as_ref(),
        q.grant.as_deref(),
        &headers,
    )
    .await?;
    crate::cards::ensure_card_access(&state.pool, user_id).await?;
    let base = state
        .cfg
        .storage_dir
        .join("hls")
        .join(path.job_id.simple().to_string());
    let asset_path = base.join(&path.asset);

    let normalized_ok = asset_path.starts_with(base);
    if !normalized_ok {
        return Err(ApiError::forbidden("path traversal blocked"));
    }

    if !tokio::fs::try_exists(&asset_path).await.unwrap_or(false) {
        return Err(ApiError::not_found("asset"));
    }

    let stream = state
        .storage
        .stream_range(&asset_path, 0, None)
        .await
        .map_err(ApiError::from)?;

    let content_type = match asset_path.extension().and_then(|e| e.to_str()) {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("ts") => "video/mp2t",
        Some("vtt") => "text/vtt; charset=utf-8",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    };

    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    Ok(response)
}

// ---------- cross-device handoff ----------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateHandoffRequest {
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RedeemHandoffRequest {
    pub code: String,
}

#[utoipa::path(post, path = "/api/playback/handoff", tag = "playback",
    security(("bearer" = [])),
    request_body = CreateHandoffRequest,
    responses((status = 201, body = crate::playback::markers::HandoffTicket), (status = 404))
)]
pub async fn create_handoff(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    body: Option<Json<CreateHandoffRequest>>,
) -> ApiResult<(StatusCode, Json<crate::playback::markers::HandoffTicket>)> {
    let ticket = crate::playback::markers::create_handoff(
        &state.pool,
        auth.id(),
        body.and_then(|Json(req)| req.session_id),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(ticket)))
}

#[utoipa::path(post, path = "/api/playback/handoff/redeem", tag = "playback",
    security(("bearer" = [])),
    request_body = RedeemHandoffRequest,
    responses((status = 200, body = crate::playback::markers::RedeemedHandoff), (status = 400))
)]
pub async fn redeem_handoff(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<RedeemHandoffRequest>,
) -> ApiResult<Json<crate::playback::markers::RedeemedHandoff>> {
    Ok(Json(
        crate::playback::markers::redeem_handoff(&state.pool, auth.id(), &req.code).await?,
    ))
}

// ---------------- predictive preload hint (Phase 41 slice) ----------------

#[derive(Debug, Serialize, ToSchema)]
pub struct NextHint {
    pub has_next: bool,
    /// false when the client-reported bandwidth makes prefetching wasteful
    pub should_prefetch: bool,
    pub media_id: Option<Uuid>,
    pub next_episode_id: Option<Uuid>,
}

/// Predicts what the user will play next (their most-recent unfinished
/// episode's successor) so clients can warm connections / prefetch manifests.
#[utoipa::path(get, path = "/api/playback/next-hint", tag = "playback",
    security(("bearer" = [])),
    responses((status = 200, body = NextHint))
)]
pub async fn next_hint(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    axum::extract::Query(bw): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<NextHint>> {
    let _ = &bw;
    let should_prefetch = bw
        .get("bandwidth_kbps")
        .and_then(|v| v.parse::<u32>().ok())
        .map(|kbps| kbps >= 4000)
        .unwrap_or(true);
    let row: Option<(Uuid,)> = sqlx::query_as(
        "WITH cur AS (
             SELECT e.media_id, s.number AS snum, e.number AS enum
             FROM watch_history wh
             JOIN episodes e ON e.id = wh.episode_id
             JOIN seasons s ON s.id = e.season_id
             WHERE wh.user_id = $1 AND wh.completed = FALSE
             ORDER BY wh.updated_at DESC LIMIT 1
         )
         SELECT (
             SELECT e.id FROM episodes e JOIN seasons s ON s.id = e.season_id
             WHERE e.media_id = cur.media_id
               AND (s.number * 100000 + e.number) > (cur.snum * 100000 + cur.enum)
             ORDER BY s.number, e.number LIMIT 1
         )
         FROM cur",
    )
    .bind(auth.id())
    .fetch_optional(&state.pool)
    .await?;

    let Some((next_episode_id,)) = row else {
        return Ok(Json(NextHint {
            has_next: false,
            should_prefetch,
            media_id: None,
            next_episode_id: None,
        }));
    };

    let media_id: Option<Uuid> = sqlx::query_scalar("SELECT media_id FROM episodes WHERE id = $1")
        .bind(next_episode_id)
        .fetch_optional(&state.pool)
        .await?
        .flatten();

    Ok(Json(NextHint {
        has_next: true,
        should_prefetch,
        media_id,
        next_episode_id: Some(next_episode_id),
    }))
}

// ---------- skip markers + chapters ----------

#[utoipa::path(get, path = "/api/files/{file_id}/markers", tag = "playback",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = [crate::playback::markers::MediaMarker]))
)]
pub async fn file_markers(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<Json<Vec<crate::playback::markers::MediaMarker>>> {
    Ok(Json(
        crate::playback::markers::markers_for_file(&state.pool, path.file_id).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ManualMarkerRequest {
    pub kind: String,
    pub seconds: f64,
}

#[utoipa::path(put, path = "/api/admin/files/{file_id}/markers", tag = "admin",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    request_body = ManualMarkerRequest,
    responses((status = 204), (status = 400))
)]
pub async fn set_marker(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
    Json(req): Json<ManualMarkerRequest>,
) -> ApiResult<StatusCode> {
    crate::playback::markers::upsert_marker(
        &state.pool,
        path.file_id,
        &req.kind,
        req.seconds,
        "manual",
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/admin/files/{file_id}/markers/detect", tag = "admin",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 200), (status = 503))
)]
pub async fn detect_markers_endpoint(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let file = crate::streaming::handlers::load_file_public(&state, path.file_id).await?;
    let full_path = crate::streaming::handlers::absolute_path_public(&file)?;
    let duration = file.row.duration_seconds.unwrap_or(0.0);

    let (intro_end, credits_start) =
        crate::playback::markers::detect_markers(&state.cfg.ffmpeg_bin, &full_path, duration)
            .await?;

    if let Some(seconds) = intro_end {
        crate::playback::markers::upsert_marker(
            &state.pool,
            path.file_id,
            "intro_start",
            0.0,
            "auto",
        )
        .await?;
        crate::playback::markers::upsert_marker(
            &state.pool,
            path.file_id,
            "intro_end",
            seconds,
            "auto",
        )
        .await?;
    }
    if let Some(seconds) = credits_start {
        crate::playback::markers::upsert_marker(
            &state.pool,
            path.file_id,
            "credits_start",
            seconds,
            "auto",
        )
        .await?;
    }

    metrics::counter!("marker_detection_runs_total").increment(1);
    Ok(Json(serde_json::json!({
        "intro_end": intro_end,
        "credits_start": credits_start,
    })))
}

/// Chapters computed on the fly from the probed duration (~8 minute scenes).
#[derive(Debug, Serialize, ToSchema)]
pub struct Chapter {
    pub start_seconds: f64,
    pub title: String,
}

#[utoipa::path(get, path = "/api/files/{file_id}/chapters", tag = "playback",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = [Chapter]))
)]
pub async fn file_chapters(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path(path): Path<crate::streaming::handlers::FileIdPath>,
) -> ApiResult<Json<Vec<Chapter>>> {
    let duration: Option<(Option<f64>,)> =
        sqlx::query_as("SELECT duration_seconds FROM media_files WHERE id = $1")
            .bind(path.file_id)
            .fetch_optional(&state.pool)
            .await?;

    let Some(Some(duration)) = duration.map(|(d,)| d) else {
        return Err(ApiError::not_found("probed file"));
    };

    let mut chapters = Vec::new();
    let step = 480.0f64;
    let mut t = 0.0f64;
    while t < duration && chapters.len() < 40 {
        chapters.push(Chapter {
            start_seconds: t,
            title: format!("Chapter {}", chapters.len() + 1),
        });
        t += step;
    }
    Ok(Json(chapters))
}


