//! Learned web sources — any media site becomes a first-class catalog.
//!
//! A "web source" is a scrape *recipe* inferred once by the bundled
//! [galaxy](https://github.com) scraper (`galaxy source learn`) from nothing
//! more than a site url (optionally a search template and one sample item).
//! At runtime SPACE replays the recipe by shelling out to the local galaxy
//! binary: search → listing extraction, resolve → detail + stream discovery.
//! No AI, no fragile per-site code, fully deterministic.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ------------------------------------------------------------------- model

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WebSourceRow {
    pub id: Uuid,
    pub name: String,
    pub label: String,
    pub site: String,
    pub category: String,
    pub search_url: String,
    #[serde(default)]
    pub sample_item: Option<String>,
    pub recipe: serde_json::Value,
    pub enabled: bool,
    pub status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    pub items_sampled: i32,
    #[serde(skip_serializing)]
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub mod repo {
    use super::WebSourceRow;
    use crate::error::{ApiError, ApiResult};
    use sqlx::PgPool;
    use uuid::Uuid;

    pub async fn list(pool: &PgPool) -> ApiResult<Vec<WebSourceRow>> {
        Ok(sqlx::query_as::<_, WebSourceRow>(
            "SELECT * FROM web_sources ORDER BY created_at",
        )
        .fetch_all(pool)
        .await?)
    }

    pub async fn get(pool: &PgPool, id: Uuid) -> ApiResult<WebSourceRow> {
        sqlx::query_as::<_, WebSourceRow>("SELECT * FROM web_sources WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("web source"))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert(
        pool: &PgPool,
        row: &WebSourceRow,
        created_by: Option<Uuid>,
    ) -> ApiResult<WebSourceRow> {
        sqlx::query_as::<_, WebSourceRow>(
            "INSERT INTO web_sources
                (id, name, label, site, category, search_url, sample_item, recipe,
                 enabled, status, items_sampled, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             RETURNING *",
        )
        .bind(row.id)
        .bind(&row.name)
        .bind(&row.label)
        .bind(&row.site)
        .bind(&row.category)
        .bind(&row.search_url)
        .bind(&row.sample_item)
        .bind(&row.recipe)
        .bind(row.enabled)
        .bind(&row.status)
        .bind(row.items_sampled)
        .bind(created_by)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn set_status(
        pool: &PgPool,
        id: Uuid,
        status: &str,
        last_error: Option<&str>,
        items_sampled: Option<i32>,
    ) -> ApiResult<()> {
        sqlx::query(
            "UPDATE web_sources SET status = $2, last_error = $3,
                items_sampled = COALESCE($4, items_sampled), updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(status)
        .bind(last_error)
        .bind(items_sampled)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn update_recipe(
        pool: &PgPool,
        id: Uuid,
        recipe: serde_json::Value,
        search_url: &str,
        category: &str,
        status: &str,
        items_sampled: i32,
    ) -> ApiResult<()> {
        sqlx::query(
            "UPDATE web_sources SET recipe = $2, search_url = $3, category = $4,
                status = $5, items_sampled = $6, last_error = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(id)
        .bind(recipe)
        .bind(search_url)
        .bind(category)
        .bind(status)
        .bind(items_sampled)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn patch(
        pool: &PgPool,
        id: Uuid,
        label: Option<&str>,
        category: Option<&str>,
        enabled: Option<bool>,
    ) -> ApiResult<WebSourceRow> {
        sqlx::query_as::<_, WebSourceRow>(
            "UPDATE web_sources SET
                label = COALESCE($2, label),
                category = COALESCE($3, category),
                enabled = COALESCE($4, enabled),
                updated_at = now()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(label)
        .bind(category)
        .bind(enabled)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn delete(pool: &PgPool, id: Uuid) -> ApiResult<()> {
        let result = sqlx::query("DELETE FROM web_sources WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("web source"));
        }
        Ok(())
    }
}

// ------------------------------------------------------------ galaxy bridge

/// Resolves the galaxy binary: explicit config → well-known locations → PATH.
pub fn resolve_galaxy_bin(cfg: &crate::config::Config) -> Option<String> {
    if let Some(bin) = &cfg.galaxy_bin {
        return Some(bin.clone());
    }
    const CANDIDATES: &[&str] = &[
        "./galaxy.exe",
        "./galaxy",
        "../galaxy/target/release/galaxy.exe",
        "../galaxy/target/release/galaxy",
        "target/release/galaxy.exe",
    ];
    for cand in CANDIDATES {
        let path = std::path::Path::new(cand);
        if path.is_file() {
            return Some(cand.to_string());
        }
    }
    // fall back to PATH via `where`/`which`
    #[cfg(windows)]
    let lookup = std::process::Command::new("where").arg("galaxy").output();
    #[cfg(not(windows))]
    let lookup = std::process::Command::new("which").arg("galaxy").output();
    if let Ok(out) = lookup
        && out.status.success()
    {
        let first = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !first.is_empty() {
            return Some(first);
        }
    }
    None
}

pub struct GalaxyOutput {
    pub stdout: String,
    pub stderr_tail: String,
}

/// Runs `galaxy source <op>` with an optional stdin payload (the recipe).
/// stdout carries pure JSON; all logs go to stderr.
pub async fn run_galaxy(
    cfg: &crate::config::Config,
    args: &[String],
    stdin_payload: Option<String>,
    timeout_secs: u64,
) -> ApiResult<GalaxyOutput> {
    if !cfg.web_sources_enabled {
        return Err(ApiError::Unavailable("web sources are disabled".into()));
    }
    let bin = resolve_galaxy_bin(cfg).ok_or_else(|| {
        ApiError::Unavailable(
            "galaxy binary not found; set GALAXY_BIN to your galaxy.exe path".into(),
        )
    })?;

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("-q") // quiet: errors only on stderr
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // never flash a console window on the server host
        const CREATE_NO_WINDOW: u16 = 0x0800;
        cmd.creation_flags(CREATE_NO_WINDOW as u32);
    }

    let mut child = cmd.spawn().map_err(|e| {
        ApiError::Unavailable(format!("cannot launch galaxy ({bin}): {e}"))
    })?;

    if let Some(mut stdin) = child.stdin.take()
        && let Some(payload) = stdin_payload
    {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    drop(child.stdin.take());

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await;

    match result {
        Err(_) => Err(ApiError::Unavailable(format!(
            "galaxy timed out after {timeout_secs}s"
        ))),
        Ok(Err(e)) => Err(ApiError::Unavailable(format!("galaxy failed: {e}"))),
        Ok(Ok(output)) => {
            let stderr_tail: String = String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            if !output.status.success() {
                return Err(ApiError::Unavailable(format!(
                    "galaxy rejected the request: {stderr_tail}"
                )));
            }
            Ok(GalaxyOutput {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr_tail,
            })
        }
    }
}

fn net_args(cfg: &crate::config::Config) -> Vec<String> {
    vec![
        "--strategy".into(),
        "http".into(),
        "--timeout-secs".into(),
        (cfg.galaxy_timeout_secs.saturating_sub(2)).to_string(),
        "--retries".into(),
        "1".into(),
    ]
}

// ------------------------------------------------------- runtime provider

const VALID_CATEGORIES: &[&str] = &["movie", "series", "book", "music", "manga"];

fn static_category(category: &str) -> &'static str {
    match category {
        "book" => "book",
        "music" => "music",
        "manga" => "manga",
        "series" => "series",
        _ => "movie",
    }
}

/// One learned source, live in the registry.
pub struct WebSource {
    pub id: Uuid,
    name: &'static str,
    label: &'static str,
    category: &'static str,
    pub site: String,
    recipe_json: String,
}

impl WebSource {
    fn from_row(row: &WebSourceRow) -> Self {
        Self {
            id: row.id,
            // Leaked intentionally: NativeSource requires 'static names and
            // sources are re-created only on rare admin mutations.
            name: Box::leak(row.name.clone().into_boxed_str()),
            label: Box::leak(if row.label.is_empty() {
                row.name.clone()
            } else {
                row.label.clone()
            }
            .into_boxed_str()),
            category: static_category(&row.category),
            site: row.site.clone(),
            recipe_json: row.recipe.to_string(),
        }
    }

    async fn call_search(
        &self,
        state: &AppState,
        query: &str,
        limit: i32,
    ) -> ApiResult<Vec<crate::sources::SourceItem>> {
        let mut args = vec![
            "source".into(),
            "search".into(),
            "--recipe".into(),
            "-".into(),
            "--limit".into(),
            limit.clamp(1, 60).to_string(),
        ];
        args.extend(net_args(&state.cfg));
        args.push("--".into());
        args.push(query.to_string());

        let out = run_galaxy(
            &state.cfg,
            &args,
            Some(self.recipe_json.clone()),
            state.cfg.galaxy_timeout_secs + 10,
        )
        .await?;
        parse_search_output(&out.stdout, self.category)
    }

    async fn call_item(
        &self,
        state: &AppState,
        external_id: &str,
    ) -> ApiResult<crate::sources::SourceDetail> {
        let mut args = vec![
            "source".into(),
            "item".into(),
            "--recipe".into(),
            "-".into(),
        ];
        args.extend(net_args(&state.cfg));
        args.push("--".into());
        args.push(external_id.to_string());

        let out = run_galaxy(
            &state.cfg,
            &args,
            Some(self.recipe_json.clone()),
            state.cfg.galaxy_timeout_secs + 10,
        )
        .await?;
        parse_item_output(&out.stdout, self.category)
    }

    /// Registry name (stable for the lifetime of the process).
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn category(&self) -> &'static str {
        self.category
    }

    /// Debug/registry description.
    pub fn manifest(&self) -> serde_json::Value {
        json!({
            "name": self.name,
            "label": self.label,
            "category": self.category,
            "transport": "web-scrape",
            "site": self.site,
            "capabilities": {
                "categories": [self.category],
                "kinds": ["mp4", "hls", "page", "file"],
                "subtitles": false,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct GalaxySearchResponse {
    #[serde(default)]
    items: Vec<GalaxyHit>,
}

#[derive(Debug, Deserialize)]
struct GalaxyHit {
    external_id: String,
    title: String,
    #[serde(default)]
    overview: String,
    year: Option<i32>,
    poster: Option<String>,
}

fn parse_search_output(
    stdout: &str,
    category: &'static str,
) -> ApiResult<Vec<crate::sources::SourceItem>> {
    let parsed: GalaxySearchResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| ApiError::Unavailable(format!("unparseable galaxy output: {e}")))?;
    Ok(parsed
        .items
        .into_iter()
        .map(|hit| crate::sources::SourceItem {
            external_id: hit.external_id,
            title: hit.title,
            overview: hit.overview,
            year: hit.year,
            poster: hit.poster,
            category,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
struct GalaxyItemResponse {
    item: GalaxyDetail,
}

#[derive(Debug, Deserialize)]
struct GalaxyDetail {
    external_id: String,
    title: String,
    #[serde(default)]
    overview: String,
    year: Option<i32>,
    poster: Option<String>,
    #[serde(default)]
    streams: Vec<GalaxyStream>,
}

#[derive(Debug, Deserialize)]
struct GalaxyStream {
    label: String,
    url: String,
    kind: String,
}

fn parse_item_output(stdout: &str, category: &'static str) -> ApiResult<crate::sources::SourceDetail> {
    let parsed: GalaxyItemResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| ApiError::Unavailable(format!("unparseable galaxy output: {e}")))?;
    let d = parsed.item;
    Ok(crate::sources::SourceDetail {
        item: crate::sources::SourceItem {
            external_id: d.external_id,
            title: d.title,
            overview: d.overview,
            year: d.year,
            poster: d.poster,
            category,
        },
        streams: d
            .streams
            .into_iter()
            .map(|s| crate::sources::StreamLink {
                label: s.label,
                url: s.url,
                kind: s.kind,
            })
            .collect(),
        chapters: None,
    })
}

// ----------------------------------------------------------------- manager

/// Holds every enabled learned source, hot-swappable from admin actions.
#[derive(Default)]
pub struct WebSourceManager {
    inner: RwLock<Vec<Arc<ManagerEntry>>>,
}

pub struct ManagerEntry {
    pub source: WebSource,
}

impl WebSourceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reloads from the DB. Failures are logged and leave the previous set.
    pub async fn reload(&self, pool: &PgPool, cfg: &crate::config::Config) {
        let rows = match repo::list(pool).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(error = %err, "web sources reload failed");
                return;
            }
        };
        if !cfg.web_sources_enabled {
            *self.inner.write().await = Vec::new();
            return;
        }
        let sources: Vec<Arc<ManagerEntry>> = rows
            .iter()
            .filter(|r| r.enabled)
            .filter(|r| r.recipe.get("version").is_some())
            .map(|r| {
                Arc::new(ManagerEntry {
                    source: WebSource::from_row(r),
                })
            })
            .collect();
        tracing::debug!(count = sources.len(), "web sources loaded");
        *self.inner.write().await = sources;
    }

    pub async fn list(&self) -> Vec<Arc<ManagerEntry>> {
        self.inner.read().await.clone()
    }

    pub async fn get(&self, name: &str) -> Option<Arc<ManagerEntry>> {
        self.inner
            .read()
            .await
            .iter()
            .find(|e| e.source.name == name)
            .cloned()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// State-aware search dispatch (used by the aggregator).
    pub async fn search(
        &self,
        state: &AppState,
        name: &str,
        query: &str,
        limit: i32,
    ) -> ApiResult<Vec<crate::sources::SourceItem>> {
        let entry = self
            .get(name)
            .await
            .ok_or_else(|| ApiError::not_found(format!("web source '{name}'")))?;
        entry.source.call_search(state, query, limit).await
    }

    /// State-aware item dispatch (used by the resolver).
    pub async fn item(
        &self,
        state: &AppState,
        name: &str,
        external_id: &str,
    ) -> ApiResult<crate::sources::SourceDetail> {
        let entry = self
            .get(name)
            .await
            .ok_or_else(|| ApiError::not_found(format!("web source '{name}'")))?;
        entry.source.call_item(state, external_id).await
    }
}
