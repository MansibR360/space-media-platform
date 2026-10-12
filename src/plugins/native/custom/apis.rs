//! The converted plugins that talk to a JSON API: KissKH, MyAnimeList (via
//! Jikan), Simkl, NASA, Radio-Browser, IPTV-org, Miruro and AnimePahe.

use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

use crate::error::{ApiError, ApiResult};
use crate::plugins::native::engines::{EngineCtx, quote_plus, slugify, truncate};

fn plus_encode(query: &str) -> String {
    query.replace(' ', "+")
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

/// Leading 4-digit year of a date string.
fn lead_year(text: &str) -> Option<i64> {
    let head: String = text.chars().take(4).collect();
    if head.len() == 4 && head.chars().all(|c| c.is_ascii_digit()) {
        head.parse().ok()
    } else {
        None
    }
}

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

// ------------------------------------------------------------------ KissKH

#[derive(Debug, Clone)]
pub struct KissKh {
    pub base: String,
}

impl KissKh {
    pub fn new() -> Self {
        Self {
            base: env_or("KISSKH_BASE", "https://kisskh.ovh"),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let url = format!(
            "{}/api/DramaList/Search?q={}",
            self.base,
            query.replace(' ', "%20")
        );
        let rows = ctx.get_json(&url).await?;
        let results: Vec<Value> = rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .take(limit.clamp(1, 30))
            .filter_map(|row| {
                let id = row.get("id").and_then(Value::as_i64)?;
                Some(json!({
                    "id": format!("kh:{id}"),
                    "title": row.get("title").and_then(Value::as_str).unwrap_or("Untitled"),
                    "overview": "",
                    "year": Value::Null,
                    "poster": "",
                    "type": "series",
                }))
            })
            .collect();
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        if parts.len() != 2 || parts[0] != "kh" || !digits(parts[1]) {
            return Err(ApiError::bad_request(
                "id must be kh:{drama_id} (from kisskh search)",
            ));
        }
        let drama = ctx
            .get_json(&format!(
                "{}/api/DramaList/Drama/{}?status=0",
                self.base, parts[1]
            ))
            .await?;
        let fallback = format!("KissKH {}", parts[1]);
        let title = drama
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(&fallback);
        let year = drama
            .get("release_date")
            .and_then(Value::as_str)
            .and_then(lead_year);
        let episodes = drama
            .get("episodes_count")
            .and_then(|v| v.as_i64().map(|n| n.to_string()))
            .unwrap_or_else(|| "?".into());

        Ok(json!({
            "title": title,
            "overview": truncate(drama.get("description").and_then(Value::as_str).unwrap_or(""), 500),
            "poster": "",
            "year": year,
            "media_type": "series",
            "streams": [{
                "label": format!("KissKH \u{b7} {episodes} episodes"),
                "url": format!("{}/Drama/{}?id={}", self.base, slugify(title), parts[1]),
                "kind": "page",
            }],
        }))
    }
}

// ------------------------------------------------------------ MyAnimeList

/// Jikan is the community MAL interface. It is rate limited (~3 req/s) and
/// answers 504 under load, so the calls retry with a short backoff.
#[derive(Debug, Clone)]
pub struct MyAnimeList;

const JIKAN: &str = "https://api.jikan.moe/v4";

impl MyAnimeList {
    async fn jikan(&self, ctx: &EngineCtx, path: &str) -> ApiResult<Value> {
        let mut last: Option<ApiError> = None;
        for attempt in 0..3u32 {
            match ctx.get_json(&format!("{JIKAN}{path}")).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let text = err.to_string();
                    let retryable = text.contains("504") || text.contains("429");
                    last = Some(err);
                    if retryable && attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            (1200 * (attempt as u64 + 1)) as u64,
                        ))
                        .await;
                        continue;
                    }
                    break;
                }
            }
        }
        Err(last.unwrap_or_else(|| ApiError::Unavailable("jikan unavailable".into())))
    }

    async fn search_kind(
        &self,
        ctx: &EngineCtx,
        kind: &str,
        query: &str,
        limit: usize,
    ) -> ApiResult<Vec<Value>> {
        let data = self
            .jikan(
                ctx,
                &format!(
                    "/{kind}?q={}&limit={}&sfw=true",
                    plus_encode(query),
                    limit.min(20)
                ),
            )
            .await?;
        let mut out = Vec::new();
        for row in data
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| row.get("title_english").and_then(Value::as_str))
                .unwrap_or("Untitled");
            let mut year = row.get("year").and_then(Value::as_i64);
            if year.is_none() && kind == "manga" {
                year = row.pointer("/published/prop/from/year").and_then(Value::as_i64);
            }
            out.push(json!({
                "id": format!("mal:{kind}:{}", row.get("mal_id").and_then(Value::as_i64).unwrap_or(0)),
                "title": title,
                "overview": truncate(row.get("synopsis").and_then(Value::as_str).unwrap_or(""), 400),
                "year": year,
                "poster": row.pointer("/images/jpg/image_url").and_then(Value::as_str).unwrap_or(""),
                "type": kind,
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let half = (limit.min(20) / 2).max(1);
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for kind in ["anime", "manga"] {
            match self.search_kind(ctx, kind, query, half).await {
                Ok(rows) => results.extend(rows),
                Err(err) => errors.push(format!("{kind}: {err}")),
            }
        }
        results.truncate(limit.max(1));
        let mut payload = json!({ "results": results });
        if !errors.is_empty() && payload["results"].as_array().is_some_and(Vec::is_empty) {
            payload["note"] = json!(format!("partial: {}", errors.join("; ")));
        }
        Ok(payload)
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        let valid = parts.len() == 3
            && parts[0] == "mal"
            && matches!(parts[1], "anime" | "manga")
            && digits(parts[2]);
        if !valid {
            return Err(ApiError::bad_request(
                "id must be mal:{anime|manga}:{mal_id}",
            ));
        }
        let (kind, mal_id) = (parts[1], parts[2]);
        let row = self.jikan(ctx, &format!("/{kind}/{mal_id}")).await?;
        let row = row.get("data").cloned().unwrap_or_else(|| json!({}));

        let fallback_title = format!("MAL {mal_id}");
        let title = row
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| row.get("title_english").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .unwrap_or(&fallback_title);
        let fallback_url = format!("https://myanimelist.net/{kind}/{mal_id}");
        let url = row
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .unwrap_or(&fallback_url);

        Ok(json!({
            "title": title,
            "overview": truncate(row.get("synopsis").and_then(Value::as_str).unwrap_or(""), 600),
            "poster": row.pointer("/images/jpg/image_url").and_then(Value::as_str).unwrap_or(""),
            "year": row.get("year").and_then(Value::as_i64),
            "media_type": "series",
            "streams": [{
                "label": "MyAnimeList \u{b7} info page",
                "url": url,
                "kind": "page",
            }],
        }))
    }
}

// ------------------------------------------------------------------- Simkl

#[derive(Debug, Clone)]
pub struct Simkl {
    pub client_id: Option<String>,
}

const SIMKL_TYPES: [(&str, &str); 3] = [("movies", "movie"), ("tv", "series"), ("anime", "manga")];

impl Simkl {
    pub fn new() -> Self {
        Self {
            client_id: std::env::var("SIMKL_CLIENT_ID")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let Some(client_id) = self.client_id.as_deref() else {
            return Ok(json!({
                "results": [],
                "note": "set SIMKL_CLIENT_ID (register an app at simkl.com/settings/apps/new)",
            }));
        };
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let per = (limit.min(20) / SIMKL_TYPES.len()) + 1;
        let per = per.max(1);
        let mut out: Vec<Value> = Vec::new();

        for (api_type, space_type) in SIMKL_TYPES {
            let url = format!(
                "https://api.simkl.com/search/{api_type}?q={}&client_id={client_id}",
                query.replace(' ', "%20")
            );
            let Ok(rows) = ctx.get_json(&url).await else {
                continue;
            };
            let Some(rows) = rows.as_array() else { continue };
            for row in rows.iter().take(per) {
                let simkl_id = row
                    .get("simkl_id")
                    .and_then(Value::as_i64)
                    .or_else(|| row.pointer("/ids/simkl").and_then(Value::as_i64));
                let Some(simkl_id) = simkl_id else { continue };

                let title = row
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| row.get("name").and_then(Value::as_str))
                    .filter(|t| !t.is_empty())
                    .or_else(|| row.pointer("/movie/title").and_then(Value::as_str))
                    .unwrap_or("Untitled");
                let year = row
                    .get("year")
                    .and_then(Value::as_i64)
                    .or_else(|| row.pointer("/movie/year").and_then(Value::as_i64));
                let poster = row.get("poster").and_then(Value::as_str).unwrap_or("");
                let poster = if !poster.is_empty() && !poster.starts_with("http") {
                    format!("https://simkl.in/posters/{poster}_m.jpg")
                } else {
                    poster.to_string()
                };

                out.push(json!({
                    "id": format!("simkl:{api_type}:{simkl_id}"),
                    "title": title,
                    "overview": "",
                    "year": year,
                    "poster": poster,
                    "type": space_type,
                }));
                if out.len() >= limit {
                    return Ok(json!({ "results": out }));
                }
            }
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, _ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        let valid = parts.len() == 3
            && parts[0] == "simkl"
            && matches!(parts[1], "movies" | "tv" | "anime")
            && digits(parts[2]);
        if !valid {
            return Err(ApiError::bad_request(
                "id must be simkl:{movies|tv|anime}:{simkl_id}",
            ));
        }
        let (api_type, simkl_id) = (parts[1], parts[2]);
        Ok(json!({
            "title": format!("Simkl {simkl_id}"),
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": if api_type == "movies" { "movie" } else { "series" },
            "streams": [{
                "label": "Simkl \u{b7} app page",
                "url": format!("https://simkl.com/{api_type}/{simkl_id}"),
                "kind": "page",
            }],
        }))
    }
}

// -------------------------------------------------------------------- NASA

#[derive(Debug, Clone)]
pub struct Nasa;

const NASA_API: &str = "https://images-api.nasa.gov";

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Nasa {
    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let limit = limit.clamp(1, 30);
        let query = query.trim();
        let query = if query.is_empty() { "space" } else { query };
        let data = ctx
            .get_json(&format!(
                "{NASA_API}/search?q={}&media_type=video&page_size={limit}",
                query.replace(' ', "%20")
            ))
            .await?;

        let items = data
            .pointer("/collection/items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for item in items.iter().take(limit) {
            let row = item.pointer("/data/0").cloned().unwrap_or_else(|| json!({}));
            let (Some(nasa_id), Some(title)) = (
                row.get("nasa_id").and_then(Value::as_str),
                row.get("title").and_then(Value::as_str),
            ) else {
                continue;
            };
            out.push(json!({
                "id": format!("nasa:{nasa_id}"),
                "title": title,
                "overview": truncate(&collapse_whitespace(row.get("description").and_then(Value::as_str).unwrap_or("")), 350),
                "year": lead_year(row.get("date_created").and_then(Value::as_str).unwrap_or("")),
                "poster": item.pointer("/links/0/href").and_then(Value::as_str).unwrap_or(""),
                "type": "movie",
            }));
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let Some(nasa_id) = id.strip_prefix("nasa:") else {
            return Err(ApiError::bad_request("id must be nasa:{nasa_id}"));
        };
        let collection = ctx
            .get_json(&format!("{NASA_API}/asset/{nasa_id}"))
            .await?;
        let mp4s: Vec<String> = collection
            .pointer("/collection/items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.get("href").and_then(Value::as_str).map(str::to_string))
            .filter(|href| href.to_lowercase().ends_with(".mp4"))
            .collect();
        // The smallest rendition is normally last, and is the one that plays
        // without stalling.
        let Some(chosen) = mp4s.last().cloned() else {
            return Err(ApiError::Unavailable("no mp4 asset found".into()));
        };

        let mut title = format!("NASA {nasa_id}");
        let mut overview = String::new();
        let mut year = None;
        if let Ok(meta) = ctx
            .get_json(&format!("{NASA_API}/search?nasa_id={nasa_id}"))
            .await
            && let Some(row) = meta.pointer("/collection/items/0/data/0")
        {
            if let Some(found) = row.get("title").and_then(Value::as_str) {
                title = found.to_string();
            }
            overview = truncate(
                &collapse_whitespace(row.get("description").and_then(Value::as_str).unwrap_or("")),
                500,
            );
            year = lead_year(row.get("date_created").and_then(Value::as_str).unwrap_or(""));
        }

        Ok(json!({
            "title": title,
            "overview": overview,
            "poster": "",
            "year": year,
            "media_type": "movie",
            "streams": [{
                "label": "NASA \u{b7} direct video",
                "url": chosen,
                "kind": "mp4",
            }],
        }))
    }
}

// ----------------------------------------------------------- Radio-Browser

#[derive(Debug, Clone)]
pub struct RadioBrowser {
    pub base: String,
}

impl RadioBrowser {
    pub fn new() -> Self {
        Self {
            base: env_or("RADIOBROWSER_BASE", "https://de1.api.radio-browser.info"),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let limit = limit.clamp(1, 40);
        let mut path = format!(
            "/json/stations/search?limit={limit}&hidebroken=true&order=votes&reverse=true"
        );
        if !query.trim().is_empty() {
            path.push_str(&format!("&name={}", plus_encode(query.trim())));
        }
        let rows = ctx.get_json(&format!("{}{path}", self.base)).await?;

        let mut out = Vec::new();
        for row in rows.as_array().cloned().unwrap_or_default() {
            let uuid = row.get("stationuuid").and_then(Value::as_str).unwrap_or("");
            let url = row
                .get("url_resolved")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
                .or_else(|| row.get("url").and_then(Value::as_str))
                .unwrap_or("");
            let name = row.get("name").and_then(Value::as_str).unwrap_or("").trim();
            if uuid.is_empty() || url.is_empty() || name.is_empty() {
                continue;
            }
            let tags: String = row
                .get("tags")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split(',')
                .take(3)
                .collect::<Vec<_>>()
                .join(",");
            let country = row.get("country").and_then(Value::as_str).unwrap_or("");
            out.push(json!({
                "id": format!("radio:{uuid}"),
                "title": name,
                "overview": format!("{country} \u{b7} {tags}").trim_matches(|c| c == ' ' || c == '\u{b7}').to_string(),
                "year": Value::Null,
                "poster": "",
                "type": "music",
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let Some(uuid) = id.strip_prefix("radio:") else {
            return Err(ApiError::bad_request("id must be radio:{stationuuid}"));
        };
        let rows = ctx
            .get_json(&format!("{}/json/stations/byuuid?uuids={uuid}", self.base))
            .await?;
        let Some(station) = rows.as_array().and_then(|r| r.first()).cloned() else {
            return Err(ApiError::Unavailable(
                "station not found (or offline)".into(),
            ));
        };
        let url = station
            .get("url_resolved")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .or_else(|| station.get("url").and_then(Value::as_str))
            .unwrap_or("");
        if url.is_empty() {
            return Err(ApiError::Unavailable("station has no stream url".into()));
        }

        let codec = station
            .get("codec")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let bitrate = station.get("bitrate").and_then(Value::as_i64);
        let parts: Vec<String> = [
            if codec.is_empty() {
                "audio".to_string()
            } else {
                codec
            },
            bitrate.map(|b| format!("{b}kbps")).unwrap_or_default(),
            station
                .get("country")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
        let label = parts.join(" \u{b7} ");

        let name = station.get("name").and_then(Value::as_str).unwrap_or("").trim();
        Ok(json!({
            "title": if name.is_empty() { "Radio" } else { name },
            "overview": station.get("tags").and_then(Value::as_str).unwrap_or(""),
            "poster": "",
            "year": Value::Null,
            "media_type": "music",
            "streams": [{
                "label": if label.is_empty() { "live audio".to_string() } else { label },
                "url": url,
                "kind": "audio",
            }],
        }))
    }
}

// -------------------------------------------------------------------- IPTV

/// The iptv-org catalog is ~14 MB across two files, so it is fetched once and
/// held for the life of the process, exactly as the Python cached it.
#[derive(Clone)]
pub struct Iptv {
    pub base: String,
    cache: Arc<OnceCell<IptvCatalog>>,
    loading: Arc<Mutex<()>>,
}

struct IptvCatalog {
    channels: std::collections::HashMap<String, ChannelInfo>,
    streams: Vec<Value>,
}

struct ChannelInfo {
    name: String,
    country: String,
    categories: Vec<String>,
}

impl Iptv {
    pub fn new() -> Self {
        Self {
            base: env_or("IPTV_API_BASE", "https://iptv-org.github.io/api"),
            cache: Arc::new(OnceCell::new()),
            loading: Arc::new(Mutex::new(())),
        }
    }

    async fn catalog(&self, ctx: &EngineCtx) -> ApiResult<&IptvCatalog> {
        if let Some(loaded) = self.cache.get() {
            return Ok(loaded);
        }
        let _guard = self.loading.lock().await;
        if let Some(loaded) = self.cache.get() {
            return Ok(loaded);
        }

        let channels_raw = ctx.get_json(&format!("{}/channels.json", self.base)).await?;
        let categories = ctx
            .get_json(&format!("{}/categories.json", self.base))
            .await
            .ok();
        let category_names: std::collections::HashMap<String, String> = categories
            .and_then(|c| c.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|c| {
                Some((
                    c.get("id").and_then(Value::as_str)?.to_string(),
                    c.get("name").and_then(Value::as_str)?.to_string(),
                ))
            })
            .collect();

        let mut channels = std::collections::HashMap::new();
        for channel in channels_raw.as_array().cloned().unwrap_or_default() {
            let Some(id) = channel.get("id").and_then(Value::as_str) else {
                continue;
            };
            let cats = channel
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split(';')
                .filter(|c| !c.is_empty())
                .map(|c| category_names.get(c).cloned().unwrap_or_else(|| c.to_string()))
                .collect();
            channels.insert(
                id.to_string(),
                ChannelInfo {
                    name: channel
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                        .to_string(),
                    country: channel
                        .get("country")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    categories: cats,
                },
            );
        }

        let streams: Vec<Value> = ctx
            .get_json(&format!("{}/streams.json", self.base))
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.get("url").and_then(Value::as_str).is_some_and(|u| !u.is_empty()))
            .collect();

        let _ = self.cache.set(IptvCatalog { channels, streams });
        self.cache
            .get()
            .ok_or_else(|| ApiError::Unavailable("iptv catalog unavailable".into()))
    }

    fn classify(url: &str) -> &'static str {
        let low = url.to_lowercase();
        let low = low.split('?').next().unwrap_or("");
        if low.contains("m3u8") {
            return "hls";
        }
        if low.ends_with(".mp4") || low.ends_with(".webm") || low.ends_with(".mov") {
            return "mp4";
        }
        if low.ends_with(".mp3") || low.ends_with(".aac") || low.ends_with(".ogg") {
            return "audio";
        }
        "page"
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let catalog = self.catalog(ctx).await?;
        let needle = query.trim().to_lowercase();
        let cap = limit.clamp(1, 40);
        let mut out = Vec::new();

        for stream in &catalog.streams {
            let info = stream
                .get("channel")
                .and_then(Value::as_str)
                .and_then(|c| catalog.channels.get(c));
            let name = stream
                .get("title")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .or_else(|| info.map(|i| i.name.clone()))
                .unwrap_or_default();
            if name.is_empty() || (!needle.is_empty() && !name.to_lowercase().contains(&needle)) {
                continue;
            }
            let quality = stream.get("quality").and_then(Value::as_str).unwrap_or("");
            let mut overview_bits: Vec<String> = Vec::new();
            if let Some(info) = info {
                if !info.country.is_empty() {
                    overview_bits.push(info.country.clone());
                }
                overview_bits.extend(info.categories.iter().take(3).cloned());
            }
            out.push(json!({
                "id": stream.get("url").and_then(Value::as_str).unwrap_or(""),
                "title": if quality.is_empty() { name } else { format!("{name} \u{b7} {quality}") },
                "overview": overview_bits.join(" \u{b7} "),
                "year": Value::Null,
                "poster": "",
                "type": "series",
            }));
            if out.len() >= cap {
                break;
            }
        }

        if out.is_empty() && needle.is_empty() {
            return Ok(json!({ "results": [], "note": "catalog loaded but no streams matched" }));
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        if !(id.starts_with("http://") || id.starts_with("https://")) {
            return Err(ApiError::bad_request("id must be a stream url from search"));
        }
        let catalog = self.catalog(ctx).await?;
        let name = catalog
            .streams
            .iter()
            .find(|s| s.get("url").and_then(Value::as_str) == Some(id))
            .and_then(|s| {
                s.get("title")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        s.get("channel")
                            .and_then(Value::as_str)
                            .and_then(|c| catalog.channels.get(c))
                            .map(|i| i.name.clone())
                    })
            })
            .unwrap_or_default();

        let fallback: String = id.rsplit('/').next().unwrap_or(id).chars().take(60).collect();
        Ok(json!({
            "title": if name.is_empty() { fallback } else { name },
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "series",
            "streams": [{ "label": "live", "url": id, "kind": Self::classify(id) }],
        }))
    }
}

// ------------------------------------------------------------------ Miruro

#[derive(Debug, Clone)]
pub struct Miruro {
    pub api: String,
    pub category: String,
}

const MIRURO_SITE: &str = "https://www.miruro.com";
const MIRURO_PROVIDERS: [&str; 5] = ["kiwi", "arc", "zoro", "pewe", "jet"];

impl Miruro {
    pub fn new() -> Self {
        Self {
            api: env_or("MIRURO_API_BASE", "https://miruro.adgstudios.co.za"),
            category: std::env::var("MIRURO_CATEGORY")
                .unwrap_or_else(|_| "sub".into())
                .to_lowercase(),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        // The API names these `query` and `perPage`; `q`/`per_page` are
        // rejected outright with a 422, which is what left this source dead.
        let data = ctx
            .get_json(&format!(
                "{}/search?query={}&perPage={}",
                self.api,
                plus_encode(query),
                limit.min(30)
            ))
            .await?;
        let rows = data
            .get("results")
            .or_else(|| data.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::new();
        for row in rows {
            let anilist_id = row
                .get("id")
                .and_then(Value::as_i64)
                .or_else(|| row.get("anilistId").and_then(Value::as_i64));
            let Some(anilist_id) = anilist_id else { continue };

            let title = row
                .pointer("/title/romaji")
                .and_then(Value::as_str)
                .or_else(|| row.pointer("/title/english").and_then(Value::as_str))
                .or_else(|| row.get("name").and_then(Value::as_str))
                .unwrap_or("Untitled");
            let description = row.get("description").and_then(Value::as_str).unwrap_or("");
            out.push(json!({
                "id": format!("anilist:{anilist_id}"),
                "title": title,
                "overview": truncate(&strip_html(description), 400),
                "year": row.get("seasonYear").and_then(Value::as_i64)
                    .or_else(|| row.get("year").and_then(Value::as_i64)),
                "poster": row.pointer("/coverImage/large").and_then(Value::as_str).unwrap_or(""),
                "type": "series",
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    /// (episode slug, provider, episode count) for the requested episode,
    /// falling back to the first episode a preferred provider offers.
    fn pick_episode(&self, providers: &Value, wanted: u32) -> (Option<String>, String, usize) {
        for name in MIRURO_PROVIDERS {
            let Some(provider) = providers.get(name) else {
                continue;
            };
            for category in [self.category.as_str(), "sub", "dub"] {
                let Some(list) = provider
                    .pointer(&format!("/episodes/{category}"))
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                if list.is_empty() {
                    continue;
                }
                for episode in list {
                    let number = episode.get("number").and_then(Value::as_f64);
                    if number.is_some_and(|n| n as u32 == wanted)
                        && let Some(id) = episode.get("id").and_then(Value::as_str)
                    {
                        return (Some(id.to_string()), name.to_string(), list.len());
                    }
                }
                if let Some(id) = list[0].get("id").and_then(Value::as_str) {
                    return (Some(id.to_string()), name.to_string(), list.len());
                }
            }
        }
        (None, String::new(), 0)
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        if parts.len() < 2 || parts[0] != "anilist" || !digits(parts[1]) {
            return Err(ApiError::bad_request(
                "id must be anilist:{anilist_id}[:episode]",
            ));
        }
        let anilist_id = parts[1];
        let episode: u32 = parts
            .get(2)
            .filter(|p| digits(p))
            .and_then(|p| p.parse().ok())
            .unwrap_or(1);

        let mut streams = vec![json!({
            "label": "Miruro \u{b7} watch page",
            "url": format!("{MIRURO_SITE}/watch?id={anilist_id}"),
            "kind": "page",
        })];

        let (slug, provider, total) = match ctx
            .get_json(&format!("{}/episodes/{anilist_id}", self.api))
            .await
        {
            Ok(payload) => {
                let providers = payload.get("providers").cloned().unwrap_or_else(|| json!({}));
                self.pick_episode(&providers, episode)
            }
            Err(_) => (None, String::new(), 0),
        };

        if let Some(slug) = slug
            && let Ok(sources) = ctx.get_json(&format!("{}/{slug}", self.api)).await
        {
            for source in sources
                .get("streams")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .take(6)
            {
                let Some(url) = source.get("url").and_then(Value::as_str) else {
                    continue;
                };
                let kind = if source.get("type").and_then(Value::as_str) == Some("hls")
                    || url.contains(".m3u8")
                {
                    "hls"
                } else {
                    "page"
                };
                let quality = source
                    .get("quality")
                    .and_then(Value::as_str)
                    .unwrap_or(kind);
                streams.insert(
                    0,
                    json!({
                        "label": format!("Miruro \u{b7} {provider} \u{b7} {quality}"),
                        "url": url,
                        "kind": kind,
                    }),
                );
            }
        }

        let note = if total > 0 {
            format!("episode {episode} of ~{total}")
        } else {
            format!("episode {episode}")
        };
        Ok(json!({
            "title": format!("AniList {anilist_id}"),
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "series",
            "note": note,
            "streams": streams,
        }))
    }
}

// --------------------------------------------------------------- AnimePahe

#[derive(Debug, Clone)]
pub struct AnimePahe {
    pub base: String,
}

impl AnimePahe {
    pub fn new() -> Self {
        Self {
            base: env_or("ANIMEPAHE_BASE", "https://animepahe.pw"),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let data = ctx
            .get_json(&format!(
                "{}/api?m=search&q={}",
                self.base,
                plus_encode(query)
            ))
            .await?;
        let mut out = Vec::new();
        for row in data
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let (Some(anime_id), Some(slug)) = (
                row.get("id").and_then(Value::as_i64),
                row.get("slug").and_then(Value::as_str),
            ) else {
                continue;
            };
            let title = row
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| row.get("title").and_then(Value::as_str))
                .or_else(|| row.get("ja").and_then(Value::as_str));
            let Some(title) = title.filter(|t| !t.is_empty()) else {
                continue;
            };
            out.push(json!({
                "id": format!("pahe:{anime_id}:{slug}"),
                "title": title,
                "overview": "",
                "year": Value::Null,
                "poster": row.get("poster").and_then(Value::as_str).unwrap_or(""),
                "type": "series",
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    /// Walks the release feed for the requested episode, bounded to 5 pages
    /// like the Python.
    async fn first_session(
        &self,
        ctx: &EngineCtx,
        anime_id: &str,
        wanted: u32,
    ) -> (Option<String>, Option<u32>) {
        for page in 1..=5 {
            let Ok(data) = ctx
                .get_json(&format!(
                    "{}/api?m=release&id={anime_id}&sort=episode_asc&page={page}",
                    self.base
                ))
                .await
            else {
                return (None, None);
            };
            let episodes = data
                .pointer("/data/episodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if episodes.is_empty() {
                return (None, None);
            }
            for episode in &episodes {
                let Some(session) = episode.get("session").and_then(Value::as_str) else {
                    continue;
                };
                let number = episode
                    .get("episode")
                    .and_then(Value::as_f64)
                    .or_else(|| episode.get("number").and_then(Value::as_f64));
                if wanted > 0 {
                    match number {
                        Some(n) if n as u32 == wanted => {
                            return (Some(session.to_string()), Some(n as u32));
                        }
                        // keep scanning for a numbered match
                        None => continue,
                        _ => {}
                    }
                }
                if wanted == 0 {
                    return (Some(session.to_string()), number.map(|n| n as u32));
                }
            }
            if data.pointer("/data/next_page_url").is_none_or(Value::is_null) {
                return (None, None);
            }
        }
        (None, None)
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        if parts.len() < 3 || parts[0] != "pahe" || !digits(parts[1]) {
            return Err(ApiError::bad_request(
                "id must be pahe:{anime_id}:{slug}[:episode]",
            ));
        }
        let (anime_id, slug) = (parts[1], parts[2]);
        let episode: u32 = parts
            .get(3)
            .filter(|p| digits(p))
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);

        let anime_page = format!("{}/anime/{slug}", self.base);
        let mut streams = vec![json!({
            "label": "AnimePahe \u{b7} all episodes",
            "url": anime_page,
            "kind": "page",
        })];

        let title = match ctx.get_text(&anime_page).await {
            Ok(html) => html_title(&html)
                .map(|t| t.replace(" - AnimePahe", "").trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("AnimePahe {slug}")),
            Err(_) => format!("AnimePahe {slug}"),
        };

        let (session, resolved) = self.first_session(ctx, anime_id, episode).await;
        if let Some(session) = session {
            let number = resolved.unwrap_or(if episode > 0 { episode } else { 1 });
            streams.insert(
                0,
                json!({
                    "label": format!("AnimePahe \u{b7} play E{number}"),
                    "url": format!("{}/play/{anime_id}/{session}", self.base),
                    "kind": "page",
                }),
            );
        }

        Ok(json!({
            "title": title,
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "series",
            "streams": streams,
        }))
    }
}

// ------------------------------------------------------------------ shared

pub fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Contents of the page's `<title>`, whitespace collapsed.
pub fn html_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title")?;
    let open = lower[start..].find('>')? + start + 1;
    let end = lower[open..].find("</title>")? + open;
    Some(collapse_whitespace(&html[open..end]))
}

pub fn urlencode(value: &str) -> String {
    quote_plus(value)
}
