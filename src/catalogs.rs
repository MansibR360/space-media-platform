//! Browsable catalogs from installed sources.
//!
//! The local `media` table only holds what has been imported. Installed
//! Stremio extensions publish their own browsable catalogs — "Popular",
//! "New", "Featured" per media type — and this module exposes them as rails
//! the client can render directly, so the app has something to show on a
//! catalog with nothing in it.
//!
//! Two endpoints, deliberately split:
//!
//! * [`list_rails`] is cheap. It reads the stored manifests and returns the
//!   rail definitions with no upstream calls, so the client can lay out the
//!   page immediately.
//! * [`rail_items`] fetches one rail. The client asks for them individually,
//!   which keeps a slow or dead addon from holding up every other rail.
//!
//! Every item carries a play `token` in the same format the resolver accepts,
//! so a card from here is playable without a catalog row behind it.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/catalogs", get(list_rails))
        .route(
            "/api/catalogs/{ext_id}/{stype}/{catalog_id}",
            get(rail_items),
        )
        .route("/api/catalogs/source/{name}", get(source_rail_items))
}

/// One browsable rail offered by an installed extension.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Rail {
    pub ext_id: String,
    pub ext_name: String,
    /// Stremio media type: movie | series | channel | tv.
    pub stype: String,
    pub catalog_id: String,
    /// Display name, e.g. "Popular".
    pub name: String,
    /// Where to fetch this rail's items.
    pub href: String,
    /// Can this rail's items actually be played?
    ///
    /// A metadata addon (Cinemeta, Kitsu) publishes rich catalogs but serves no
    /// streams, so its rows only play if some *other* installed source happens
    /// to carry the same title. Saying so lets the client lead with what works
    /// instead of filling the page with titles that dead-end.
    pub playable: bool,
}

/// A card from a source catalog.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CatalogItem {
    /// Play token — feed straight to `POST /api/resolve`.
    pub token: String,
    /// Stremio id (`tt0111161`), kept for metadata lookups.
    pub external_id: String,
    pub title: String,
    pub poster: Option<String>,
    /// Wide artwork. Without this a hero has only a portrait poster to work
    /// with and falls back to a flat gradient.
    pub background: Option<String>,
    /// Transparent title treatment, when the source has one.
    pub logo: Option<String>,
    pub year: Option<i32>,
    pub description: Option<String>,
    /// IMDb rating when the addon supplies one.
    pub rating: Option<f64>,
    pub stype: String,
}

/// Every rail across every enabled extension.
#[utoipa::path(get, path = "/api/catalogs", tag = "discovery",
    responses((status = 200, body = [Rail])))]
pub async fn list_rails(State(state): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let rows = crate::extensions::repo::list(&state.pool).await?;

    let mut rails = Vec::new();
    for row in rows.iter().filter(|r| r.enabled) {
        for cat in crate::extensions::catalogs(&row.manifest) {
            rails.push(Rail {
                href: format!(
                    "/api/catalogs/{}/{}/{}",
                    row.id, cat.stype, cat.catalog_id
                ),
                ext_id: row.id.to_string(),
                ext_name: row.name.clone(),
                stype: cat.stype,
                catalog_id: cat.catalog_id,
                name: cat.name,
                playable: crate::extensions::supports_resource(&row.manifest, "stream"),
            });
        }
    }

    // Built-in sources come first: unlike a metadata addon, anything they list
    // is something they can also play.
    let mut all = builtin_rails(&state);
    all.extend(rails);

    Ok(Json(serde_json::json!({ "rails": all })))
}

/// Rails for the built-in sources.
///
/// These are the ones that actually hold media — Internet Archive films,
/// LibriVox, MangaDex and friends — so everything they list can be played.
fn builtin_rails(state: &AppState) -> Vec<Rail> {
    state
        .sources
        .describe()
        .into_iter()
        .filter_map(|manifest| {
            let name = manifest.get("name")?.as_str()?.to_string();
            let label = manifest
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&name)
                .to_string();
            let category = manifest
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            Some(Rail {
                href: format!("/api/catalogs/source/{name}"),
                ext_id: name,
                ext_name: label,
                stype: category,
                catalog_id: "browse".to_string(),
                name: "Latest".to_string(),
                playable: true,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct RailQuery {
    #[serde(default)]
    pub skip: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
    /// Optional genre filter, when the catalog advertises `genre` support.
    #[serde(default)]
    pub genre: Option<String>,
}

/// Items for one rail, normalized into cards with play tokens.
#[utoipa::path(get, path = "/api/catalogs/{ext_id}/{stype}/{catalog_id}", tag = "discovery",
    params(RailQuery), responses((status = 200, body = [CatalogItem])))]
pub async fn rail_items(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path((ext_id, stype, catalog_id)): Path<(String, String, String)>,
    Query(q): Query<RailQuery>,
) -> ApiResult<Json<Value>> {
    let entry = state
        .extensions
        .find(&ext_id)
        .await
        .ok_or_else(|| ApiError::not_found("extension"))?;

    // Stremio paginates by `skip`, not by page size — the addon decides how
    // many it returns, so `limit` only trims what came back.
    let skip = q.skip.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(30).clamp(1, 100) as usize;

    let skip_str = skip.to_string();
    let mut extra: Vec<(&str, &str)> = Vec::new();
    if skip > 0 {
        extra.push(("skip", &skip_str));
    }
    if let Some(g) = q.genre.as_deref().filter(|g| !g.is_empty()) {
        extra.push(("genre", g));
    }

    let body = crate::extensions::http_get_json_cfg(
        &state.cfg,
        &entry.target.catalog_url(&stype, &catalog_id, &extra),
    )
    .await?;

    let source = format!("ext:{}#{}", entry.addon_id(), catalog_id);
    let metas = body
        .get("metas")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::Unavailable("catalog returned no items".into()))?;

    let items: Vec<CatalogItem> = metas
        .iter()
        .take(limit)
        .filter_map(|m| to_item(m, &stype, &source))
        .collect();

    Ok(Json(serde_json::json!({
        "ext_id": ext_id,
        "stype": stype,
        "catalog_id": catalog_id,
        "skip": skip,
        "count": items.len(),
        "items": items,
    })))
}

/// Items from a built-in source's browse feed, in the same card shape.
#[utoipa::path(get, path = "/api/catalogs/source/{name}", tag = "discovery",
    params(RailQuery), responses((status = 200, body = [CatalogItem])))]
pub async fn source_rail_items(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path(name): Path<String>,
    Query(q): Query<RailQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100) as i32;

    // One unreachable source must not fail the request. The client renders a
    // rail per source, and a 503 here would turn a single flaky backend into a
    // page full of error states.
    let items = match crate::sources::handlers::browse_items(&state, &name, limit).await {
        Ok(items) => items,
        Err(e) => {
            tracing::info!(source = %name, error = %e, "source browse unavailable");
            Vec::new()
        }
    };

    let cards: Vec<CatalogItem> = items
        .into_iter()
        .map(|item| CatalogItem {
            token: crate::sources::aggregator::make_token(&name, &item.external_id),
            external_id: item.external_id,
            title: item.title,
            poster: item.poster.clone(),
            // Archive sources ship one image; reuse it rather than leaving the
            // hero with nothing behind it.
            background: item.poster,
            logo: None,
            year: item.year,
            description: Some(item.overview).filter(|o| !o.is_empty()),
            rating: None,
            stype: item.category.to_string(),
        })
        .collect();

    Ok(Json(serde_json::json!({
        "source": name,
        "count": cards.len(),
        "items": cards,
    })))
}

fn to_item(meta: &Value, stype: &str, source: &str) -> Option<CatalogItem> {
    let external_id = meta.get("id")?.as_str()?.to_string();
    let title = meta
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Untitled")
        .to_string();

    // The resolver keys an extension item by `<type>|<id>`.
    let token = crate::sources::aggregator::make_token(source, &format!("{stype}|{external_id}"));

    Some(CatalogItem {
        token,
        external_id,
        title,
        poster: meta
            .get("poster")
            .and_then(Value::as_str)
            .map(str::to_string),
        background: meta
            .get("background")
            .and_then(Value::as_str)
            .map(str::to_string),
        logo: meta.get("logo").and_then(Value::as_str).map(str::to_string),
        // `releaseInfo` is "2026" for a film and "2019-2024" for a run.
        year: meta
            .get("releaseInfo")
            .and_then(Value::as_str)
            .and_then(|r| r.get(..4))
            .and_then(|y| y.parse().ok())
            .or_else(|| meta.get("year").and_then(Value::as_i64).map(|y| y as i32)),
        description: meta
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        rating: meta
            .get("imdbRating")
            .and_then(|r| {
                r.as_f64()
                    .or_else(|| r.as_str().and_then(|s| s.parse().ok()))
            }),
        stype: stype.to_string(),
    })
}
