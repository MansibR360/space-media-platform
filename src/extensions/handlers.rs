//! Extension-system endpoints.
//!
//! Admin management mirrors Stremio's install flow (paste a manifest url),
//! plus discovery through the central addon index. User endpoints expose the
//! raw SDK resources 1:1 so any stremio-shaped client works.

use axum::extract::{ConnectInfo, Path as AxPath, Query as AxQuery, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::extensions::{
    self, catalogs, http_get_json_cfg, repo, searchable_catalogs, supports_resource,
    ExtensionRow,
};
use crate::state::AppState;

fn require_loopback(peer: SocketAddr) -> ApiResult<()> {
    if crate::admin_gate::is_loopback(peer.ip()) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "extension management is strictly localhost-only (home servers)",
        ))
    }
}

// ---------------------------------------------------------------- install

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct InstallExtensionRequest {
    /// e.g. https://torrentio.strem.fun/manifest.json
    pub url: String,
}

fn manifest_view(row: &ExtensionRow) -> Value {
    json!({
        "id": row.id,
        "addon_id": row.addon_id,
        "name": row.name,
        "version": row.manifest.get("version").cloned().unwrap_or(json!("?")),
        "manifest_url": row.manifest_url,
        "enabled": row.enabled,
        "types": row.manifest.get("types").cloned().unwrap_or(json!([])),
        "resources": row.manifest.get("resources").cloned().unwrap_or(json!([])),
        "catalogs": catalogs(&row.manifest),
        "logo": row.manifest.get("logo").cloned(),
        "description": row.manifest.get("description").cloned(),
    })
}

/// Installs an extension by manifest url (the exact stremio flow).
#[utoipa::path(post, path = "/api/admin/extensions", tag = "admin",
    security(("bearer" = [])),
    request_body = InstallExtensionRequest,
    responses((status = 201), (status = 400), (status = 403), (status = 503))
)]
pub async fn install_extension(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<InstallExtensionRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_loopback(peer)?;
    let url = req.url.trim().trim_end_matches('/').to_string();
    let manifest_url = if url.ends_with("/manifest.json") {
        url.clone()
    } else {
        format!("{url}/manifest.json")
    };
    if !manifest_url.starts_with("http://") && !manifest_url.starts_with("https://") {
        return Err(ApiError::bad_request("url must be absolute http(s)"));
    }

    let manifest = extensions::fetch_manifest(&state.cfg, &manifest_url).await?;
    let row = ExtensionRow {
        id: crate::utils::new_id(),
        manifest_url: manifest_url.clone(),
        manifest: manifest.clone(),
        addon_id: manifest["id"].as_str().unwrap_or_default().to_string(),
        name: manifest["name"].as_str().unwrap_or("Extension").to_string(),
        enabled: true,
        created_by: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let created_by = admin.0.id();
    let inserted = repo::insert(&state.pool, &ExtensionRow { created_by: Some(created_by), ..row }).await?;
    state.extensions.reload(&state.pool, &state.cfg).await;
    metrics::counter!("extensions_installed_total").increment(1);

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "extension": manifest_view(&inserted),
            "search_catalogs": searchable_catalogs(&inserted.manifest).len(),
            "has_streams": supports_resource(&inserted.manifest, "stream"),
        })),
    ))
}

#[utoipa::path(get, path = "/api/admin/extensions", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_extensions(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> ApiResult<Json<Value>> {
    require_loopback(peer)?;
    let rows = repo::list(&state.pool).await?;
    let live = state.extensions.list().await;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut v = manifest_view(r);
            v["live"] = json!(live.iter().any(|e| e.row.id == r.id));
            v
        })
        .collect();
    Ok(Json(json!({ "extensions": items })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PatchExtensionRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[utoipa::path(patch, path = "/api/admin/extensions/{id}", tag = "admin",
    security(("bearer" = [])),
    request_body = PatchExtensionRequest,
    responses((status = 200), (status = 404))
)]
pub async fn patch_extension(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<uuid::Uuid>,
    Json(req): Json<PatchExtensionRequest>,
) -> ApiResult<Json<Value>> {
    require_loopback(peer)?;
    let row = repo::set_enabled(&state.pool, id, req.enabled.unwrap_or(true)).await?;
    state.extensions.reload(&state.pool, &state.cfg).await;
    Ok(Json(json!({ "extension": manifest_view(&row) })))
}

#[utoipa::path(delete, path = "/api/admin/extensions/{id}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn delete_extension(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<uuid::Uuid>,
) -> ApiResult<StatusCode> {
    require_loopback(peer)?;
    repo::delete(&state.pool, id).await?;
    state.extensions.reload(&state.pool, &state.cfg).await;
    Ok(StatusCode::NO_CONTENT)
}

// -------------------------------------------------------------- discovery


/// Addons known to work, kept in the binary.
///
/// Stremio retired the public `api.stremio.com/addons/all` index, so live
/// discovery now always fails. Rather than leaving the admin with a dead
/// browser, this ships a small vetted set — metadata, subtitles and
/// legal-availability lookups — that is verified reachable. The live index is
/// still tried first, so if it ever returns the richer catalog wins.
const CURATED_ADDONS: &[(&str, &str, &str, &str)] = &[
    (
        "com.linvo.cinemeta",
        "Cinemeta",
        "Official Stremio metadata: posters, descriptions, cast and the Popular / New / Featured catalogs for films and series.",
        "https://v3-cinemeta.strem.io/manifest.json",
    ),
    (
        "org.stremio.opensubtitlesv3",
        "OpenSubtitles v3",
        "Subtitles in most languages, matched by IMDb id and file hash.",
        "https://opensubtitles-v3.strem.io/manifest.json",
    ),
    (
        "kitsu.addon.anime",
        "Anime Kitsu",
        "Anime catalog and metadata from Kitsu, with seasonal and trending lists.",
        "https://anime-kitsu.strem.fun/manifest.json",
    ),
    (
        "org.stremio.watchhub",
        "WatchHub",
        "Where a title can be watched legally — links out to the streaming and rental services carrying it.",
        "https://watchhub.strem.io/manifest.json",
    ),
];

/// The curated set as discovery entries, marking what is already installed.
async fn curated_entries(state: &AppState, needle: &str) -> Vec<Value> {
    let installed = repo::list(&state.pool).await.unwrap_or_default();

    CURATED_ADDONS
        .iter()
        .filter(|(id, name, desc, _)| {
            needle.is_empty()
                || name.to_lowercase().contains(needle)
                || id.to_lowercase().contains(needle)
                || desc.to_lowercase().contains(needle)
        })
        .map(|(id, name, desc, url)| {
            json!({
                "addon_id": id,
                "name": name,
                "description": desc,
                "install_url": url,
                "curated": true,
                "installed": installed.iter().any(|row| row.addon_id == *id),
            })
        })
        .collect()
}

/// Proxies the central Stremio addon index so users can browse/install
/// community addons without leaving SPACE.
#[utoipa::path(get, path = "/api/admin/extensions/discover", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 503))
)]
pub async fn discover_extensions(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxQuery(q): AxQuery<DiscoverQuery>,
) -> ApiResult<Json<Value>> {
    require_loopback(peer)?;
    let needle = q.q.to_lowercase();

    let body = match extensions::http_get_json_cfg(
        &state.cfg,
        "https://api.stremio.com/addons/all",
    )
    .await
    {
        Ok(body) => body,
        Err(e) => {
            // The index is retired; serving the vetted set beats a 503.
            tracing::debug!(error = %e, "stremio index unreachable; serving curated addons");
            let addons = curated_entries(&state, &needle).await;
            return Ok(Json(json!({
                "source": "curated",
                "count": addons.len(),
                "addons": addons,
            })));
        }
    };

    // index shape has drifted over time; accept array or {addons:[…]}
    let entries: Vec<&Value> = match &body {
        Value::Array(arr) => arr.iter().collect(),
        obj => obj
            .get("addons")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
    };

    let filtered: Vec<Value> = entries
        .iter()
        .filter_map(|entry| {
            let manifest = entry.get("manifest").unwrap_or(entry);
            let name = manifest.get("name").and_then(Value::as_str).unwrap_or("");
            let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
            let desc = manifest
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !needle.is_empty()
                && !name.to_lowercase().contains(&needle)
                && !id.to_lowercase().contains(&needle)
                && !desc.to_lowercase().contains(&needle)
            {
                return None;
            }
            let transport = entry
                .get("transportUrl")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(json!({
                "addon_id": id,
                "name": name,
                "description": desc,
                "types": manifest.get("types").cloned().unwrap_or(json!([])),
                "install_url": transport.unwrap_or_else(|| {
                    format!("{}/manifest.json", id.trim_end_matches('/'))
                }),
            }))
        })
        .take(200)
        .collect();

    Ok(Json(json!({
        "source": "index",
        "count": filtered.len(),
        "addons": filtered,
    })))
}

#[derive(Debug, Deserialize)]
pub struct DiscoverQuery {
    #[serde(default)]
    pub q: String,
}

// ------------------------------------------------------ resource surface

async fn find_entry(state: &AppState, ext_id: &str) -> ApiResult<Arc<extensions::ExtEntry>> {
    state
        .extensions
        .find(ext_id)
        .await
        .ok_or_else(|| ApiError::not_found("extension"))
}

#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub skip: Option<String>,
    #[serde(default)]
    pub extra: Vec<String>,
}

/// 1:1 catalog call: GET /api/extensions/{ext}/catalog/{type}/{cid}
#[utoipa::path(get, path = "/api/extensions/{ext_id}/catalog/{stype}/{catalog_id}",
    tag = "extensions", security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn catalog_resource(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<(String, String, String)>,
    AxQuery(q): AxQuery<CatalogQuery>,
) -> ApiResult<Json<Value>> {
    let _ = auth;
    let entry = find_entry(&state, &path.0).await?;
    let mut extra: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = q.search.as_deref() {
        extra.push(("search", s));
    }
    if let Some(g) = q.genre.as_deref() {
        extra.push(("genre", g));
    }
    if let Some(s) = q.skip.as_deref() {
        extra.push(("skip", s));
    }
    for kv in &q.extra {
        if let Some((k, v)) = kv.split_once('=') {
            extra.push((k, v));
        }
    }
    let body = extensions::http_get_json_cfg(
        &state.cfg,
        &entry.target.catalog_url(&path.1, &path.2, &extra),
    )
    .await?;
    Ok(Json(body))
}

/// 1:1 stream call across one extension: GET /api/extensions/{ext}/stream/…
#[utoipa::path(get, path = "/api/extensions/{ext_id}/stream/{stype}/{sid}",
    tag = "extensions", security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn stream_resource(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let _ = auth;
    let entry = find_entry(&state, &path.0).await?;
    let body = extensions::http_get_json_cfg(
        &state.cfg,
        &entry.target.stream_url(&path.1, &path.2),
    )
    .await?;
    Ok(Json(body))
}

/// 1:1 meta call.
#[utoipa::path(get, path = "/api/extensions/{ext_id}/meta/{stype}/{mid}",
    tag = "extensions", security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn meta_resource(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let _ = auth;
    let entry = find_entry(&state, &path.0).await?;
    let body =
        extensions::http_get_json_cfg(&state.cfg, &entry.target.meta_url(&path.1, &path.2))
            .await?;
    Ok(Json(body))
}

/// Subtitles across every extension that supports them.
#[utoipa::path(get, path = "/api/extensions/subtitles/{stype}/{sid}",
    tag = "extensions", security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn subtitles_all(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<(String, String)>,
) -> ApiResult<Json<Value>> {
    let _ = auth;
    let mut subtitles: Vec<Value> = Vec::new();
    for entry in state.extensions.list().await {
        if !supports_resource(&entry.row.manifest, "subtitles") {
            continue;
        }
        let url = entry.target.subtitles_url(&path.0, &path.1, &[]);
        if let Ok(body) = extensions::http_get_json_cfg(&state.cfg, &url).await
            && let Some(list) = body.get("subtitles").and_then(Value::as_array)
        {
            for sub in list {
                let mut s = sub.clone();
                s["from"] = json!(entry.addon_id());
                subtitles.push(s);
                if subtitles.len() >= 100 {
                    break;
                }
            }
        }
    }
    Ok(Json(json!({ "subtitles": subtitles })))
}
