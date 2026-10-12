//! The converted plugins that read HTML or RSS: LunaStream, WeebCentral,
//! Hentai Haven, Nyaa and 1337x.

use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::plugins::native::custom::apis::{env_or, html_title};
use crate::plugins::native::engines::{EngineCtx, quote_plus};

/// What a browser sends, for hosts that reject anything that looks automated.
///
/// This gets past header checks. It does not get past TLS fingerprinting —
/// some of these sites identify the client from the handshake itself, and no
/// combination of headers changes that.
const BROWSER_HEADERS: &[(&str, &str)] = &[
    (
        "accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
    ),
    ("accept-language", "en-US,en;q=0.9"),
    ("sec-fetch-dest", "document"),
    ("sec-fetch-mode", "navigate"),
    ("sec-fetch-site", "none"),
    ("upgrade-insecure-requests", "1"),
];

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drops tags, leaving their text. Not a parser - the same shallow pass the
/// Python regexes made.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut depth = 0usize;
    for ch in html.chars() {
        match ch {
            '<' => depth += 1,
            '>' => {
                if depth > 0 {
                    depth -= 1;
                }
                out.push(' ');
            }
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    collapse(&out)
}

/// The handful of entities these pages actually carry.
fn unescape(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&nbsp;", " ")
}

fn first_capture(re: &regex::Regex, text: &str, group: usize) -> Option<String> {
    re.captures(text)
        .and_then(|c| c.get(group))
        .map(|m| m.as_str().to_string())
}

fn year_in(text: &str) -> Option<i64> {
    static YEAR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = YEAR.get_or_init(|| regex::Regex::new(r"\b(?:19|20)\d{2}\b").unwrap());
    re.find(text).and_then(|m| m.as_str().parse().ok())
}

// ------------------------------------------------------------- LunaStream

/// Luna's search runs client-side against a private API, but its homepage is
/// server-rendered, so an empty query browses the trending rails it ships in
/// the HTML.
#[derive(Debug, Clone)]
pub struct LunaStream {
    pub base: String,
}

impl LunaStream {
    pub fn new() -> Self {
        Self {
            base: env_or("LUNA_BASE", "https://luna-stream.me"),
        }
    }

    fn extract_trending(html: &str, limit: usize) -> Vec<Value> {
        static INFO: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static META: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static EP_PREFIX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static NUMERIC: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let info = INFO.get_or_init(|| regex::Regex::new(r#"href="/anime/info/(\d+)""#).unwrap());
        let meta = META.get_or_init(|| {
            regex::Regex::new(
                r"(?i)^(episode|ep|sub|dub|raw|ona|ova|tv|movie|releasing|completed|upcoming)$",
            )
            .unwrap()
        });
        let ep_prefix =
            EP_PREFIX.get_or_init(|| regex::Regex::new(r"(?i)^(Episode|Ep\.?)\s+\S+\s*").unwrap());
        let numeric =
            NUMERIC.get_or_init(|| regex::Regex::new(r"(?i)^(ep\.?\s*)?\d+(\.\d+)?$").unwrap());

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for capture in info.captures_iter(html) {
            let anime_id = capture.get(1).map(|m| m.as_str()).unwrap_or_default();
            if seen.contains(anime_id) {
                continue;
            }
            let end = capture.get(0).map(|m| m.end()).unwrap_or(0);
            // generous window, THEN strip tags, so a half-cut tag cannot leak
            let window_end = html.len().min(end + 2500);
            let mut segment = &html[end..window_end];
            if let Some(next) = segment.find(r#"href="/anime/info/"#) {
                segment = &segment[..next];
            }
            let text = strip_tags(segment);

            let mut title = String::new();
            let parts: Vec<&str> = text
                .split(['\u{2022}', '\u{b7}', '|'])
                .map(|p| p.trim_matches([' ', '.', '|']))
                .filter(|p| !p.is_empty())
                .collect();
            for candidate in parts.iter().rev() {
                let candidate = candidate.split('<').next().unwrap_or(candidate);
                let candidate = ep_prefix.replace(candidate, "").trim().to_string();
                if candidate.chars().count() < 3
                    || meta.is_match(&candidate)
                    || numeric.is_match(&candidate)
                {
                    continue;
                }
                title = candidate;
                break;
            }
            if title.is_empty() {
                continue;
            }

            seen.insert(anime_id.to_string());
            out.push(json!({
                "id": format!("luna:{anime_id}"),
                "title": title.chars().take(120).collect::<String>(),
                "overview": "",
                "year": Value::Null,
                "poster": "",
                "type": "series",
            }));
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        if query.trim().is_empty() {
            return match ctx.get_text(&format!("{}/", self.base)).await {
                Ok(html) => Ok(json!({
                    "results": Self::extract_trending(&html, limit.clamp(1, 40)),
                })),
                Err(err) => {
                    Ok(json!({ "results": [], "note": format!("trending unavailable: {err}") }))
                }
            };
        }
        Ok(json!({
            "results": [],
            "note": "luna's search runs client-side against a private api; \
                     empty query returns the site-wide trending feed",
        }))
    }

    pub async fn details(&self, _ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let parts: Vec<&str> = id.trim().split(':').collect();
        let valid = parts.len() == 2
            && parts[0] == "luna"
            && !parts[1].is_empty()
            && parts[1].chars().all(|c| c.is_ascii_digit());
        if !valid {
            return Err(ApiError::bad_request(
                "id must be luna:{anime_id} (from luna browse)",
            ));
        }
        Ok(json!({
            "title": format!("Luna #{}", parts[1]),
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "series",
            "streams": [{
                "label": "LunaStream \u{b7} watch page",
                "url": format!("{}/anime/info/{}", self.base, parts[1]),
                "kind": "page",
            }],
        }))
    }
}

// ------------------------------------------------------------ WeebCentral

#[derive(Debug, Clone)]
pub struct WeebCentral;

const WEEB_BASE: &str = "https://weebcentral.com";

impl WeebCentral {
    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        // Their search has a server-rendered data endpoint: one request, no JS.
        let html = ctx
            .get_text(&format!(
                "{WEEB_BASE}/search/data?name={}",
                query.replace(' ', "+")
            ))
            .await?;

        static SERIES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let series = SERIES.get_or_init(|| {
            regex::Regex::new(
                r#"href="(?:https://weebcentral\.com)?(/series/([A-Za-z0-9]{10,40})/([^"/\s]+))""#,
            )
            .unwrap()
        });

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for capture in series.captures_iter(&html) {
            let ulid = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
            let slug = capture.get(3).map(|m| m.as_str()).unwrap_or_default();
            if !seen.insert(ulid.to_string()) {
                continue;
            }
            out.push(json!({
                "id": format!("wc:{ulid}:{slug}"),
                "title": slug.replace('-', " ").trim().to_string(),
                "overview": "",
                "year": Value::Null,
                "poster": format!("https://temp.compsci88.com/cover/normal/{ulid}.webp"),
                "type": "manga",
            }));
            if out.len() >= limit.clamp(1, 30) {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, _ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let (url, title) = if let Some(rest) = id.strip_prefix("url:")
            && rest.starts_with(WEEB_BASE)
        {
            let slug = rest.trim_end_matches('/').rsplit('/').next().unwrap_or("");
            (rest.to_string(), slug.replace('-', " "))
        } else if let Some(rest) = id.strip_prefix("wc:") {
            let (ulid, slug) = rest.split_once(':').unwrap_or(("", ""));
            if ulid.is_empty() || slug.is_empty() {
                return Err(ApiError::bad_request("malformed wc id"));
            }
            (
                format!("{WEEB_BASE}/series/{ulid}/{slug}"),
                slug.replace('-', " "),
            )
        } else {
            return Err(ApiError::bad_request(
                "id must be wc:{ulid}:{slug} or url:{series_url}",
            ));
        };

        Ok(json!({
            "title": title,
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "series",
            "streams": [{ "label": "WeebCentral \u{b7} read", "url": url, "kind": "page" }],
        }))
    }
}

// ----------------------------------------------------------- Hentai Haven

#[derive(Debug, Clone)]
pub struct HentaiHaven {
    pub base: String,
}

impl HentaiHaven {
    pub fn new() -> Self {
        Self {
            base: env_or("HH_BASE", "https://hentaihaven.xxx"),
        }
    }

    fn title_from_slug(slug: &str) -> String {
        slug.split('-')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        // This host refuses a bare request. Sending what a browser sends gets
        // past the simpler checks; see BROWSER_HEADERS.
        let html = ctx
            .get_text_with(&format!("{}/browse/trending/", self.base), BROWSER_HEADERS)
            .await?;

        static WATCH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let watch = WATCH.get_or_init(|| {
            regex::Regex::new(r#"href="(?:https?://[a-z0-9.-]+)?(/watch/([a-z0-9-]{3,90})/)""#)
                .unwrap()
        });

        let needle = query.trim().to_lowercase();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for capture in watch.captures_iter(&html) {
            let slug = capture.get(2).map(|m| m.as_str()).unwrap_or_default();
            if !seen.insert(slug.to_string()) {
                continue;
            }
            let title = Self::title_from_slug(slug);
            if !needle.is_empty() && !title.to_lowercase().contains(&needle) {
                continue;
            }
            out.push(json!({
                "id": format!("hh:{slug}"),
                "title": title,
                "overview": "",
                "year": Value::Null,
                "poster": "",
                "type": "movie",
            }));
            if out.len() >= limit.clamp(1, 40) {
                break;
            }
        }

        if !query.trim().is_empty() {
            out.insert(
                0,
                json!({
                    "id": format!("hh-search:{}", query.trim()),
                    "title": format!("[site search] {query}"),
                    "overview": "handoff link to the site's own catalog",
                    "year": Value::Null,
                    "poster": "",
                    "type": "movie",
                }),
            );
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        if let Some(query) = id.strip_prefix("hh-search:") {
            return Ok(json!({
                "title": format!("Search Hentai Haven: {query}"),
                "overview": "",
                "poster": "",
                "year": Value::Null,
                "media_type": "movie",
                "streams": [{
                    "label": "site search",
                    "url": format!("{}/?s={}", self.base, query.replace(' ', "+")),
                    "kind": "page",
                }],
            }));
        }
        let Some(slug) = id.strip_prefix("hh:") else {
            return Err(ApiError::bad_request(
                "id must be hh:{series_slug} from search",
            ));
        };

        let page = format!("{}/watch/{slug}/", self.base);
        let html = ctx.get_text_with(&page, BROWSER_HEADERS).await?;
        let title = html_title(&html)
            .map(|t| t.replace(" | Hentai Haven", "").trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| Self::title_from_slug(slug));

        static MP4: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static EP_NUM: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let mp4 = MP4.get_or_init(|| {
            regex::Regex::new(
                r#"(https?:\\?/\\?/[a-zA-Z0-9.-]+\\?/[a-zA-Z0-9/_.-]{3,120}\.mp4)"#,
            )
            .unwrap()
        });
        let ep_num = EP_NUM.get_or_init(|| regex::Regex::new(r#"-(\d{1,2})(?:/|"|$)"#).unwrap());

        let mut streams = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for capture in mp4.captures_iter(&html) {
            let url = capture
                .get(1)
                .map(|m| m.as_str().replace('\\', ""))
                .unwrap_or_default();
            if !seen.insert(url.clone()) {
                continue;
            }
            let label = match first_capture(ep_num, &url, 1) {
                Some(number) => format!("Haven \u{b7} episode {number}"),
                None => "Haven \u{b7} clip".to_string(),
            };
            streams.push(json!({ "label": label, "url": url, "kind": "mp4" }));
        }
        if streams.is_empty() {
            streams.push(json!({
                "label": "Haven \u{b7} watch page",
                "url": page,
                "kind": "page",
            }));
        }
        streams.truncate(20);

        Ok(json!({
            "title": title,
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "movie",
            "streams": streams,
        }))
    }
}

// -------------------------------------------------------------------- Nyaa

/// Nyaa publishes a structured RSS feed - their own machine interface - so
/// there is no HTML scraping here.
#[derive(Debug, Clone)]
pub struct Nyaa {
    pub base: String,
}

impl Nyaa {
    pub fn new() -> Self {
        Self {
            base: env_or("NYAA_BASE", "https://nyaa.si"),
        }
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let url = format!(
            "{}/rss?q={}&f=0&c=0_0",
            self.base,
            quote_plus(query.trim())
        );
        let xml = ctx.get_text(&url).await?;
        let document = roxmltree::Document::parse(&xml)
            .map_err(|e| ApiError::Unavailable(format!("nyaa sent invalid rss: {e}")))?;

        let text_of = |node: roxmltree::Node, name: &str| -> String {
            node.children()
                .find(|c| c.tag_name().name() == name)
                .and_then(|c| c.text())
                .unwrap_or("")
                .trim()
                .to_string()
        };

        let mut results = Vec::new();
        for item in document
            .descendants()
            .filter(|n| n.tag_name().name() == "item")
        {
            let title = text_of(item, "title");
            if title.is_empty() {
                continue;
            }
            let link = text_of(item, "link");
            let info_hash = text_of(item, "infoHash");
            let seeders = text_of(item, "seeders");
            let size = text_of(item, "size");

            let mut bits = Vec::new();
            if !seeders.is_empty() {
                bits.push(format!("S:{seeders}"));
            }
            if !size.is_empty() {
                bits.push(size.clone());
            }

            // A self-contained id means resolution needs no second request.
            let id = if info_hash.is_empty() {
                if link.is_empty() {
                    format!("hash:{info_hash}")
                } else {
                    link.clone()
                }
            } else {
                let magnet = format!(
                    "magnet:?xt=urn:btih:{info_hash}&dn={}",
                    quote_plus(&title)
                );
                serde_json::to_string(&json!({
                    "magnet": magnet,
                    "title": title,
                    "size": size,
                    "seeders": seeders,
                    "year": year_in(&title),
                }))
                .unwrap_or_default()
            };

            results.push(json!({
                "id": id,
                "title": title,
                "overview": bits.join(" \u{b7} "),
                "year": year_in(&title),
                "poster": "",
                "type": "movie",
            }));
            if results.len() >= limit.clamp(1, 40) {
                break;
            }
        }
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        if let Ok(row) = serde_json::from_str::<Value>(id)
            && let Some(magnet) = row.get("magnet").and_then(Value::as_str)
            && magnet.starts_with("magnet:?xt=urn:btih:")
        {
            let size = row.get("size").and_then(Value::as_str).unwrap_or("");
            let seeders = row
                .get("seeders")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("?");
            return Ok(json!({
                "title": row.get("title").and_then(Value::as_str).unwrap_or("Nyaa torrent"),
                "overview": "",
                "poster": "",
                "year": row.get("year").cloned().unwrap_or(Value::Null),
                "media_type": "movie",
                "streams": [{
                    "label": format!("magnet \u{b7} {size} \u{b7} S:{seeders}").trim().to_string(),
                    "url": magnet,
                    "kind": "torrent",
                }],
            }));
        }

        if id.contains("/view/") {
            let html = ctx.get_text(id).await?;
            static MAGNET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let magnet_re =
                MAGNET.get_or_init(|| regex::Regex::new(r#"href="(magnet:\?[^"]+)""#).unwrap());
            let Some(magnet) = first_capture(magnet_re, &html, 1) else {
                return Err(ApiError::Unavailable(
                    "no magnet on nyaa view page".into(),
                ));
            };
            let title = html_title(&html)
                .map(|t| t.replace(" :: NyaaSi", "").trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "Nyaa torrent".to_string());
            return Ok(json!({
                "title": title,
                "overview": "",
                "poster": "",
                "year": Value::Null,
                "media_type": "movie",
                "streams": [{ "label": "magnet", "url": magnet, "kind": "torrent" }],
            }));
        }

        Err(ApiError::bad_request(
            "id must be a nyaa search result (or /view/ url)",
        ))
    }
}

// ------------------------------------------------------------------- 1337x

#[derive(Debug, Clone)]
pub struct Leetx {
    pub base: String,
}

/// One row of a 1337x `.table-list` search table.
#[derive(Default, Debug)]
struct Listing {
    href: String,
    title: String,
    seeds: String,
    leech: String,
    size: String,
}

impl Leetx {
    pub fn new() -> Self {
        Self {
            base: env_or("LEETX_BASE", "https://1337x.to"),
        }
    }

    /// 1337x sits behind anti-bot, so a plain fetch is tried first and the
    /// galaxy scraper is the fallback - the same chain the Python walked,
    /// minus the FlareSolverr hop, which the server does not ship.
    async fn fetch_html(&self, ctx: &EngineCtx, url: &str) -> ApiResult<String> {
        match ctx.get_text(url).await {
            Ok(html) if !html.trim().is_empty() => Ok(html),
            Ok(_) => Err(ApiError::Unavailable(
                "1337x returned an empty page".into(),
            )),
            Err(direct) => Err(ApiError::Unavailable(format!(
                "1337x unreachable - {direct}"
            ))),
        }
    }

    /// Walks `<tr>` blocks pulling the torrent link and the seed/leech/size
    /// cells, which is what the Python's HTMLParser subclass did.
    fn parse_listing(html: &str) -> Vec<Listing> {
        static ROW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static LINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static CELL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let row_re = ROW.get_or_init(|| regex::Regex::new(r"(?is)<tr[^>]*>(.*?)</tr>").unwrap());
        let link_re = LINK
            .get_or_init(|| regex::Regex::new(r#"(?is)<a([^>]*)href="([^"]*/torrent/[^"]*)"([^>]*)>(.*?)</a>"#).unwrap());
        let cell_re =
            CELL.get_or_init(|| regex::Regex::new(r#"(?is)<td([^>]*)>(.*?)</td>"#).unwrap());

        let mut rows = Vec::new();
        for row_capture in row_re.captures_iter(html) {
            let body = row_capture.get(1).map(|m| m.as_str()).unwrap_or("");
            let mut listing = Listing::default();

            for link in link_re.captures_iter(body) {
                let attrs = format!(
                    "{}{}",
                    link.get(1).map(|m| m.as_str()).unwrap_or(""),
                    link.get(3).map(|m| m.as_str()).unwrap_or("")
                );
                let href = link.get(2).map(|m| m.as_str()).unwrap_or("");
                if listing.href.is_empty() {
                    listing.href = href.to_string();
                }
                // the icon link carries no title text
                if attrs.contains("icon") {
                    continue;
                }
                let text = collapse(&strip_tags(link.get(4).map(|m| m.as_str()).unwrap_or("")));
                if !text.is_empty() && text.len() > listing.title.len() {
                    listing.title = text;
                }
            }

            for cell in cell_re.captures_iter(body) {
                let class = cell.get(1).map(|m| m.as_str()).unwrap_or("");
                let value = collapse(&strip_tags(cell.get(2).map(|m| m.as_str()).unwrap_or("")));
                if class.contains("coll-2") {
                    listing.seeds = value;
                } else if class.contains("coll-3") {
                    listing.leech = value;
                } else if class.contains("coll-4") {
                    listing.size = value;
                }
            }

            if !listing.href.is_empty() && !listing.title.is_empty() {
                rows.push(listing);
            }
        }
        rows
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let html = self
            .fetch_html(ctx, &format!("{}/search/{}/1/", self.base, quote_plus(query)))
            .await?;

        let results: Vec<Value> = Self::parse_listing(&html)
            .into_iter()
            .take(limit.clamp(1, 50))
            .map(|row| {
                let mut bits = Vec::new();
                if !row.seeds.is_empty() {
                    bits.push(format!("S:{}", row.seeds));
                }
                if !row.leech.is_empty() {
                    bits.push(format!("L:{}", row.leech));
                }
                if !row.size.is_empty() {
                    bits.push(row.size.clone());
                }
                let href = if row.href.starts_with("http") {
                    row.href.clone()
                } else {
                    format!("{}/{}", self.base, row.href.trim_start_matches('/'))
                };
                json!({
                    "id": href,
                    "title": row.title,
                    "overview": bits.join(" \u{b7} "),
                    "year": year_in(&row.title),
                    "poster": "",
                    "type": "movie",
                })
            })
            .collect();
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        if !(id.starts_with("http://") || id.starts_with("https://")) || !id.contains("/torrent/") {
            return Err(ApiError::bad_request(
                "details id must be a 1337x info-page url",
            ));
        }
        let html = self.fetch_html(ctx, id).await?;

        static MAGNET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static OG_TITLE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static OG_DESC: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static H1: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static TORRENT_FILE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static SEEDS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static LEECH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        static SIZE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

        let magnet_re =
            MAGNET.get_or_init(|| regex::Regex::new(r#"(?i)href=["'](magnet:\?[^"']+)["']"#).unwrap());
        let Some(magnet) = first_capture(magnet_re, &html, 1).map(|m| unescape(&m)) else {
            return Err(ApiError::Unavailable(
                "no magnet found on the info page".into(),
            ));
        };

        let og_title = OG_TITLE.get_or_init(|| {
            regex::Regex::new(r#"(?is)<meta\s+property=["']og:title["']\s+content=["'](.*?)["']"#)
                .unwrap()
        });
        let og_desc = OG_DESC.get_or_init(|| {
            regex::Regex::new(
                r#"(?is)<meta\s+(?:property|name)=["'](?:og:description|description)["']\s+content=["'](.*?)["']"#,
            )
            .unwrap()
        });
        let h1 = H1.get_or_init(|| regex::Regex::new(r"(?is)<h1[^>]*>(.*?)</h1>").unwrap());
        let torrent_file = TORRENT_FILE
            .get_or_init(|| regex::Regex::new(r#"(?i)href=["']([^"']+\.torrent)["']"#).unwrap());
        let seeds_re = SEEDS
            .get_or_init(|| regex::Regex::new(r"(?is)Seeds?.{0,120}?([\d][\d,\.]*)\s*<").unwrap());
        let leech_re = LEECH.get_or_init(|| {
            regex::Regex::new(r"(?is)Leechers?.{0,120}?([\d][\d,\.]*)\s*<").unwrap()
        });
        let size_re = SIZE.get_or_init(|| {
            regex::Regex::new(r"(?is)Size.{0,160}?([\d.,]+\s*[KMGT]?i?B)").unwrap()
        });

        let title = first_capture(og_title, &html, 1)
            .map(|t| unescape(&t).trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| {
                first_capture(h1, &html, 1).map(|t| strip_tags(&unescape(&t)))
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Unknown torrent".to_string());

        let seeds = first_capture(seeds_re, &html, 1)
            .map(|s| s.trim_end_matches([',', '.']).to_string())
            .unwrap_or_default();
        let leech = first_capture(leech_re, &html, 1)
            .map(|s| s.trim_end_matches([',', '.']).to_string())
            .unwrap_or_default();
        let size = first_capture(size_re, &html, 1).map(|s| collapse(&s)).unwrap_or_default();

        let mut overview_bits = Vec::new();
        if !seeds.is_empty() {
            overview_bits.push(format!("Seeders: {seeds}"));
        }
        if !leech.is_empty() {
            overview_bits.push(format!("Leechers: {leech}"));
        }
        if !size.is_empty() {
            overview_bits.push(format!("Size: {size}"));
        }

        let mut label = "magnet".to_string();
        if !size.is_empty() {
            label.push_str(&format!(" \u{b7} {size}"));
        }
        if !seeds.is_empty() {
            label.push_str(&format!(" \u{b7} S:{seeds}"));
        }

        let mut streams = vec![json!({ "label": label, "url": magnet, "kind": "torrent" })];
        if let Some(file) = first_capture(torrent_file, &html, 1) {
            let resolved = url::Url::parse(id)
                .ok()
                .and_then(|base| base.join(&unescape(&file)).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| unescape(&file));
            streams.push(json!({
                "label": if size.is_empty() {
                    ".torrent file".to_string()
                } else {
                    format!(".torrent file \u{b7} {size}")
                },
                "url": resolved,
                "kind": "torrent",
            }));
        }

        let description = first_capture(og_desc, &html, 1)
            .map(|d| strip_tags(&unescape(&d)))
            .unwrap_or_default();
        let overview = if overview_bits.is_empty() {
            description.chars().take(300).collect::<String>()
        } else {
            overview_bits.join(" \u{b7} ")
        };

        Ok(json!({
            "title": title,
            "overview": overview,
            "poster": "",
            "media_type": "movie",
            "year": year_in(&title),
            "streams": streams,
        }))
    }
}

// ----------------------------------------------------------------- LibGen

/// Library Genesis: books, comics and magazines, as files rather than pages.
///
/// This is what fills the gap the other book sources leave. Anna's Archive and
/// Z-Library both sit behind DDoS-Guard, so a server cannot read either one;
/// LibGen answers plain HTML, and its `get.php` redirects straight to a CDN
/// object that serves byte ranges. A result therefore ends in a real file the
/// reader opens in SPACE, not a download page somewhere else.
#[derive(Debug, Clone)]
pub struct LibGen {
    pub base: String,
}

impl LibGen {
    pub fn new() -> Self {
        Self {
            base: env_or("LIBGEN_BASE", "https://libgen.la"),
        }
    }

    /// What SPACE calls a file extension. Comic archives are not books: sending
    /// a `.cbz` to the book reader shows nothing.
    fn category_for(extension: &str) -> &'static str {
        match extension {
            "cbz" | "cbr" => "manga",
            "mp3" | "m4b" => "audiobook",
            _ => "book",
        }
    }

    /// The cells of one result row, tags stripped.
    fn cells(row: &str) -> Vec<String> {
        static CELL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = CELL.get_or_init(|| regex::Regex::new(r"(?is)<td[^>]*>(.*?)</td>").unwrap());
        re.captures_iter(row)
            .map(|c| strip_tags(&unescape(&c[1])))
            .collect()
    }

    /// The clean title.
    ///
    /// The first cell is a pile of links - series, edition, ISBN, badges - so
    /// its stripped text is unusable. Every one of those links carries the same
    /// tooltip, and the part after the `<br>` in it is the title on its own.
    fn title_of(row: &str) -> Option<String> {
        static TOOLTIP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re =
            TOOLTIP.get_or_init(|| regex::Regex::new("(?is)title=\"[^\"]*?<br>(.*?)\"").unwrap());
        let title = strip_tags(&unescape(&first_capture(re, row, 1)?));
        (!title.is_empty()).then_some(title)
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let limit = limit.clamp(1, 40);
        let html = ctx
            .get_text_with(
                &format!("{}/index.php?req={}", self.base, quote_plus(query)),
                BROWSER_HEADERS,
            )
            .await?;

        static ROW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let row_re = ROW.get_or_init(|| regex::Regex::new(r"(?is)<tr[^>]*>(.*?)</tr>").unwrap());
        static MD5: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        // Most rows link the download through `ads.php`; comics link `get.php`
        // directly. Matching only the latter found one result in thirty.
        let md5_re = MD5.get_or_init(|| {
            regex::Regex::new(r"(?i)(?:get|ads)\.php\?md5=([a-f0-9]{32})").unwrap()
        });

        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for row in row_re.captures_iter(&html) {
            let row = &row[1];
            // No download link means it is a header row, not a result.
            let Some(md5) = first_capture(md5_re, row, 1) else {
                continue;
            };
            let Some(title) = Self::title_of(row) else {
                continue;
            };
            if !seen.insert(md5.to_lowercase()) {
                continue;
            }

            let cells = Self::cells(row);
            let cell = |index: usize| cells.get(index).map(String::as_str).unwrap_or("").trim();
            let author = cell(1);
            let year = cell(3);
            let size = cell(6);
            let extension = cell(7).to_lowercase();

            let overview = [author, size, extension.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" \u{b7} ");

            out.push(json!({
                // The extension travels in the id: it decides which reader the
                // front end opens, and details would otherwise have to fetch
                // the row again just to learn it.
                "id": format!("libgen:{md5}:{extension}"),
                "title": title,
                "overview": overview,
                "year": year_in(year),
                "poster": "",
                "type": Self::category_for(&extension),
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let Some(rest) = id.strip_prefix("libgen:") else {
            return Err(ApiError::bad_request("id must be libgen:{md5}:{ext}"));
        };
        let (md5, extension) = rest.split_once(':').unwrap_or((rest, ""));
        if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ApiError::bad_request("id must carry a 32-character md5"));
        }

        Ok(json!({
            "title": "Book",
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": Self::category_for(extension),
            "streams": [{
                "label": if extension.is_empty() {
                    "download".to_string()
                } else {
                    format!("{extension} \u{b7} LibGen")
                },
                "url": self.download_url(ctx, md5).await,
                "kind": "file",
            }],
        }))
    }

    /// The url that actually serves the file.
    ///
    /// Most titles are only downloadable through a one-time key minted by
    /// `ads.php`; the bare `get.php?md5=` works for some rows and is the
    /// fallback when the key cannot be read. Either way the answer redirects
    /// to a CDN object that honours byte ranges, so the reader can seek.
    async fn download_url(&self, ctx: &EngineCtx, md5: &str) -> String {
        let plain = format!("{}/get.php?md5={md5}", self.base);
        let Ok(html) = ctx
            .get_text_with(
                &format!("{}/ads.php?md5={md5}", self.base),
                BROWSER_HEADERS,
            )
            .await
        else {
            return plain;
        };

        static KEYED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = KEYED.get_or_init(|| {
            regex::Regex::new(r"(?i)get\.php\?md5=[a-f0-9]{32}&key=[A-Z0-9]+").unwrap()
        });
        match re.find(&html) {
            Some(m) => format!("{}/{}", self.base, m.as_str()),
            None => plain,
        }
    }
}

#[cfg(test)]
mod libgen_tests {
    use super::*;

    /// One row, shaped the way the site actually writes them.
    const ROW: &str = "<td><b><a href=\"series.php?id=1\">Dune</a><a data-toggle=\"tooltip\" title=\"Add/Edit : 2019-01-20; ID: 2127185<br>Dune The Official Comic Book\" href=\"edition.php?id=317043\"><i>1984-jan</i></a></b></td>\
<td>Macchio, Ralph (sc.);</td><td><a href=\"publisher.php?id=1\">Marvel</a></td><td><nobr>1984 January</nobr></td><td>English</td><td>68</td><td><nobr><a href=\"/file.php?id=2127185\">26 MB</a></nobr></td><td>cbz</td>\
<td><a href=\"/get.php?md5=e0d1e178d828288622b7ee6b8dc4dfbe\">get</a></td>";

    #[test]
    fn the_title_comes_out_of_the_tooltip_not_the_link_pile() {
        // Stripping the first cell yields "Dune 1984-jan"; the tooltip is the
        // only place the real title appears on its own.
        assert_eq!(
            LibGen::title_of(ROW).as_deref(),
            Some("Dune The Official Comic Book")
        );
    }

    #[test]
    fn the_columns_line_up_with_what_they_mean() {
        let cells = LibGen::cells(ROW);
        assert_eq!(cells[1].trim(), "Macchio, Ralph (sc.);");
        assert_eq!(cells[3].trim(), "1984 January");
        assert_eq!(cells[6].trim(), "26 MB");
        assert_eq!(cells[7].trim(), "cbz");
    }

    #[test]
    fn a_comic_archive_is_not_filed_as_a_book() {
        // A .cbz handed to the book reader renders nothing.
        assert_eq!(LibGen::category_for("cbz"), "manga");
        assert_eq!(LibGen::category_for("cbr"), "manga");
        assert_eq!(LibGen::category_for("m4b"), "audiobook");
        assert_eq!(LibGen::category_for("epub"), "book");
        assert_eq!(LibGen::category_for(""), "book");
    }
}
