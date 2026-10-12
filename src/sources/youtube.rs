//! YouTube / YouTube Music source — the Metrolist approach, server-side, and
//! engineered to survive upstream breakage ("bulletproof mode").
//!
//! Discovery mirrors metrolistgroup/metrolist: the public InnerTube
//! (`WEB_REMIX`) endpoint powers YouTube Music search, with Piped/Invidious
//! as alternates. Streams come from whatever layer answers first.
//!
//! ## Defense in depth — every layer can fail without user impact
//!
//! | # | Strategy | Breaks when |
//! |---|----------|-------------|
//! | 0 | InnerTube player/search (WEB_REMIX + ANDROID clients) | YT changes InnerTube |
//! | 1 | Piped instances (rotating pool, health-cached) | every Piped node dies |
//! | 2 | Invidious instances (rotating pool, health-cached) | every Invidious node dies |
//! | 3 | Built-in scraper — YouTube's own HTML (`ytInitialPlayerResponse` / `ytInitialData`) | YT blocks plain HTTP fetches |
//! | 4 | Embed fallback (`kind:"page"`, browser iframe player) | effectively never |
//!
//! Self-healing:
//! - Instance pools grow from the community registries whenever nodes fail
//!   or hourly from the resilience loop — never pinned to hardcoded hosts.
//! - The background loop rotates pools, clears caches, flips a degradation
//!   gauge and broadcasts `youtube.degraded` / `youtube.recovered` events.
//! - Successful resolutions are cached ~20 min so transient flaps never
//!   reach users; `item()` cannot hard-fail thanks to the embed guarantee.

use std::sync::{OnceLock, atomic::{AtomicU64, AtomicU8, Ordering}};
use std::time::{Duration, Instant};

use super::{NativeSource, SourceCapabilities, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::RwLock;

/// Seed lists — extended automatically from community registries.
const PIPED_INSTANCES: &[&str] = &[
    "https://pipedapi.kavin.rocks",
    "https://api.piped.private.coffee",
    "https://pipedapi.adminforge.de",
    "https://pipedapi.reallyaweso.me",
    "https://pipedapi.drgns.space",
];

const INVIDIOUS_INSTANCES: &[&str] = &[
    "https://yewtu.be",
    "https://inv.nadeko.net",
    "https://invidious.nerdvpn.de",
    "https://invidious.jing.rocks",
];

const INNERTUBE_MUSIC_SEARCH: &str =
    "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
const INNERTUBE_PLAYER: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";

const STREAM_CACHE_TTL: Duration = Duration::from_secs(20 * 60);
const STREAM_CACHE_MAX: usize = 2_000;
const POOL_CAP: usize = 14;

/// Strategy identifiers — index-aligned with [`Stats`] counters.
pub const STRATEGY_NAMES: [&str; 5] = ["innertube", "piped", "invidious", "scrape", "embed"];

#[derive(Default)]
pub struct Youtube {
    piped_pool: OnceLock<RwLock<Vec<String>>>,
    invidious_pool: OnceLock<RwLock<Vec<String>>>,
    active_piped: OnceLock<RwLock<Option<String>>>,
    active_invidious: OnceLock<RwLock<Option<String>>>,
    stream_cache: OnceLock<dashmap::DashMap<String, (Instant, Vec<StreamLink>)>>,
    pub stats: Stats,
}

/// Per-strategy telemetry surfaced on `/api/admin/youtube/status`.
#[derive(Default)]
pub struct Stats {
    pub ok: [AtomicU64; 5],
    pub fail: [AtomicU64; 5],
    /// rolling count used by the resilience loop to trigger healing
    pub consecutive_failures: AtomicU8,
    pub last_error: RwLock<Option<String>>,
    pub healed_count: AtomicU64,
}

impl Stats {
    fn record(&self, strategy: usize, success: bool) {
        if success {
            self.ok[strategy].fetch_add(1, Ordering::Relaxed);
            self.consecutive_failures.store(0, Ordering::Relaxed);
        } else {
            self.fail[strategy].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> Value {
        let strategies: Vec<Value> = STRATEGY_NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                json!({
                    "id": i,
                    "name": name,
                    "ok": self.ok[i].load(Ordering::Relaxed),
                    "fail": self.fail[i].load(Ordering::Relaxed),
                })
            })
            .collect();
        json!({
            "strategies": strategies,
            "consecutive_failures": self.consecutive_failures.load(Ordering::Relaxed),
            "last_error": self.last_error.try_read().ok().and_then(|g| g.clone()),
            "healed_count": self.healed_count.load(Ordering::Relaxed),
        })
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/124.0 Safari/537.36 space-media-server"
        ))
        .build()
        .expect("reqwest client")
}

impl Youtube {
    pub fn new() -> Self {
        Self::default()
    }

    fn piped_pool(&self) -> &RwLock<Vec<String>> {
        self.piped_pool
            .get_or_init(|| RwLock::new(PIPED_INSTANCES.iter().map(|s| s.to_string()).collect()))
    }

    fn invidious_pool(&self) -> &RwLock<Vec<String>> {
        self.invidious_pool.get_or_init(|| {
            RwLock::new(INVIDIOUS_INSTANCES.iter().map(|s| s.to_string()).collect())
        })
    }

    fn active_piped(&self) -> &RwLock<Option<String>> {
        self.active_piped.get_or_init(|| RwLock::new(None))
    }

    fn active_invidious(&self) -> &RwLock<Option<String>> {
        self.active_invidious.get_or_init(|| RwLock::new(None))
    }

    fn stream_cache(&self) -> &dashmap::DashMap<String, (Instant, Vec<StreamLink>)> {
        self.stream_cache.get_or_init(dashmap::DashMap::new)
    }

    // ------------------------------------------------------------ pools

    /// Active node first, then the rest of the pool (rotation under failure).
    async fn candidates(
        &self,
        pool: &RwLock<Vec<String>>,
        active: &RwLock<Option<String>>,
    ) -> Vec<String> {
        let list = pool.read().await.clone();
        let current = active.read().await.clone();
        let mut ordered = Vec::with_capacity(list.len());
        if let Some(cur) = &current
            && list.contains(cur)
        {
            ordered.push(cur.clone());
        }
        for item in list {
            if Some(&item) != current.as_ref() && !ordered.contains(&item) {
                ordered.push(item);
            }
        }
        ordered
    }

    async fn remember_active(&self, active: &RwLock<Option<String>>, base: &str) {
        *active.write().await = Some(base.to_string());
    }

    /// Merges fresh community-registry instances into runtime pools.
    pub async fn refresh_instance_registries(&self) {
        let http = client();

        // Invidious: official health-sorted registry.
        if let Ok(resp) = http
            .get("https://api.invidious.io/instances.json?sort_by=type,health")
            .send()
            .await
            && let Ok(body) = resp.json::<Value>().await
        {
            let mut found = Vec::new();
            for row in body.as_array().into_iter().flatten() {
                let Some(entry) = row.as_array() else { continue };
                let meta = entry.get(1);
                let uri = meta
                    .and_then(|m| m.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let api_ok = meta
                    .and_then(|m| m.get("api"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if api_ok && uri.starts_with("https://") {
                    found.push(uri.trim_end_matches('/').to_string());
                }
                if found.len() >= 10 {
                    break;
                }
            }
            if !found.is_empty() {
                let mut pool = self.invidious_pool().write().await;
                for uri in found {
                    if !pool.contains(&uri) {
                        pool.push(uri);
                    }
                }
                pool.truncate(POOL_CAP);
            }
        }

        // Piped: community JSON index.
        if let Ok(resp) = http.get("https://piped-instances.kavin.rocks/").send().await
            && let Ok(body) = resp.json::<Value>().await
        {
            let mut found = Vec::new();
            for row in body.as_array().into_iter().flatten() {
                let api = row.get("api_url").and_then(Value::as_str).unwrap_or("");
                if api.starts_with("https://") {
                    found.push(api.trim_end_matches('/').to_string());
                }
                if found.len() >= 10 {
                    break;
                }
            }
            if !found.is_empty() {
                let mut pool = self.piped_pool().write().await;
                for uri in found {
                    if !pool.contains(&uri) {
                        pool.push(uri);
                    }
                }
                pool.truncate(POOL_CAP);
            }
        }
    }

    /// Full heal: refresh registries, forget stuck actives, drop caches.
    pub async fn heal(&self) -> Value {
        self.refresh_instance_registries().await;
        *self.active_piped().write().await = None;
        *self.active_invidious().write().await = None;
        let dropped = self.stream_cache().len();
        self.stream_cache().clear();
        self.stats.healed_count.fetch_add(1, Ordering::Relaxed);
        json!({
            "healed": true,
            "stream_cache_dropped": dropped,
            "piped_pool": self.piped_pool().read().await.len(),
            "invidious_pool": self.invidious_pool().read().await.len(),
        })
    }

    // ------------------------------------------------------------ shared

    async fn get_json(&self, url: &str) -> Option<Value> {
        let resp = client()
            .get(url)
            .header("accept", "application/json")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json().await.ok()
    }

    // ------------------------------------------------------------ search

    async fn innertube_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let body = json!({
            "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": "1.20240401.01.00", "hl": "en", "gl": "US"}},
            "query": query,
        });
        let response: Value = client()
            .post(INNERTUBE_MUSIC_SEARCH)
            .json(&body)
            .header("content-type", "application/json")
            .header("origin", "https://music.youtube.com")
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("innertube unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let mut renderers = Vec::new();
        collect_key_objects(&response, "musicResponsiveRenderer", &mut renderers);

        let items: Vec<SourceItem> = renderers
            .iter()
            .filter_map(responsive_to_item)
            .take(limit.clamp(1, 50) as usize)
            .collect();
        if items.is_empty() {
            return Err(ApiError::Unavailable("innertube returned no items".into()));
        }
        Ok(items)
    }

    async fn piped_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        for base in self.candidates(self.piped_pool(), self.active_piped()).await {
            if let Some(body) = self
                .get_json(&format!(
                    "{base}/search?q={}&filter=music_songs",
                    urlencode(query)
                ))
                .await
            {
                let items = parse_piped_search(&body, limit);
                if !items.is_empty() {
                    self.remember_active(self.active_piped(), &base).await;
                    return Ok(items);
                }
            }
        }
        Err(ApiError::Unavailable("all Piped instances failed".into()))
    }

    async fn invidious_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        for base in self.candidates(self.invidious_pool(), self.active_invidious()).await {
            if let Some(body) = self
                .get_json(&format!(
                    "{base}/api/v1/search?type=video&q={}",
                    urlencode(query)
                ))
                .await
            {
                let items = parse_invidious_search(&body, limit);
                if !items.is_empty() {
                    self.remember_active(self.active_invidious(), &base).await;
                    return Ok(items);
                }
            }
        }
        Err(ApiError::Unavailable("all Invidious instances failed".into()))
    }

    /// Built-in scraper measure: read YouTube's own results page HTML and
    /// pull `videoRenderer` blobs from `ytInitialData`. No third-party API
    /// involved — works as long as plain HTTP GETs work.
    async fn scrape_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let html = fetch_html(&format!(
            "https://www.youtube.com/results?search_query={}&hl=en&sp=EgIQAQ%253D%253D",
            urlencode(query)
        ))
        .await?;
        let initial = extract_json_blob(&html, "ytInitialData")
            .ok_or_else(|| ApiError::Unavailable("scrape: ytInitialData not found".into()))?;

        let mut renderers = Vec::new();
        collect_key_objects(&Value::Object(initial), "videoRenderer", &mut renderers);

        let items: Vec<SourceItem> = renderers
            .iter()
            .filter_map(video_renderer_to_item)
            .take(limit.clamp(1, 50) as usize)
            .collect();
        if items.is_empty() {
            return Err(ApiError::Unavailable("scrape returned no items".into()));
        }
        Ok(items)
    }

    // ------------------------------------------------------------ streams

    /// Strategy 0: InnerTube player probe (ANDROID client) — many responses
    /// carry unencrypted progressive URLs; ciphered entries are skipped.
    async fn innertube_player_streams(
        &self,
        video_id: &str,
    ) -> ApiResult<(Value, Vec<StreamLink>)> {
        let body = json!({
            "context": {"client": {"clientName": "ANDROID", "clientVersion": "19.09.37", "androidSdkVersion": 30, "hl": "en"}},
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let response: Value = client()
            .post(INNERTUBE_PLAYER)
            .json(&body)
            .header("content-type", "application/json")
            .header("user-agent", "com.google.android.youtube/19.09.37 (Linux; U; Android 11)")
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("innertube player unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let playable = response
            .pointer("/playabilityStatus/status")
            .and_then(Value::as_str)
            == Some("OK");
        let streams = streams_from_player(&response)?;
        if !playable || streams.is_empty() {
            return Err(ApiError::Unavailable("innertube player: no open streams".into()));
        }
        Ok((response, streams))
    }

    async fn piped_streams(&self, video_id: &str) -> ApiResult<(Value, Vec<StreamLink>)> {
        for base in self.candidates(self.piped_pool(), self.active_piped()).await {
            if let Some(body) = self.get_json(&format!("{base}/streams/{video_id}")).await {
                if let Ok(links) = streams_from_payload(&body) {
                    self.remember_active(self.active_piped(), &base).await;
                    return Ok((body, links));
                }
            }
        }
        Err(ApiError::Unavailable("all Piped instances failed".into()))
    }

    async fn invidious_streams(&self, video_id: &str) -> ApiResult<(Value, Vec<StreamLink>)> {
        for base in self
            .candidates(self.invidious_pool(), self.active_invidious())
            .await
        {
            if let Some(body) = self.get_json(&format!("{base}/api/v1/videos/{video_id}")).await {
                if let Ok(links) = invidious_streams_from_payload(&body) {
                    self.remember_active(self.active_invidious(), &base).await;
                    return Ok((body, links));
                }
            }
        }
        Err(ApiError::Unavailable("all Invidious instances failed".into()))
    }

    /// Strategy 3: built-in scraper reads the watch page like a browser and
    /// extracts `ytInitialPlayerResponse` (title, author, open formats).
    async fn scrape_watch(&self, video_id: &str) -> ApiResult<(Value, Vec<StreamLink>)> {
        let html = fetch_html(&format!(
            "https://www.youtube.com/watch?v={video_id}&hl=en&has_verified=1"
        ))
        .await?;
        let player = extract_json_blob(&html, "ytInitialPlayerResponse").ok_or_else(|| {
            ApiError::Unavailable("scrape: ytInitialPlayerResponse not found".into())
        })?;
        let value = Value::Object(player);
        let streams = streams_from_player(&value)?;
        if streams.is_empty() {
            return Err(ApiError::Unavailable("scrape: no open streams".into()));
        }
        Ok((value, streams))
    }

    /// The guarantee: browser-context embed that plays regardless of any
    /// extraction breakage. Always present, ranked last by the resolver.
    fn embed_fallback(&self, video_id: &str) -> StreamLink {
        StreamLink {
            label: "YouTube embed · browser player".into(),
            url: format!("https://www.youtube.com/embed/{video_id}"),
            kind: "page".into(),
        }
    }

    /// Full stream pipeline: cache → strategy chain → embed guarantee.
    /// Never returns an empty link list, so playback resolution never
    /// dead-ends for users even mid-outage.
    async fn resolve_streams(&self, video_id: &str) -> (Value, Vec<StreamLink>) {
        if let Some(entry) = self.stream_cache().get(video_id)
            && entry.0.elapsed() < STREAM_CACHE_TTL
        {
            return (Value::Null, entry.1.clone());
        }

        let chain: Vec<BoxFuture<'_, ApiResult<(Value, Vec<StreamLink>)>>> = vec![
            Box::pin(self.innertube_player_streams(video_id)),
            Box::pin(self.piped_streams(video_id)),
            Box::pin(self.invidious_streams(video_id)),
            Box::pin(self.scrape_watch(video_id)),
        ];

        let mut payload: Option<Value> = None;
        let mut links: Vec<StreamLink> = Vec::new();
        let mut last_error: Option<String> = None;

        for (idx, fut) in chain.into_iter().enumerate() {
            match fut.await {
                Ok((body, streams)) => {
                    payload = Some(body);
                    links = streams;
                    self.stats.record(idx, true);
                    metrics::counter!(
                        "youtube_strategy_total",
                        "strategy" => STRATEGY_NAMES[idx], "result" => "ok"
                    )
                    .increment(1);
                    break;
                }
                Err(err) => {
                    self.stats.record(idx, false);
                    metrics::counter!(
                        "youtube_strategy_total",
                        "strategy" => STRATEGY_NAMES[idx], "result" => "fail"
                    )
                    .increment(1);
                    last_error = Some(err.to_string());
                }
            }
        }

        if links.is_empty() {
            // Total extraction failure: degrade loudly, still serve embed.
            self.stats.consecutive_failures.fetch_add(1, Ordering::Relaxed);
            *self.stats.last_error.write().await = last_error;
            links = vec![self.embed_fallback(video_id)];
            self.stats.record(4, true);
            metrics::counter!(
                "youtube_strategy_total", "strategy" => "embed", "result" => "ok"
            )
            .increment(1);
            payload = Some(json!({ "degraded": true }));
            metrics::gauge!("youtube_degraded").set(1.0);
        } else {
            self.cache_streams(video_id, links.clone());
        }

        (
            payload.unwrap_or_else(|| json!({ "degraded": true })),
            links,
        )
    }

    fn cache_streams(&self, video_id: &str, links: Vec<StreamLink>) {
        let cache = self.stream_cache();
        if cache.len() > STREAM_CACHE_MAX {
            cache.retain(|_, (at, _)| at.elapsed() < STREAM_CACHE_TTL);
            if cache.len() > STREAM_CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(video_id.to_string(), (Instant::now(), links));
    }

    /// Metadata lookup that tolerates every backend failing (title degrades
    /// to the id rather than erroring). oEmbed is tiny, keyless, and one of
    /// Google's most stable public endpoints.
    async fn best_effort_meta(&self, video_id: &str) -> SourceItem {
        if let Some(meta) = self.oembed_meta(video_id).await {
            return meta;
        }
        SourceItem {
            external_id: video_id.to_string(),
            title: format!("YouTube · {video_id}"),
            overview: "YouTube Music".into(),
            year: None,
            poster: Some(format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")),
            category: "music",
        }
    }

    async fn oembed_meta(&self, video_id: &str) -> Option<SourceItem> {
        let body = self
            .get_json(&format!(
                "https://www.youtube.com/oembed?url=https://youtu.be/{video_id}&format=json"
            ))
            .await?;
        Some(SourceItem {
            external_id: video_id.to_string(),
            title: body
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("YouTube video")
                .to_string(),
            overview: body
                .get("author_name")
                .and_then(Value::as_str)
                .map(|a| format!("by {a}"))
                .unwrap_or_default(),
            year: None,
            poster: body
                .get("thumbnail_url")
                .and_then(Value::as_str)
                .map(str::to_string),
            category: "music",
        })
    }
}

// -------------------------------------------------------------- payload maps

fn parse_piped_search(body: &Value, limit: i32) -> Vec<SourceItem> {
    body.get("items")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let watch_url = row.get("url")?.as_str()?;
                    let video_id = watch_url.rsplit("v=").next()?.to_string();
                    if video_id.is_empty() {
                        return None;
                    }                    Some(SourceItem {
                        external_id: video_id,
                        title: row.get("title")?.as_str()?.to_string(),
                        overview: row
                            .get("uploaderName")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        year: None,
                        poster: row.get("thumbnail").and_then(Value::as_str).map(str::to_string),
                        category: "music",
                    })
                })
                .take(limit.clamp(1, 50) as usize)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_invidious_search(body: &Value, limit: i32) -> Vec<SourceItem> {
    body.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(SourceItem {
                        external_id: row.get("videoId")?.as_str()?.to_string(),
                        title: row.get("title")?.as_str()?.to_string(),
                        overview: row
                            .get("author")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        year: None,
                        poster: row
                            .pointer("/videoThumbnails/0/url")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        category: "music",
                    })
                })
                .take(limit.clamp(1, 50) as usize)
                .collect()
        })
        .unwrap_or_default()
}

fn video_renderer_to_item(renderer: &Value) -> Option<SourceItem> {
    Some(SourceItem {
        external_id: renderer.get("videoId")?.as_str()?.to_string(),
        title: renderer
            .pointer("/title/runs/0/text")
            .and_then(Value::as_str)?
            .to_string(),
        overview: renderer
            .pointer("/ownerText/runs/0/text")
            .or_else(|| renderer.pointer("/longBylineText/runs/0/text"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        year: None,
        poster: renderer
            .pointer("/thumbnail/thumbnails")
            .and_then(Value::as_array)
            .and_then(|t| t.last())
            .and_then(|t| t.get("url"))
            .and_then(Value::as_str)
            .map(str::to_string),
        category: "music",
    })
}

fn responsive_to_item(renderer: &Value) -> Option<SourceItem> {
    let video_id = renderer
        .pointer("/flexColumns/0/musicResponsiveRendererFlexColumn/text/runs/0/navigationEndpoint/watchEndpoint/videoId")
        .or_else(|| renderer.pointer("/navigationEndpoint/watchEndpoint/videoId"))
        .and_then(Value::as_str)?
        .to_string();

    let column_text = |idx: usize| -> String {
        renderer
            .pointer(&format!(
                "/flexColumns/{idx}/musicResponsiveRendererFlexColumn/text/runs"
            ))
            .and_then(Value::as_array)
            .map(|runs| {
                runs.iter()
                    .filter_map(|r| r.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    };

    let title = column_text(0);
    if title.is_empty() {
        return None;
    }
    let meta = column_text(1);
    let overview = if meta.to_lowercase().contains("views") {
        format!("{meta} · video")
    } else if meta.is_empty() {
        "YouTube Music".to_string()
    } else {
        meta
    };

    let poster = renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|thumbs| thumbs.last())
        .and_then(|t| t.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(SourceItem {
        external_id: video_id,
        title,
        overview,
        year: None,
        poster,
        category: "music",
    })
}

/// Ranked audio-first stream list from a Piped `/streams` payload.
fn streams_from_payload(body: &Value) -> ApiResult<Vec<StreamLink>> {
    let mut audio: Vec<&Value> = body
        .get("audioStreams")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    audio.sort_by_key(|s| -s.get("bitrate").and_then(Value::as_i64).unwrap_or(0));

    let mut video: Vec<&Value> = body
        .get("videoStreams")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    video.sort_by_key(|s| -s.get("bitrate").and_then(Value::as_i64).unwrap_or(0));

    build_links(audio, video, |v| {
        v.get("quality").and_then(Value::as_str).unwrap_or("").to_string()
    })
}

/// Same ranking from InnerTube-player-shaped payloads. Only unciphered
/// entries are usable here; ciphered ones fall through to later strategies.
fn streams_from_player(player: &Value) -> ApiResult<Vec<StreamLink>> {
    // Owned clones sidestep closure-lifetime friction; payloads are small.
    let usable = |formats: Option<&Vec<Value>>| -> (Vec<Value>, Vec<Value>) {
        let mut audio = Vec::new();
        let mut video = Vec::new();
        for f in formats.into_iter().flatten() {
            let has_url = f
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|u| u.starts_with("http"));
            let ciphered =
                f.get("signatureCipher").is_some() || f.get("cipher").is_some();
            if !has_url || ciphered {
                continue;
            }
            let mime = f.get("mimeType").and_then(Value::as_str).unwrap_or("");
            if mime.starts_with("audio/") {
                audio.push(f.clone());
            } else if mime.starts_with("video/") {
                video.push(f.clone());
            }
        }
        (audio, video)
    };

    let (mut a_adaptive, mut v_adaptive) =
        usable(player.pointer("/streamingData/adaptiveFormats").and_then(Value::as_array));
    let (mut a_regular, mut v_regular) =
        usable(player.pointer("/streamingData/formats").and_then(Value::as_array));
    a_adaptive.append(&mut a_regular);
    v_adaptive.append(&mut v_regular);
    a_adaptive.sort_by_key(|s| -s.get("bitrate").and_then(Value::as_i64).unwrap_or(0));
    v_adaptive.sort_by_key(|s| -s.get("height").and_then(Value::as_i64).unwrap_or(0));

    build_links_owned(a_adaptive, v_adaptive, |v| {
        v.get("qualityLabel")
            .or_else(|| v.get("quality"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    })
}

/// Shared builder for owned payload entries: top-3 audio + up to 2 video.
fn build_links_owned(
    mut audio: Vec<Value>,
    mut video: Vec<Value>,
    video_label: impl Fn(&Value) -> String,
) -> ApiResult<Vec<StreamLink>> {
    let mut links = Vec::new();
    for stream in audio.drain(..).take(3) {
        let Some(url) = stream.get("url").and_then(Value::as_str) else { continue };
        let quality = stream
            .get("quality")
            .or_else(|| stream.get("qualityLabel"))
            .and_then(Value::as_str)
            .unwrap_or("audio");
        links.push(StreamLink {
            label: format!("YouTube audio · {quality}"),
            url: url.to_string(),
            kind: "audio".into(),
        });
    }
    let mut shown = 0;
    for stream in video.drain(..) {
        if shown >= 2 {
            break;
        }
        let Some(url) = stream.get("url").and_then(Value::as_str) else { continue };
        let q = video_label(&stream);
        let q = if q.is_empty() { "video".to_string() } else { q };
        links.push(StreamLink {
            label: format!("YouTube {q}"),
            url: url.to_string(),
            kind: "mp4".into(),
        });
        shown += 1;
    }
    if links.is_empty() {
        return Err(ApiError::Unavailable("payload had no open streams".into()));
    }
    Ok(links)
}

/// Shared builder: top-3 audio + up to 2 video entries.
fn build_links(
    mut audio: Vec<&Value>,
    mut video: Vec<&Value>,
    video_label: impl Fn(&Value) -> String,
) -> ApiResult<Vec<StreamLink>> {
    let mut links = Vec::new();
    for stream in audio.drain(..).take(3) {
        let Some(url) = stream.get("url").and_then(Value::as_str) else { continue };
        let quality = stream
            .get("quality")
            .or_else(|| stream.get("qualityLabel"))
            .and_then(Value::as_str)
            .unwrap_or("audio");
        links.push(StreamLink {
            label: format!("YouTube audio · {quality}"),
            url: url.to_string(),
            kind: "audio".into(),
        });
    }
    let mut shown = 0;
    for stream in video.drain(..) {
        if shown >= 2 {
            break;
        }
        let Some(url) = stream.get("url").and_then(Value::as_str) else { continue };
        let q = video_label(stream);
        let q = if q.is_empty() { "video".to_string() } else { q };
        links.push(StreamLink {
            label: format!("YouTube {q}"),
            url: url.to_string(),
            kind: "mp4".into(),
        });
        shown += 1;
    }
    if links.is_empty() {
        return Err(ApiError::Unavailable("payload had no open streams".into()));
    }
    Ok(links)
}

/// Invidious `/api/v1/videos/{id}` shape.
fn invidious_streams_from_payload(body: &Value) -> ApiResult<Vec<StreamLink>> {
    let mut audio: Vec<&Value> = body
        .get("adaptiveFormats")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|f| {
                    f.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.starts_with("audio/mp4"))
                })
                .collect()
        })
        .unwrap_or_default();
    audio.sort_by_key(|f| -f.get("bitrate").and_then(Value::as_i64).unwrap_or(0));

    let video: Vec<&Value> = body
        .get("formatStreams")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();

    build_links(audio, video, |f| {
        f.get("qualityLabel").and_then(Value::as_str).unwrap_or("").to_string()
    })
}

// ------------------------------------------------------------------ helpers

fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn fetch_html(url: &str) -> ApiResult<String> {
    client()
        .get(url)
        .header("accept-language", "en-US,en;q=0.9")
        .header("cookie", "CONSENT=YES+cb")
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("youtube unreachable: {e}")))?
        .text()
        .await
        .map_err(ApiError::from)
}

/// Recursively collects every object stored under `key` anywhere in a JSON
/// tree — layout changes upstream do not break extraction.
fn collect_key_objects(value: &Value, key: &str, out: &mut Vec<Value>) {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                out.push(found.clone());
            }
            for child in map.values() {
                collect_key_objects(child, key, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_key_objects(item, key, out);
            }
        }
        _ => {}
    }
}

/// Pulls `NAME = {...};</script>` blobs off YouTube HTML pages. Regexes are
/// compiled per call — this is the rare last-resort path, not the hot one.
fn extract_json_blob(html: &str, name: &str) -> Option<serde_json::Map<String, Value>> {
    let pattern = format!(r#"(?s){}\s*=\s*(\{{.*?\}});\s*</script>"#, name);
    let blob = regex::Regex::new(&pattern).ok()?;
    let caps = blob.captures(html)?;
    serde_json::from_str(caps.get(1)?.as_str()).ok()
}

/// Accepts bare ids and every common YouTube url shape.
fn extract_video_id(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 128 {
        return None;
    }
    if input.len() == 11
        && input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(input.to_string());
    }
    for host_prefix in ["watch?v=", "&v=", "/shorts/", "/embed/", "/live/", "youtu.be/"] {
        if let Some(idx) = input.find(host_prefix) {
            let rest = &input[idx + host_prefix.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(11)
                .collect();
            if id.len() == 11 {
                return Some(id);
            }
        }
    }
    None
}

/// Test shims for the private scrapers (used by lyrics-module unit tests).
#[cfg(test)]
pub mod test_hooks {
    use serde_json::Value;

    pub fn extract_video_id_for_tests(input: &str) -> Option<String> {
        super::extract_video_id(input)
    }

    pub fn extract_json_blob_for_tests(
        html: &str,
        name: &str,
    ) -> Option<serde_json::Map<String, Value>> {
        super::extract_json_blob(html, name)
    }
}

// ------------------------------------------------------------- native source

/// Shared-handle delegation: lets the registry box `Arc<Youtube>` while
/// AppState keeps a direct handle for the resilience loop and admin API.
#[async_trait]
impl NativeSource for std::sync::Arc<Youtube> {
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn label(&self) -> &'static str {
        (**self).label()
    }
    fn category(&self) -> &'static str {
        (**self).category()
    }
    fn capabilities(&self) -> SourceCapabilities {
        (**self).capabilities()
    }
    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        (**self).search(query, limit).await
    }
    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        (**self).item(external_id).await
    }
}

#[async_trait]
impl NativeSource for Youtube {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn label(&self) -> &'static str {
        "YouTube Music · InnerTube/Piped/Invidious/scrape chain"
    }

    fn category(&self) -> &'static str {
        "music"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            categories: vec!["music".into()],
            kinds: vec!["audio".into(), "mp4".into(), "page".into()],
            subtitles: false,
        }
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let query = query.trim();
        let effective = if query.is_empty() { "popular music" } else { query };

        let chain: Vec<BoxFuture<'_, ApiResult<Vec<SourceItem>>>> = vec![
            Box::pin(self.innertube_search(effective, limit)),
            Box::pin(self.piped_search(effective, limit)),
            Box::pin(self.invidious_search(effective, limit)),
            Box::pin(self.scrape_search(effective, limit)),
        ];
        for (idx, fut) in chain.into_iter().enumerate() {
            match fut.await {
                Ok(items) if !items.is_empty() => {
                    self.stats.record(idx, true);
                    metrics::counter!(
                        "youtube_strategy_total",
                        "strategy" => STRATEGY_NAMES[idx], "result" => "ok"
                    )
                    .increment(1);
                    return Ok(items);
                }
                Ok(_) | Err(_) => {
                    self.stats.record(idx, false);
                    metrics::counter!(
                        "youtube_strategy_total",
                        "strategy" => STRATEGY_NAMES[idx], "result" => "fail"
                    )
                    .increment(1);
                }
            }
        }
        self.stats.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        Err(ApiError::Unavailable(
            "youtube search exhausted every strategy (innertube/piped/invidious/scrape)".into(),
        ))
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let video_id = extract_video_id(external_id)
            .ok_or_else(|| ApiError::bad_request("invalid youtube video id"))?;

        let (_, streams) = self.resolve_streams(&video_id).await;
        let meta = self.best_effort_meta(&video_id).await;
        let item = SourceItem { external_id: video_id, ..meta };

        Ok(SourceDetail { streams, item, chapters: None })
    }
}

// --------------------------------------------------------- resilience loop

/// Admin surface for the YouTube pipeline.
pub mod admin {
    use super::{STRATEGY_NAMES, Youtube};
    use crate::auth::middleware::AdminUser;
    use crate::state::AppState;
    use axum::Json;
    use axum::extract::State;
    use serde_json::json;
    use std::sync::Arc;

    /// `GET /api/admin/youtube/status` — strategy health, pools, cache size.
    #[utoipa::path(get, path = "/api/admin/youtube/status", tag = "admin",
        security(("bearer" = [])),
        responses((status = 200))
    )]
    pub async fn status(
        State(state): State<Arc<AppState>>,
        _admin: AdminUser,
    ) -> Json<serde_json::Value> {
        let yt: &Youtube = &state.youtube;
        Json(json!({
            "stats": yt.stats.snapshot(),
            "pools": {
                "piped": *yt.piped_pool().read().await,
                "invidious": *yt.invidious_pool().read().await,
                "active_piped": *yt.active_piped().read().await,
                "active_invidious": *yt.active_invidious().read().await,
            },
            "stream_cache_entries": yt.stream_cache().len(),
            "strategies": STRATEGY_NAMES,
            "fallback_guarantee": "embed (browser iframe player)",
        }))
    }

    /// `POST /api/admin/youtube/heal` — refresh instance registries, rotate
    /// backends, clear caches. Also runs automatically on failure spikes.
    #[utoipa::path(post, path = "/api/admin/youtube/heal", tag = "admin",
        security(("bearer" = [])),
        responses((status = 200))
    )]
    pub async fn heal(
        State(state): State<Arc<AppState>>,
        _admin: AdminUser,
    ) -> Json<serde_json::Value> {
        let result = state.youtube.heal().await;
        tracing::info!(result = %result, "youtube manual heal triggered");
        Json(result)
    }
}

/// Background watchdog spawned by [`crate::state::AppState::build`]:
/// hourly registry refresh, failure-spike healing, degradation events.
pub fn spawn_resilience_loop(
    state: std::sync::Arc<crate::state::AppState>,
    youtube: std::sync::Arc<Youtube>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        let mut was_degraded = false;
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = state.cancel.cancelled() => break,
                _ = interval.tick() => {
                    let failing = youtube.stats.consecutive_failures.load(Ordering::Relaxed);
                    if failing >= 3 {
                        tracing::warn!(failures = failing, "youtube degradation detected — healing");
                        youtube.heal().await;
                        metrics::gauge!("youtube_degraded").set(1.0);
                        if !was_degraded {
                            was_degraded = true;
                            crate::events::emit(
                                &state.pool,
                                &state.hub,
                                "youtube.degraded",
                                json!({
                                    "message": "YouTube extraction degraded — rotating backends and refreshing instance registries",
                                    "consecutive_failures": failing,
                                }),
                                None,
                            )
                            .await;
                        }
                    } else {
                        youtube.refresh_instance_registries().await;
                        if was_degraded && failing == 0 {
                            was_degraded = false;
                            metrics::gauge!("youtube_degraded").set(0.0);
                            crate::events::emit(
                                &state.pool,
                                &state.hub,
                                "youtube.recovered",
                                json!({"message": "YouTube extraction recovered"}),
                                None,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    });
}

// ====================================================================
// Video search — the section SPACE shows in its own right
// ====================================================================
//
// The `NativeSource` search above asks YouTube *Music*, because that is what
// that source is for. A video row wants the other catalogue: trailers, clips,
// talks, whole films uploaded to the site. Same resilience chain, different
// question — and the results are labelled `movie` so the front end opens them
// in the video player rather than the audio one.

/// Regular (non-music) InnerTube search.
const INNERTUBE_SEARCH: &str = "https://www.youtube.com/youtubei/v1/search?prettyPrint=false";

/// What YouTube calls "videos only, sorted by relevance". Without it the
/// results are half channels and playlists, neither of which plays.
const FILTER_VIDEOS_ONLY: &str = "EgIQAQ%3D%3D";

impl Youtube {
    /// Videos for a query, through the same four layers as everything else.
    ///
    /// Every layer is tried in turn and the first non-empty answer wins, so a
    /// dead Piped pool or an InnerTube change costs latency, not the section.
    pub async fn video_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let chain: Vec<BoxFuture<'_, ApiResult<Vec<SourceItem>>>> = vec![
            Box::pin(self.innertube_video_search(query, limit)),
            Box::pin(self.piped_video_search(query, limit)),
            Box::pin(self.invidious_search(query, limit)),
            Box::pin(self.scrape_search(query, limit)),
        ];
        for (idx, fut) in chain.into_iter().enumerate() {
            match fut.await {
                Ok(items) if !items.is_empty() => {
                    self.stats.record(idx, true);
                    return Ok(as_video(items));
                }
                Ok(_) | Err(_) => self.stats.record(idx, false),
            }
        }
        // A missing row is better than a broken page: the caller renders
        // nothing rather than an error banner over the rest of the results.
        Ok(Vec::new())
    }

    async fn innertube_video_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let body = json!({
            "context": {"client": {"clientName": "WEB", "clientVersion": "2.20240401.01.00", "hl": "en", "gl": "US"}},
            "query": query,
            "params": "EgIQAQ%3D%3D",
        });
        let response: Value = client()
            .post(INNERTUBE_SEARCH)
            .json(&body)
            .header("content-type", "application/json")
            .header("origin", "https://www.youtube.com")
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("innertube unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let mut renderers = Vec::new();
        collect_key_objects(&response, "videoRenderer", &mut renderers);
        let items: Vec<SourceItem> = renderers
            .iter()
            .filter_map(video_renderer_to_item)
            .take(limit.clamp(1, 50) as usize)
            .collect();
        if items.is_empty() {
            return Err(ApiError::Unavailable("innertube returned no videos".into()));
        }
        Ok(items)
    }

    async fn piped_video_search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        for base in self.candidates(self.piped_pool(), self.active_piped()).await {
            if let Some(body) = self
                .get_json(&format!(
                    "{base}/search?q={}&filter=videos",
                    urlencode(query)
                ))
                .await
            {
                let items = parse_piped_search(&body, limit);
                if !items.is_empty() {
                    self.remember_active(self.active_piped(), &base).await;
                    return Ok(items);
                }
            }
        }
        Err(ApiError::Unavailable("all Piped instances failed".into()))
    }

    /// What the section shows before anybody has searched.
    ///
    /// Trending is a real endpoint on Invidious and Piped; when neither
    /// answers, YouTube's own trending page is scraped. An empty list is a
    /// legitimate answer - the row simply does not appear.
    pub async fn trending(&self, limit: i32) -> Vec<SourceItem> {
        for base in self
            .candidates(self.invidious_pool(), self.active_invidious())
            .await
        {
            if let Some(body) = self.get_json(&format!("{base}/api/v1/trending")).await {
                let items = parse_invidious_search(&body, limit);
                if !items.is_empty() {
                    self.remember_active(self.active_invidious(), &base).await;
                    return as_video(items);
                }
            }
        }
        for base in self.candidates(self.piped_pool(), self.active_piped()).await {
            if let Some(body) = self.get_json(&format!("{base}/trending?region=US")).await {
                // Piped's trending rows are a bare array, shaped like
                // Invidious', not the `{items:[...]}` its search returns.
                let items = parse_invidious_search(&body, limit);
                if !items.is_empty() {
                    self.remember_active(self.active_piped(), &base).await;
                    return as_video(items);
                }
            }
        }
        if let Ok(html) = fetch_html("https://www.youtube.com/feed/trending?hl=en").await {
            if let Some(initial) = extract_json_blob(&html, "ytInitialData") {
                let mut renderers = Vec::new();
                collect_key_objects(&Value::Object(initial), "videoRenderer", &mut renderers);
                let items: Vec<SourceItem> = renderers
                    .iter()
                    .filter_map(video_renderer_to_item)
                    .take(limit.clamp(1, 50) as usize)
                    .collect();
                if !items.is_empty() {
                    return as_video(items);
                }
            }
        }
        // Nothing carries a trending feed a server can read today: the public
        // Invidious and Piped nodes are behind browser challenges, and
        // YouTube's own trending page needs a session. Search still answers,
        // so the row falls back to what this app is actually for — trailers -
        // rather than disappearing.
        self.video_search(TRENDING_FALLBACK_QUERY, limit)
            .await
            .unwrap_or_default()
    }
}

/// What the home row shows when no trending feed answers. Deliberately about
/// film and television rather than YouTube at large: this is a media app, and
/// a row of unrelated viral clips is worse than no row.
const TRENDING_FALLBACK_QUERY: &str = "movie official trailer";

/// The parsers all stamp `music`, because that is what they were written for.
/// A video row has to say `movie`, or the front end opens an audio player over
/// a video.
fn as_video(items: Vec<SourceItem>) -> Vec<SourceItem> {
    items
        .into_iter()
        .map(|mut item| {
            item.category = "movie";
            item
        })
        .collect()
}

/// `GET /api/youtube/search?q=&limit=` - the video section, on any page.
#[utoipa::path(
    get,
    path = "/api/youtube/search",
    tag = "youtube",
    params(
        ("q" = String, Query, description = "what to search for"),
        ("limit" = Option<i32>, Query, description = "how many videos, 1-50")
    ),
    responses((status = 200, description = "YouTube videos for the query"))
)]
pub async fn search_videos(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    axum::extract::Query(params): axum::extract::Query<VideoQuery>,
) -> ApiResult<axum::Json<Value>> {
    let limit = params.limit.unwrap_or(12).clamp(1, 50);
    let query = params.q.unwrap_or_default();
    // No query is not an error: the section falls back to what is trending,
    // which is exactly what the home page wants.
    let items = if query.trim().is_empty() {
        state.youtube.trending(limit).await
    } else {
        state.youtube.video_search(&query, limit).await?
    };
    Ok(axum::Json(json!({ "source": "youtube", "results": as_results(items) })))
}

/// `GET /api/youtube/trending` - what the section shows on the home page.
#[utoipa::path(
    get,
    path = "/api/youtube/trending",
    tag = "youtube",
    params(("limit" = Option<i32>, Query, description = "how many videos, 1-50")),
    responses((status = 200, description = "Trending YouTube videos"))
)]
pub async fn trending_videos(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    axum::extract::Query(params): axum::extract::Query<VideoQuery>,
) -> axum::Json<Value> {
    let limit = params.limit.unwrap_or(12).clamp(1, 50);
    let items = state.youtube.trending(limit).await;
    axum::Json(json!({ "source": "youtube", "results": as_results(items) }))
}

/// The shape the front end already knows how to open.
///
/// A bare video id is not enough: playback goes through `/api/resolve`, which
/// takes a play token. Minting it here means a card can link straight to the
/// watch page instead of the client having to know how tokens are built.
fn as_results(items: Vec<SourceItem>) -> Vec<Value> {
    items
        .into_iter()
        .map(|item| {
            json!({
                "id": super::aggregator::make_token("youtube", &item.external_id),
                "video_id": item.external_id,
                "title": item.title,
                "overview": item.overview,
                "year": item.year,
                "poster": item.poster,
                "type": item.category,
                "sources": ["youtube"],
            })
        })
        .collect()
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct VideoQuery {
    pub q: Option<String>,
    pub limit: Option<i32>,
}

#[cfg(test)]
mod video_section_tests {
    use super::*;

    #[test]
    fn a_video_row_is_never_labelled_music() {
        // The shared parsers stamp "music"; sending that to the front end
        // opens the audio player over a video.
        let items = as_video(vec![SourceItem {
            external_id: "abc".into(),
            title: "Trailer".into(),
            overview: String::new(),
            year: None,
            poster: None,
            category: "music",
        }]);
        assert_eq!(items[0].category, "movie");
    }

    #[test]
    fn a_result_carries_a_play_token_not_a_bare_video_id() {
        let rows = as_results(vec![SourceItem {
            external_id: "dQw4w9WgXcQ".into(),
            title: "Clip".into(),
            overview: String::new(),
            year: None,
            poster: None,
            category: "movie",
        }]);
        let token = rows[0]["id"].as_str().unwrap();
        assert_ne!(token, "dQw4w9WgXcQ", "the id must be resolvable, not raw");
        assert_eq!(
            super::super::aggregator::token_parts(token).unwrap(),
            ("youtube".to_string(), "dQw4w9WgXcQ".to_string())
        );
        // The video id is still there for anything that wants to embed it.
        assert_eq!(rows[0]["video_id"], "dQw4w9WgXcQ");
    }

    #[tokio::test]
    async fn an_empty_query_is_not_a_search() {
        let youtube = Youtube::new();
        assert!(
            youtube.video_search("   ", 5).await.unwrap().is_empty(),
            "a blank box must not go to the network"
        );
    }
}
