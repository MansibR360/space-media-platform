use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::streaming::parser::{self, ParsedFileName};
use crate::utils::new_id;

pub const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "avi", "webm", "mov", "m4v", "ts"];

/// Books are first-class citizens: the scanner ingests ebooks alongside video.
pub const BOOK_EXTENSIONS: &[&str] = &["epub", "pdf", "mobi", "azw3", "fb2", "djvu", "cbz", "cbr"];

struct DiscoveredFile {
    relative_path: String,
    file_name: String,
    size: u64,
    modified: Option<std::time::SystemTime>,
}

fn collect_video_files(root: &Path) -> Vec<DiscoveredFile> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(8)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if !VIDEO_EXTENSIONS.contains(&ext.as_str()) && !BOOK_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        files.push(DiscoveredFile {
            relative_path: relative,
            file_name: name,
            size: meta.len(),
            modified: meta.modified().ok(),
        });
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    files
}

#[derive(Debug, Default, serde::Serialize)]
pub struct ScanReport {
    pub library_path: PathBuf,
    pub scanned_files: usize,
    pub created_media: usize,
    pub linked_existing: usize,
    pub created_episodes: usize,
    pub updated_files: usize,
    /// files whose size matched — skipped by the incremental fast path
    pub skipped_unchanged: usize,
    /// same path, different bytes → replacement event recorded
    pub replaced: usize,
    /// previously seen files absent from disk during this scan
    pub missing: usize,
}

async fn ensure_library_path(
    pool: &PgPool,
    root: &str,
    label: Option<&str>,
) -> anyhow::Result<uuid::Uuid> {
    let existing: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM library_paths WHERE root_path = $1")
            .bind(root)
            .fetch_optional(pool)
            .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let id = new_id();
    sqlx::query("INSERT INTO library_paths (id, root_path, label) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(root)
        .bind(label)
        .execute(pool)
        .await?;
    Ok(id)
}

async fn upsert_book_file(
    pool: &PgPool,
    library_id: uuid::Uuid,
    media_id: uuid::Uuid,
    file: &DiscoveredFile,
    ext: &str,
) -> anyhow::Result<()> {
    let modified_at: Option<chrono::DateTime<chrono::Utc>> = file
        .modified
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0));

    sqlx::query(
        "INSERT INTO media_files (id, media_id, library_path_id, relative_path, file_name, \
         file_size, modified_at, container)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (library_path_id, relative_path) DO UPDATE SET
            media_id = EXCLUDED.media_id,
            file_size = EXCLUDED.file_size,
            modified_at = EXCLUDED.modified_at,
            updated_at = now()",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(library_id)
    .bind(&file.relative_path)
    .bind(&file.file_name)
    .bind(file.size as i64)
    .bind(modified_at)
    .bind(Some(ext.to_string()))
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn scan_library(pool: &PgPool, root: &Path) -> anyhow::Result<ScanReport> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_str = canonical.to_string_lossy().to_string();
    let library_id = ensure_library_path(pool, &root_str, None).await?;
    let scan_started = chrono::Utc::now();

    let report_root = canonical.clone();
    let discovered = tokio::task::spawn_blocking(move || collect_video_files(&report_root))
        .await
        .map_err(|e| anyhow::anyhow!("scan task panicked: {e}"))?;

    let mut report = ScanReport {
        library_path: canonical.clone(),
        scanned_files: discovered.len(),
        ..Default::default()
    };

    let mut media_cache: HashMap<String, uuid::Uuid> = HashMap::new();

    for file in discovered {
        let stem = Path::new(&file.file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| file.file_name.clone());
        let parsed: ParsedFileName = parser::parse(&stem);

        // Books skip the series/episode machinery entirely.
        let ext_lower = Path::new(&file.file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if BOOK_EXTENSIONS.contains(&ext_lower.as_str()) {
            let existing = crate::catalog::repo::find_media_by_title(pool, &parsed.title).await?;
            let media_id = match existing {
                Some(media) => {
                    report.linked_existing += 1;
                    media.id
                }
                None => {
                    let created = crate::catalog::repo::create(
                        pool,
                        &crate::catalog::models::CreateMediaRequest {
                            media_type: crate::catalog::models::MediaType::Book,
                            title: parsed.title.clone(),
                            status: Some(crate::catalog::models::MediaStatus::Released),
                            overview: Some("Imported ebook.".into()),
                            original_title: None,
                            release_date: parsed
                                .year
                                .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1)),
                            runtime_minutes: None,
                            poster_url: None,
                            backdrop_url: None,
                            is_anime: false,
                            genres: Vec::new(),
                            studios: Vec::new(),
                        },
                    )
                    .await?;
                    report.created_media += 1;
                    created.id
                }
            };
            upsert_book_file(pool, library_id, media_id, &file, &ext_lower).await?;
            report.updated_files += 1;
            continue;
        }

        let is_episodic = parsed.season.is_some() && parsed.episode.is_some();

        let media_id = match media_cache.get(&parsed.title) {
            Some(id) => *id,
            None => {
                let existing =
                    crate::catalog::repo::find_media_by_title(pool, &parsed.title).await?;
                let id = match existing {
                    Some(media) => {
                        report.linked_existing += 1;
                        media.id
                    }
                    None => {
                        let created = crate::catalog::repo::create(
                            pool,
                            &crate::catalog::models::CreateMediaRequest {
                                // Season/episode markers in the filename are
                                // what make something a series. Without them
                                // it is a film — this used to import every
                                // file as a series, so a standalone movie
                                // never appeared under Movies at all.
                                media_type: if is_episodic {
                                    crate::catalog::models::MediaType::Series
                                } else {
                                    crate::catalog::models::MediaType::Movie
                                },
                                title: parsed.title.clone(),
                                status: Some(if is_episodic {
                                    crate::catalog::models::MediaStatus::Ongoing
                                } else {
                                    crate::catalog::models::MediaStatus::Released
                                }),
                                overview: Some(format!(
                                    "Imported automatically from {}.",
                                    root_str
                                )),
                                original_title: None,
                                release_date: parsed
                                    .year
                                    .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1)),
                                runtime_minutes: None,
                                poster_url: None,
                                backdrop_url: None,
                                is_anime: false,
                                genres: Vec::new(),
                                studios: Vec::new(),
                            },
                        )
                        .await?;
                        report.created_media += 1;
                        created.id
                    }
                };
                media_cache.insert(parsed.title.clone(), id);
                id
            }
        };

        let episode_id = match (parsed.season, parsed.episode) {
            (Some(season), Some(episode)) => {
                let season_row =
                    crate::catalog::repo::ensure_season_number(pool, media_id, season).await?;
                let episode_row = crate::catalog::repo::ensure_episode(
                    pool,
                    media_id,
                    season_row.id,
                    episode,
                    &file.file_name,
                )
                .await?;
                report.created_episodes += 1;
                Some(episode_row.id)
            }
            _ => None,
        };

        let modified_at: Option<chrono::DateTime<chrono::Utc>> = file
            .modified
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                let secs = d.as_secs() as i64;
                chrono::DateTime::from_timestamp(secs, 0).map(|t| t)
            })
            .flatten();

        // ---- incremental fast path: unchanged files only get a touch ----
        let existing: Option<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, file_size FROM media_files \
             WHERE library_path_id = $1 AND relative_path = $2",
        )
        .bind(library_id)
        .bind(&file.relative_path)
        .fetch_optional(pool)
        .await?;

        if let Some((existing_id, existing_size)) = existing {
            if existing_size == file.size as i64 {
                sqlx::query(
                    "UPDATE media_files SET last_seen_at = now(), missing = FALSE WHERE id = $1",
                )
                .bind(existing_id)
                .execute(pool)
                .await?;
                report.skipped_unchanged += 1;
                continue;
            }
            // Size changed → the file was replaced; keep an audit trail.
            sqlx::query(
                "INSERT INTO file_events (id, file_id, kind, detail) VALUES ($1, $2, 'replaced', $3)",
            )
            .bind(new_id())
            .bind(existing_id)
            .bind(
                serde_json::json!({
                    "old_size": existing_size,
                    "new_size": file.size,
                }),
            )
            .execute(pool)
            .await?;
            report.replaced += 1;
        }

        let updated = sqlx::query(
            "INSERT INTO media_files (id, media_id, episode_id, library_path_id, relative_path, \
             file_name, file_size, modified_at, container, last_seen_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
             ON CONFLICT (library_path_id, relative_path) DO UPDATE SET
                media_id = EXCLUDED.media_id,
                episode_id = EXCLUDED.episode_id,
                file_size = EXCLUDED.file_size,
                modified_at = EXCLUDED.modified_at,
                last_seen_at = now(),
                missing = FALSE,
                updated_at = now()
             RETURNING id",
        )
        .bind(new_id())
        .bind(media_id)
        .bind(episode_id)
        .bind(library_id)
        .bind(&file.relative_path)
        .bind(&file.file_name)
        .bind(file.size as i64)
        .bind(modified_at)
        .bind(
            Path::new(&file.file_name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase()),
        )
        .fetch_one(pool)
        .await?;
        let file_row_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM media_files WHERE library_path_id = $1 AND relative_path = $2",
        )
        .bind(library_id)
        .bind(&file.relative_path)
        .fetch_one(pool)
        .await?;
        let _ = updated;

        if existing.is_none() {
            sqlx::query("INSERT INTO file_events (id, file_id, kind) VALUES ($1, $2, 'added')")
                .bind(new_id())
                .bind(file_row_id)
                .execute(pool)
                .await?;
        }
        report.updated_files += 1;
    }

    // ---- missing-file detection: seen before this scan but absent now ----
    let missing_count = sqlx::query_scalar::<_, i64>(
        "WITH marked AS (
             UPDATE media_files SET missing = TRUE
             WHERE library_path_id = $1 AND COALESCE(last_seen_at, created_at) < $2
             RETURNING id
         )
         SELECT COUNT(*)::bigint FROM marked",
    )
    .bind(library_id)
    .bind(scan_started)
    .fetch_one(pool)
    .await?;
    report.missing = missing_count as usize;

    Ok(report)
}

pub async fn scan_all_configured(pool: &PgPool, roots: &[PathBuf]) -> Vec<ScanReport> {
    let mut reports = Vec::new();
    for root in roots {
        match scan_library(pool, root).await {
            Ok(report) => {
                tracing::info!(root = %root.display(), scanned = report.scanned_files, "library scan done");
                reports.push(report);
            }
            Err(err) => {
                tracing::error!(root = %root.display(), error = %err, "library scan failed")
            }
        }
    }
    reports
}

pub fn spawn_scheduler(
    pool: PgPool,
    roots: Vec<PathBuf>,
    interval: Duration,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    scan_all_configured(&pool, &roots).await;
                }
            }
        }
    });
}
