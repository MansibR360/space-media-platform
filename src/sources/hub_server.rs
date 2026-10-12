//! Hub server surface: `/api/hub/*`.
//!
//! Lets other SPACE instances use this server as their fallback source
//! provider and recipe store. Auth model (defense in depth):
//!
//! * `SOURCE_HUB_TOKEN` set → every request must carry a matching
//!   `X-Hub-Token` header, loopback or not.
//! * token unset → only loopback peers are served at all (homelab safe).

use axum::extract::{ConnectInfo, Query as AxQuery, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

fn authorize(
    state: &AppState,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> ApiResult<()> {
    let provided = headers
        .get("x-hub-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match &state.cfg.source_hub_token {
        Some(expected) => {
            if provided.as_deref() == Some(expected.as_str()) {
                Ok(())
            } else {
                metrics::counter!("hub_rejected_total", "reason" => "token").increment(1);
                Err(ApiError::forbidden("invalid hub token"))
            }
        }
        None => {
            if crate::admin_gate::is_loopback(peer.ip()) {
                Ok(())
            } else {
                metrics::counter!("hub_rejected_total", "reason" => "loopback").increment(1);
                Err(ApiError::forbidden(
                    "hub access requires SOURCE_HUB_TOKEN for remote peers",
                ))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct HubFindQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i32,
}

fn default_limit() -> i32 {
    20
}

/// Cross-source search over this server's learned web sources.
#[utoipa::path(get, path = "/api/hub/find", tag = "hub",
    responses((status = 200), (status = 403))
)]
pub async fn find(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxQuery(q): AxQuery<HubFindQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&state, peer, &headers)?;
    let started = std::time::Instant::now();
    let per_source = q.limit.clamp(1, 60);
    let mut results: Vec<serde_json::Value> = Vec::new();

    let entries = state.web_sources.list().await;
    let jobs = entries.iter().map(|entry| {
        let state = state.clone();
        let name = entry.source.name().to_string();
        let category = entry.source.category().to_string();
        let query = q.q.clone();
        let wanted_category = q.category.clone();
        async move {
            let items = state.web_sources.search(&state, &name, &query, per_source).await;
            (name, category, wanted_category, items)
        }
    });
    let settled = futures_util::future::join_all(jobs.collect::<Vec<_>>()).await;
    for (name, category, wanted, items) in settled {
        if let Ok(items) = items {
            for item in items {
                if let Some(want) = &wanted
                    && !want.is_empty()
                    && item.category != want.as_str()
                {
                    continue;
                }
                results.push(json!({
                    "source": name,
                    "id": item.external_id,
                    "title": item.title,
                    "overview": item.overview,
                    "year": item.year,
                    "poster": item.poster,
                    "category": category,
                }));
                if results.len() >= (per_source * 4) as usize {
                    break;
                }
            }
        }
    }

    metrics::histogram!("hub_find_duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(Json(json!({ "results": results })))
}

#[derive(Debug, Deserialize)]
pub struct HubItemQuery {
    pub source: String,
    pub id: String,
}

/// Detail + streams for one remote item (the "give it to me to watch" hop).
#[utoipa::path(get, path = "/api/hub/item", tag = "hub",
    responses((status = 200), (status = 403), (status = 404))
)]
pub async fn item(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AxQuery(q): AxQuery<HubItemQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&state, peer, &headers)?;
    let detail = state.web_sources.item(&state, &q.source, &q.id).await?;
    Ok(Json(serde_json::to_value(detail)?))
}

/// Recipe catalog exposed to pulling peers.
#[utoipa::path(get, path = "/api/hub/sources", tag = "hub",
    responses((status = 200), (status = 403))
)]
pub async fn list_sources(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    authorize(&state, peer, &headers)?;
    let rows = super::web::repo::list(&state.pool).await?;
    let sources: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| r.enabled)
        .map(|r| {
            json!({
                "name": r.name,
                "label": r.label,
                "site": r.site,
                "category": r.category,
                "search_url": r.search_url,
                "sample_item": r.sample_item,
                "recipe": r.recipe,
                "items_sampled": r.items_sampled,
            })
        })
        .collect();
    Ok(Json(json!({ "sources": sources })))
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct HubUploadRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    pub site: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub search_url: Option<String>,
    #[serde(default)]
    pub sample_item: Option<String>,
    pub recipe: serde_json::Value,
}

fn default_category() -> String {
    "movie".into()
}

/// Push target: imports a learned recipe into this server's web_sources.
#[utoipa::path(post, path = "/api/hub/sources", tag = "hub",
    request_body = HubUploadRequest,
    responses((status = 201), (status = 400), (status = 403))
)]
pub async fn upload_source(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<HubUploadRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    authorize(&state, peer, &headers)?;

    let payload = serde_json::to_value(&req)?;
    let inserted = super::web_handlers::import_recipe(&state, payload).await?;
    metrics::counter!("hub_recipes_imported_total").increment(1);

    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "source": { "id": inserted.id, "name": inserted.name } })),
    ))
}
