//! Admin management for remote source hubs (strictly localhost-managed,
//! mirroring web sources).

use axum::extract::{ConnectInfo, Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::AdminUser;
use crate::error::{ApiError, ApiResult};
use crate::sources::hub::{self, repo};
use crate::state::AppState;

fn require_loopback(peer: SocketAddr) -> ApiResult<()> {
    if crate::admin_gate::is_loopback(peer.ip()) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "hub management is strictly localhost-only (home servers)",
        ))
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateHubRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// e.g. http://192.168.1.20:8080 — another SPACE acting as a hub
    pub url: String,
    /// shared secret expected by the remote `/api/hub/*`
    #[serde(default)]
    pub token: Option<String>,
}

fn slug_from_url(url: &str) -> String {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "hub".into());
    crate::utils::slugify(&host.trim_start_matches("www.").replace('.', "-"))
}

trait Pipe: Sized {
    fn pipe<F: FnOnce(Self) -> Self>(self, f: F) -> Self {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Registers a remote hub after a live reachability probe.
#[utoipa::path(post, path = "/api/admin/hubs", tag = "admin",
    security(("bearer" = [])),
    request_body = CreateHubRequest,
    responses((status = 201), (status = 400), (status = 403), (status = 503))
)]
pub async fn create_hub(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<CreateHubRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    require_loopback(peer)?;
    let url = req.url.trim().trim_end_matches('/').to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(ApiError::bad_request("hub url must be absolute http(s)"));
    }

    // probe before saving
    let probe_hub = hub::HubRow {
        id: Uuid::nil(),
        name: "probe".into(),
        base_url: url.clone(),
        token: req.token.clone(),
        enabled: true,
        last_sync_at: None,
        created_by: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let recipes = hub::sources(&state.cfg, probe_hub.r#ref()).await?;
    if recipes.is_empty() {
        return Err(ApiError::Unavailable(
            "hub answered but exposes no learned sources".into(),
        ));
    }

    let base = crate::utils::slugify(req.name.as_deref().unwrap_or("").trim())
        .pipe(|s| if s.is_empty() { slug_from_url(&url) } else { s });
    let mut name = base.clone();
    let mut suffix = 2;
    while sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM source_hubs WHERE LOWER(name) = LOWER($1))",
    )
    .bind(&name)
    .fetch_one(&state.pool)
    .await?
    {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }

    let row = hub::HubRow {
        id: crate::utils::new_id(),
        name: name.clone(),
        base_url: url,
        token: req.token.filter(|t| !t.trim().is_empty()),
        enabled: true,
        last_sync_at: Some(chrono::Utc::now()),
        created_by: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let created_by = admin.0.id();
    let inserted = repo::insert(&state.pool, &row, Some(created_by)).await?;
    metrics::counter!("hubs_registered_total").increment(1);

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "hub": inserted,
            "remote_sources": recipes.len(),
            "hint": "POST /api/admin/hubs/{id}/pull to import recipes; hubs also serve as automatic search/resolve fallbacks",
        })),
    ))
}

#[utoipa::path(get, path = "/api/admin/hubs", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_hubs(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> ApiResult<Json<serde_json::Value>> {
    require_loopback(peer)?;
    let rows = repo::list(&state.pool).await?;
    let hubs: Vec<serde_json::Value> = rows
        .iter()
        .map(|h| {
            json!({
                "id": h.id,
                "name": h.name,
                "base_url": h.base_url,
                "has_token": h.token.is_some(),
                "enabled": h.enabled,
                "last_sync_at": h.last_sync_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "hubs": hubs,
        "server_side": {
            "enabled": state.cfg.source_hub_token.is_some() || true,
            "token_protected": state.cfg.source_hub_token.is_some(),
            "note": if state.cfg.source_hub_token.is_some() {
                "/api/hub/* requires X-Hub-Token"
            } else {
                "/api/hub/* answers loopback peers only (set SOURCE_HUB_TOKEN for remote peers)"
            },
        },
    })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PatchHubRequest {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[utoipa::path(patch, path = "/api/admin/hubs/{id}", tag = "admin",
    security(("bearer" = [])),
    request_body = PatchHubRequest,
    responses((status = 200), (status = 404))
)]
pub async fn patch_hub(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<Uuid>,
    Json(req): Json<PatchHubRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_loopback(peer)?;
    let row = repo::patch(
        &state.pool,
        id,
        req.url.as_deref(),
        req.token.as_deref().map(Some),
        req.enabled,
    )
    .await?;
    Ok(Json(json!({ "hub": { "id": row.id, "name": row.name } })))
}

#[utoipa::path(delete, path = "/api/admin/hubs/{id}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn delete_hub(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    require_loopback(peer)?;
    repo::delete(&state.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Imports the hub's learned recipes into local web_sources.
#[utoipa::path(post, path = "/api/admin/hubs/{id}/pull", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 503))
)]
pub async fn pull_hub(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_loopback(peer)?;
    let row = repo::get(&state.pool, id).await?;
    let recipes = hub::sources(&state.cfg, row.r#ref()).await?;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    for recipe in recipes {
        let name = recipe
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM web_sources WHERE LOWER(name) = LOWER($1))",
        )
        .bind(&name)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(true);
        if exists || name.is_empty() {
            skipped += 1;
            continue;
        }
        let payload = json!({
            "name": name,
            "label": recipe.get("label").cloned().unwrap_or(json!(name.clone())),
            "site": recipe.get("site").cloned().unwrap_or_else(|| json!("https://unknown.example")),
            "category": recipe.get("category").cloned().unwrap_or_else(|| json!("movie")),
            "search_url": recipe.get("search_url").cloned().unwrap_or_else(|| json!("")),
            "sample_item": recipe.get("sample_item").cloned().unwrap_or(json!(null)),
            "recipe": recipe.get("recipe").cloned().unwrap_or(json!({})),
        });
        match super::web_handlers::import_recipe(&state, payload).await {
            Ok(_) => imported += 1,
            Err(_) => skipped += 1,
        }
    }
    repo::touch_sync(&state.pool, id).await?;
    metrics::counter!("hub_pull_imported_total").increment(imported as u64);
    Ok(Json(json!({ "imported": imported, "skipped": skipped })))
}

/// Uploads every local learned recipe to the hub.
#[utoipa::path(post, path = "/api/admin/hubs/{id}/push", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 503))
)]
pub async fn push_hub(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_loopback(peer)?;
    let row = repo::get(&state.pool, id).await?;
    let mine = super::web::repo::list(&state.pool).await?;

    let mut pushed = 0usize;
    let mut failed = 0usize;
    for source in mine.iter().filter(|s| s.enabled) {
        let payload = json!({
            "name": source.name,
            "label": source.label,
            "site": source.site,
            "category": source.category,
            "search_url": source.search_url,
            "sample_item": source.sample_item,
            "recipe": source.recipe,
        });
        match hub::upload_recipe(&state.cfg, row.r#ref(), payload).await {
            Ok(()) => pushed += 1,
            Err(err) => {
                tracing::warn!(source = %source.name, error = %err, "push failed");
                failed += 1;
            }
        }
    }
    repo::touch_sync(&state.pool, id).await?;
    Ok(Json(json!({ "pushed": pushed, "failed": failed })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TestHubRequest {
    #[serde(default = "default_query")]
    pub q: String,
}

fn default_query() -> String {
    String::new()
}

/// Live end-to-end query against the hub ("does it find things?").
#[utoipa::path(post, path = "/api/admin/hubs/{id}/test", tag = "admin",
    security(("bearer" = [])),
    request_body = TestHubRequest,
    responses((status = 200), (status = 503))
)]
pub async fn test_hub(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<TestHubRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    require_loopback(peer)?;
    let row = repo::get(&state.pool, id).await?;
    let q = body.map(|Json(b)| b.q).unwrap_or_default();
    let started = std::time::Instant::now();
    let hits = hub::find(&state.cfg, row.r#ref(), &q, None, 8).await?;
    Ok(Json(json!({
        "ok": !hits.is_empty(),
        "elapsed_ms": started.elapsed().as_millis(),
        "count": hits.len(),
        "results": hits,
    })))
}
