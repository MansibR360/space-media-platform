//! Multi-provider lyrics pipeline.
//!
//! Fans a lookup across every popular lyrics backend in priority order and
//! normalizes the winners into one shape:
//!
//! - `betterlyrics` — betterlyrics.org API (word-synced TTML, the same
//!   backend powering the Better Lyrics extension for YouTube Music)
//! - `lrclib`       — lrclib.net (open synced LRC + plain text)
//! - `netease`      — NetEase Cloud Music (LRC, strong for CJK catalogs)
//! - `genius`       — Genius page scrape (plain text, widest western catalog)
//!
//! Results are cached in memory (hot path) and in Postgres (`lyrics_cache`)
//! so repeat lookups never leave the server. "Lyrics as media" is served by
//! [`crate::lyrics::handlers::media_lyrics`], which resolves from any catalog
//! item's title and links the cached row back to the media id.

pub mod handlers;
pub mod providers;
pub mod routes;

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// One lyric line; `time` is seconds from track start when synced.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LyricsLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<f64>,
    pub text: String,
}

/// Normalized lyrics payload — whatever the winning provider returned.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LyricsPayload {
    /// provider that produced this result: betterlyrics | lrclib | netease | genius
    pub source: String,
    pub title: String,
    pub artist: String,
    pub synced: bool,
    /// lrc | ttml | plain
    pub format: String,
    pub instrumental: bool,
    #[serde(default)]
    pub lines: Vec<LyricsLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_lrc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttml: Option<String>,
}

impl LyricsPayload {
    fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn from_json(v: &Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }
}

/// What the client asks with. Any combination works; providers use what they
/// support (`video_id` routes straight at Better Lyrics-style lookups).
#[derive(Debug, Clone, Default, utoipa::IntoParams, utoipa::ToSchema)]
pub struct LyricsQuery {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// track duration in seconds — sharpens LRCLIB exact matching
    pub duration: Option<f64>,
    /// YouTube video id — enables id-keyed lookup paths
    pub video_id: Option<String>,
}

impl LyricsQuery {
    /// Cache identity: video-id keyed queries are distinct from text queries.
    pub fn cache_key(&self) -> String {
        if let Some(v) = self.video_id.as_deref().filter(|v| !v.trim().is_empty()) {
            return format!("yt:{v}");
        }
        let dur = self.duration.map(|d| format!("{d:.0}")).unwrap_or_default();
        format!(
            "{}|{}|{}|{}",
            normalize(&self.title.as_deref().unwrap_or("")),
            normalize(&self.artist.as_deref().unwrap_or("")),
            normalize(&self.album.as_deref().unwrap_or("")),
            dur
        )
    }
}

fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

// ------------------------------------------------------------- in-mem cache

struct CacheEntry {
    at: Instant,
    payload: Option<LyricsPayload>, // None = negative cache (not found)
}

const MEM_TTL: Duration = Duration::from_secs(60 * 60 * 6);

fn mem_cache() -> &'static dashmap::DashMap<String, CacheEntry> {
    static CACHE: std::sync::LazyLock<dashmap::DashMap<String, CacheEntry>> =
        std::sync::LazyLock::new(dashmap::DashMap::new);
    &CACHE
}

// -------------------------------------------------------------- orchestration

/// Ordered provider chain — first hit wins.
const PROVIDERS: &[&str] = &["betterlyrics", "lrclib", "netease", "genius"];

pub fn provider_list() -> Vec<Value> {
    vec![
        serde_json::json!({
            "id": "betterlyrics", "label": "Better Lyrics",
            "url": "https://betterlyrics.org",
            "api": "https://lyrics-api.boidu.dev/getLyrics",
            "format": "ttml", "synced": "word", "priority": 1,
            "notes": "Word-synced TTML via Musixmatch-backed Better Lyrics API",
        }),
        serde_json::json!({
            "id": "lrclib", "label": "LRCLIB",
            "url": "https://lrclib.net",
            "api": "https://lrclib.net/api/search",
            "format": "lrc", "synced": "line", "priority": 2,
            "notes": "Open community database, synced + plain",
        }),
        serde_json::json!({
            "id": "netease", "label": "NetEase Cloud Music",
            "url": "https://music.163.com",
            "api": "https://music.163.com/api/song/lyric",
            "format": "lrc", "synced": "line", "priority": 3,
            "notes": "Strong CJK coverage; may be region-gated",
        }),
        serde_json::json!({
            "id": "genius", "label": "Genius",
            "url": "https://genius.com",
            "api": "https://genius.com/api/search/multi",
            "format": "plain", "synced": "no", "priority": 4,
            "notes": "Widest western catalog; page-scraped plain text",
        }),
    ]
}

/// Full lookup: memory → DB → live providers (in priority order).
pub async fn resolve(state: &AppState, query: &LyricsQuery) -> ApiResult<LyricsQueryResult> {
    if query.title.as_deref().map(str::trim).unwrap_or("").is_empty()
        && query.video_id.as_deref().map(str::trim).unwrap_or("").is_empty()
    {
        return Err(ApiError::bad_request(
            "title (or artist+title), or video_id is required",
        ));
    }

    let key = query.cache_key();

    // 1. hot memory
    if let Some(entry) = mem_cache().get(&key)
        && entry.at.elapsed() < MEM_TTL
    {
        return Ok(LyricsQueryResult {
            found: entry.payload.is_some(),
            cached: true,
            payload: entry.payload.clone(),
        });
    }

    // 2. persisted cache
    if let Ok(Some((source, content))) =
        sqlx::query_as::<_, (String, Value)>(
            "SELECT source, content FROM lyrics_cache WHERE cache_key = $1",
        )
        .bind(&key)
        .fetch_optional(&state.pool)
        .await
        && let Some(payload) = LyricsPayload::from_json(&content)
    {
        tracing::debug!(provider = %source, "lyrics served from db cache");
        mem_cache().insert(key.clone(), CacheEntry { at: Instant::now(), payload: Some(payload.clone()) });
        return Ok(LyricsQueryResult { found: true, cached: true, payload: Some(payload) });
    }

    // 3. live fan-chain
    let payload = fetch_live(query).await;
    let found = payload.is_some();
    persist(state, &key, query, None, payload.as_ref()).await;
    mem_cache().insert(key, CacheEntry { at: Instant::now(), payload: payload.clone() });
    Ok(LyricsQueryResult { found, cached: false, payload })
}

/// Same as [`resolve`] but attributes the cached row to a catalog item
/// ("lyrics as media") when one exists.
pub async fn resolve_for_media(
    state: &AppState,
    query: &LyricsQuery,
    media_id: uuid::Uuid,
) -> ApiResult<LyricsQueryResult> {
    let mut keyed = query.clone();
    keyed.video_id = keyed
        .video_id
        .clone()
        .filter(|v| !v.trim().is_empty());
    let result = resolve(state, &keyed).await?;
    if !result.cached
        && let Some(payload) = &result.payload
    {
        let key = keyed.cache_key();
        persist(state, &key, &keyed, Some(media_id), Some(payload)).await;
    }
    Ok(result)
}

async fn fetch_live(query: &LyricsQuery) -> Option<LyricsPayload> {
    for provider in PROVIDERS {
        let result = match *provider {
            "betterlyrics" => providers::betterlyrics::fetch(query).await,
            "lrclib" => providers::lrclib::fetch(query).await,
            "netease" => providers::netease::fetch(query).await,
            "genius" => providers::genius::fetch(query).await,
            _ => continue,
        };
        match result {
            Ok(Some(payload)) => {
                metrics::counter!("lyrics_fetch_total", "provider" => *provider).increment(1);
                return Some(payload);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(provider, error = %err, "lyrics provider skipped");
                metrics::counter!("lyrics_provider_failures_total", "provider" => *provider)
                    .increment(1);
            }
        }
    }
    metrics::counter!("lyrics_miss_total").increment(1);
    None
}

async fn persist(
    state: &AppState,
    key: &str,
    query: &LyricsQuery,
    media_id: Option<uuid::Uuid>,
    payload: Option<&LyricsPayload>,
) {
    // Negative rows are stored as empty JSON so repeat misses stay cheap.
    let content = payload
        .map(LyricsPayload::to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    let (source, title, artist, synced, format, video_id) = match payload {
        Some(p) => (
            p.source.as_str(),
            p.title.as_str(),
            p.artist.as_str(),
            p.synced,
            p.format.as_str(),
            query.video_id.as_deref(),
        ),
        None => ("miss", "", "", false, "plain", query.video_id.as_deref()),
    };
    let result = sqlx::query(
        "INSERT INTO lyrics_cache
            (id, cache_key, source, title, artist, media_id, video_id, synced, format, content)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (cache_key) DO UPDATE SET
            source = EXCLUDED.source, title = EXCLUDED.title, artist = EXCLUDED.artist,
            media_id = COALESCE(EXCLUDED.media_id, lyrics_cache.media_id),
            video_id = COALESCE(EXCLUDED.video_id, lyrics_cache.video_id),
            synced = EXCLUDED.synced, format = EXCLUDED.format,
            content = EXCLUDED.content, created_at = now()",
    )
    .bind(crate::utils::new_id())
    .bind(key)
    .bind(source)
    .bind(title)
    .bind(artist)
    .bind(media_id)
    .bind(video_id)
    .bind(synced)
    .bind(format)
    .bind(content)
    .execute(&state.pool)
    .await;
    if let Err(err) = result {
        tracing::debug!(error = %err, "lyrics cache write failed");
    }
}

/// Public answer envelope around the optional payload.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LyricsQueryResult {
    pub found: bool,
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<LyricsPayload>,
}

// ------------------------------------------------------------------ parsers

/// Parses standard LRC timestamps into timed lines. Header metadata
/// (`[ar:]`, `[offset:]`, …) is dropped; bare-text lines survive unsynced.
pub(crate) fn parse_lrc(lrc: &str) -> Vec<LyricsLine> {
    let mut lines = Vec::new();
    for raw in lrc.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut time: Option<f64> = None;
        let mut rest = trimmed;

        // Consume leading bracket groups: timestamps accumulate (last wins),
        // non-timestamp tags before any timestamp mark the line as metadata.
        while rest.starts_with('[') && rest.contains(']') {
            let close = rest.find(']').expect("contains ]");
            match parse_lrc_stamp(&rest[1..close]) {
                Ok(t) => {
                    time = Some(t);
                    rest = rest[close + 1..].trim_start();
                }
                Err(_) => {
                    if time.is_none() {
                        // Metadata-only line ([ar:…], [by:…], …).
                        rest = "";
                    }
                    break;
                }
            }
        }

        if time.is_none() && rest.is_empty() {
            continue;
        }
        lines.push(LyricsLine { time, text: rest.trim().to_string() });
    }
    lines
}

fn parse_lrc_stamp(stamp: &str) -> Result<f64, ()> {
    let (mins, secs) = stamp.split_once(':').ok_or(())?;
    let minutes: f64 = mins.trim().parse().map_err(|_| ())?;
    let seconds: f64 = secs.trim().parse().map_err(|_| ())?;
    let total = minutes * 60.0 + seconds;
    if !(0.0..=600_000.0).contains(&total) {
        return Err(());
    }
    Ok((total * 100.0).round() / 100.0)
}

/// Minimal TTML reader: line-level begin times from `<p>` nodes, text joined
/// from nested word `<span>`s. Word timings stay available through `ttml`.
pub(crate) fn parse_ttml(xml: &str) -> Vec<LyricsLine> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for node in doc.descendants() {
        if node.tag_name().name() != "p" {
            continue;
        }
        let begin = node
            .attribute("begin")
            .and_then(parse_ttml_clock)
            .or_else(|| node.attribute("begin").and_then(|b| b.parse::<f64>().ok()));
        let text = node
            .descendants()
            .filter(|n| n.is_text())
            .map(|n| n.text().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() {
            lines.push(LyricsLine { time: begin, text });
        }
    }
    lines
}

/// Accepts both clock values ("00:01:09.731") and bare seconds ("9.731").
fn parse_ttml_clock(raw: &str) -> Option<f64> {
    let parts: Vec<&str> = raw.split(':').collect();
    match parts.len() {
        3 => {
            let h: f64 = parts[0].trim().parse().ok()?;
            let m: f64 = parts[1].trim().parse().ok()?;
            let s: f64 = parts[2].trim().parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        2 => {
            let m: f64 = parts[0].trim().parse().ok()?;
            let s: f64 = parts[1].trim().parse().ok()?;
            Some(m * 60.0 + s)
        }
        _ => raw.trim().parse::<f64>().ok(),
    }
}

/// Strips HTML tags/entities for scraped plain-text providers; block-level
/// breaks become newlines so verse structure survives.
pub(crate) fn html_to_text(html: &str) -> String {
    static TAGS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]*>").expect("tag regex"));
    static BREAKS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)<br\s*/?>|</p>").expect("br regex"));

    let with_breaks = BREAKS.replace_all(html, "\n");
    let no_tags = TAGS.replace_all(&with_breaks, "");
    no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

// ------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_timestamps_parse() {
        let lrc = "[00:12.00]first line\n[01:02.50]second line\n[ar:Artist]\n[by:Bot]\n";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time, Some(12.0));
        assert_eq!(lines[0].text, "first line");
        assert_eq!(lines[1].time, Some(62.5));

        // Metadata tags never leak into lyric lines.
        let with_meta = "[ti:Song]\n[00:03.00]hook\n";
        let parsed = parse_lrc(with_meta);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "hook");
    }

    #[test]
    fn ttml_lines_extract_with_times() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div>
            <p begin="9.731"><span begin="9.731">The</span> <span>club</span></p>
            <p begin="00:01:12.105">next</p>
        </div></body></tt>"#;
        let lines = parse_ttml(xml);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time, Some(9.731));
        assert_eq!(lines[0].text, "The club");
        assert_eq!(lines[1].time, Some(72.105));
    }

    #[test]
    fn cache_keys_are_stable_and_normalized() {
        let a = LyricsQuery {
            title: Some("Shape of You!".into()),
            artist: Some("Ed Sheeran".into()),
            ..Default::default()
        };
        let b = LyricsQuery {
            title: Some("shape   of you".into()),
            artist: Some("ed sheeran ".into()),
            ..Default::default()
        };
        assert_eq!(a.cache_key(), b.cache_key());
        let yt = LyricsQuery { video_id: Some("abc123".into()), ..Default::default() };
        assert_eq!(yt.cache_key(), "yt:abc123");
    }

    #[test]
    fn html_entities_decode() {
        // <br/> becomes a newline; entities decode.
        let out = html_to_text("a &amp; b<br/>c &#39;d&#39;");
        assert_eq!(out.lines().count(), 2);
        assert_eq!(out.lines().next(), Some("a & b"));
        assert!(out.contains("c 'd'"));
    }

    #[test]
    fn video_id_extraction_covers_common_shapes() {
        use crate::sources::youtube::test_hooks as yt;
        for input in [
            "dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?t=1",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                yt::extract_video_id_for_tests(input).as_deref(),
                Some("dQw4w9WgXcQ"),
                "failed for {input}"
            );
        }
        assert_eq!(yt::extract_video_id_for_tests("not a video"), None);
    }

    #[test]
    fn json_blob_extractor_finds_player_response() {
        let html = r#"<html><script>var ytInitialPlayerResponse = {"streamingData":{"formats":[]}};</script></html>"#;
        let blob = crate::sources::youtube::test_hooks::extract_json_blob_for_tests(
            html,
            "ytInitialPlayerResponse",
        );
        assert!(blob.is_some());
        assert!(blob.unwrap().contains_key("streamingData"));
    }
}
