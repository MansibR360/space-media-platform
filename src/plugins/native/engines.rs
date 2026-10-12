//! The four site engines, in Rust.
//!
//! Every site plugin was once a Python file wrapping one of four factories,
//! called with constant arguments. Each of those files reduced to a single
//! row of [`super::sites`], and this module is those factories rewritten
//! natively - so a row plus an engine is the whole plugin: no interpreter, no
//! per-site process, no port.
//!
//! The JSON shapes returned here are the shapes the Python servers returned,
//! because the aggregator, resolver and admin console already speak that
//! contract.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};

const TMDB: &str = "https://api.themoviedb.org/3";
const IMG: &str = "https://image.tmdb.org/t/p";
const ANILIST_GQL: &str = "https://graphql.anilist.co";

const ANILIST_QUERY: &str = r#"
query ($search: String, $perPage: Int) {
  Page(page: 1, perPage: $perPage) {
    media(search: $search, type: ANIME, sort: SEARCH_MATCH) {
      id
      title { romaji english }
      description(asHtml: false)
      seasonYear
      coverImage { large }
    }
  }
}
"#;

/// Shared HTTP client + credentials for every native source.
/// How long a resolved stream stays good. Long enough that a replay or a
/// second viewer is free, short enough that an expiring CDN token is not
/// served past its life.
const RESOLVE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

#[derive(Clone)]
pub struct EngineCtx {
    pub client: reqwest::Client,
    pub tmdb_key: Option<String>,
    /// Resolved details per item, so playback does not re-walk the embed chain
    /// on every request.
    resolved: std::sync::Arc<tokio::sync::RwLock<HashMap<String, (std::time::Instant, Value)>>>,
}

impl EngineCtx {
    pub fn new(tmdb_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            )
            .build()
            .expect("reqwest client");
        Self {
            client,
            tmdb_key,
            resolved: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    /// A previously resolved item, if it has not aged out.
    pub async fn cached_details(&self, key: &str) -> Option<Value> {
        let cache = self.resolved.read().await;
        cache
            .get(key)
            .filter(|(stored, _)| stored.elapsed() < RESOLVE_TTL)
            .map(|(_, value)| value.clone())
    }

    pub async fn cache_details(&self, key: &str, value: Value) {
        let mut cache = self.resolved.write().await;
        // Drop anything stale before inserting, so the map cannot grow without
        // bound on a long-running server.
        cache.retain(|_, (stored, _)| stored.elapsed() < RESOLVE_TTL);
        cache.insert(key.to_string(), (std::time::Instant::now(), value));
    }

    /// GET with extra headers. Embed providers gate their stream endpoints on
    /// `Referer` and `X-Requested-With`, so resolving playback needs them.
    pub async fn get_text_with(&self, url: &str, headers: &[(&str, &str)]) -> ApiResult<String> {
        let mut request = self.client.get(url);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "upstream answered HTTP {}",
                resp.status()
            )));
        }
        resp.text()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream sent no body: {e}")))
    }

    /// As [`Self::get_text_with`], parsed as JSON.
    pub async fn get_json_with(&self, url: &str, headers: &[(&str, &str)]) -> ApiResult<Value> {
        let text = self.get_text_with(url, headers).await?;
        serde_json::from_str(&text)
            .map_err(|e| ApiError::Unavailable(format!("upstream sent invalid json: {e}")))
    }

    /// JSON POST returning JSON, for indexes that take a query body.
    pub async fn post_json(&self, url: &str, body: &Value) -> ApiResult<Value> {
        let resp = self
            .client
            .post(url)
            .header("accept", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "upstream answered HTTP {}",
                resp.status()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream sent invalid json: {e}")))
    }

    /// Form-encoded POST returning JSON - the pan.do/ra API shape.
    pub async fn post_form(&self, url: &str, form: &[(&str, &str)]) -> ApiResult<Value> {
        let resp = self
            .client
            .post(url)
            .header("accept", "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "upstream answered HTTP {}",
                resp.status()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream sent invalid json: {e}")))
    }

    /// Body as text, for the plugins that read HTML or RSS.
    pub async fn get_text(&self, url: &str) -> ApiResult<String> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "upstream answered HTTP {}",
                resp.status()
            )));
        }
        resp.text()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream sent no body: {e}")))
    }

    pub async fn get_json(&self, url: &str) -> ApiResult<Value> {
        let resp = self
            .client
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream unreachable: {e}")))?;
        if !resp.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "upstream answered HTTP {}",
                resp.status()
            )));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ApiError::Unavailable(format!("upstream sent invalid json: {e}")))
    }
}


/// Is this id addressed to a different source?
///
/// The resolver does title-based failover: when one source cannot play a
/// title it offers the same id to the others, so every source is regularly
/// handed ids in formats it does not own - `movie|tt123…` from a Stremio
/// extension, `source|id` from a hub.
///
/// Declining those has to be instant and harmless. Calling upstream with a
/// foreign id wastes a round trip and, worse, the failure comes back as
/// "service unavailable", which trips this source's circuit breaker and drops
/// a perfectly healthy source out of the fan-out.
pub fn foreign_id(id: &str) -> bool {
    // `|` is the aggregator's own composite separator and never appears in an
    // id a native source minted.
    id.contains('|')
}

/// `urllib.parse.quote_plus`.
pub fn quote_plus(input: &str) -> String {
    url::form_urlencoded::byte_serialize(input.as_bytes()).collect()
}

fn clamp_limit(limit: usize) -> usize {
    limit.clamp(1, 30)
}

/// Trailing path segment as a human title - the Python `urldecode_slug`.
fn urldecode_slug(item_url: &str) -> String {
    let trimmed = item_url.trim_end_matches('/');
    let slug = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let pretty = percent_decode(slug).replace(['-', '_'], " ");
    let pretty = pretty.trim();
    if pretty.is_empty() {
        item_url.to_string()
    } else {
        pretty.to_string()
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn host_of(raw: &str) -> Option<String> {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

/// Mirrors are legitimate: accept any host sharing the registrable domain.
fn same_site(item_url: &str, base: &str) -> bool {
    let (Some(item_host), Some(base_host)) = (host_of(item_url), host_of(base)) else {
        return false;
    };
    let parts: Vec<&str> = base_host.split('.').collect();
    let suffix = if parts.len() >= 2 {
        parts[parts.len() - 2..].join(".")
    } else {
        base_host.clone()
    };
    item_host == base_host || item_host.ends_with(&format!(".{suffix}"))
}

pub fn year_from_date(date: &str) -> Option<i64> {
    let head: String = date.chars().take(4).collect();
    if head.len() == 4 && head.chars().all(|c| c.is_ascii_digit()) {
        head.parse().ok()
    } else {
        None
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn page_stream(name: &str, url: &str) -> Value {
    json!({ "label": format!("{name} \u{b7} watch page"), "url": url, "kind": "page" })
}

// ------------------------------------------------------------------- TMDB

pub async fn tmdb_search(ctx: &EngineCtx, query: &str, limit: usize) -> Vec<Value> {
    let Some(key) = ctx.tmdb_key.as_deref() else {
        return Vec::new();
    };
    let url = format!(
        "{TMDB}/search/multi?api_key={key}&query={}&include_adult=false&page=1",
        quote_plus(query)
    );
    let Some(data) = crate::catalog::tmdb_cache::fetch(&ctx.client, &url).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for row in data
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let media_type = row.get("media_type").and_then(Value::as_str).unwrap_or("");
        if media_type != "movie" && media_type != "tv" {
            continue;
        }
        let date = row
            .get("release_date")
            .or_else(|| row.get("first_air_date"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let poster = row.get("poster_path").and_then(Value::as_str);
        out.push(json!({
            "id": format!("{media_type}:{}", row.get("id").and_then(Value::as_i64).unwrap_or(0)),
            "title": row.get("title").or_else(|| row.get("name")).and_then(Value::as_str).unwrap_or("Untitled"),
            "overview": truncate(row.get("overview").and_then(Value::as_str).unwrap_or(""), 400),
            "year": year_from_date(date),
            "poster": poster.map(|p| format!("{IMG}/w342{p}")).unwrap_or_default(),
            "type": media_type,
        }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

pub async fn tmdb_trending(ctx: &EngineCtx, limit: usize) -> Vec<Value> {
    let Some(key) = ctx.tmdb_key.as_deref() else {
        return Vec::new();
    };
    let mut out: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for kind in ["movie", "tv"] {
        let url = format!("{TMDB}/trending/{kind}/week?api_key={key}");
        let Some(data) = crate::catalog::tmdb_cache::fetch(&ctx.client, &url).await else {
            continue;
        };
        for row in data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(id) = row.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let key_id = format!("{kind}:{id}");
            if !seen.insert(key_id.clone()) {
                continue;
            }
            let date = row
                .get("release_date")
                .or_else(|| row.get("first_air_date"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let poster = row.get("poster_path").and_then(Value::as_str);
            out.push(json!({
                "id": key_id,
                "title": row.get("title").or_else(|| row.get("name")).and_then(Value::as_str).unwrap_or("Untitled"),
                "overview": truncate(row.get("overview").and_then(Value::as_str).unwrap_or(""), 300),
                "year": year_from_date(date),
                "poster": poster.map(|p| format!("{IMG}/w342{p}")).unwrap_or_default(),
                "type": kind,
            }));
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

pub async fn tmdb_enrich(ctx: &EngineCtx, kind: &str, tmdb_id: &str) -> Option<Value> {
    let key = ctx.tmdb_key.as_deref()?;
    let data = crate::catalog::tmdb_cache::fetch(
        &ctx.client,
        &format!("{TMDB}/{kind}/{tmdb_id}?api_key={key}"),
    )
    .await?;
    let date = data
        .get("release_date")
        .or_else(|| data.get("first_air_date"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let poster = data
        .get("poster_path")
        .or_else(|| data.get("backdrop_path"))
        .and_then(Value::as_str);
    Some(json!({
        "title": data.get("title").or_else(|| data.get("name")).and_then(Value::as_str).unwrap_or(""),
        "overview": truncate(data.get("overview").and_then(Value::as_str).unwrap_or(""), 600),
        "year": year_from_date(date),
        "poster": poster.map(|p| format!("{IMG}/w500{p}")).unwrap_or_default(),
    }))
}

/// A parsed media id: kind, tmdb id, and - for series - season and episode,
/// which default to 1 exactly as the Python did.
pub struct MediaId {
    pub kind: String,
    pub tmdb_id: String,
    pub season: u32,
    pub episode: u32,
}

/// A player that accepts an AniList id.
///
/// Anime frontends are AniList-keyed shells whose watch routes are built in
/// the browser, so there is no url on the site to link to. The embed players
/// they load take the same id, which is what makes the episode playable.
pub fn anime_embed_url(anilist_id: &str, episode: u32, dub: bool) -> String {
    let base = std::env::var("ANIME_EMBED_BASE")
        .ok()
        .map(|v| v.trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://megaplay.buzz/stream/ani".to_string());
    let category = if dub { "dub" } else { "sub" };
    format!("{base}/{anilist_id}/{episode}/{category}")
}

/// A player that accepts a bare TMDB id.
///
/// Most sites in the catalogue are TMDB-keyed shells with no public watch
/// route, so there is nothing on them to link to. Their content is reachable
/// through the embed players those sites themselves load, which take the same
/// id. Overridable, since these hosts move.
pub fn tmdb_embed_url(kind: &str, tmdb_id: &str, season: u32, episode: u32) -> String {
    let base = std::env::var("TMDB_EMBED_BASE")
        .ok()
        .map(|v| v.trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "https://www.vidking.net/embed".to_string());

    if kind == "movie" {
        format!("{base}/movie/{tmdb_id}")
    } else {
        format!("{base}/tv/{tmdb_id}/{season}/{episode}")
    }
}

/// `movie:{tmdb}` | `tv:{tmdb}` | `tv:{tmdb}:{season}:{episode}`.
pub fn parse_media_id_msg(external_id: &str, message: &'static str) -> ApiResult<MediaId> {
    let parts: Vec<&str> = external_id.trim().split(':').collect();
    let numeric = parts
        .get(1)
        .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if parts.len() >= 2 && parts[0] == "movie" && numeric {
        return Ok(MediaId {
            kind: "movie".into(),
            tmdb_id: parts[1].to_string(),
            season: 1,
            episode: 1,
        });
    }
    if (parts.len() == 2 || parts.len() == 4) && parts[0] == "tv" && numeric {
        let digits = |i: usize| -> Option<u32> {
            parts.get(i).and_then(|p| p.parse::<u32>().ok())
        };
        return Ok(MediaId {
            kind: "tv".into(),
            tmdb_id: parts[1].to_string(),
            season: if parts.len() == 4 { digits(2).unwrap_or(1) } else { 1 },
            episode: if parts.len() == 4 { digits(3).unwrap_or(1) } else { 1 },
        });
    }
    Err(ApiError::bad_request(message))
}

fn parse_media_id(external_id: &str) -> ApiResult<(String, String)> {
    let id = parse_media_id_msg(
        external_id,
        "id must be movie:{tmdb_id} or tv:{tmdb_id}[:s]:[e]",
    )?;
    Ok((id.kind, id.tmdb_id))
}

/// `_plugin_server.slugify`: lowercase, non-alphanumerics collapsed to dashes,
/// trimmed to 60 characters, never empty.
pub fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    let slug: String = out.chars().take(60).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "watch".into() } else { slug }
}

// ---------------------------------------------------------------- AniList

/// One AniList entry, fetched by its id.
///
/// Distinct from [`anilist_search`], which matches on a title: asking it for
/// "20" searches for the *text* 20 and answers with whatever show happens to
/// match, not AniList 20.
async fn anilist_by_id(ctx: &EngineCtx, anilist_id: &str) -> Option<Value> {
    const QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id, type: ANIME) {
    id
    title { romaji english }
    description(asHtml: false)
    seasonYear
    coverImage { large }
  }
}
"#;
    let id: i64 = anilist_id.parse().ok()?;
    let payload = json!({ "query": QUERY, "variables": { "id": id } });

    let data: Value = ctx
        .client
        .post(ANILIST_GQL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&payload)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let media = data.pointer("/data/Media")?;
    let titles = media.get("title");
    let title = titles
        .and_then(|t| t.get("english"))
        .and_then(Value::as_str)
        .or_else(|| titles.and_then(|t| t.get("romaji")).and_then(Value::as_str))
        .unwrap_or("Untitled");

    Some(json!({
        "id": format!("anilist:{anilist_id}"),
        "title": title,
        "overview": truncate(media.get("description").and_then(Value::as_str).unwrap_or(""), 400),
        "year": media.get("seasonYear").and_then(Value::as_i64),
        "poster": media.pointer("/coverImage/large").and_then(Value::as_str).unwrap_or(""),
        "type": "series",
    }))
}



async fn anilist_search(ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Vec<Value>> {
    let payload = json!({
        "query": ANILIST_QUERY,
        "variables": { "search": query, "perPage": clamp_limit(limit) },
    });
    let resp = ctx
        .client
        .post(ANILIST_GQL)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("anilist unreachable: {e}")))?;
    let data: Value = resp
        .json()
        .await
        .map_err(|e| ApiError::Unavailable(format!("anilist sent invalid json: {e}")))?;

    let media = data
        .pointer("/data/Page/media")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(media
        .iter()
        .map(|row| {
            let titles = row.get("title");
            let title = titles
                .and_then(|t| t.get("english"))
                .and_then(Value::as_str)
                .or_else(|| titles.and_then(|t| t.get("romaji")).and_then(Value::as_str))
                .unwrap_or("Untitled");
            json!({
                "id": format!("anilist:{}", row.get("id").and_then(Value::as_i64).unwrap_or(0)),
                "title": title,
                "overview": truncate(row.get("description").and_then(Value::as_str).unwrap_or(""), 400),
                "year": row.get("seasonYear").and_then(Value::as_i64),
                "poster": row.pointer("/coverImage/large").and_then(Value::as_str).unwrap_or(""),
                "type": "series",
            })
        })
        .collect())
}


/// First string value among a set of candidate keys.
pub fn pick_str(row: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = row.get(key) {
            if let Some(text) = value.as_str() {
                return Some(text.trim().to_string());
            }
            // some sites nest the display name one level down
            if let Some(text) = value.get("name").and_then(Value::as_str) {
                return Some(text.trim().to_string());
            }
            if let Some(text) = value.get("english").or_else(|| value.get("romaji")).and_then(Value::as_str) {
                return Some(text.trim().to_string());
            }
        }
    }
    None
}

pub fn pick_num(row: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| row.get(key).and_then(Value::as_i64))
}

/// The first array of objects that looks like search results.
///
/// Sites bury their hits under `results`, `data`, `items`, `list`, `hits` or
/// return them at the root, sometimes nested two levels down. Rather than
/// hard-code each site's envelope, this walks the document for the first array
/// whose objects carry something title-shaped.
pub fn find_item_array(body: &Value) -> Option<Vec<Value>> {
    fn looks_like_items(array: &[Value]) -> bool {
        !array.is_empty()
            && array.iter().take(3).all(|row| {
                row.is_object()
                    && ["title", "name", "title_english", "originalTitle", "original_title", "label"]
                        .iter()
                        .any(|key| row.get(key).is_some())
            })
    }

    fn walk(value: &Value, depth: usize) -> Option<Vec<Value>> {
        if depth > 4 {
            return None;
        }
        if let Some(array) = value.as_array() {
            if looks_like_items(array) {
                return Some(array.clone());
            }
            return array.iter().find_map(|child| walk(child, depth + 1));
        }
        if let Some(map) = value.as_object() {
            // preferred envelopes first, so a stray array cannot win
            for key in ["results", "data", "items", "list", "hits", "shows", "movies", "content"] {
                if let Some(child) = map.get(key)
                    && let Some(found) = walk(child, depth + 1)
                {
                    return Some(found);
                }
            }
            return map.values().find_map(|child| walk(child, depth + 1));
        }
        None
    }

    walk(body, 0)
}

// ----------------------------------------------------------------- engines

/// Which engine a site uses. The tag `sites.rs` names; [`Engine`] is the
/// same choice with that site's arguments bound to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Passthrough,
    WpCatalog,
    TmdbFrontend,
    AnilistFrontend,
    Pandora,
    JsonApi,
}

#[derive(Debug, Clone)]
pub enum Engine {
    /// JS-shell catalogs with no usable public search: search is honestly
    /// empty, details still resolve any known item url on the host.
    Passthrough { search_note: String },
    /// WordPress sites exposing `/wp-json/wp/v2/search`.
    WpCatalog,
    /// TMDB-keyed frontends; deep links follow captured route templates.
    TmdbFrontend {
        watch_route: Option<String>,
        tv_route: Option<String>,
    },
    /// AniList-keyed anime frontends whose watch routes are JS-only.
    AnilistFrontend {
        search_path: String,
        watch_template: Option<String>,
    },
    /// pan.do/ra archives, which expose a real JSON API at `/api/`.
    Pandora,
    /// A site whose own JSON search endpoint was fingerprinted. `search_path`
    /// is where its results come from; `watch_path` is the page that embeds
    /// the player, which is what makes the source playable rather than a
    /// handoff link.
    JsonApi {
        search_path: String,
        watch_path: Option<String>,
    },
}

impl Engine {
    pub fn id(&self) -> &'static str {
        match self {
            Engine::Passthrough { .. } => "passthrough",
            Engine::WpCatalog => "wp_catalog",
            Engine::TmdbFrontend { .. } => "tmdb_frontend",
            Engine::AnilistFrontend { .. } => "anilist_frontend",
            Engine::Pandora => "pandora",
            Engine::JsonApi { .. } => "json_api",
        }
    }
}

/// One site: a registry row bound to its engine.
#[derive(Debug, Clone)]
pub struct NativeSource {
    pub name: String,
    pub label: String,
    pub category: String,
    pub base: String,
    pub engine: Engine,
}

impl NativeSource {
    pub fn manifest(&self) -> Value {
        json!({
            "name": self.name,
            "label": self.label,
            "category": self.category,
            "kind": "source",
            "transport": "http",
        })
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        // Python clamps the upstream page size but slices the returned rows
        // with the caller's raw limit; both are kept so outputs match exactly.
        let raw_limit = limit;
        let limit = clamp_limit(limit);

        match &self.engine {
            Engine::Passthrough { search_note } => {
                Ok(json!({ "results": [], "note": search_note }))
            }

            Engine::WpCatalog => {
                if query.is_empty() {
                    return Ok(json!({ "results": [] }));
                }
                let url = format!(
                    "{}/wp-json/wp/v2/search?search={}&per_page={limit}",
                    self.base.trim_end_matches('/'),
                    quote_plus(query)
                );
                // An unreachable catalog is a fact about one site, not an error
                // for the caller: the fan-out keeps every other source and the
                // note explains the empty list.
                let rows = match ctx.get_json(&url).await {
                    Ok(rows) => rows,
                    Err(err) => {
                        return Ok(
                            json!({ "results": [], "note": format!("catalog unreachable: {err}") }),
                        );
                    }
                };
                let results: Vec<Value> = rows
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .take(raw_limit)
                    .filter_map(|row| {
                        let post_url = row.get("url").and_then(Value::as_str)?;
                        Some(json!({
                            "id": post_url,
                            "title": row.get("title").and_then(Value::as_str).unwrap_or("Untitled"),
                            "overview": "",
                            "year": Value::Null,
                            "poster": "",
                            "type": self.category,
                        }))
                    })
                    .collect();
                Ok(json!({ "results": results }))
            }

            Engine::TmdbFrontend { .. } => {
                let results = if query.is_empty() {
                    tmdb_trending(ctx, limit).await
                } else {
                    tmdb_search(ctx, query, limit).await
                };
                Ok(json!({ "results": results }))
            }

            Engine::Pandora => self.pandora_search(ctx, query, limit).await,

            Engine::JsonApi { search_path, .. } => {
                self.json_api_search(ctx, search_path, query, limit).await
            }

            Engine::AnilistFrontend { .. } => {
                if query.is_empty() {
                    return Ok(json!({ "results": [] }));
                }
                match anilist_search(ctx, query, limit).await {
                    Ok(results) => Ok(json!({ "results": results })),
                    Err(err) => Ok(
                        json!({ "results": [], "note": format!("anilist search failed: {err}") }),
                    ),
                }
            }
        }
    }

    /// pan.do/ra exposes a documented JSON API at `{base}/api/`: a
    /// form-encoded POST carrying an `action` and a JSON `data` payload. These
    /// archives were shipped as `Passthrough` (search always empty) because
    /// nobody had fingerprinted the stack; talking to the real API turns them
    /// into a source that actually answers.
    async fn pandora_api(
        &self,
        ctx: &EngineCtx,
        action: &str,
        data: Value,
    ) -> ApiResult<Value> {
        let url = format!("{}/api/", self.base.trim_end_matches('/'));
        let body = ctx
            .post_form(&url, &[("action", action), ("data", &data.to_string())])
            .await?;
        let code = body.pointer("/status/code").and_then(Value::as_i64);
        if code != Some(200) {
            let text = body
                .pointer("/status/text")
                .and_then(Value::as_str)
                .unwrap_or("request refused");
            // A refused lookup means this archive does not hold the item, not
            // that the archive is down - reporting it as an outage would trip
            // the circuit breaker on a healthy source.
            if matches!(code, Some(404) | Some(403)) {
                return Err(ApiError::not_found("item"));
            }
            return Err(ApiError::Unavailable(format!(
                "{} answered: {text}",
                self.name
            )));
        }
        Ok(body.get("data").cloned().unwrap_or_else(|| json!({})))
    }

    fn pandora_item(&self, row: &Value) -> Option<Value> {
        let id = row.get("id").and_then(Value::as_str)?;
        let base = self.base.trim_end_matches('/');
        Some(json!({
            "id": id,
            "title": row.get("title").and_then(Value::as_str).unwrap_or("Untitled"),
            "overview": truncate(row.get("summary").and_then(Value::as_str).unwrap_or(""), 400),
            "year": row.get("year").and_then(Value::as_i64),
            "poster": format!("{base}/{id}/poster.jpg"),
            "type": self.category,
        }))
    }

    async fn pandora_search(
        &self,
        ctx: &EngineCtx,
        query: &str,
        limit: usize,
    ) -> ApiResult<Value> {
        // An empty query browses the archive, most-voted first, which is what
        // the site's own front page shows.
        let conditions = if query.is_empty() {
            json!([])
        } else {
            json!([{ "key": "*", "value": query, "operator": "=" }])
        };
        // Sorting by votes alongside a query makes these instances drop the
        // filter and answer with the archive's most-voted items instead, so
        // the sort is only asked for when browsing.
        let mut data = json!({
            "query": { "conditions": conditions, "operator": "&" },
            "keys": ["id", "title", "year", "summary"],
            "range": [0, limit],
        });
        if query.is_empty() {
            data["sort"] = json!([{ "key": "votes", "operator": "-" }]);
        }

        let payload = match self.pandora_api(ctx, "find", data).await {
            Ok(payload) => payload,
            Err(err) => {
                return Ok(json!({ "results": [], "note": format!("archive unreachable: {err}") }));
            }
        };
        let results: Vec<Value> = payload
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| self.pandora_item(row))
            .collect();
        Ok(json!({ "results": results }))
    }

    async fn pandora_details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let id = id.trim();
        // An archive id is a bare token; anything else belongs to some other
        // source and is refused without a request.
        let ours = !id.is_empty()
            && !foreign_id(id)
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ours {
            return Err(ApiError::bad_request(format!(
                "id must be a {} item id (from search)",
                self.name
            )));
        }
        let data = json!({
            "id": id,
            "keys": ["id", "title", "year", "summary", "director", "duration", "rightslevel"],
        });
        let row = self.pandora_api(ctx, "get", data).await?;
        if row.get("id").is_none() {
            return Err(ApiError::not_found("item"));
        }

        let base = self.base.trim_end_matches('/');
        let directors: Vec<&str> = row
            .get("director")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut overview = truncate(row.get("summary").and_then(Value::as_str).unwrap_or(""), 600);
        if !directors.is_empty() {
            overview = format!("{} \u{b7} {overview}", directors.join(", "));
        }

        Ok(json!({
            "title": row.get("title").and_then(Value::as_str).unwrap_or("Untitled"),
            "overview": overview,
            "poster": format!("{base}/{id}/poster.jpg"),
            "year": row.get("year").and_then(Value::as_i64),
            "media_type": if self.category == "movie" { "movie" } else { "series" },
            "streams": [{
                "label": format!("{} \u{b7} watch", self.name),
                "url": format!("{base}/{id}"),
                "kind": "page",
            }],
        }))
    }

    /// Reads a search response whose field names are not known in advance.
    ///
    /// These sites were shipped as `Passthrough` because nobody had worked out
    /// how their search worked. Fingerprinting found the endpoint, but every
    /// one of them names its fields differently - `title`/`name`,
    /// `poster`/`image`/`cover` - so rather than record a schema per site, the
    /// rows carry only the path and the shape is read here.
    async fn json_api_search(
        &self,
        ctx: &EngineCtx,
        search_path: &str,
        query: &str,
        limit: usize,
    ) -> ApiResult<Value> {
        let base = self.base.trim_end_matches('/');
        // Some sites keep their API on a different host from the site itself,
        // so a fingerprinted path may be an absolute url. Prefixing the base
        // onto one of those would produce nonsense.
        let path = search_path.replace("{q}", &quote_plus(query));
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path
        } else {
            format!("{base}{path}")
        };
        let body = match ctx.get_json(&url).await {
            Ok(body) => body,
            Err(err) => {
                return Ok(json!({ "results": [], "note": format!("search unavailable: {err}") }));
            }
        };

        let rows = find_item_array(&body).unwrap_or_default();
        let mut results: Vec<Value> = rows
            .iter()
            .filter_map(|row| self.json_api_item(row))
            .collect();

        // Several of these endpoints are catalogues rather than searches: a
        // channel line-up or a today's-fixtures feed answers the same list
        // whatever is asked of it. Filtering here makes those usable as a
        // source instead of flooding every query with the whole catalogue.
        // If the endpoint *did* search, its hits already match and survive;
        // if filtering leaves nothing, the upstream answer is kept rather
        // than turning a fuzzy match into an empty result.
        if !query.is_empty() {
            let needle = query.to_lowercase();
            let matched: Vec<Value> = results
                .iter()
                .filter(|row| {
                    row.get("title")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.to_lowercase().contains(&needle))
                })
                .cloned()
                .collect();
            // No fallback to the unfiltered list: for a catalogue endpoint
            // that would answer every query with the whole line-up, and these
            // results are merged into a source-anonymous unified search where
            // that noise is indistinguishable from a real hit.
            results = matched;
        }

        results.truncate(limit);
        Ok(json!({ "results": results }))
    }

    fn json_api_item(&self, row: &Value) -> Option<Value> {
        let title = pick_str(row, &["title", "name", "title_english", "originalTitle", "original_title", "label"])
            .filter(|t| !t.is_empty())?;
        // An id we can resolve later: a real id, a slug, or the item url.
        let id = pick_str(row, &["url", "link", "href", "slug", "id", "_id", "permalink"])
            .filter(|v| !v.is_empty())
            .or_else(|| row.get("id").and_then(|v| v.as_i64()).map(|n| n.to_string()))?;

        let year = pick_str(row, &["year", "release_date", "first_air_date", "released", "date"])
            .and_then(|v| year_from_date(&v))
            .or_else(|| pick_num(row, &["year", "releaseYear", "seasonYear"]));

        let poster = pick_str(
            row,
            &[
                "poster", "image", "cover", "thumbnail", "img", "img_url", "imgUrl",
                "posterUrl", "poster_path", "coverImage", "image_url", "banner", "artwork",
            ],
        )
        .unwrap_or_default();
        let poster = if !poster.is_empty() && poster.starts_with('/') && !poster.starts_with("//") {
            format!("{}{poster}", self.base.trim_end_matches('/'))
        } else {
            poster
        };

        Some(json!({
            "id": id,
            "title": title,
            "overview": truncate(
                &pick_str(row, &["overview", "description", "synopsis", "plot", "summary"])
                    .unwrap_or_default(),
                400,
            ),
            "year": year,
            "poster": poster,
            "type": self.category,
        }))
    }

    /// Resolves a search hit to something playable.
    ///
    /// The id is a slug or an item url; `watch_path` turns it into the page
    /// that embeds the player, and any embed on that page is handed to a
    /// provider extractor for the real stream. When no extractor matches, the
    /// watch page itself is returned - the behaviour the Python had for every
    /// site, and still the honest answer for a player we cannot read.
    ///
    /// `id` may carry an episode as `slug:episode`.
    async fn json_api_details(
        &self,
        ctx: &EngineCtx,
        watch_path: Option<&str>,
        id: &str,
    ) -> ApiResult<Value> {
        use crate::plugins::native::custom::embeds;

        let id = id.trim();
        if id.is_empty() || foreign_id(id) {
            return Err(ApiError::bad_request(format!(
                "id must be a {} search result id",
                self.name
            )));
        }

        let (slug, episode) = match id.rsplit_once(':') {
            Some((head, tail)) if !head.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
                (head, tail.to_string())
            }
            _ => (id, "1".to_string()),
        };

        let base = self.base.trim_end_matches('/');
        let page_url = if slug.starts_with("http://") || slug.starts_with("https://") {
            slug.to_string()
        } else {
            match watch_path {
                Some(template) => format!(
                    "{base}{}",
                    template
                        .replace("{id}", slug.trim_start_matches('/'))
                        .replace("{ep}", &episode)
                ),
                None => format!("{base}/{}", slug.trim_start_matches('/')),
            }
        };

        // Resolving a stream is three round trips (watch page, embed page,
        // source api). Cache the result so a replay, a seek or a second
        // viewer does not repeat the whole chain.
        let cache_key = format!("{}|{page_url}", self.name);
        if let Some(cached) = ctx.cached_details(&cache_key).await {
            return Ok(cached);
        }

        let mut streams = Vec::new();
        if let Ok(html) = ctx.get_text(&page_url).await {
            // Every embed the page leads to, best provider first; if the first
            // yields nothing playable the next one is tried.
            for embed in embeds::discover(&html, base, &episode) {
                if !embeds::is_known_provider(&embed) {
                    continue;
                }
                if let Ok(found) = embeds::resolve(ctx, &embed, &page_url).await
                    && !found.is_empty()
                {
                    streams.extend(found);
                    break;
                }
            }
        }
        // The watch page is always offered last, as a fallback the user can
        // open when no extractor produced a direct stream.
        streams.push(page_stream(&self.name, &page_url));

        let payload = json!({
            "title": urldecode_slug(&page_url),
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": self.category,
            "streams": streams,
        });
        // Only worth remembering if something actually resolved: a page-only
        // answer may just mean the site was briefly unreachable.
        if payload["streams"].as_array().is_some_and(|s| s.len() > 1) {
            ctx.cache_details(&cache_key, payload.clone()).await;
        }
        Ok(payload)
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        match &self.engine {
            Engine::Passthrough { .. } => {
                if !id.starts_with(&self.base) {
                    return Err(ApiError::bad_request(format!(
                        "id must be an absolute {} item url",
                        self.name
                    )));
                }
                Ok(json!({
                    "title": urldecode_slug(id),
                    "overview": "",
                    "poster": "",
                    "year": Value::Null,
                    "media_type": self.category,
                    "streams": [page_stream(&self.name, id)],
                }))
            }

            Engine::WpCatalog => {
                let absolute = id.starts_with("http://") || id.starts_with("https://");
                if !absolute || !same_site(id, &self.base) {
                    return Err(ApiError::bad_request(format!(
                        "id must be an absolute {} post url",
                        self.name
                    )));
                }
                Ok(json!({
                    "title": urldecode_slug(id),
                    "overview": "",
                    "poster": "",
                    "year": Value::Null,
                    "media_type": self.category,
                    "streams": [page_stream(&self.name, id)],
                }))
            }

            Engine::TmdbFrontend {
                watch_route,
                tv_route,
            } => {
                let media = parse_media_id_msg(
                    id,
                    "id must be movie:{tmdb_id} or tv:{tmdb_id}[:s]:[e]",
                )?;
                let (kind, tmdb_id) = (media.kind.clone(), media.tmdb_id.clone());
                let info = tmdb_enrich(ctx, &kind, &tmdb_id).await.unwrap_or(json!({}));
                let fallback = if kind == "movie" {
                    format!("Movie {tmdb_id}")
                } else {
                    format!("TV Show {tmdb_id}")
                };
                let title = info
                    .get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .unwrap_or(&fallback)
                    .to_string();

                let active = if kind == "movie" {
                    watch_route.as_deref()
                } else {
                    tv_route.as_deref()
                };
                let base = self.base.trim_end_matches('/');

                // A site with a captured route plays the title directly.
                // Without one this used to hand back the site's own *search*
                // url, so pressing play opened a results page instead of the
                // film. A TMDB id is all a player needs, so fall back to an
                // embed player keyed by that id and keep the site's search
                // behind it as a last resort.
                let mut streams = Vec::new();
                match active.filter(|route| route.contains("{id}")) {
                    Some(route) => streams.push(json!({
                        "label": format!("{} \u{b7} watch", self.name),
                        "url": format!("{base}{}", route.replace("{id}", &tmdb_id)),
                        "kind": "page",
                    })),
                    None => {
                        streams.push(json!({
                            "label": format!("{} \u{b7} player", self.name),
                            "url": tmdb_embed_url(&kind, &tmdb_id, media.season, media.episode),
                            "kind": "page",
                        }));
                        streams.push(json!({
                            "label": format!("{} \u{b7} find '{title}'", self.name),
                            "url": format!("{base}/search?q={}", quote_plus(&title)),
                            "kind": "page",
                        }));
                    }
                }

                Ok(json!({
                    "title": title,
                    "overview": info.get("overview").and_then(Value::as_str).unwrap_or(""),
                    "poster": info.get("poster").and_then(Value::as_str).unwrap_or(""),
                    "year": info.get("year").cloned().unwrap_or(Value::Null),
                    "media_type": if kind == "movie" { "movie" } else { "series" },
                    "streams": streams,
                }))
            }

            Engine::Pandora => self.pandora_details(ctx, id).await,

            Engine::JsonApi { watch_path, .. } => {
                self.json_api_details(ctx, watch_path.as_deref(), id).await
            }

            Engine::AnilistFrontend {
                search_path,
                watch_template,
            } => {
                // `anilist:{id}` and `anilist:{id}:{episode}` are both valid:
                // picking an episode has to be expressible, or every episode
                // after the first is unreachable.
                let parts: Vec<&str> = id.trim().split(':').collect();
                let valid = (parts.len() == 2 || parts.len() == 3)
                    && parts[0] == "anilist"
                    && !parts[1].is_empty()
                    && parts[1].chars().all(|c| c.is_ascii_digit());
                if !valid {
                    return Err(ApiError::bad_request(format!(
                        "id must be anilist:{{anilist_id}}[:episode] (from {} search)",
                        self.name
                    )));
                }
                let anilist_id = parts[1];

                // Resolve the title through AniList so the handoff link is
                // precise; a failure downgrades the link, it does not fail.
                // Look the media up *by id*. Searching AniList for the id as
                // if it were a title is what the Python did, and it returned
                // whatever happened to match the digits — so AniList 20
                // (Naruto) came back named after an unrelated show.
                let hit = anilist_by_id(ctx, anilist_id).await;
                let title = hit
                    .as_ref()
                    .and_then(|h| h.get("title").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let title = if title.is_empty() {
                    format!("AniList {anilist_id}")
                } else {
                    title
                };

                // anilist_frontend_plugin uses the site value as given
                let base = self.base.as_str();
                let mut streams = Vec::new();
                if let Some(template) = watch_template {
                    streams.push(json!({
                        "label": format!("{} \u{b7} episode player", self.name),
                        "url": template.replace("{site}", base).replace("{anilist_id}", anilist_id),
                        "kind": "page",
                    }));
                } else {
                    // Without a captured route this offered only the site's
                    // *search* page, so pressing play on an episode opened a
                    // list of search results. An AniList id is all a player
                    // needs, so play it.
                    let episode = id
                        .trim()
                        .split(':')
                        .nth(2)
                        .and_then(|e| e.parse::<u32>().ok())
                        .unwrap_or(1);
                    streams.push(json!({
                        "label": format!("{} \u{b7} episode {episode}", self.name),
                        "url": anime_embed_url(anilist_id, episode, false),
                        "kind": "page",
                    }));
                }
                streams.push(json!({
                    "label": format!("{} \u{b7} find '{title}' on site", self.name),
                    "url": format!("{base}{search_path}{}", quote_plus(&title)),
                    "kind": "page",
                }));

                Ok(json!({
                    "title": title,
                    "overview": hit.as_ref().and_then(|h| h.get("overview").cloned()).unwrap_or(json!("")),
                    "poster": hit.as_ref().and_then(|h| h.get("poster").cloned()).unwrap_or(json!("")),
                    "year": hit.as_ref().and_then(|h| h.get("year").cloned()).unwrap_or(Value::Null),
                    "media_type": "series",
                    "streams": streams,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_becomes_a_title() {
        assert_eq!(
            urldecode_slug("https://x.tv/watch/the-big-show/"),
            "the big show"
        );
        assert_eq!(urldecode_slug("https://x.tv/a%20b"), "a b");
    }

    #[test]
    fn mirrors_share_the_registrable_domain() {
        assert!(same_site("https://www.site.tv/a", "https://site.tv"));
        assert!(same_site("https://site.tv/a", "https://site.tv"));
        assert!(!same_site("https://evil.com/a", "https://site.tv"));
    }

    #[test]
    fn media_ids_parse_like_the_python() {
        assert_eq!(
            parse_media_id("movie:42").unwrap(),
            ("movie".into(), "42".into())
        );
        assert_eq!(parse_media_id("tv:7:2:3").unwrap(), ("tv".into(), "7".into()));
        assert!(parse_media_id("tv:abc").is_err());
        assert!(parse_media_id("nonsense").is_err());
    }
}
