use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use sqlx::PgPool;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct TranscodeJob {
    pub id: Uuid,
    pub file_id: Uuid,
    pub job_kind: String,
    pub status: String,
    pub progress: i16,
    pub output_dir: Option<String>,
    pub error: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct TranscodeService {
    pool: PgPool,
    cfg: Arc<crate::config::Config>,
    tx: mpsc::UnboundedSender<Uuid>,
    #[allow(dead_code)]
    cancel: CancellationToken,
    ffmpeg_available: bool,
}

impl TranscodeService {
    pub async fn start(
        pool: PgPool,
        cfg: Arc<crate::config::Config>,
        #[allow(dead_code)] cancel: CancellationToken,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let ffmpeg_available = which_exists(&cfg.ffmpeg_bin).await;
        if !ffmpeg_available {
            tracing::warn!("ffmpeg not found; transcoding jobs will fail gracefully");
        }

        let service = Self {
            pool,
            cfg,
            tx,
            cancel: cancel.clone(),
            ffmpeg_available,
        };

        let worker_pool = service.pool.clone();
        let worker_cfg = service.cfg.clone();
        let worker_cancel = cancel.clone();
        let ffmpeg = ffmpeg_available;
        tokio::spawn(async move {
            run_worker(worker_pool, worker_cfg, rx, worker_cancel, ffmpeg).await;
        });

        service
    }

    pub async fn enqueue_hls(&self, file_id: Uuid) -> ApiResult<Uuid> {
        self.enqueue(file_id, "hls_transcode").await
    }

    pub async fn enqueue_thumbnails(&self, file_id: Uuid) -> ApiResult<Uuid> {
        self.enqueue(file_id, "thumbnails").await
    }

    async fn enqueue(&self, file_id: Uuid, kind: &str) -> ApiResult<Uuid> {
        if !self.ffmpeg_available {
            return Err(ApiError::Unavailable(format!(
                "ffmpeg binary '{}' is not installed on the server",
                self.cfg.ffmpeg_bin
            )));
        }
        // Predictive cancellation: a newer request supersedes identical queued work.
        sqlx::query(
            "UPDATE transcode_jobs SET status = 'cancelled', error = 'superseded', finished_at = now() \
             WHERE file_id = $1 AND job_kind = $2 AND status = 'queued'",
        )
        .bind(file_id)
        .bind(kind)
        .execute(&self.pool)
        .await
        .map_err(ApiError::from)?;

        let job_id = new_id();
        sqlx::query("INSERT INTO transcode_jobs (id, file_id, job_kind) VALUES ($1, $2, $3)")
            .bind(job_id)
            .bind(file_id)
            .bind(kind)
            .execute(&self.pool)
            .await
            .map_err(ApiError::from)?;
        self.tx
            .send(job_id)
            .map_err(|_| ApiError::internal_msg("transcode queue closed"))?;
        Ok(job_id)
    }

    pub async fn job_status(&self, job_id: Uuid) -> ApiResult<TranscodeJob> {
        sqlx::query_as::<_, TranscodeJob>("SELECT * FROM transcode_jobs WHERE id = $1")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| ApiError::not_found("transcode job"))
    }

    #[allow(dead_code)]
    pub async fn jobs_for_file(&self, file_id: Uuid) -> ApiResult<Vec<TranscodeJob>> {
        Ok(sqlx::query_as::<_, TranscodeJob>(
            "SELECT * FROM transcode_jobs WHERE file_id = $1 ORDER BY created_at DESC LIMIT 20",
        )
        .bind(file_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn latest_completed_hls_dir(&self, file_id: Uuid) -> ApiResult<PathBuf> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT output_dir FROM transcode_jobs \
             WHERE file_id = $1 AND job_kind = 'hls_transcode' AND status = 'completed' AND output_dir IS NOT NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|(dir,)| PathBuf::from(dir))
            .ok_or_else(|| ApiError::not_found("completed hls transcode"))
    }

    pub async fn cancel_job(&self, job_id: Uuid) -> ApiResult<()> {
        let result =
            sqlx::query("UPDATE transcode_jobs SET status = 'cancelled', finished_at = now() WHERE id = $1 AND status IN ('queued', 'running')")
                .bind(job_id)
                .execute(&self.pool)
                .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("active transcode job"));
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn heights(&self) -> Vec<u32> {
        self.cfg.transcode_heights.clone()
    }
}

async fn which_exists(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok()
}

async fn run_worker(
    pool: PgPool,
    cfg: Arc<crate::config::Config>,
    mut rx: mpsc::UnboundedReceiver<Uuid>,
    #[allow(dead_code)] cancel: CancellationToken,
    _ffmpeg_available: bool,
) {
    while let Some(job_id) = rx.recv().await {
        if cancel.is_cancelled() {
            break;
        }
        let pool = pool.clone();
        let cfg = cfg.clone();
        let child_cancel = cancel.child_token();
        tokio::spawn(async move {
            if let Err(err) = execute_job(&pool, &cfg, job_id, child_cancel).await {
                tracing::error!(%job_id, error = %err, "transcode job failed");
                let _ = sqlx::query(
                    "UPDATE transcode_jobs SET status = 'failed', error = $2, finished_at = now() WHERE id = $1",
                )
                .bind(job_id)
                .bind(err.to_string())
                .execute(&pool)
                .await;
            }
        });
    }
}

struct JobContext {
    file_id: Uuid,
    kind: String,
    video_path: PathBuf,
}

async fn load_job_context(pool: &PgPool, job_id: Uuid) -> ApiResult<JobContext> {
    let row: Option<(Uuid, String, Option<String>, String)> = sqlx::query_as(
        "SELECT j.file_id, j.job_kind, lp.root_path, f.relative_path
         FROM transcode_jobs j
         JOIN media_files f ON f.id = j.file_id
         LEFT JOIN library_paths lp ON lp.id = f.library_path_id
         WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;

    let Some((file_id, kind, Some(root), relative_path)) = row else {
        return Err(ApiError::not_found("transcode job"));
    };

    let full_path = PathBuf::from(&root).join(&relative_path);
    if !full_path.starts_with(&root) {
        return Err(ApiError::forbidden("path traversal blocked"));
    }

    Ok(JobContext {
        file_id,
        kind,
        video_path: full_path,
    })
}

async fn mark_running(pool: &PgPool, job_id: Uuid, output_dir: &Path) -> ApiResult<()> {
    sqlx::query(
        "UPDATE transcode_jobs SET status = 'running', started_at = COALESCE(started_at, now()), output_dir = $2 WHERE id = $1",
    )
    .bind(job_id)
    .bind(output_dir.to_string_lossy().to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_progress(pool: &PgPool, job_id: Uuid, percent: i16) {
    let _ =
        sqlx::query("UPDATE transcode_jobs SET progress = $2 WHERE id = $1 AND status = 'running'")
            .bind(job_id)
            .bind(percent.clamp(0, 100))
            .execute(pool)
            .await;
}

async fn mark_completed(pool: &PgPool, job_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        "UPDATE transcode_jobs SET status = 'completed', progress = 100, finished_at = now() WHERE id = $1",
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn execute_job(
    pool: &PgPool,
    cfg: &crate::config::Config,
    job_id: Uuid,
    #[allow(dead_code)] cancel: CancellationToken,
) -> anyhow::Result<()> {
    let ctx = load_job_context(pool, job_id).await?;
    match ctx.kind.as_str() {
        "hls_transcode" => run_hls_job(pool, cfg, job_id, &ctx, cancel).await,
        "thumbnails" => run_thumbnails_job(pool, cfg, job_id, &ctx, cancel).await,
        other => Err(anyhow::anyhow!("unknown job kind '{other}'")),
    }
}

async fn run_hls_job(
    pool: &PgPool,
    cfg: &crate::config::Config,
    job_id: Uuid,
    ctx: &JobContext,
    #[allow(dead_code)] cancel: CancellationToken,
) -> anyhow::Result<()> {
    let duration = probe_duration(cfg, &ctx.video_path).await.unwrap_or(0.0);
    let source_height = probe_height(cfg, &ctx.video_path).await;
    // Per-title ladder: never upscale beyond the source.
    let heights: Vec<u32> = cfg
        .transcode_heights
        .iter()
        .copied()
        .filter(|h| source_height.map(|src| *h <= src as u32).unwrap_or(true))
        .collect();
    let heights = if heights.is_empty() {
        vec![*cfg.transcode_heights.iter().min().unwrap_or(&480)]
    } else {
        heights
    };
    let master_dir = cfg
        .storage_dir
        .join("hls")
        .join(job_id.simple().to_string());
    mark_running(pool, job_id, &master_dir).await?;

    for height in heights.iter().copied() {
        let out_dir = hls_variant_dir(cfg, job_id, height);
        tokio::fs::create_dir_all(&out_dir).await?;

        let playlist = out_dir.join("index.m3u8");
        let segment_pattern = out_dir.join("seg_%04d.ts");

        mark_running_for_variant(pool, job_id).await?;

        let mut command = Command::new(&cfg.ffmpeg_bin);
        command
            .args(["-y", "-v", "error", "-i"])
            .arg(&ctx.video_path)
            .args(["-vf", &format!("scale=-2:{height}")])
            .args(encoder_args(cfg))
            .args([
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-c:a",
                "aac",
                "-ac",
                "2",
                "-b:a",
                "128k",
                "-progress",
                "pipe:1",
                "-hls_time",
                "6",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(&segment_pattern)
            .arg(&playlist)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn()?;
        pump_progress(&mut child, pool, job_id, duration, cancel.clone()).await;

        let status = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                anyhow::bail!("cancelled");
            }
            status = child.wait() => status?,
        };
        if !status.success() {
            anyhow::bail!("ffmpeg exited with {status} for height {height}");
        }
    }

    write_master_playlist_dynamic(cfg, job_id, &heights).await?;
    if cfg.hls_encryption {
        // Phase J: stamp AES-128 KEY lines onto variant playlists. Keys live
        // behind /api/playback/keys/{file_id} (grant-gated), never in files.
        match encrypt_hls_playlists(pool, cfg, ctx.file_id, &master_dir, &heights).await {
            Ok(()) => tracing::info!(%job_id, "hls playlists encrypted"),
            Err(err) => tracing::warn!(%job_id, error = %err, "hls encryption skipped"),
        }
    }
    set_progress(pool, job_id, 99).await;
    mark_completed(pool, job_id).await?;
    tracing::info!(%job_id, "hls transcode completed");

    Ok(())
}

/// Inserts `#EXT-X-KEY` into every variant playlist (idempotent).
async fn encrypt_hls_playlists(
    pool: &PgPool,
    _cfg: &crate::config::Config,
    file_id: Uuid,
    master_dir: &std::path::Path,
    heights: &[u32],
) -> anyhow::Result<()> {
    let media_ref = file_id.to_string();
    // Mint v1 if this file has no active key yet.
    if crate::security::keys::active_media_key(pool, &media_ref).await.is_err() {
        crate::security::keys::issue_media_key(pool, &media_ref).await?;
    }
    let (_, media_key) = crate::security::keys::active_media_key(pool, &media_ref).await?;
    let content_key = crate::security::keys::hls_content_key(&media_key);

    use sha2::{Digest, Sha256};
    let iv: [u8; 16] = Sha256::digest(&content_key)[..16]
        .try_into()
        .expect("16 bytes from sha256");
    let iv_hex = hex::encode(iv);
    let key_line = format!(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"/api/playback/keys/{file_id}\",IV=0x{iv_hex}"
    );

    for height in heights {
        let playlist = master_dir.join(height.to_string()).join("index.m3u8");
        let Ok(existing) = tokio::fs::read_to_string(&playlist).await else {
            continue;
        };
        if existing.contains("#EXT-X-KEY") {
            continue;
        }
        let mut updated = String::with_capacity(existing.len() + key_line.len() + 2);
        for line in existing.lines() {
            updated.push_str(line);
            updated.push('\n');
            if line.trim() == "#EXTM3U" {
                updated.push_str(&key_line);
                updated.push('\n');
            }
        }
        tokio::fs::write(&playlist, updated).await?;
    }
    Ok(())
}

async fn mark_running_for_variant(pool: &PgPool, job_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE transcode_jobs SET status = 'running', started_at = COALESCE(started_at, now()) WHERE id = $1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn hls_variant_dir(cfg: &crate::config::Config, job_id: Uuid, height: u32) -> PathBuf {
    cfg.storage_dir
        .join("hls")
        .join(job_id.simple().to_string())
        .join(format!("{height}p"))
}


fn bandwidth_for(height: u32) -> u32 {
    match height {
        0..=360 => 800_000,
        361..=480 => 1_400_000,
        481..=720 => 3_000_000,
        721..=1080 => 6_000_000,
        _ => 12_000_000,
    }
}

async fn pump_progress(
    child: &mut Child,
    pool: &PgPool,
    job_id: Uuid,
    total: f64,
    #[allow(dead_code)] cancel: CancellationToken,
) {
    let Some(stdout) = child.stdout.take() else {
        return;
    };
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                break;
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(micros) = line.strip_prefix("out_time_ms=") {
                            if let Ok(us) = micros.trim().parse::<f64>() {
                                if total > 0.0 {
                                    let pct = ((us / 1_000_000.0) / total * 100.0) as i16;
                                    set_progress(pool, job_id, pct).await;
                                }
                            }
                        }
                    }
                    _ => break,
                }
            }
        }
    }
}

async fn probe_duration(cfg: &crate::config::Config, path: &Path) -> anyhow::Result<f64> {
    let output = Command::new(&cfg.ffprobe_bin)
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0))
}

async fn run_thumbnails_job(
    pool: &PgPool,
    cfg: &crate::config::Config,
    job_id: Uuid,
    ctx: &JobContext,
    #[allow(dead_code)] cancel: CancellationToken,
) -> anyhow::Result<()> {
    let out_dir = cfg
        .storage_dir
        .join("thumbs")
        .join(ctx.file_id.simple().to_string());
    tokio::fs::create_dir_all(&out_dir).await?;
    let pattern = out_dir.join("thumb_%05d.jpg");
    mark_running(pool, job_id, &out_dir).await?;

    let mut command = Command::new(&cfg.ffmpeg_bin);
    command
        .args(["-y", "-v", "error", "-skip_frame", "nokey", "-i"])
        .arg(&ctx.video_path)
        .args(["-vf", "fps=1/10,scale=320:-2", "-frames:v", "200"])
        .arg(&pattern)
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command.spawn()?;
    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            anyhow::bail!("cancelled");
        }
        status = child.wait() => status?,
    };

    if !status.success() {
        anyhow::bail!("thumbnail generation failed: {status}");
    }

    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&out_dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        entries.push(entry.file_name().to_string_lossy().to_string());
    }
    entries.sort();

    let mut tx = pool.begin().await?;
    for (position, name) in entries.iter().enumerate() {
        let offset = (position as i32) * 10;
        sqlx::query(
            "INSERT INTO thumbnails (id, file_id, second_offset, file_path) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (file_id, second_offset) DO NOTHING",
        )
        .bind(new_id())
        .bind(ctx.file_id)
        .bind(offset)
        .bind(out_dir.join(name).to_string_lossy().to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    mark_completed(pool, job_id).await?;
    tracing::info!(%job_id, thumbnails = entries.len(), "thumbnails generated");
    Ok(())
}

fn encoder_args(cfg: &crate::config::Config) -> [&'static str; 2] {
    match cfg.hwaccel.as_str() {
        "nvenc" => ["-c:v", "h264_nvenc"],
        "qsv" => ["-c:v", "h264_qsv"],
        _ => ["-c:v", "libx264"],
    }
}

async fn probe_height(cfg: &crate::config::Config, path: &Path) -> Option<i32> {
    let output = Command::new(&cfg.ffprobe_bin)
        .args([
            "-v",
            "quiet",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub async fn write_master_playlist_dynamic(
    cfg: &crate::config::Config,
    job_id: Uuid,
    heights: &[u32],
) -> anyhow::Result<()> {
    let dir = cfg
        .storage_dir
        .join("hls")
        .join(job_id.simple().to_string());
    let mut master = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    for height in heights {
        master.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION=-2x{}\n{height}p/index.m3u8\n",
            bandwidth_for(*height),
            height
        ));
    }
    tokio::fs::write(dir.join("master.m3u8"), master).await?;
    Ok(())
}

