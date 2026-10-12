//! Native site plugins: the Python plugin directory, compiled into the binary.
//!
//! What used to be ~500 Python processes - one port each, plus a generated
//! `register_all.sh` to tell the server about them - is now [`sites`], a Rust
//! table of every site, driven by the four engines in [`engines`]. No
//! interpreter, no subprocess, no port per site, and nothing to register by
//! hand: [`sync`] upserts one `plugins` row per site at startup, so the
//! aggregator, health prober, benchmark worker and admin console pick them up
//! through the paths they already use.

pub mod custom;
pub mod engines;
pub mod sites;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use custom::CustomEngine;
use engines::{Engine, EngineCtx, EngineKind, NativeSource};
use sites::SITES;

/// Path prefix the loopback base urls point at.
pub const ROUTE_PREFIX: &str = "/api/native-plugins";

/// Marks the plugin rows this module owns, so a later sweep can retire the
/// ones whose site left the table without touching hand-registered plugins.
const MANAGED_BY: &str = "native-engine";

/// A site the server serves natively: either one of the table-driven sites in
/// [`sites`], or one of the hand-converted plugins in [`custom`].
pub enum NativeEntry {
    Table(NativeSource),
    Custom {
        name: String,
        label: String,
        category: String,
        engine: CustomEngine,
    },
}

impl NativeEntry {
    pub fn name(&self) -> &str {
        match self {
            Self::Table(source) => &source.name,
            Self::Custom { name, .. } => name,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Table(source) => &source.label,
            Self::Custom { label, .. } => label,
        }
    }

    pub fn category(&self) -> &str {
        match self {
            Self::Table(source) => &source.category,
            Self::Custom { category, .. } => category,
        }
    }

    /// The upstream site a table row points at. A hand-converted plugin may
    /// talk to several hosts, so it reports none.
    pub fn site(&self) -> &str {
        match self {
            Self::Table(source) => &source.base,
            Self::Custom { .. } => "",
        }
    }

    pub fn engine_id(&self) -> &'static str {
        match self {
            Self::Table(source) => source.engine.id(),
            Self::Custom { .. } => "custom",
        }
    }

    pub fn manifest(&self) -> Value {
        match self {
            Self::Table(source) => source.manifest(),
            Self::Custom {
                name,
                label,
                category,
                ..
            } => json!({
                "name": name,
                "label": label,
                "category": category,
                "kind": "source",
                "transport": "http",
            }),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        match self {
            Self::Table(source) => source.search(ctx, query, limit).await,
            Self::Custom { engine, .. } => engine.search(ctx, query, limit).await,
        }
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        match self {
            Self::Table(source) => source.details(ctx, id).await,
            Self::Custom { engine, .. } => engine.details(ctx, id).await,
        }
    }

    pub fn items(&self) -> Value {
        json!({ "items": [], "note": "library sync not supported" })
    }
}

pub struct NativeRegistry {
    order: Vec<Arc<NativeEntry>>,
    by_name: HashMap<String, Arc<NativeEntry>>,
    ctx: EngineCtx,
}

impl NativeRegistry {
    /// Bind every site in [`sites::SITES`] to its engine.
    pub fn load(cfg: &Config) -> Self {
        let mut order: Vec<Arc<NativeEntry>> = Vec::with_capacity(SITES.len());
        let mut by_name: HashMap<String, Arc<NativeEntry>> = HashMap::new();

        for site in SITES.iter() {
            if !crate::plugins::models::valid_plugin_name(site.name) {
                tracing::warn!(name = %site.name, "native plugin name rejected");
                continue;
            }

            // Same override the Python honoured: `{NAME}_BASE` wins over the
            // url the site was converted with.
            let base = std::env::var(site.env_key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| site.base.to_string());

            let engine = match site.engine {
                EngineKind::Passthrough => Engine::Passthrough {
                    search_note: site.a1.unwrap_or_default().to_string(),
                },
                EngineKind::WpCatalog => Engine::WpCatalog,
                EngineKind::Pandora => Engine::Pandora,
                EngineKind::JsonApi => Engine::JsonApi {
                    // a1: the fingerprinted search path, a2: the watch page
                    search_path: site.a1.unwrap_or("/api/search?q={q}").to_string(),
                    watch_path: site.a2.map(str::to_string),
                },
                EngineKind::TmdbFrontend => {
                    let watch_route = site.a1.map(str::to_string);
                    // `tv_route or watch_route`, with the "none" sentinel
                    // meaning "movies deep-link, series fall back to search".
                    let tv_route = match site
                        .a2
                        .map(str::to_string)
                        .or_else(|| watch_route.clone())
                    {
                        Some(route) if route == "none" => None,
                        other => other,
                    };
                    Engine::TmdbFrontend {
                        watch_route,
                        tv_route,
                    }
                }
                EngineKind::AnilistFrontend => Engine::AnilistFrontend {
                    search_path: site.a1.unwrap_or("/search?q=").to_string(),
                    watch_template: std::env::var(watch_template_key(site.name))
                        .ok()
                        .filter(|v| !v.trim().is_empty()),
                },
            };

            let source = Arc::new(NativeEntry::Table(NativeSource {
                name: site.name.to_string(),
                label: if site.label.is_empty() {
                    site.name.to_string()
                } else {
                    site.label.to_string()
                },
                category: site.category.to_string(),
                base,
                engine,
            }));

            if by_name.insert(site.name.to_string(), source.clone()).is_some() {
                tracing::warn!(name = %site.name, "duplicate native plugin name, later row wins");
            }
            order.push(source);
        }

        // The hand-converted plugins join the same registry. A name clash
        // means the converted plugin is the real implementation of that site,
        // so it replaces the table row rather than sitting beside it.
        for site in custom::all() {
            if !crate::plugins::models::valid_plugin_name(site.name) {
                tracing::warn!(name = %site.name, "custom plugin name rejected");
                continue;
            }
            let entry = Arc::new(NativeEntry::Custom {
                name: site.name.to_string(),
                label: site.label.to_string(),
                category: site.category.to_string(),
                engine: site.engine,
            });
            if by_name.insert(site.name.to_string(), entry.clone()).is_some() {
                order.retain(|existing| existing.name() != site.name);
            }
            order.push(entry);
        }

        tracing::info!(count = order.len(), "native site plugins loaded");
        Self {
            order,
            by_name,
            ctx: EngineCtx::new(cfg.tmdb_api_key.clone()),
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<Arc<NativeEntry>> {
        self.by_name.get(name).cloned()
    }

    pub fn all(&self) -> &[Arc<NativeEntry>] {
        &self.order
    }

    pub fn ctx(&self) -> &EngineCtx {
        &self.ctx
    }
}

/// An empty cell in the table means "argument absent", not "empty string".
fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn watch_template_key(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{upper}_WATCH_TEMPLATE")
}

// ------------------------------------------------------------------ routes

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DetailsQuery {
    #[serde(default)]
    pub id: String,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(ROUTE_PREFIX, get(index))
        .route(
            &format!("{ROUTE_PREFIX}/{{name}}/manifest.json"),
            get(manifest),
        )
        .route(&format!("{ROUTE_PREFIX}/{{name}}/items"), get(items))
        .route(&format!("{ROUTE_PREFIX}/{{name}}/search"), get(search))
        .route(&format!("{ROUTE_PREFIX}/{{name}}/details"), get(details))
}

fn lookup(state: &AppState, name: &str) -> ApiResult<Arc<NativeEntry>> {
    state
        .native_plugins
        .get(name)
        .ok_or_else(|| ApiError::not_found("plugin"))
}

async fn index(State(state): State<Arc<AppState>>) -> axum::Json<Value> {
    let base = self_base(&state.cfg);
    let plugins: Vec<Value> = state
        .native_plugins
        .all()
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name(),
                "label": entry.label(),
                "category": entry.category(),
                "engine": entry.engine_id(),
                "site": entry.site(),
                "base_url": format!("{base}{ROUTE_PREFIX}/{}", entry.name()),
            })
        })
        .collect();
    axum::Json(json!({ "count": plugins.len(), "plugins": plugins }))
}

async fn manifest(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<axum::Json<Value>> {
    Ok(axum::Json(lookup(&state, &name)?.manifest()))
}

/// These sites expose no bulk library; the Python answered the same way.
async fn items(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<axum::Json<Value>> {
    Ok(axum::Json(lookup(&state, &name)?.items()))
}

async fn search(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<axum::Json<Value>> {
    let source = lookup(&state, &name)?;
    let limit = query.limit.unwrap_or(20);
    let body = source
        .search(state.native_plugins.ctx(), &query.q, limit)
        .await?;
    Ok(axum::Json(body))
}

async fn details(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<DetailsQuery>,
) -> ApiResult<axum::Json<Value>> {
    let source = lookup(&state, &name)?;
    let body = source
        .details(state.native_plugins.ctx(), &query.id)
        .await?;
    Ok(axum::Json(body))
}

// ------------------------------------------------------- self-registration

/// Where this server reaches its own routes. Overridable for deployments that
/// terminate somewhere other than the bound port.
fn self_base(cfg: &Config) -> String {
    if let Some(base) = cfg.native_plugins_base.clone() {
        return base.trim_end_matches('/').to_string();
    }
    format!("http://127.0.0.1:{}", cfg.port)
}

/// Upsert one `plugins` row per native site, then retire rows this module owns
/// whose site is no longer in the table.
///
/// `enabled` is only set on insert. The AI ops loop and the admin console both
/// disable failing sources, and a startup sweep that re-enabled everything
/// would undo their work on every restart.
pub async fn sync(state: &AppState) -> ApiResult<usize> {
    let base = self_base(&state.cfg);
    let mut names: Vec<String> = Vec::with_capacity(state.native_plugins.len());

    for entry in state.native_plugins.all() {
        // A passthrough site answers every search with nothing, and can only
        // resolve an item url the viewer has no way to obtain — so it can
        // never produce playback, while still costing a request in every
        // fan-out and a probe in every health cycle. Registering those made
        // search slower for no possible benefit.
        //
        // They stay in the table so a fingerprint can promote one the moment
        // its real endpoint is found; set NATIVE_PLUGINS_INCLUDE_PASSTHROUGH
        // to register them anyway.
        if !state.cfg.native_plugins_include_passthrough
            && matches!(
                entry.as_ref(),
                NativeEntry::Table(source) if matches!(source.engine, Engine::Passthrough { .. })
            )
        {
            continue;
        }

        let base_url = format!("{base}{ROUTE_PREFIX}/{}", entry.name());
        let config = json!({
            "managed_by": MANAGED_BY,
            "engine": entry.engine_id(),
            "label": entry.label(),
            "category": entry.category(),
            "site": entry.site(),
        });
        match crate::plugins::repo::upsert_managed(
            &state.pool,
            crate::utils::new_id(),
            entry.name(),
            "source",
            &base_url,
            config,
        )
        .await
        {
            Ok(_) => names.push(entry.name().to_string()),
            Err(err) => {
                tracing::warn!(plugin = %entry.name(), error = %err, "native plugin upsert failed")
            }
        }
    }

    let retired = crate::plugins::repo::disable_missing_managed(&state.pool, MANAGED_BY, &names)
        .await
        .unwrap_or(0);
    if retired > 0 {
        tracing::info!(retired, "native plugins retired");
    }

    metrics::gauge!("native_plugins_registered").set(names.len() as f64);
    tracing::info!(registered = names.len(), "native site plugins registered");
    Ok(names.len())
}

/// Run one site's search and details for real, printing what it answers.
/// Backs `space --probe-native <name> [query]`, which is how a converted
/// source is checked against the live site without booting the server.
pub async fn probe(registry: &NativeRegistry, name: &str, query: &str) -> Value {
    let Some(entry) = registry.get(name) else {
        return json!({ "error": format!("no native source named {name}") });
    };
    let ctx = registry.ctx();

    let search = match entry.search(ctx, query, 5).await {
        Ok(body) => body,
        Err(err) => json!({ "error": err.to_string() }),
    };

    // Resolve the first hit, which is what the aggregator would do next.
    let first_id = search
        .get("results")
        .and_then(Value::as_array)
        .and_then(|r| r.first())
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let details = match &first_id {
        Some(id) => match entry.details(ctx, id).await {
            Ok(body) => body,
            Err(err) => json!({ "error": err.to_string() }),
        },
        None => json!({ "skipped": "search returned nothing to resolve" }),
    };

    json!({
        "name": entry.name(),
        "engine": entry.engine_id(),
        "site": entry.site(),
        "search": search,
        "resolved_id": first_id,
        "details": details,
    })
}

/// Fingerprints a site by reading what its own front-end does.
///
/// These sites were shipped as `Passthrough` - "search always returns
/// nothing" - because nobody had worked out how they search. Guessing at
/// endpoint paths finds almost none of them: they are JS shells, and the
/// answer is in the bundle. So this fetches the page, follows its scripts,
/// and reads the urls the app actually calls.
///
/// What comes back decides the engine:
///
/// * calls `api.themoviedb.org` -> it is a TMDB frontend, and SPACE already
///   has that engine; the watch route is recovered from the bundle's router
/// * calls `graphql.anilist.co` -> an AniList frontend, likewise
/// * calls an API of its own -> [`Engine::JsonApi`] with that exact path
///
/// Backs `space --detect-passthrough`.
pub async fn detect_passthrough(registry: &NativeRegistry, concurrency: usize) -> Value {
    use futures_util::stream::{self, StreamExt};

    let ctx = registry.ctx();
    let candidates: Vec<(String, String)> = registry
        .all()
        .iter()
        .filter_map(|entry| match entry.as_ref() {
            NativeEntry::Table(source) if matches!(source.engine, Engine::Passthrough { .. }) => {
                Some((source.name.clone(), source.base.clone()))
            }
            _ => None,
        })
        .collect();

    let total = candidates.len();
    let findings: Vec<Value> = stream::iter(candidates)
        .map(|(name, base)| async move {
            let detected = detect_one(ctx, &base).await;
            json!({ "name": name, "base": base, "detected": detected })
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for finding in &findings {
        let key = finding["detected"]
            .as_str()
            .unwrap_or("unknown")
            .split('\t')
            .next()
            .unwrap_or("unknown")
            .to_string();
        *counts.entry(key).or_default() += 1;
    }

    json!({ "probed": total, "summary": counts, "sites": findings })
}

/// Script urls the page loads, absolutised, most promising first.
fn script_urls(html: &str, base: &str) -> Vec<String> {
    static SCRIPT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = SCRIPT.get_or_init(|| {
        regex::Regex::new(r#"(?i)<script[^>]+src=["']([^"']+)["']"#).unwrap()
    });
    let mut out = Vec::new();
    for capture in re.captures_iter(html) {
        let raw = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        if raw.is_empty() {
            continue;
        }
        let absolute = if raw.starts_with("http") {
            raw.to_string()
        } else if let Some(rest) = raw.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            format!("{}/{}", base.trim_end_matches('/'), raw.trim_start_matches('/'))
        };
        if !out.contains(&absolute) {
            out.push(absolute);
        }
    }
    // App bundles carry the API calls; vendor/analytics chunks rarely do.
    out.sort_by_key(|u| {
        let name = u.rsplit('/').next().unwrap_or("");
        let interesting = name.contains("main")
            || name.contains("app")
            || name.contains("index")
            || name.contains("bundle")
            || name.contains("client");
        (!interesting, u.len())
    });
    out.truncate(6);
    out
}

/// Endpoint-looking strings in a script or page.
fn endpoint_candidates(source: &str) -> Vec<String> {
    static ENDPOINT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ENDPOINT.get_or_init(|| {
        regex::Regex::new(
            r#"["'`](https?://[a-zA-Z0-9._\-]+)?(/(?:api|ajax|search|data|v1|v2)[a-zA-Z0-9._/\-]{0,60})["'`]"#,
        )
        .unwrap()
    });
    let mut out: Vec<String> = Vec::new();
    for capture in re.captures_iter(source) {
        let host = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        let path = capture.get(2).map(|m| m.as_str()).unwrap_or("");
        if path.len() < 4 {
            continue;
        }
        let full = format!("{host}{path}");
        if !out.contains(&full) {
            out.push(full);
        }
        if out.len() >= 40 {
            break;
        }
    }
    out
}

/// A watch route like `/movie/{id}` recovered from the bundle's router.
fn watch_route(source: &str) -> Option<String> {
    static ROUTE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ROUTE.get_or_init(|| {
        regex::Regex::new(r#"["'`](/(?:movie|watch|film|tv|series)/(?:\$\{[^}]*\}|:[a-z_]+|\[[a-z]+\]))["'`]"#)
            .unwrap()
    });
    re.captures(source)
        .and_then(|c| c.get(1))
        .map(|m| {
            // normalise whatever placeholder syntax the bundle used
            let raw = m.as_str();
            let cut = raw.rfind('/').unwrap_or(0);
            format!("{}/{{id}}", &raw[..cut])
        })
}


/// Does this endpoint actually search, or does it just return a list?
///
/// Two traps make a naive probe useless. A placeholder term like "test"
/// legitimately matches nothing, so requiring results rejects endpoints that
/// work fine; and plenty of paths (`/api/site-config`, a cached homepage feed)
/// answer with list-shaped JSON no matter what is appended to them.
///
/// So the endpoint is asked several real questions and one nonsense one. It
/// counts as search only if some real term returns rows *and* the nonsense
/// term does not return the same rows - a static feed answers everything
/// identically. A nonsense term that errors or returns nothing is itself
/// evidence of a real search.
async fn is_search_endpoint(ctx: &EngineCtx, path: &str, param: &str, joiner: char) -> bool {
    let ask = |term: &'static str| {
        let url = format!("{path}{joiner}{param}={term}");
        async move {
            ctx.get_json(&url)
                .await
                .ok()
                .and_then(|body| crate::plugins::native::engines::find_item_array(&body))
        }
    };

    // Terms broad enough to hit in a film, anime or live-TV catalogue. One
    // hit is enough; catalogues differ too much for a single probe word.
    let mut hits = None;
    for term in ["one", "the", "man", "a"] {
        if let Some(rows) = ask(term).await
            && !rows.is_empty()
        {
            hits = Some(rows);
            break;
        }
    }
    let Some(hits) = hits else {
        return false;
    };

    // A real search finds nothing for gibberish. An endpoint that errors or
    // returns nothing here is searching; one that repeats itself is a feed.
    match ask("zqxjkvwmpb").await {
        None => true,
        Some(nonsense) => nonsense.is_empty() || nonsense != hits,
    }
}

/// Reads the site the way a browser would, and reports what it is built on.
async fn detect_one(ctx: &EngineCtx, base: &str) -> String {
    let base = base.trim_end_matches('/');

    // pan.do/ra announces itself on a documented API
    if let Ok(body) = ctx
        .post_form(
            &format!("{base}/api/"),
            &[
                ("action", "find"),
                (
                    "data",
                    &json!({
                        "query": { "conditions": [], "operator": "&" },
                        "keys": ["id", "title"],
                        "range": [0, 1],
                    })
                    .to_string(),
                ),
            ],
        )
        .await
        && body.pointer("/status/code").and_then(Value::as_i64) == Some(200)
        && body.pointer("/data/items").is_some()
    {
        return "pandora".into();
    }

    // WordPress with its REST search route left on
    if let Ok(body) = ctx
        .get_json(&format!("{base}/wp-json/wp/v2/search?search=a&per_page=1"))
        .await
        && body.as_array().is_some_and(|rows| {
            rows.iter()
                .all(|row| row.get("url").is_some() || row.get("id").is_some())
        })
    {
        return "wp_catalog".into();
    }

    let Ok(html) = ctx.get_text(base).await else {
        return "unreachable".into();
    };

    // Read the page and its bundles, then judge by what they call.
    let mut corpus = html.clone();
    for url in script_urls(&html, base) {
        if let Ok(script) = ctx.get_text(&url).await {
            // bundles are large; the calls live in the first stretch
            corpus.push_str(&script.chars().take(400_000).collect::<String>());
        }
        if corpus.len() > 1_500_000 {
            break;
        }
    }

    if corpus.contains("api.themoviedb.org") || corpus.contains("image.tmdb.org") {
        return match watch_route(&corpus) {
            Some(route) => format!("tmdb_frontend\t{route}"),
            None => "tmdb_frontend\t".to_string(),
        };
    }
    if corpus.contains("graphql.anilist.co") || corpus.contains("anilist.co/api") {
        return "anilist_frontend\t".into();
    }

    // Its own API: try each endpoint the bundle mentions, and keep only the
    // ones that behave like a search.
    for candidate in endpoint_candidates(&corpus) {
        let path = if candidate.starts_with("http") {
            candidate.clone()
        } else {
            format!("{base}{candidate}")
        };
        for param in ["q", "query", "keyword", "s", "search", "title", "name"] {
            let joiner = if path.contains('?') { '&' } else { '?' };
            if is_search_endpoint(ctx, &path, param, joiner).await {
                return format!("json_api\t{candidate}{joiner}{param}={{q}}");
            }
        }
    }

    "alive-no-api".into()
}

// ------------------------------------------------------------- parity dump

/// Fixed, offline inputs every engine answers deterministically.
///
/// The Python plugins these engines replaced are still on disk;
/// `tools/parity_check.py` drives them through the same inputs and diffs the
/// two dumps, which is what "1:1" is checked against rather than asserted.
/// Every vector here avoids the network: with no TMDB key the enrich step is
/// skipped, an empty query never reaches AniList or WordPress, and the error
/// cases are pure validation.
pub async fn parity_vectors(registry: &NativeRegistry) -> Value {
    let ctx = registry.ctx();
    let mut out = serde_json::Map::new();

    for entry in registry.all() {
        // The hand-converted plugins reach the network for anything
        // meaningful, so they have no offline vector to compare.
        let NativeEntry::Table(source) = entry.as_ref() else {
            continue;
        };
        // These answer from a live API - nothing offline to compare.
        if matches!(source.engine, Engine::Pandora | Engine::JsonApi { .. }) {
            continue;
        }
        let mut vectors = serde_json::Map::new();
        vectors.insert("manifest".into(), source.manifest());
        vectors.insert(
            "search_empty".into(),
            to_value(source.search(ctx, "", 20).await),
        );

        let (ok_id, bad_id) = match &source.engine {
            // The pan.do/ra archives answer from a live API, so they have no
            // offline vector; their ids are opaque archive ids.
            Engine::Pandora => ("0133093".to_string(), "not/an/id".to_string()),
            Engine::JsonApi { .. } => ("parity-test".to_string(), String::new()),
            Engine::Passthrough { .. } => (
                format!("{}/watch/parity-test-title/", source.base.trim_end_matches('/')),
                "https://not-this-site.example/x".to_string(),
            ),
            Engine::WpCatalog => (
                format!("{}/parity-test-post/", source.base.trim_end_matches('/')),
                "https://not-this-site.example/x".to_string(),
            ),
            Engine::TmdbFrontend { .. } => ("movie:603".to_string(), "garbage".to_string()),
            Engine::AnilistFrontend { .. } => {
                ("anilist:not-a-number".to_string(), "garbage".to_string())
            }
        };

        if matches!(source.engine, Engine::Passthrough { .. }) {
            vectors.insert(
                "search_query".into(),
                to_value(source.search(ctx, "parity probe", 20).await),
            );
        }
        vectors.insert("details_ok".into(), to_value(source.details(ctx, &ok_id).await));
        vectors.insert(
            "details_bad".into(),
            to_value(source.details(ctx, &bad_id).await),
        );

        out.insert(source.name.clone(), Value::Object(vectors));
    }

    Value::Object(out)
}

/// Errors are part of the contract, so they are compared too: the Python
/// handler turned a `ValueError` into `400 {"error": msg}`.
fn to_value(result: ApiResult<Value>) -> Value {
    match result {
        Ok(value) => value,
        Err(err) => json!({ "error": err.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_site_is_usable() {
        assert!(
            SITES.len() > 400,
            "expected the full site table, got {}",
            SITES.len()
        );
        let mut seen = std::collections::HashSet::new();
        for site in SITES.iter() {
            assert!(
                crate::plugins::models::valid_plugin_name(site.name),
                "invalid source name: {:?}",
                site.name
            );
            assert!(
                site.base.starts_with("http://") || site.base.starts_with("https://"),
                "{} has a non-absolute base: {:?}",
                site.name,
                site.base
            );
            assert!(
                site.env_key.ends_with("_BASE"),
                "{} has a malformed env key: {:?}",
                site.name,
                site.env_key
            );
            assert!(seen.insert(site.name), "duplicate site name: {}", site.name);
        }
    }

    /// Every Python plugin file is accounted for: the table covers the ones
    /// that reduced to a shared engine, custom covers the rest.
    #[test]
    fn the_whole_plugin_directory_is_converted() {
        let table = SITES.len();
        let custom = custom::all().len();
        assert_eq!(table, 481, "table-driven sites");

        // 23 came from the Python directory. Anything beyond that was added
        // since — a torrent index, and a library server when one is configured
        // — so this is a floor, not a fixed count.
        assert!(
            custom >= 23,
            "every converted plugin must still be here, got {custom}"
        );

        // A converted plugin that shares a name with a table row is the real
        // implementation of that site and replaces it in the loader.
        let table_names: std::collections::HashSet<&str> =
            SITES.iter().map(|s| s.name).collect();
        let overlap = custom::all()
            .iter()
            .filter(|s| table_names.contains(s.name))
            .count();
        // 481 table rows + 23 converted plugins, minus any that share a name.
        // Sources added since (the torrent index, a configured library server)
        // push the total above that floor.
        assert!(
            table + custom - overlap >= 504 - overlap,
            "every converted site must still be served, got {}",
            table + custom - overlap
        );
    }

    #[test]
    fn env_keys_match_the_converted_plugins() {
        assert_eq!(watch_template_key("7reels"), "7REELS_WATCH_TEMPLATE");
        assert_eq!(watch_template_key("anidap-lol"), "ANIDAP_LOL_WATCH_TEMPLATE");
    }
}
