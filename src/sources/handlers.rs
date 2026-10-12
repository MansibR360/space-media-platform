use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

fn resolve<'a>(state: &'a AppState, name: &str) -> ApiResult<&'a dyn crate::sources::NativeSource> {
    state
        .sources
        .get(name)
        .ok_or_else(|| ApiError::not_found(format!("source '{name}'")))
}

/// State-aware dispatch across built-ins and learned web sources.
pub(crate) async fn search_any(
    state: &AppState,
    name: &str,
    query: &str,
    limit: i32,
) -> ApiResult<Vec<crate::sources::SourceItem>> {
    if state.web_sources.get(name).await.is_some() {
        return state.web_sources.search(state, name, query, limit).await;
    }
    resolve(state, name)?.search(query, limit).await
}

pub(crate) async fn item_any(
    state: &AppState,
    name: &str,
    external_id: &str,
) -> ApiResult<crate::sources::SourceDetail> {
    if state.web_sources.get(name).await.is_some() {
        return state.web_sources.item(state, name, external_id).await;
    }
    resolve(state, name)?.item(external_id).await
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SourceSearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_source_limit")]
    pub limit: i32,
}

fn default_source_limit() -> i32 {
    24
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SourcePath {
    pub name: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SourceItemPath {
    pub name: String,
    pub external_id: String,
}

/// Lists builtin native sources, learned web sources, and remote source plugins.
#[utoipa::path(get, path = "/api/sources", tag = "sources",
    responses((status = 200))
)]
pub async fn list_sources(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut all = state.sources.describe();
    for entry in state.web_sources.list().await {
        all.push(entry.source.manifest());
    }
    for plugin in crate::plugins::repo::list(&state.pool)
        .await
        .unwrap_or_default()
    {
        if plugin.kind == "source" && plugin.enabled {
            // Name, label and category all live in the plugin's own config,
            // written there when it registered. Reporting "mixed" for every one
            // of them made the listing useless for grouping.
            let config = |key: &str| {
                plugin
                    .config
                    .get(key)
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            };
            all.push(json!({
                "name": plugin.name,
                "label": config("label").unwrap_or(&plugin.name),
                "category": config("category").unwrap_or("mixed"),
                "transport": "http",
            }));
        }
    }
    Json(json!({ "sources": all }))
}

#[utoipa::path(get, path = "/api/sources/{name}/search", tag = "sources",
    params(SourcePath, SourceSearchQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn search_source(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(path): AxPath<SourcePath>,
    AxQuery(query): AxQuery<SourceSearchQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let items = search_any(&state, &path.name, &query.q, query.limit).await?;
    Ok(Json(json!({ "items": items })))
}

#[utoipa::path(get, path = "/api/sources/{name}/item/{external_id}", tag = "sources",
    params(SourceItemPath),
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn source_item(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(path): AxPath<SourceItemPath>,
) -> ApiResult<Json<crate::sources::SourceDetail>> {
    let detail = item_any(&state, &path.name, &path.external_id).await?;
    Ok(Json(detail))
}

// ---------------- manga helpers ----------------

#[utoipa::path(get, path = "/api/sources/mangadex/chapters/{manga_id}", tag = "sources",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn manga_chapters(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(manga_id): AxPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let provider = resolve(&state, "mangadex")?;
    // Downcast-free: route straight through the concrete type.
    let chapters = crate::sources::mangadex::MangaDex::default()
        .chapters_for(&manga_id)
        .await?;
    let _ = provider;
    Ok(Json(json!({ "chapters": chapters })))
}

#[utoipa::path(get, path = "/api/sources/mangadex/pages/{chapter_id}", tag = "sources",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn manga_pages(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(chapter_id): AxPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let _ = resolve(&state, "mangadex")?;
    let pages = crate::sources::mangadex::MangaDex::default()
        .chapter_pages(&chapter_id)
        .await?;
    metrics::counter!("manga_pages_resolved_total").increment(pages.len() as u64);
    Ok(Json(json!({ "pages": pages })))
}

// ---------------- browse (sources give sources; search is separate) ----------------

/// Trending / popular feed for a single source. No query required.
#[utoipa::path(get, path = "/api/sources/{name}/browse", tag = "sources",
    params(SourcePath),
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
/// Browse one source, whatever transport it speaks.
///
/// Shared with the catalogs module so a source's feed can be rendered as a
/// home-page rail, not just fetched through its own endpoint.
pub async fn browse_items(
    state: &AppState,
    name: &str,
    limit: i32,
) -> ApiResult<Vec<crate::sources::SourceItem>> {
    if state.web_sources.get(name).await.is_some() {
        return search_any(state, name, "", limit).await;
    }
    let provider = resolve(state, name)?;
    match provider.browse(limit).await? {
        Some(items) => Ok(items),
        // No native browse hook; an empty query is these sources' "latest".
        None => Ok(provider.search("", limit).await?),
    }
}

pub async fn browse_source(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(path): AxPath<SourcePath>,
    AxQuery(query): AxQuery<SourceSearchQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    // Learned web sources have no paged browse hook; empty-query search acts
    // as their "latest" feed.
    if state.web_sources.get(&path.name).await.is_some() {
        let items = search_any(&state, &path.name, "", query.limit).await?;
        return Ok(Json(json!({ "source": path.name, "items": items })));
    }
    let provider = resolve(&state, &path.name)?;
    // Prefer the native browse hook; fall back to an empty-query search.
    let items = match provider.browse(query.limit).await? {
        Some(items) => items,
        None => provider.search("", query.limit).await?,
    };
    Ok(Json(json!({ "source": path.name, "items": items })))
}

/// Live health of every known source (circuit-breaker state).
#[utoipa::path(get, path = "/api/sources/status", tag = "sources",
    responses((status = 200))
)]
pub async fn status_all(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut snapshots = state.sources_health.snapshot_all().await;

    // Include never-probed natives + learned sources so the UI shows them as
    // unknown, not missing.
    let mut known_names: Vec<String> = state
        .sources
        .list()
        .iter()
        .map(|s| s.to_string())
        .collect();
    for entry in state.web_sources.list().await {
        known_names.push(entry.source.name().to_string());
    }
    let unknown = state.sources_health.snapshot_missing(&known_names).await;
    snapshots.extend(unknown);

    Json(json!({ "sources": snapshots }))
}

// ---------------- one-paste source registration (auto-detect) ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RegisterUrlRequest {
    /// Base URL of the plugin service, e.g. https://my-plugin.fly.dev
    pub url: String,
}

fn slug_from_url(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "plugin".into());
    let slug: String = host
        .trim_start_matches("www.")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}


/// Does this response actually carry JSON?
///
/// The probe used to accept any 2xx, so a site that serves its index page for
/// unknown paths — every single-page app — passed as a media source.
fn looks_like_json(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("json"))
}

/// Paste a base URL; SPACE probes it and registers whichever protocol speaks:
/// manifest.json (rich) → /items (source) → /search (metadata).
#[utoipa::path(post, path = "/api/admin/sources/register-url", tag = "admin",
    security(("bearer" = [])),
    request_body = RegisterUrlRequest,
    responses((status = 201), (status = 400))
)]
pub async fn register_source_url(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    Json(req): Json<RegisterUrlRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    use crate::plugins::http_provider::HttpPluginProvider;

    let url = req.url.trim_end_matches('/').to_string();
    HttpPluginProvider::checked("probe".into(), url.clone())?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client");

    // 1) manifest?
    let mut kind = String::new();
    let mut name_override: Option<String> = None;
    let mut detected = json!({});
    // Tracks whether anything answered at all. "Nothing is listening there" and
    // "something is listening but speaks a protocol we do not know" are
    // different problems with different fixes, and one message for both sent
    // people hunting a plugin bug when the process simply was not running.
    let mut reachable = false;

    if let Ok(resp) = client.get(format!("{url}/manifest.json")).send().await {
        reachable = true;
        if resp.status().is_success() {
            if let Ok(manifest) = resp.json::<serde_json::Value>().await {
                kind = manifest
                    .get("kind")
                    .or_else(|| manifest.get("type"))
                    .and_then(|k| k.as_str())
                    .unwrap_or("metadata")
                    .to_string();
                name_override = manifest
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| crate::utils::slugify(s));
                detected = manifest.clone();
            }
        }
    }

    // 2) items? (source)
    if kind.is_empty() {
        if let Ok(resp) = client
            .get(format!("{url}/items"))
            .query(&[("limit", "1")])
            .send()
            .await
        {
            reachable = true;
            // A 200 is not enough: any SPA answers 200 with its index.html for
            // unknown paths, which was enough to get a plain website
            // registered as a media source and dropped into the aggregator.
            if resp.status().is_success() && looks_like_json(&resp) {
                kind = "source".into();
            }
        }
    }

    // 3) search? (metadata)
    if kind.is_empty() {
        if let Ok(resp) = client
            .get(format!("{url}/search"))
            .query(&[("q", "probe")])
            .send()
            .await
        {
            reachable = true;
            if resp.status().is_success() && looks_like_json(&resp) {
                kind = "metadata".into();
            }
        }
    }

    if kind.is_empty() {
        return Err(ApiError::Unavailable(if reachable {
            format!(
                "{url} responded but does not look like a plugin - manifest.json, /items and /search returned no JSON"
            )
        } else {
            format!(
                "nothing answered at {url} - is the plugin process running on that port? (redirects are not followed, so give the final URL)"
            )
        }));
    }

    let mut name = name_override.unwrap_or_else(|| slug_from_url(&url));
    let mut suffix = 2;
    while crate::plugins::repo::find_by_name(&state.pool, &name)
        .await?
        .is_some()
    {
        name = format!("{}-{suffix}", slug_from_url(&url));
        suffix += 1;
    }

    let config = json!({ "detected_via_manifest": !detected.is_null(), "manifest": detected });
    let plugin = crate::plugins::repo::insert(
        &state.pool,
        crate::utils::new_id(),
        name.trim(),
        &kind,
        &url,
        config,
        true,
        Some(admin.0.id()),
    )
    .await?;

    // Hot-load metadata plugins into the live registry. A failure here leaves
    // the registry stale rather than breaking the source that was just added,
    // so it is logged rather than returned.
    if let Err(e) = crate::plugins::handlers::reload_metadata(&state).await {
        tracing::warn!(error = %e, "metadata registry reload failed after source autoregister");
    }

    metrics::counter!("sources_autoregistered_total").increment(1);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "plugin": plugin.view(),
            "detected_kind": kind,
        })),
    ))
}

async fn import_item(
    state: &AppState,
    source_name: &str,
    item: &crate::sources::SourceItem,
) -> ApiResult<Uuid> {
    use crate::catalog::models::{CreateMediaRequest, MediaStatus, MediaType};

    let media_type = match item.category {
        "movie" => MediaType::Movie,
        "series" => MediaType::Series,
        "book" => MediaType::Book,
        "music" => MediaType::Music,
        "manga" => MediaType::Manga,
        "podcast" => MediaType::Podcast,
        "audiobook" => MediaType::Audiobook,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown source category '{other}'"
            )));
        }
    };

    let artist = if item.category == "music" && item.overview.starts_with("by ") {
        Some(item.overview[3..].to_string())
    } else {
        None
    };

    let created = crate::catalog::repo::create(
        &state.pool,
        &CreateMediaRequest {
            media_type,
            title: item.title.clone(),
            is_anime: false,
            original_title: None,
            overview: Some(item.overview.clone()),
            release_date: item
                .year
                .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1)),
            runtime_minutes: None,
            status: Some(MediaStatus::Released),
            poster_url: item.poster.clone(),
            backdrop_url: None,
            genres: Vec::new(),
            studios: Vec::new(),
        },
    )
    .await?;

    sqlx::query("UPDATE media SET artist = COALESCE($2, artist) WHERE id = $1")
        .bind(created.id)
        .bind(artist)
        .execute(&state.pool)
        .await?;

    crate::catalog::repo::upsert_external_mapping(
        &state.pool,
        created.id,
        source_name,
        &item.external_id,
        json!({}),
    )
    .await?;

    crate::automation::dispatch(&state.pool, &state.hub, "media.added",
        serde_json::json!({ "source": source_name, "external_id": item.external_id, "title": item.title, "category": item.category })).await;
    crate::webhooks::emit(
        &state.pool,
        "media.added",
        serde_json::json!({ "source": source_name, "external_id": item.external_id, "title": item.title, "category": item.category }),
    )
    .await;
    Ok(created.id)
}

// ---------------- import / sync ----------------

/// Imports top/trending items from a native or HTTP source into the catalog.
#[utoipa::path(post, path = "/api/admin/sources/{name}/sync", tag = "admin",
    params(SourcePath),
    security(("bearer" = [])),
    request_body = serde_json::Value,
    responses((status = 200))
)]
pub async fn sync_source(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<SourcePath>,
    body: Option<Json<serde_json::Value>>,
) -> ApiResult<Json<serde_json::Value>> {
    crate::auth::permissions::require(
        auth.ctx.as_ref(),
        crate::auth::permissions::Permission::ManageLibrary,
    )?;

    let limit = body
        .as_ref()
        .and_then(|Json(b)| b.get("limit"))
        .and_then(|l| l.as_i64())
        .unwrap_or(25)
        .clamp(1, 100);
    let query = body
        .as_ref()
        .and_then(|Json(b)| b.get("query"))
        .and_then(|q| q.as_str())
        .unwrap_or("")
        .to_string();

    // Native path — direct call, no HTTP hop.
    if let Some(provider) = state.sources.get(&path.name) {
        let items = provider.search(&query, limit as i32).await?;
        let mut imported = 0usize;
        for item in &items {
            if import_item(&state, path.name.as_str(), item).await.is_ok() {
                imported += 1;
            }
        }
        metrics::counter!("source_sync_imported_total").increment(imported as u64);
        return Ok(Json(json!({ "imported": imported, "total": items.len() })));
    }

    // Learned web-source path (galaxy recipe).
    if state.web_sources.get(&path.name).await.is_some() {
        let items = search_any(&state, &path.name, &query, limit as i32).await?;
        let mut imported = 0usize;
        let mut skipped = 0usize;
        for item in &items {
            if crate::catalog::repo::media_for_external(&state.pool, &path.name, &item.external_id)
                .await?
                .is_some()
            {
                skipped += 1;
                continue;
            }
            if import_item(&state, path.name.as_str(), item).await.is_ok() {
                imported += 1;
            } else {
                skipped += 1;
            }
        }
        metrics::counter!("source_sync_imported_total").increment(imported as u64);
        return Ok(Json(json!({ "imported": imported, "skipped": skipped })));
    }

    // Remote HTTP source-plugin path.
    let plugin = crate::plugins::repo::find_by_name(&state.pool, &path.name)
        .await?
        .filter(|p| p.kind == "source")
        .ok_or_else(|| ApiError::not_found("source"))?;
    let base = plugin
        .base_url
        .clone()
        .ok_or_else(|| ApiError::bad_request("builtin plugins cannot sync"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    let resp = client
        .get(format!("{}/items", base.trim_end_matches('/')))
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("source plugin unreachable: {e}")))?;
    let body: serde_json::Value = resp.json().await.map_err(ApiError::from)?;
    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for item in items.into_iter().take(limit as usize) {
        let (Some(external_id), Some(title)) = (
            item.get("id").and_then(|v| v.as_str()).map(str::to_string),
            item.get("title")
                .and_then(|t| t.as_str())
                .map(str::to_string),
        ) else {
            skipped += 1;
            continue;
        };
        if crate::catalog::repo::media_for_external(&state.pool, &plugin.name, &external_id)
            .await?
            .is_some()
        {
            skipped += 1;
            continue;
        }

        let category = match item.get("type").and_then(|t| t.as_str()) {
            Some("movie") | Some("film") => "movie",
            Some("book") | Some("ebook") => "book",
            Some("track") | Some("song") => "music",
            Some("manga") => "manga",
            _ => "series",
        };
        let mapped = crate::sources::SourceItem {
            external_id,
            title,
            overview: item
                .get("overview")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string(),
            year: item.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
            poster: item
                .get("poster")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            category,
        };
        if import_item(&state, &plugin.name, &mapped).await.is_ok() {
            imported += 1;
        } else {
            skipped += 1;
        }
    }
    metrics::counter!("source_sync_imported_total").increment(imported as u64);
    Ok(Json(json!({ "imported": imported, "skipped": skipped })))
}
