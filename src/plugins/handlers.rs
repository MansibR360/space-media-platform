use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::AdminUser;
use crate::catalog::metadata::MetadataProvider;
use crate::error::{ApiError, ApiResult};
use crate::plugins::http_provider::HttpPluginProvider;
use crate::plugins::models::{CreatePluginRequest, PluginView, UpdatePluginRequest};
use crate::state::AppState;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PluginsResponse {
    pub builtin: Vec<String>,
    pub plugins: Vec<PluginView>,
    pub active_sources: Vec<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PluginTestResult {
    pub manifest_ok: bool,
    pub search_ok: bool,
    pub sample_results: usize,
}

pub(crate) async fn reload_metadata(state: &AppState) -> ApiResult<()> {
    let mut registry = crate::catalog::metadata::MetadataRegistry::from_config(&state.cfg);

    for plugin in crate::plugins::repo::list(&state.pool).await? {
        if !plugin.enabled || plugin.kind != "metadata" {
            continue;
        }
        if let Some(base_url) = plugin.base_url.clone() {
            match HttpPluginProvider::checked(plugin.name.clone(), base_url) {
                Ok(provider) => registry.insert(Arc::new(provider)),
                Err(err) => {
                    tracing::warn!(plugin = %plugin.name, error = %err, "skipping invalid plugin")
                }
            }
        }
    }

    *state.metadata.write().await = registry;
    Ok(())
}

#[utoipa::path(get, path = "/api/admin/plugins", tag = "plugins",
    security(("bearer" = [])),
    responses((status = 200, body = PluginsResponse))
)]
pub async fn list_plugins(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<PluginsResponse>> {
    let plugins: Vec<PluginView> = crate::plugins::repo::list(&state.pool)
        .await?
        .iter()
        .map(|p| p.view())
        .collect();
    let active = state.metadata.read().await.sources();

    Ok(Json(PluginsResponse {
        builtin: vec!["tmdb".into(), "imdb".into(), "anilist".into()],
        plugins,
        active_sources: active,
    }))
}

/// Public listing of enabled metadata sources (safe for regular users).
#[utoipa::path(get, path = "/api/metadata/sources", tag = "metadata",
    responses((status = 200))
)]
pub async fn list_enabled_sources(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.metadata.read().await.sources())
}

#[utoipa::path(post, path = "/api/admin/plugins", tag = "plugins",
    security(("bearer" = [])),
    request_body = CreatePluginRequest,
    responses((status = 201, body = PluginView), (status = 400), (status = 409))
)]
pub async fn create_plugin(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    Json(req): Json<CreatePluginRequest>,
) -> ApiResult<(StatusCode, Json<PluginView>)> {
    if !crate::plugins::models::valid_plugin_name(&req.name) {
        return Err(ApiError::bad_request(
            "name must be 1-48 chars of letters, digits, dash or underscore",
        ));
    }
    if req.kind != "metadata" && req.kind != "streaming" {
        return Err(ApiError::bad_request(
            "kind must be 'metadata' or 'streaming'",
        ));
    }
    if crate::plugins::repo::find_by_name(&state.pool, &req.name)
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict("plugin name already registered".into()));
    }

    HttpPluginProvider::checked(req.name.clone(), req.base_url.clone())?;

    let plugin = crate::plugins::repo::insert(
        &state.pool,
        crate::utils::new_id(),
        req.name.trim(),
        &req.kind,
        req.base_url.trim_end_matches('/'),
        req.config,
        req.enabled,
        Some(admin.0.id()),
    )
    .await?;

    reload_metadata(&state).await?;
    tracing::info!(plugin = %plugin.name, "metadata plugin registered");
    Ok((StatusCode::CREATED, Json(plugin.view())))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PluginIdPath {
    pub plugin_id: Uuid,
}

#[utoipa::path(patch, path = "/api/admin/plugins/{plugin_id}", tag = "plugins",
    params(PluginIdPath),
    security(("bearer" = [])),
    request_body = UpdatePluginRequest,
    responses((status = 200, body = PluginView))
)]
pub async fn update_plugin(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<PluginIdPath>,
    Json(req): Json<UpdatePluginRequest>,
) -> ApiResult<Json<PluginView>> {
    if let Some(base_url) = &req.base_url {
        HttpPluginProvider::checked("probe".into(), base_url.clone())?;
    }
    let plugin = crate::plugins::repo::update(
        &state.pool,
        path.plugin_id,
        req.enabled,
        req.base_url.as_deref().map(|u| u.trim_end_matches('/')),
        req.config,
    )
    .await?;
    reload_metadata(&state).await?;
    Ok(Json(plugin.view()))
}

#[utoipa::path(delete, path = "/api/admin/plugins/{plugin_id}", tag = "plugins",
    params(PluginIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn delete_plugin(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<PluginIdPath>,
) -> ApiResult<StatusCode> {
    crate::plugins::repo::delete(&state.pool, path.plugin_id).await?;
    reload_metadata(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/admin/plugins/{plugin_id}/test", tag = "plugins",
    params(PluginIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = PluginTestResult), (status = 404))
)]
pub async fn test_plugin(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Path(path): Path<PluginIdPath>,
) -> ApiResult<Json<PluginTestResult>> {
    let plugin = crate::plugins::repo::get(&state.pool, path.plugin_id).await?;
    let base_url = plugin
        .base_url
        .clone()
        .ok_or_else(|| ApiError::bad_request("builtin plugins cannot be tested"))?;

    let provider = HttpPluginProvider::checked(plugin.name.clone(), base_url)?;
    let search = provider.search("test").await;
    let sample_results = search.as_ref().map(|r| r.len()).unwrap_or(0);
    let search_ok = search.is_ok();

    Ok(Json(PluginTestResult {
        manifest_ok: true,
        search_ok,
        sample_results,
    }))
}
