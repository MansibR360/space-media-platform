use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, random_hex, sha256_hex};

/// Cross-device "resume anywhere" handoff: the watching device mints a
/// short-lived code (rendered as a QR), another device redeems it and gets
/// the exact file + position to continue. Single use, 3 minute TTL.
#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct HandoffTicket {
    pub id: Uuid,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Only ever shown once, at creation.
    pub code: String,
    pub position_seconds: f64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RedeemedHandoff {
    pub file_id: Uuid,
    pub media_id: Option<Uuid>,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
}

pub async fn create_handoff(
    pool: &PgPool,
    user_id: Uuid,
    session_id: Option<Uuid>,
) -> ApiResult<HandoffTicket> {
    let source: Option<(Uuid, f64)> = match session_id {
        Some(sid) => {
            sqlx::query_as(
                "SELECT file_id, position_seconds FROM playback_sessions \
                 WHERE id = $1 AND user_id = $2 AND ended_at IS NULL",
            )
            .bind(sid)
            .bind(user_id)
            .fetch_optional(pool)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT file_id, position_seconds FROM playback_sessions \
                 WHERE user_id = $1 AND ended_at IS NULL ORDER BY last_active_at DESC LIMIT 1",
            )
            .bind(user_id)
            .fetch_optional(pool)
            .await?
        }
    };

    let Some((file_id, position)) = source else {
        return Err(ApiError::not_found("active playback session"));
    };

    let code = format!(
        "{}-{}",
        random_hex(4).to_ascii_uppercase(),
        random_hex(4).to_ascii_uppercase()
    );
    let ticket = sqlx::query_as::<_, HandoffTicket>(
        "INSERT INTO playback_handoffs (id, token_hash, user_id, file_id, position_seconds, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + INTERVAL '3 minutes')
         RETURNING id, expires_at, $6 AS code, position_seconds",
    )
    .bind(new_id())
    .bind(sha256_hex(&code))
    .bind(user_id)
    .bind(file_id)
    .bind(position)
    .bind(&code)
    .fetch_one(pool)
    .await?;
    Ok(ticket)
}

pub async fn redeem_handoff(
    pool: &PgPool,
    user_id: Uuid,
    code: &str,
) -> ApiResult<RedeemedHandoff> {
    let mut tx = pool.begin().await?;

    let row: Option<(Uuid, Option<Uuid>, f64)> = sqlx::query_as(
        "UPDATE playback_handoffs SET used_at = now()
         WHERE token_hash = $1 AND user_id = $2 AND used_at IS NULL AND expires_at > now()
         RETURNING file_id, (SELECT media_id FROM media_files WHERE id = playback_handoffs.file_id), position_seconds",
    )
    .bind(sha256_hex(code.trim().to_ascii_uppercase().as_str()))
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((file_id, media_id, position)) = row else {
        tx.rollback().await.ok();
        return Err(ApiError::bad_request(
            "handoff code is invalid, expired or already used",
        ));
    };
    tx.commit().await?;

    let duration: Option<f64> =
        sqlx::query_scalar("SELECT duration_seconds FROM media_files WHERE id = $1")
            .bind(file_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    Ok(RedeemedHandoff {
        file_id,
        media_id,
        position_seconds: position,
        duration_seconds: duration,
    })
}

// ---------- intro / credits markers ----------

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct MediaMarker {
    pub kind: String,
    pub seconds: f64,
    pub source: String,
}

pub async fn markers_for_file(pool: &PgPool, file_id: Uuid) -> ApiResult<Vec<MediaMarker>> {
    Ok(sqlx::query_as::<_, MediaMarker>(
        "SELECT kind::text AS kind, seconds, source FROM media_markers WHERE file_id = $1 ORDER BY seconds",
    )
    .bind(file_id)
    .fetch_all(pool)
    .await?)
}

pub async fn upsert_marker(
    pool: &PgPool,
    file_id: Uuid,
    kind: &str,
    seconds: f64,
    source: &str,
) -> ApiResult<()> {
    if !matches!(kind, "intro_start" | "intro_end" | "credits_start") {
        return Err(ApiError::bad_request(
            "kind must be intro_start, intro_end or credits_start",
        ));
    }
    if !(0.0..=100_000.0).contains(&seconds) {
        return Err(ApiError::bad_request("seconds out of range"));
    }
    sqlx::query(
        "INSERT INTO media_markers (id, file_id, kind, seconds, source)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (file_id, kind) DO UPDATE SET seconds = EXCLUDED.seconds, source = EXCLUDED.source",
    )
    .bind(crate::utils::new_id())
    .bind(file_id)
    .bind(kind)
    .bind(seconds.max(0.0))
    .bind(source)
    .execute(pool)
    .await?;
    Ok(())
}

/// Heuristic auto-detection using ffmpeg silencedetect over the opening window.
/// The first sustained silence between 25s and 150s is treated as the end of
/// the cold open / start of credits-free zone; silence after 85% of runtime
/// marks credits. Good enough to power a Skip Intro button; users can correct
/// via the manual endpoint.
pub async fn detect_markers(
    ffmpeg_bin: &str,
    video_path: &std::path::Path,
    duration_hint: f64,
) -> ApiResult<(Option<f64>, Option<f64>)> {
    use tokio::process::Command;

    let output = Command::new(ffmpeg_bin)
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(video_path)
        .args(["-af", "silencedetect=noise=-35dB:d=0.8", "-f", "null", "-"])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| ApiError::Unavailable(format!("ffmpeg unavailable: {e}")))?;

    if !output.status.success() && output.stderr.is_empty() {
        return Err(ApiError::Unavailable("silence detection failed".into()));
    }

    let text = String::from_utf8_lossy(&output.stderr);
    let mut silences: Vec<f64> = Vec::new();
    for line in text.lines() {
        if let Some(idx) = line.find("silence_start:") {
            if let Ok(start) = line[idx + "silence_start:".len()..]
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .parse::<f64>()
            {
                silences.push(start);
            }
        }
    }

    let intro_end = silences
        .iter()
        .copied()
        .find(|t| (25.0..=150.0).contains(t));
    let credits_start = if duration_hint > 1.0 {
        silences
            .iter()
            .rev()
            .copied()
            .find(|t| *t >= duration_hint * 0.85)
    } else {
        None
    };

    Ok((intro_end, credits_start))
}
