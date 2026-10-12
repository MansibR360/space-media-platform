//! Admin surface for learned web sources.

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::AdminUser;
use crate::error::{ApiError, ApiResult};
use crate::sources::web::{self, repo, WebSourceRow};
use crate::state::AppState;

// ------------------------------------------------------------- create/learn

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateWebSourceRequest {
    /// Optional pretty name; derived from the host when omitted.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The site root, e.g. `https://movies.example` — this is all you need.
    pub site: String,
    /// Optional explicit search template containing `{query}`.
    #[serde(default)]
    pub search_url: Option<String>,
    /// Optional example item page; defaults to the first search hit.
    #[serde(default)]
    pub sample_item: Option<String>,
    /// movie | series | book | music | manga
    #[serde(default = "default_category")]
    pub category: String,
}

fn default_category() -> String {
    "movie".into()
}

struct LearnedRecipe {
    recipe: serde_json::Value,
    items_found: usize,
    streams_found: usize,
    notes: Vec<String>,
}

async fn run_learn(state: &AppState, req: &CreateWebSourceRequest) -> ApiResult<LearnedRecipe> {
    let mut args: Vec<String> = vec![
        "source".into(),
        "learn".into(),
        "--site".into(),
        req.site.trim().trim_end_matches('/').to_string(),
        "--category".into(),
        req.category.to_lowercase(),
    ];
    if let Some(tpl) = req.search_url.as_deref() {
        let tpl = tpl.trim();
        args.push("--search-url".into());
        args.push(tpl.to_string());
    }
    if let Some(item) = req.sample_item.as_deref() {
        args.push("--sample-item".into());
        args.push(item.trim().to_string());
    }
    args.push("--timeout-secs".into());
    args.push("20".into());
    args.push("--retries".into());
    args.push("1".into());

    let out = web::run_galaxy(&state.cfg, &args, None, 120).await?;
    let parsed: serde_json::Value = serde_json::from_str(out.stdout.trim())
        .map_err(|e| ApiError::Unavailable(format!("unparseable galaxy learn output: {e}")))?;
    let Some(recipe) = parsed.get("recipe").cloned() else {
        return Err(ApiError::Unavailable(format!(
            "galaxy learn returned no recipe: {}",
            out.stderr_tail
        )));
    };
    let validation = parsed.get("validation").cloned().unwrap_or(json!({}));
    Ok(LearnedRecipe {
        items_found: validation
            .get("items_found")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        streams_found: validation
            .get("streams_found")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        notes: parsed
            .get("notes")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        recipe,
    })
}

fn slug_from_site(site: &str) -> String {
    let host = url::Url::parse(site)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "site".into());
    let cleaned = host.trim_start_matches("www.").replace('.', "-");
    crate::utils::slugify(&cleaned)
}

/// One call to add any site as a source: SPACE asks galaxy to learn how the
/// site's search works and what an item page looks like, then stores the
/// resulting recipe. The learned source immediately joins unified search.
#[utoipa::path(post, path = "/api/admin/websources", tag = "admin",
    security(("bearer" = [])),
    request_body = CreateWebSourceRequest,
    responses((status = 201), (status = 403), (status = 503))
)]
pub async fn create_web_source(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(req): Json<CreateWebSourceRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let site = req.site.trim().trim_end_matches('/').to_string();
    if !site.starts_with("http://") && !site.starts_with("https://") {
        return Err(ApiError::bad_request(
            "site must be an absolute http(s) url",
        ));
    }
    if let Some(tpl) = &req.search_url {
        if !tpl.contains("{query}") {
            return Err(ApiError::bad_request(
                "search_url must contain the {query} placeholder, e.g. https://site/search?q={query}",
            ));
        }
    }
    let category = req.category.to_lowercase();
    if !["movie", "series", "book", "music", "manga"].contains(&category.as_str()) {
        return Err(ApiError::bad_request(
            "category must be movie | series | book | music | manga",
        ));
    }

    let learned = run_learn(&state, &req).await.map_err(|e| {
        tracing::warn!(error = %e, "web source learn failed");
        e
    })?;

    let created_by = _admin.0.id();
    let inserted = insert_learned_source(
        &state,
        req.name.as_deref().unwrap_or("").trim(),
        req.label.clone(),
        &site,
        &category,
        learned
            .recipe
            .pointer("/search/url_template")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        req.sample_item.clone(),
        learned.recipe.clone(),
        learned.items_found as i32,
        "learned",
        Some(created_by),
    )
    .await?;

    metrics::counter!("websources_learned_total").increment(1);

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "source": inserted,
            "items_found": learned.items_found,
            "streams_found": learned.streams_found,
            "notes": learned.notes,
            "hint": "results now flow through /api/search/all; test with POST /api/admin/websources/{id}/test",
        })),
    ))
}

trait Pipe: Sized {
    fn pipe<F: FnOnce(Self) -> Self>(self, f: F) -> Self {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Shared insertion path for learned/imported recipes: unique slug, row
/// insert, registry hot-reload.
pub(crate) async fn insert_learned_source(
    state: &AppState,
    name_hint: &str,
    label: Option<String>,
    site: &str,
    category: &str,
    search_url: String,
    sample_item: Option<String>,
    recipe: serde_json::Value,
    items_sampled: i32,
    status: &str,
    created_by: Option<Uuid>,
) -> ApiResult<WebSourceRow> {
    let base = crate::utils::slugify(name_hint.trim())
        .pipe(|s| if s.is_empty() { slug_from_site(site) } else { s });
    let mut name = base.clone();
    let mut suffix = 2;
    while sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM web_sources WHERE LOWER(name) = LOWER($1))",
    )
    .bind(&name)
    .fetch_one(&state.pool)
    .await?
    {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }

    let row = WebSourceRow {
        id: crate::utils::new_id(),
        name: name.clone(),
        label: label.unwrap_or_else(|| name.clone()),
        site: site.trim().trim_end_matches('/').to_string(),
        category: category.to_lowercase(),
        search_url,
        sample_item,
        recipe,
        enabled: true,
        status: status.into(),
        last_error: None,
        items_sampled,
        created_by: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let inserted = repo::insert(&state.pool, &row, created_by).await?;
    state.web_sources.reload(&state.pool, &state.cfg).await;
    Ok(inserted)
}

/// Imports a hub-payload recipe ({name?, label?, site, category?, …, recipe}).
pub(crate) async fn import_recipe(
    state: &AppState,
    payload: serde_json::Value,
) -> ApiResult<WebSourceRow> {
    let get = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let site = get("site")
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        .ok_or_else(|| ApiError::bad_request("payload needs an absolute http(s) 'site'"))?;
    let recipe = payload
        .get("recipe")
        .cloned()
        .filter(|r| r.get("version").is_some() && r.get("search").is_some())
        .ok_or_else(|| ApiError::bad_request("payload is not a valid galaxy recipe"))?;
    let category = get("category").unwrap_or_else(|| "movie".into());
    let search_url = get("search_url").unwrap_or_default();
    let name = get("name").unwrap_or_default();

    insert_learned_source(
        state,
        &name,
        get("label"),
        &site,
        &category,
        search_url,
        payload.get("sample_item").and_then(|v| v.as_str()).map(str::to_string),
        recipe,
        payload
            .get("items_sampled")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        "imported",
        None,
    )
    .await
}

// ------------------------------------------------------------------ list

#[utoipa::path(get, path = "/api/admin/websources", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_web_sources(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = repo::list(&state.pool).await?;
    let live = state.web_sources.list().await;
    let sources: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "label": r.label,
                "site": r.site,
                "category": r.category,
                "search_url": r.search_url,
                "enabled": r.enabled,
                "status": r.status,
                "last_error": r.last_error,
                "items_sampled": r.items_sampled,
                "live": live.iter().any(|e| e.source.id == r.id),
                "created_at": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "sources": sources,
        "galaxy_available": web::resolve_galaxy_bin(&state.cfg).is_some(),
    })))
}

// ----------------------------------------------------------------- update

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PatchWebSourceRequest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[utoipa::path(patch, path = "/api/admin/websources/{id}", tag = "admin",
    security(("bearer" = [])),
    request_body = PatchWebSourceRequest,
    responses((status = 200), (status = 403), (status = 404))
)]
pub async fn patch_web_source(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
    Json(req): Json<PatchWebSourceRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(cat) = &req.category {
        let cat = cat.to_lowercase();
        if !["movie", "series", "book", "music", "manga"].contains(&cat.as_str()) {
            return Err(ApiError::bad_request("invalid category"));
        }
    }
    let row = repo::patch(
        &state.pool,
        id,
        req.label.as_deref(),
        req.category.as_deref().map(|c| c.to_lowercase()).as_deref(),
        req.enabled,
    )
    .await?;
    state.web_sources.reload(&state.pool, &state.cfg).await;
    Ok(Json(json!({ "source": row })))
}

#[utoipa::path(delete, path = "/api/admin/websources/{id}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 204), (status = 403), (status = 404))
)]
pub async fn delete_web_source(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<StatusCode> {
    repo::delete(&state.pool, id).await?;
    state.web_sources.reload(&state.pool, &state.cfg).await;
    metrics::counter!("websources_deleted_total").increment(1);
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------ relearn/test

/// Re-runs the learning pipeline with the stored inputs (site/search/sample).
#[utoipa::path(post, path = "/api/admin/websources/{id}/relearn", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 403), (status = 503))
)]
pub async fn relearn_web_source(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = repo::get(&state.pool, id).await?;
    let req = CreateWebSourceRequest {
        name: Some(row.name.clone()),
        label: None,
        site: row.site.clone(),
        // keep the known-good template when we have one, else rediscover
        search_url: (!row.search_url.is_empty()).then(|| row.search_url.clone()),
        sample_item: row.sample_item.clone(),
        category: row.category.clone(),
    };
    match run_learn(&state, &req).await {
        Ok(learned) => {
            repo::update_recipe(
                &state.pool,
                id,
                learned.recipe.clone(),
                learned
                    .recipe
                    .pointer("/search/url_template")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&row.search_url),
                &row.category,
                "learned",
                learned.items_found as i32,
            )
            .await?;
            state.web_sources.reload(&state.pool, &state.cfg).await;
            Ok(Json(json!({
                "ok": true,
                "items_found": learned.items_found,
                "notes": learned.notes,
            })))
        }
        Err(err) => {
            repo::set_status(&state.pool, id, "failed", Some(&err.to_string()), None).await?;
            let _ = admin;
            Err(err)
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TestWebSourceRequest {
    #[serde(default = "default_test_query")]
    pub query: String,
}

fn default_test_query() -> String {
    String::new()
}

/// Live end-to-end check: runs one search (+ first-item resolve) through
/// galaxy using the stored recipe.
#[utoipa::path(post, path = "/api/admin/websources/{id}/test", tag = "admin",
    security(("bearer" = [])),
    request_body = TestWebSourceRequest,
    responses((status = 200), (status = 403), (status = 404))
)]
pub async fn test_web_source(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<TestWebSourceRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = repo::get(&state.pool, id).await?;
    let query = body
        .as_ref()
        .map(|Json(b)| b.query.clone())
        .unwrap_or_default();

    let started = std::time::Instant::now();
    let result = state
        .web_sources
        .search(&state, &row.name, &query, 6)
        .await;

    match result {
        Ok(items) => {
            let mut deep_check = json!({ "skipped": true });
            if let Some(first) = items.first() {
                match state
                    .web_sources
                    .item(&state, &row.name, &first.external_id)
                    .await
                {
                    Ok(detail) => {
                        deep_check = json!({
                            "ok": true,
                            "title": detail.item.title,
                            "streams": detail.streams.len(),
                        });
                    }
                    Err(e) => deep_check = json!({ "ok": false, "error": e.to_string() }),
                }
            }
            repo::set_status(
                &state.pool,
                id,
                "healthy",
                None,
                Some(items.len() as i32),
            )
            .await?;
            let _ = auth;
            Ok(Json(json!({
                "ok": !items.is_empty(),
                "query": query,
                "elapsed_ms": started.elapsed().as_millis(),
                "count": items.len(),
                "items": items.iter().take(6).collect::<Vec<_>>(),
                "item_check": deep_check,
            })))
        }
        Err(err) => {
            repo::set_status(&state.pool, id, "failing", Some(&err.to_string()), None).await?;
            Err(err)
        }
    }
}

/// Raw recipe inspection/debugging (also editable via PATCH tooling outside).
#[utoipa::path(get, path = "/api/admin/websources/{id}/recipe", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 403), (status = 404))
)]
pub async fn get_recipe(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = repo::get(&state.pool, id).await?;
    Ok(Json(row.recipe))
}
