use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PlaybackSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub file_id: Uuid,
    pub device_name: String,
    pub client: String,
    pub audio_stream_id: Option<Uuid>,
    pub subtitle_stream_id: Option<Uuid>,
    pub position_seconds: f64,
    pub started_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    user_id: Uuid,
    file_id: Uuid,
    device_name: String,
    client: String,
    audio_stream_id: Option<Uuid>,
    subtitle_stream_id: Option<Uuid>,
    position_seconds: f64,
    started_at: DateTime<Utc>,
    last_active_at: DateTime<Utc>,
}

impl From<SessionRow> for PlaybackSession {
    fn from(row: SessionRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            file_id: row.file_id,
            device_name: row.device_name,
            client: row.client,
            audio_stream_id: row.audio_stream_id,
            subtitle_stream_id: row.subtitle_stream_id,
            position_seconds: row.position_seconds,
            started_at: row.started_at,
            last_active_at: row.last_active_at,
        }
    }
}

pub struct CreateSession {
    pub file_id: Uuid,
    pub device_name: String,
    pub client: String,
    pub audio_stream_id: Option<Uuid>,
    pub subtitle_stream_id: Option<Uuid>,
}

pub async fn create(
    pool: &PgPool,
    user_id: Uuid,
    req: &CreateSession,
) -> ApiResult<PlaybackSession> {
    let linked: (Option<Uuid>,) =
        sqlx::query_as::<_, (Option<Uuid>,)>("SELECT media_id FROM media_files WHERE id = $1")
            .bind(req.file_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("file"))?;

    if linked.0.is_none() {
        return Err(ApiError::Conflict(
            "file is not matched to a catalog item yet; run the library scanner".into(),
        ));
    }

    let session = sqlx::query_as::<_, SessionRow>(
        "INSERT INTO playback_sessions (id, user_id, file_id, device_name, client, audio_stream_id, subtitle_stream_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING *",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(req.file_id)
    .bind(&req.device_name)
    .bind(&req.client)
    .bind(req.audio_stream_id)
    .bind(req.subtitle_stream_id)
    .fetch_one(pool)
    .await?;

    // NOTE: automation dispatch for playback.started runs in handlers (has hub).
    crate::webhooks::emit(
        pool,
        "playback.started",
        serde_json::json!({ "user_id": user_id, "file_id": req.file_id }),
    )
    .await;
    Ok(session.into())
}

pub async fn heartbeat(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Uuid,
    position_seconds: f64,
) -> ApiResult<()> {
    let result = sqlx::query(
        "UPDATE playback_sessions SET position_seconds = $3, last_active_at = now() \
         WHERE id = $1 AND user_id = $2 AND ended_at IS NULL",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(position_seconds.max(0.0))
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("playback session"));
    }
    Ok(())
}

pub async fn end(pool: &PgPool, user_id: Uuid, session_id: Uuid) -> ApiResult<()> {
    sqlx::query("UPDATE playback_sessions SET ended_at = now() WHERE id = $1 AND user_id = $2")
        .bind(session_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn my_active(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<PlaybackSession>> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT * FROM playback_sessions WHERE user_id = $1 AND ended_at IS NULL \
         ORDER BY last_active_at DESC LIMIT 20",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Episode-specific variant used for autoplay-next flows.
pub async fn resume_for_episode(
    pool: &PgPool,
    user_id: Uuid,
    episode_id: Uuid,
    max_height: i32,
) -> ApiResult<ResumeInfo> {
    let best_file: Option<(Uuid, f64)> = sqlx::query_as(
        "SELECT id, COALESCE(duration_seconds, 0) FROM media_files \
         WHERE episode_id = $1 AND height IS NOT NULL AND height <= $2
         ORDER BY height DESC LIMIT 1",
    )
    .bind(episode_id)
    .bind(max_height)
    .fetch_optional(pool)
    .await?;

    let Some((file_id, duration)) = best_file else {
        return Err(ApiError::not_found("playable file for this episode"));
    };

    let position: Option<(f64,)> = sqlx::query_as(
        "SELECT position_seconds FROM watch_history \
         WHERE user_id = $1 AND episode_id = $2 AND completed = FALSE \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(episode_id)
    .fetch_optional(pool)
    .await?;

    Ok(ResumeInfo {
        file_id,
        position_seconds: position.map(|(p,)| p).unwrap_or(0.0),
        duration_seconds: duration,
        audio_stream_id: None,
        subtitle_stream_id: None,
    })
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct ResumeInfo {
    pub file_id: Uuid,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub audio_stream_id: Option<Uuid>,
    pub subtitle_stream_id: Option<Uuid>,
}

/// Picks the best available file for a piece of media and returns resume state.
pub async fn resume_for_media(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    max_height: i32,
) -> ApiResult<ResumeInfo> {
    let best_file: Option<(Uuid, f64)> = sqlx::query_as(
        "SELECT id, COALESCE(duration_seconds, 0) FROM media_files \
         WHERE media_id = $1 AND height IS NOT NULL AND height <= $2
         ORDER BY height DESC LIMIT 1",
    )
    .bind(media_id)
    .bind(max_height)
    .fetch_optional(pool)
    .await?;

    let best_file = match best_file {
        Some(found) => Some(found),
        None => {
            sqlx::query_as::<_, (Uuid, f64)>(
                "SELECT id, COALESCE(duration_seconds, 0) FROM media_files WHERE media_id = $1 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(media_id)
            .fetch_optional(pool)
            .await?
        }
    };

    let Some((file_id, duration)) = best_file else {
        return Err(ApiError::not_found("playable file for this media"));
    };

    let position: Option<(f64,)> = sqlx::query_as(
        "SELECT position_seconds FROM watch_history \
         WHERE user_id = $1 AND media_id = $2 AND completed = FALSE \
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    let prefs: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT audio_stream_id, subtitle_stream_id FROM playback_sessions \
         WHERE user_id = $1 AND file_id IN (SELECT id FROM media_files WHERE media_id = $2) \
           AND (audio_stream_id IS NOT NULL OR subtitle_stream_id IS NOT NULL) \
         ORDER BY last_active_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    Ok(ResumeInfo {
        file_id,
        position_seconds: position.map(|(p,)| p).unwrap_or(0.0),
        duration_seconds: duration,
        audio_stream_id: prefs.as_ref().and_then(|(a, _)| *a),
        subtitle_stream_id: prefs.and_then(|(_, s)| s),
    })
}
