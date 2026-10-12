//! Serving script plugins as ordinary sources.
//!
//! Each plugin is exposed on a loopback route and registered in the `plugins`
//! table like any other source, so the aggregator, the health prober and the
//! resolver reach it through the paths they already use — nothing else in the
//! server needs to know a plugin is JavaScript.
//!
//! They are registered `resolve_only`: a provider answers "what can play this
//! TMDB id", not "what titles exist", so it supplies streams without adding
//! noise to search.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const ROUTE_PREFIX: &str = "/api/script-plugins";
const MANAGED_BY: &str = "script-plugin";

#[derive(Debug, Deserialize)]
pub struct DetailsQuery {
    #[serde(default)]
    pub id: String,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(ROUTE_PREFIX, get(index))
        .route(
            &format!("{ROUTE_PREFIX}/{{id}}/manifest.json"),
            get(manifest),
        )
        .route(&format!("{ROUTE_PREFIX}/{{id}}/items"), get(items))
        .route(&format!("{ROUTE_PREFIX}/{{id}}/search"), get(search))
        .route(&format!("{ROUTE_PREFIX}/{{id}}/details"), get(details))
}

async fn index(State(state): State<Arc<AppState>>) -> axum::Json<Value> {
    axum::Json(state.script_plugins.describe())
}

async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::Json<Value>> {
    let plugin = state
        .script_plugins
        .get(&id)
        .ok_or_else(|| ApiError::not_found("plugin"))?;
    Ok(axum::Json(json!({
        "name": plugin.manifest.id,
        "label": plugin.name(),
        "category": "movie",
        "kind": "source",
        "transport": "http",
    })))
}

/// Health-probe target. A provider has no library to list.
async fn items(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::Json<Value>> {
    state
        .script_plugins
        .get(&id)
        .ok_or_else(|| ApiError::not_found("plugin"))?;
    Ok(axum::Json(
        json!({ "items": [], "note": "provider resolves streams only" }),
    ))
}

/// Providers resolve; they do not search. Answering empty keeps them out of
/// search results while still letting the prober treat them as healthy.
async fn search(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<axum::Json<Value>> {
    state
        .script_plugins
        .get(&id)
        .ok_or_else(|| ApiError::not_found("plugin"))?;
    Ok(axum::Json(json!({ "results": [] })))
}

/// Run the provider for a TMDB-keyed id and hand back what it found.
async fn details(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DetailsQuery>,
) -> ApiResult<axum::Json<Value>> {
    let plugin = state
        .script_plugins
        .get(&id)
        .ok_or_else(|| ApiError::not_found("plugin"))?;

    let media = crate::plugins::native::engines::parse_media_id_msg(
        &query.id,
        "id must be movie:{tmdb_id} or tv:{tmdb_id}[:season:episode]",
    )?;
    let media_type = if media.kind == "movie" { "movie" } else { "tv" };
    if !plugin.handles(media_type) {
        return Err(ApiError::bad_request(format!(
            "{} does not handle {media_type}",
            plugin.manifest.id
        )));
    }

    let (season, episode) = if media_type == "tv" {
        (Some(media.season), Some(media.episode))
    } else {
        (None, None)
    };

    let found = plugin
        .streams(
            state.script_client.clone(),
            &media.tmdb_id,
            media_type,
            season,
            episode,
        )
        .await?;

    // A provider returns direct urls, which is the whole point: these play in
    // our own player instead of sending the viewer to somebody's website.
    let streams: Vec<Value> = found
        .iter()
        .map(|s| {
            let kind = if s.url.contains(".m3u8") {
                "hls"
            } else if s.url.contains(".mp4") {
                "mp4"
            } else {
                "page"
            };
            let label = [s.name.as_str(), s.title.as_str(), s.quality.as_str()]
                .iter()
                .filter(|p| !p.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" \u{b7} ");
            json!({
                "label": if label.is_empty() { plugin.name().to_string() } else { label },
                "url": s.url,
                "kind": kind,
                "headers": s.headers,
            })
        })
        .collect();

    Ok(axum::Json(json!({
        "title": plugin.name(),
        "overview": "",
        "poster": "",
        "year": Value::Null,
        "media_type": if media_type == "movie" { "movie" } else { "series" },
        "streams": streams,
    })))
}

/// Register every loaded plugin as a source row.
pub async fn sync(state: &AppState) -> ApiResult<usize> {
    let base = state
        .cfg
        .native_plugins_base
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", state.cfg.port));
    let base = base.trim_end_matches('/');

    let mut names = Vec::new();
    for plugin in state.script_plugins.all() {
        let id = &plugin.manifest.id;
        let config = json!({
            "managed_by": MANAGED_BY,
            "label": plugin.name(),
            // providers answer for a known id; they never contribute to search
            "resolve_only": true,
            "version": plugin.manifest.version,
        });
        match crate::plugins::repo::upsert_managed(
            &state.pool,
            crate::utils::new_id(),
            id,
            "source",
            &format!("{base}{ROUTE_PREFIX}/{id}"),
            config,
        )
        .await
        {
            Ok(_) => names.push(id.clone()),
            Err(err) => tracing::warn!(plugin = %id, error = %err, "script plugin upsert failed"),
        }
    }

    let retired =
        crate::plugins::repo::disable_missing_managed(&state.pool, MANAGED_BY, &names).await?;
    if retired > 0 {
        tracing::info!(retired, "script plugins retired");
    }
    if !names.is_empty() {
        tracing::info!(registered = names.len(), "script plugins registered");
    }
    Ok(names.len())
}
