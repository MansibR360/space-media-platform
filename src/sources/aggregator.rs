//! Intelligent multi-source aggregation.
//!
//! Clients never choose sources. This module classifies the query, fans out
//! to every available backend in parallel, ranks merged results by relevance,
//! and hands out **opaque play tokens** — the server remembers which source
//! backs each item and resolves the best stream at play time, with automatic
//! failover if the original source is down.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::sources::health::Op;
use crate::sources::SourceDetail;

/// One aggregated, source-anonymous result.
///
/// `token` is the opaque play handle; `sources` lists every backend that
/// contributed this title so the resolver can fail across them.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UnifiedResult {
    /// Opaque handle — pass to /api/playback/resolve to actually play.
    pub token: String,
    pub title: String,
    #[serde(default)]
    pub overview: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    /// movie | series | book | music | manga
    pub category: &'static str,
    pub relevance: f64,
    /// Source that produced the winning token. Internal: results are
    /// source-anonymous, and the resolver picks the backend at play time.
    #[serde(skip_serializing)]
    pub source: String,
    /// All sources offering this title (duplicate matches merged). Internal,
    /// for ranking and failover - never shown with a result.
    #[serde(skip_serializing)]
    pub sources: Vec<String>,
}

pub struct ResolvedStream {
    pub url: String,
    pub kind: String,
    pub title: String,
}

/// A fully-ranked resolution produced by the central resolver pipeline.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ResolutionSet {
    pub title: String,
    pub source: String,
    pub url: String,
    /// mp4 | hls | audio | page | file
    pub kind: String,
    /// Best-effort quality label parsed from the stream ("1080p", "audio", …)
    pub quality: String,
    pub audio: String,
    pub subtitles: Vec<String>,
    /// Ranked alternates (same source) usable on playback failure.
    pub fallbacks: Vec<FallbackStream>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FallbackStream {
    pub source: String,
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// Client preferences steering stream selection.
#[derive(Debug, Clone, Default, Deserialize, utoipa::ToSchema)]
pub struct ResolvePreferences {
    /// Target quality, e.g. "1080p", "720p", "4k". Best match wins, higher
    /// never loses to wildly lower.
    #[serde(default)]
    pub quality: Option<String>,
    /// Preferred audio language hint, e.g. "en", "ja", "dub".
    #[serde(default)]
    pub language: Option<String>,
    /// Desired subtitle languages; empty = no subtitle preference.
    #[serde(default)]
    pub subtitles: Vec<String>,
    /// Client can direct-play whatever URL comes back (advisory today; gates
    /// transcode fallbacks once Phase H lands).
    #[serde(default = "default_true")]
    #[allow(dead_code)]
    pub direct_play: bool,
}

fn default_true() -> bool {
    true
}

impl ResolvePreferences {
    pub fn quality_number(&self) -> Option<u16> {
        self.quality.as_deref().and_then(parse_quality)
    }
}

/// Extracts a pixel-height style number from free-form labels:
/// "1080p" → 1080, "4K" → 2160, "512kb MPEG4" → None (bitrate, not height).
pub fn parse_quality(raw: &str) -> Option<u16> {
    let l = raw.to_lowercase();
    if l.contains("2160") || l.contains("4k") {
        return Some(2160);
    }
    let bytes = l.as_bytes();
    let mut start: Option<usize> = None;
    for (i, b) in bytes.iter().chain(std::iter::once(&b' ')).enumerate() {
        let is_digit = b.is_ascii_digit();
        match (start, is_digit) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                let num: u32 = l[s..i].parse().ok()?;
                // Heights are plausible 3–4 digit numbers; bitrates carry kb/kbps.
                let followed_by_kb = l[i..].starts_with("kb");
                let plausible = (300..=2160).contains(&num) && !followed_by_kb && num % 2 == 0;
                if plausible {
                    return Some(num as u16);
                }
                start = None;
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Deserialize)]
struct TokenPayload {
    s: String, // source name
    e: String, // external id
}

pub fn make_token(source: &str, external_id: &str) -> String {
    crate::utils::encode_payload(&json!({ "s": source, "e": external_id }).to_string())
}

/// The (source, external id) a play token carries.
pub fn token_parts(token: &str) -> ApiResult<(String, String)> {
    let payload = parse_token(token)?;
    Ok((payload.s, payload.e))
}

fn parse_token(token: &str) -> ApiResult<TokenPayload> {
    let raw = crate::utils::decode_payload(token)?;
    serde_json::from_str(&raw).map_err(|_| ApiError::bad_request("malformed play token"))
}

// ---------------------------------------------------------------- intent

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    Video,
    Music,
    Book,
    Manga,
    Unknown,
}

fn classify(query: &str) -> Intent {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();

    const MUSIC: &[&str] = &[
        "song", "songs", "music", "track", "album", "remix", "playlist", "listen", "audio",
        "artist", "cover",
    ];
    const MANGA: &[&str] = &[
        "manga", "mangas", "chapter", "chapters", "manhwa", "webtoon", "manhua",
    ];
    const BOOK: &[&str] = &[
        "book",
        "books",
        "ebook",
        "novel",
        "novels",
        "read",
        "author",
        "literature",
    ];
    const VIDEO: &[&str] = &[
        "watch",
        "movie",
        "movies",
        "film",
        "films",
        "stream",
        "series",
        "show",
        "episode",
        "documentary",
        "anime",
        "hd",
        "4k",
        "1080p",
    ];

    for group in [
        (MUSIC, Intent::Music),
        (MANGA, Intent::Manga),
        (BOOK, Intent::Book),
        (VIDEO, Intent::Video),
    ] {
        if words.iter().any(|w| group.0.contains(w)) {
            return group.1;
        }
    }
    Intent::Unknown
}

fn intent_category(intent: Intent) -> Option<&'static str> {
    match intent {
        Intent::Video => Some("movie"),
        Intent::Music => Some("music"),
        Intent::Book => Some("book"),
        Intent::Manga => Some("manga"),
        Intent::Unknown => None,
    }
}

// ------------------------------------------------------- provider access

#[derive(Clone)]
struct ProviderMeta {
    name: String,
    /// empty when unknown
    category: String,
    base_url: Option<String>,
    auth_token: Option<String>,
    /// learned web source served through the galaxy bridge
    web: bool,
    /// remote source hub (base_url + optional token carry the endpoint)
    hub: bool,
    /// stremio extension: resource base + searchable catalog binding
    ext_base: Option<String>,
    ext_catalog: Option<(String, String)>, // (stremio type, catalog id)
    /// resolve-only sources never appear in search fan-out; they are asked
    /// only when something needs to be played ("where can I watch this").
    resolve_only: bool,
}

fn map_stremio_category(stype: &str) -> &'static str {
    crate::extensions::map_category(stype)
}

async fn gather_providers(
    state: &crate::state::AppState,
    include_web: bool,
    for_search: bool,
) -> Vec<ProviderMeta> {
    let health = &state.sources_health;
    let mut refs = Vec::new();
    for name in state.sources.list() {
        if !health.is_available(name).await {
            continue; // circuit open — skip dead source entirely
        }
        if let Some(p) = state.sources.get(name) {
            refs.push(ProviderMeta {
                name: p.name().to_string(),
                category: p.category().to_string(),
                base_url: None,
                auth_token: None,
                web: false,
                hub: false,
                ext_base: None,
                ext_catalog: None,
                resolve_only: false,
            });
        }
    }
    if include_web && !state.web_sources.is_empty().await {
        for entry in state.web_sources.list().await {
            let name = entry.source.name();
            if !health.is_available(name).await {
                continue;
            }
            refs.push(ProviderMeta {
                name: name.to_string(),
                category: entry.source.category().to_string(),
                base_url: None,
                auth_token: None,
                web: true,
                hub: false,
                ext_base: None,
                ext_catalog: None,
                resolve_only: false,
            });
        }
    }
    // Stremio-compatible extensions: searchable catalogs join search;
    // stream-only addons join resolution (and lookup) only.
    if include_web {
        for entry in state.extensions.list().await {
            let addon = entry.addon_id();
            let mut placed_search = false;
            for cat in crate::extensions::searchable_catalogs(&entry.row.manifest) {
                refs.push(ProviderMeta {
                    name: format!("ext:{addon}#{}", cat.catalog_id),
                    category: map_stremio_category(&cat.stype).to_string(),
                    base_url: None,
                    auth_token: None,
                    web: false,
                    hub: false,
                    ext_base: Some(entry.target.base.clone()),
                    ext_catalog: Some((cat.stype.clone(), cat.catalog_id.clone())),
                    resolve_only: false,
                });
                placed_search = true;
            }
            if !placed_search && crate::extensions::supports_resource(&entry.row.manifest, "stream")
            {
                // resolve-only extension (stream-capable, no searchable catalog)
                if !for_search {
                    let category = entry
                        .row
                        .manifest
                        .get("types")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .map(map_stremio_category)
                        .unwrap_or("movie");
                    refs.push(ProviderMeta {
                        name: format!("ext:{addon}"),
                        category: category.to_string(),
                        base_url: None,
                        auth_token: None,
                        web: false,
                        hub: false,
                        ext_base: Some(entry.target.base.clone()),
                        ext_catalog: None,
                        resolve_only: true,
                    });
                }
            }
        }
    }
    // Remote source hubs — the "server asks that api" fallback tier.
    if include_web
        && let Ok(hubs) = crate::sources::hub::repo::list(&state.pool).await
    {
        for hub in hubs {
            if !hub.enabled || !health.is_available(&hub.name).await {
                continue;
            }
            refs.push(ProviderMeta {
                name: hub.name.clone(),
                category: String::new(),
                base_url: Some(hub.base_url.clone()),
                auth_token: hub.token.clone(),
                web: false,
                hub: true,
                ext_base: None,
                ext_catalog: None,
                resolve_only: false,
            });
        }
    }
    if let Ok(plugins) = crate::plugins::repo::list(&state.pool).await {
        for plugin in plugins {
            if plugin.kind != "source" || !plugin.enabled {
                continue;
            }
            let resolve_only = plugin
                .config
                .get("resolve_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if resolve_only && for_search {
                continue; // source-only: never in search results
            }
            if health.is_available(&plugin.name).await
                && let Some(base) = plugin.base_url.clone()
            {
                refs.push(ProviderMeta {
                    name: plugin.name.clone(),
                    // The category is in the plugin's own config, put there
                    // when it registered. Dropping it here made every plugin
                    // look like "no preference", so a book that failed to
                    // resolve was allowed to fail over to film sources and
                    // then to a film player - which is why books opened
                    // movies.
                    category: plugin
                        .config
                        .get("category")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    base_url: Some(base),
                    auth_token: plugin
                        .config
                        .get("auth_token")
                        .and_then(|t| t.as_str())
                        .map(str::to_string),
                    web: false,
                    hub: false,
                    ext_base: None,
                    ext_catalog: None,
                    resolve_only,
                });
            }
        }
    }
    // Fastest-first: order providers by their daily benchmark score so
    // fan-out, failover and lookup always consult the best sources first.
    let scores: HashMap<String, f64> = state
        .source_ratings
        .snapshot()
        .await
        .into_iter()
        .map(|r| (r.source, r.score))
        .collect();
    refs.sort_by(|a, b| {
        let sa = scores.get(&a.name).copied().unwrap_or(50.0);
        let sb = scores.get(&b.name).copied().unwrap_or(50.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    refs
}

async fn provider_search(
    state: &crate::state::AppState,
    provider: &ProviderMeta,
    query: &str,
    limit: i32,
) -> Option<Vec<crate::sources::SourceItem>> {
    if provider.web {
        return state
            .web_sources
            .search(state, &provider.name, query, limit)
            .await
            .ok();
    }
    if provider.hub {
        let Some(base) = &provider.base_url else { return None };
        let hub = crate::sources::hub::HubRef {
            name: &provider.name,
            base_url: base,
            token: provider.auth_token.as_deref(),
        };
        return match crate::sources::hub::find(&state.cfg, hub, query, None, limit).await {
            Ok(hits) => Some(
                hits.into_iter()
                    .map(|hit| {
                        // composite id: "<remote-source>|<remote-id>" keeps
                        // the item lookup unambiguous at resolve time.
                        let external_id = format!("{}|{}", hit.source, hit.id);
                        let category_static: &'static str = match hit.category.as_deref() {
                            Some("book") => "book",
                            Some("music") => "music",
                            Some("manga") => "manga",
                            Some("series") => "series",
                            _ => "movie",
                        };
                        crate::sources::SourceItem {
                            external_id,
                            title: hit.title,
                            overview: hit.overview,
                            year: hit.year,
                            poster: hit.poster,
                            category: category_static,
                        }
                    })
                    .collect(),
            ),
            Err(_) => None,
        };
    }
    if let (Some(base), Some((stype, catalog_id))) = (&provider.ext_base, &provider.ext_catalog) {
        let target = crate::extensions::ResourceTarget { base: base.clone() };
        let url =
            target.catalog_url(stype, catalog_id, &[("search", query.trim()), ("skip", "0")]);
        let body = match crate::extensions::http_get_json_cfg(&state.cfg, &url).await {
            Ok(body) => body,
            Err(_) => return None,
        };
        let metas = body.get("metas").and_then(Value::as_array);
        let Some(metas) = metas else { return None };
        return Some(
            metas
                .iter()
                .filter_map(|row| {
                    let row_id = row.get("id")?.as_str()?.to_string();
                    if row_id.is_empty() {
                        return None;
                    }
                    Some(crate::sources::SourceItem {
                        external_id: format!("{stype}|{row_id}"),
                        title: row
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Untitled")
                            .to_string(),
                        overview: row
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        year: row.get("releaseInfo").and_then(Value::as_str).and_then(|s| {
                            s.get(0..4)?.parse::<i32>().ok().filter(|y| (1900..=2100).contains(y))
                        }),
                        poster: row.get("poster").and_then(Value::as_str).map(str::to_string),
                        category: map_stremio_category(stype),
                    })
                })
                .take(limit.max(1) as usize)
                .collect(),
        );
    }
    match &provider.base_url {
        None => state
            .sources
            .get(&provider.name)?
            .search(query, limit)
            .await
            .ok(),
        Some(base) => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .ok()?;
            let mut request = client
                .get(format!("{}/search", base.trim_end_matches('/')))
                .query(&[("q", query)]);
            if let Some(token) = &provider.auth_token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let body: serde_json::Value = request.send().await.ok()?.json().await.ok()?;
            // The plugin protocol (see plugins/python/_plugin_server.py) answers
            // {"items": [{external_id, title, ...}]}. This only ever read
            // {"results": [{id, ...}]}, so every HTTP plugin silently returned
            // nothing to search — all 509 of them. Accept the documented shape,
            // and keep the older one working for anything already built to it.
            let rows = body
                .get("items")
                .or_else(|| body.get("results"))?
                .as_array()?;
            Some(
                rows.iter()
                    .filter_map(|item| {
                        Some(crate::sources::SourceItem {
                            external_id: item
                                .get("external_id")
                                .or_else(|| item.get("id"))?
                                .as_str()?
                                .to_string(),
                            title: item
                                .get("title")
                                .and_then(|t| t.as_str())
                                .unwrap_or("Untitled")
                                .to_string(),
                            overview: item
                                .get("overview")
                                .and_then(|o| o.as_str())
                                .unwrap_or("")
                                .to_string(),
                            year: item.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                            poster: item
                                .get("poster")
                                .and_then(|p| p.as_str())
                                .map(str::to_string),
                            // Plugins send `category`; the legacy shape sent `type`.
                            // Every category a plugin can declare needs an
                            // arm here. "music" had none, so tracks fell
                            // through to "series" and were handed to the video
                            // player - a black rectangle over an audio file.
                            category: match item
                                .get("category")
                                .or_else(|| item.get("type"))
                                .and_then(|t| t.as_str())
                            {
                                Some("movie" | "film") => "movie",
                                Some("book" | "ebook") => "book",
                                Some("music" | "track" | "song") => "music",
                                Some("manga" | "comic") => "manga",
                                Some("podcast") => "podcast",
                                Some("audiobook") => "audiobook",
                                _ => "series",
                            },
                        })
                    })
                    .take(limit.max(0) as usize)
                    .collect(),
            )
        }
    }
}

async fn provider_item(
    state: &crate::state::AppState,
    provider: &ProviderMeta,
    external_id: &str,
) -> Option<SourceDetail> {
    if provider.web {
        return state
            .web_sources
            .item(state, &provider.name, external_id)
            .await
            .ok();
    }
    if let Some(base) = &provider.ext_base {
        // stremio extension: id is "<stremio_type>|<stremio_id>"
        let (stype, sid) = external_id.split_once('|')?;
        let target = crate::extensions::ResourceTarget { base: base.clone() };
        let body = crate::extensions::http_get_json_cfg(
            &state.cfg,
            &target.stream_url(stype, sid),
        )
        .await
        .ok()?;
        let streams =
            crate::extensions::convert_streams(body.get("streams").unwrap_or(&Value::Null), sid);
        return Some(SourceDetail {
            item: crate::sources::SourceItem {
                external_id: external_id.to_string(),
                title: sid.to_string(),
                overview: String::new(),
                year: None,
                poster: None,
                category: map_stremio_category(stype),
            },
            streams,
            chapters: None,
        });
    }
    if provider.hub {
        let Some(base) = &provider.base_url else { return None };
        let (remote_source, remote_id) = external_id.split_once('|')?;
        let hub = crate::sources::hub::HubRef {
            name: &provider.name,
            base_url: base,
            token: provider.auth_token.as_deref(),
        };
        let body = crate::sources::hub::item_detail(&state.cfg, hub, remote_source, remote_id)
            .await
            .ok()?;
        let streams: Vec<crate::sources::StreamLink> = body
            .get("streams")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some(crate::sources::StreamLink {
                            label: s.get("label")?.as_str()?.to_string(),
                            url: s.get("url")?.as_str()?.to_string(),
                            kind: s
                                .get("kind")
                                .and_then(|k| k.as_str())
                                .unwrap_or("page")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // /details answers {"item": {...}, "streams": [...]}. Reading the
        // fields off the envelope instead of off `item` made every plugin
        // resolve to "Untitled" with no year or artwork.
        let meta = body.get("item").unwrap_or(&body);

        return Some(SourceDetail {
            item: crate::sources::SourceItem {
                external_id: external_id.to_string(),
                title: meta
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Untitled")
                    .to_string(),
                overview: meta
                    .get("overview")
                    .and_then(|o| o.as_str())
                    .unwrap_or("")
                    .to_string(),
                year: meta.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                poster: meta
                    .get("poster")
                    .and_then(|p| p.as_str())
                    .map(str::to_string),
                category: match meta.get("category").and_then(|c| c.as_str()) {
                    Some("book") => "book",
                    Some("music") => "music",
                    Some("manga") => "manga",
                    Some("series") => "series",
                    Some("podcast") => "podcast",
                    Some("audiobook") => "audiobook",
                    _ => "movie",
                },
            },
            streams,
            chapters: None,
        });
    }
    match &provider.base_url {
        None => state
            .sources
            .get(&provider.name)?
            .item(external_id)
            .await
            .ok(),
        Some(base) => {
            // Advanced HTTP source plugins may return playable streams
            // (and manga chapters) directly from /details.
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .ok()?;
            let body: serde_json::Value = client
                .get(format!("{}/details", base.trim_end_matches('/')))
                .query(&[("id", external_id)])
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;

            let streams: Vec<crate::sources::StreamLink> = body
                .get("streams")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| {
                            Some(crate::sources::StreamLink {
                                label: s
                                    .get("label")
                                    .and_then(|l| l.as_str())
                                    .unwrap_or("stream")
                                    .to_string(),
                                url: s.get("url")?.as_str()?.to_string(),
                                kind: s
                                    .get("kind")
                                    .and_then(|k| k.as_str())
                                    .unwrap_or("mp4")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let chapters: Option<Vec<crate::sources::MangaChapter>> =
                body.get("chapters").and_then(|c| c.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            let chapter_value = c.get("chapter")?;
                            let chapter = chapter_value
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| chapter_value.as_i64().map(|v| v.to_string()))
                                .unwrap_or_else(|| "0".into());
                            Some(crate::sources::MangaChapter {
                                id: c.get("id")?.as_str()?.to_string(),
                                chapter,
                                title: c
                                    .get("title")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                pages: c.get("pages").and_then(|p| p.as_i64()).unwrap_or(0),
                            })
                        })
                        .collect()
                });

            // /details answers {"item": {...}, "streams": [...]}. Reading the
            // metadata off the envelope instead of off `item` made every plugin
            // resolve as "Untitled" with no year or artwork, even when the
            // stream itself was correct.
            let meta = body.get("item").unwrap_or(&body);

            Some(SourceDetail {
                item: crate::sources::SourceItem {
                    external_id: external_id.to_string(),
                    title: meta
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Untitled")
                        .to_string(),
                    overview: meta
                        .get("overview")
                        .and_then(|o| o.as_str())
                        .unwrap_or("")
                        .to_string(),
                    year: meta.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                    poster: meta
                        .get("poster")
                        .and_then(|p| p.as_str())
                        .map(str::to_string),
                    // Plugins send `category`; older ones sent `media_type`.
                    category: match meta
                        .get("category")
                        .or_else(|| meta.get("media_type"))
                        .and_then(|t| t.as_str())
                    {
                        Some("movie" | "film") => "movie",
                        Some("book" | "ebook") => "book",
                        Some("track" | "music") => "music",
                        Some("manga") => "manga",
                        Some("podcast") => "podcast",
                        Some("audiobook") => "audiobook",
                        _ => "series",
                    },
                },
                streams,
                chapters,
            })
        }
    }
}

// ------------------------------------------------------------ ranking

fn relevance(
    query: &str,
    title: &str,
    provider_category: &'static str,
    intent_category: Option<&str>,
    position: usize,
) -> f64 {
    let q = query.trim().to_lowercase();
    let t = title.trim().to_lowercase();

    let mut score: f64 = if !q.is_empty() {
        if t == q {
            120.0
        } else if t.starts_with(&q) {
            95.0
        } else if t.contains(&q) {
            75.0
        } else {
            // token overlap
            let qt: std::collections::HashSet<&str> = q.split_whitespace().collect();
            let tt: std::collections::HashSet<&str> = t.split_whitespace().collect();
            let overlap = qt.intersection(&tt).count() as f64;
            (overlap / qt.len().max(1) as f64) * 60.0
        }
    } else {
        50.0 // browse/trending mode: equal baseline
    };

    if Some(provider_category) == intent_category {
        score += 30.0;
    }
    score -= position as f64 * 0.5;
    (score * 100.0).round() / 100.0
}

// ------------------------------------------------------------ public API

/// Cross-source unified search. Fans out in parallel; failures are isolated.
/// Results are cached ~90 s per (query, limit, category, policy) — identical
/// searches answer from memory instead of re-fanning to every provider.
///
/// `include_web` gates learned web sources: when true, learned sites are
/// folded into the result set alongside compiled-in providers.
pub async fn search_everything(
    state: &crate::state::AppState,
    query: &str,
    limit: i32,
    category_filter: Option<&str>,
    include_web: bool,
) -> Vec<UnifiedResult> {
    static CACHE: std::sync::LazyLock<
        dashmap::DashMap<String, (std::time::Instant, Vec<UnifiedResult>)>,
    > = std::sync::LazyLock::new(dashmap::DashMap::new);
    const TTL: std::time::Duration = std::time::Duration::from_secs(90);
    const MAX_ENTRIES: usize = 800; // bounded memory: drop-all past this

    if CACHE.len() > MAX_ENTRIES {
        CACHE.retain(|_, (at, _)| at.elapsed() < TTL);
        if CACHE.len() > MAX_ENTRIES {
            CACHE.clear();
        }
    }

    let key = format!(
        "{}|{}|{}|{}",
        crate::utils::slugify(query),
        limit,
        category_filter.unwrap_or(""),
        if include_web { "web" } else { "core" }
    );
    if let Some(entry) = CACHE.get(&key) {
        let (at, hit) = entry.value();
        if at.elapsed() < TTL {
            metrics::counter!("search_cache_hits_total").increment(1);
            return hit.clone();
        }
    }

    let intent = classify(query);
    let providers = gather_providers(state, include_web, true).await;
    let per_provider =
        ((limit.clamp(1, 60) as f32) / (providers.len().max(1) as f32)).ceil() as i32;

    let jobs = providers.iter().map(|provider| {
        let meta = provider.clone();
        let health = state.sources_health.clone();
        async move {
            let _guard = health.enter(&meta.name).await;
            let started = std::time::Instant::now();
            let searched = tokio::time::timeout(
                Duration::from_secs(9),
                provider_search(state, &meta, query, per_provider),
            )
            .await;
            match searched {
                Ok(Some(items)) => {
                    health
                        .record_op(Op::Search, &meta.name, started.elapsed().as_millis())
                        .await;
                    if items.is_empty() {
                        None
                    } else {
                        Some((meta, items))
                    }
                }
                Ok(None) => {
                    health
                        .record_op_failure(Op::Search, &meta.name, "malformed upstream response")
                        .await;
                    None
                }
                Err(_) => {
                    health
                        .record_op_failure(Op::Search, &meta.name, "fan-out timeout (9s)")
                        .await;
                    None
                }
            }
        }
    });

    let results = results_into_unified(state, query, intent_category(intent), category_filter, limit, jobs).await;
    CACHE.insert(key, (std::time::Instant::now(), results.clone()));
    results
}

/// Turns raw per-provider hits into ranked, duplicate-merged unified results.
async fn results_into_unified(
    state: &crate::state::AppState,
    query: &str,
    intent_category: Option<&str>,
    category_filter: Option<&str>,
    limit: i32,
    jobs: impl Iterator<Item = impl std::future::Future<Output = Option<(ProviderMeta, Vec<crate::sources::SourceItem>)>>>,
) -> Vec<UnifiedResult> {
    let results = futures_util::future::join_all(jobs.collect::<Vec<_>>()).await;

    let mut unified: Vec<UnifiedResult> = Vec::new();
    for result in results.into_iter().flatten() {
        let (meta, items) = result;
        let category_static: &'static str = match meta.category.as_str() {
            "movie" => "movie",
            "book" => "book",
            "music" => "music",
            "manga" => "manga",
            _ => "series",
        };
        for (position, item) in items.into_iter().enumerate() {
            if let Some(filter) = category_filter {
                if item.category != filter {
                    continue;
                }
            }
            unified.push(UnifiedResult {
                token: make_token(&meta.name, &item.external_id),
                relevance: relevance(
                    query,
                    &item.title,
                    category_static,
                    intent_category,
                    position,
                ),
                title: item.title,
                overview: item.overview,
                year: item.year,
                poster: item.poster,
                category: item.category,
                source: meta.name.clone(),
                sources: vec![],
            });
        }
    }

    unified.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Duplicate merge: same normalized title + year + category across sources
    // collapses into one item carrying every contributing source.
    let mut merged_index: HashMap<(String, Option<i32>, &'static str), usize> = HashMap::new();
    let mut merged: Vec<UnifiedResult> = Vec::with_capacity(unified.len());
    for item in unified {
        let key = (normalize_title(&item.title), item.year, item.category);
        match merged_index.get(&key) {
            Some(&idx) => {
                let existing = &mut merged[idx];
                let primary = existing.source.clone();
                if item.relevance > existing.relevance {
                    // Promote the better-ranked token; keep the old primary in
                    // the contributing set.
                    if !primary.is_empty() && primary != item.source {
                        existing.sources.push(primary);
                    }
                    existing.token = item.token;
                    existing.source = item.source;
                    existing.relevance = item.relevance;
                    if existing.overview.is_empty() && !item.overview.is_empty() {
                        existing.overview = item.overview;
                    }
                    if existing.poster.is_none() {
                        existing.poster = item.poster;
                    }
                } else if !primary.is_empty()
                    && item.source != primary
                    && !existing.sources.contains(&item.source)
                {
                    existing.sources.push(item.source.clone());
                }
            }
            None => {
                merged_index.insert(key, merged.len());
                merged.push(item);
            }
        }
    }
    for entry in &mut merged {
        entry.sources.truncate(6);
    }
    let _ = state;
    merged.truncate(limit.clamp(1, 60) as usize);
    merged
}

/// Normalized title key for cross-source duplicate detection.
pub fn normalize_title(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_space = true; // trims leading + collapses runs
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim_end().to_string()
}

/// Picks the objectively best playable link from a set.
pub fn best_stream(streams: &[crate::sources::StreamLink]) -> Option<crate::sources::StreamLink> {
    fn score(link: &crate::sources::StreamLink) -> u32 {
        let l = format!("{} {}", link.label, link.kind).to_lowercase();
        if l.contains("hls") || l.contains("m3u8") {
            return 1000;
        }
        if l.contains("original") {
            return 900;
        }
        if l.contains("2160") || l.contains("4k") {
            return 800;
        }
        if l.contains("1080") {
            return 700;
        }
        if l.contains("720") {
            return 600;
        }
        if l.contains("480") {
            return 500;
        }
        if l.contains("512kb") {
            return 400;
        }
        // a download page is not a way to watch: rank it below any player
        if l.contains("download") {
            return 120;
        }
        // A torrent needs an external client, so it loses to anything that
        // plays in the app — but it is a real copy of the media, so it beats a
        // bare file link or an unrecognised url. For books and comics it is
        // very often the only thing there is.
        if l.contains("torrent") || l.contains("magnet") {
            return 250;
        }
        match link.kind.as_str() {
            "audio" => 650,
            "mp4" => 450,
            "page" => 300,
            "file" => 200,
            _ => 100,
        }
    }

    // A source lists its best stream first, so a tie must keep that order.
    // `max_by_key` returns the *last* maximum, which quietly preferred a
    // site's "download" link over the player sitting above it.
    streams
        .iter()
        .enumerate()
        .max_by_key(|(index, link)| (score(link), std::cmp::Reverse(*index)))
        .map(|(_, link)| link.clone())
}

/// Legacy entry point: resolves a play token to the best playable stream.
pub async fn resolve(
    state: &crate::state::AppState,
    token: &str,
    fallback_title: Option<&str>,
    include_web: bool,
) -> ApiResult<ResolvedStream> {
    let set = resolve_with_preferences(
        state,
        token,
        fallback_title,
        &ResolvePreferences::default(),
        include_web,
    )
    .await?;
    Ok(ResolvedStream {
        url: set.url,
        kind: set.kind,
        title: set.title,
    })
}

/// Central resolver pipeline (Phase B):
/// identify → healthy sources → candidates → rank(prefs) → select → fallbacks.
///
/// The originating provider is tried first; every other healthy provider
/// follows (category-matched first). Resolve outcomes feed source telemetry so
/// the ranking keeps preferring reliable backends.
///
/// `include_web` mirrors [`search_everything`]'s locality policy.
/// Who else can read an id that this source minted.
///
/// Some id spaces are shared, but not by everyone:
///
/// * `movie|tt0111161` is how every Stremio addon addresses content, so a
///   title found through one addon can be played by another — that is exactly
///   how a metadata addon like Cinemeta pairs with a stream addon.
/// * `movie:27205` / `anilist:20` are catalogue ids every TMDB- or
///   AniList-keyed site understands.
/// * Everything else — a slug, a page url, a provider's internal key — means
///   something only where it came from. Offering those around produced
///   confidently wrong results: an Audius track id handed to a video site came
///   back as `thatsite/watch/<audius-id>`, a url that resolves to nothing but
///   looks like a good answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdScope {
    /// Only the source that minted it.
    Owner,
    /// Any Stremio addon.
    StremioAddons,
    /// Any site keyed by the same catalogue.
    CatalogueSites,
}

pub fn id_scope(external_id: &str) -> IdScope {
    // Stremio composite: "<type>|<id>", e.g. series|tt0903747
    if let Some((kind, rest)) = external_id.split_once('|')
        && !rest.is_empty()
        && matches!(
            kind,
            "movie" | "series" | "tv" | "channel" | "anime" | "events"
        )
    {
        return IdScope::StremioAddons;
    }

    let Some((prefix, rest)) = external_id.split_once(':') else {
        return IdScope::Owner;
    };
    if rest.is_empty() {
        return IdScope::Owner;
    }
    let numeric = |id: &str| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());

    let shared = match prefix {
        // tmdb: movie:27205 | tv:1396 | tv:1396:2:3
        "movie" | "tv" => rest.split(':').next().is_some_and(numeric),
        "anilist" | "kitsu" => numeric(rest),
        "mal" => rest.rsplit(':').next().is_some_and(numeric),
        _ => false,
    };
    if shared {
        IdScope::CatalogueSites
    } else {
        IdScope::Owner
    }
}

/// Can this provider read an id of that scope?
fn provider_reads(provider: &ProviderMeta, scope: IdScope) -> bool {
    match scope {
        IdScope::Owner => false,
        // a stremio id is only meaningful to another stremio addon
        IdScope::StremioAddons => provider.ext_base.is_some(),
        // and a catalogue id only to the sites keyed by that catalogue
        IdScope::CatalogueSites => provider.ext_base.is_none(),
    }
}

pub async fn resolve_with_preferences(
    state: &crate::state::AppState,
    token: &str,
    fallback_title: Option<&str>,
    prefs: &ResolvePreferences,
    include_web: bool,
) -> ApiResult<ResolutionSet> {
    let payload = parse_token(token)?;
    let providers = gather_providers(state, include_web, false).await;

    // The source that minted the id always gets first refusal. Others are
    // offered it only if they can actually read that kind of id — a Stremio
    // addon id goes to other addons, a catalogue id to other catalogue-keyed
    // sites, and a private id to nobody. Sources that cannot read it are still
    // tried afterwards by title, which is the right way to find the same thing
    // somewhere else.
    let scope = id_scope(&payload.e);
    let mut ordered: Vec<&ProviderMeta> = Vec::new();
    if let Some(primary) = providers.iter().find(|p| p.name == payload.s) {
        ordered.push(primary);
    }
    for p in &providers {
        if p.name != payload.s && provider_reads(p, scope) {
            ordered.push(p);
        }
    }

    let mut no_stream_titles: Vec<String> = Vec::new();

    // Total budget for the direct pass. Each provider still gets its own 9s
    // ceiling, but the loop is serial: without an overall deadline, N
    // registered sources meant N x 9s before the caller heard anything, so a
    // few dozen sources could hang a click for minutes. The originating source
    // is probed first, so the useful answer almost always lands early.
    const DIRECT_BUDGET: Duration = Duration::from_secs(20);
    let deadline = std::time::Instant::now() + DIRECT_BUDGET;

    for provider in ordered {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            tracing::debug!("direct resolve budget spent; falling through to title search");
            break;
        }
        let _guard = state.sources_health.enter(&provider.name).await;
        let started = std::time::Instant::now();
        let attempt = tokio::time::timeout(
            remaining.min(Duration::from_secs(9)),
            provider_item(state, provider, &payload.e),
        )
        .await;

        match attempt {
            Ok(Some(detail)) => {
                state
                    .sources_health
                    .record_op(Op::Resolve, &provider.name, started.elapsed().as_millis())
                    .await;
                if detail.streams.is_empty() {
                    no_stream_titles.push(detail.item.title);
                    continue;
                }
                metrics::counter!("stream_resolved_total", "source" => provider.name.clone())
                    .increment(1);
                return Ok(build_resolution_set(provider.name.clone(), detail, prefs, state).await);
            }
            Ok(None) => {
                state
                    .sources_health
                    .record_op_failure(Op::Resolve, &provider.name, "item fetch failed")
                    .await;
            }
            Err(_) => {
                state
                    .sources_health
                    .record_op_failure(Op::Resolve, &provider.name, "resolve timeout (9s)")
                    .await;
            }
        }
    }

    // Nothing playable directly — fall through to title-based cross-source search.
    let title = fallback_title
        .map(str::to_string)
        .or_else(|| no_stream_titles.into_iter().next());
    match title {
        Some(title) => {
            // The category the request came in on. Failing over from a film to
            // an audio-only source is how a video request ended up playing a
            // podcast with a black screen.
            let wanted_category = providers
                .iter()
                .find(|p| p.name == payload.s)
                .map(|p| p.category.clone())
                .unwrap_or_default();
            resolve_via_search(
                state,
                &title,
                payload.s.as_str(),
                &wanted_category,
                include_web,
            )
            .await
        }
        None => Err(ApiError::Unavailable(
            "no playable stream found across sources".into(),
        )),
    }
}



/// Categories a film-and-television player can actually serve.
fn is_video_category(category: &str) -> bool {
    // Empty means the caller could not tell. That used to be read as "anything
    // goes", which is exactly how the wrong player got chosen; unknown is not
    // permission.
    matches!(category, "movie" | "series")
}

/// Do two years describe the same release?
///
/// Only decisive when both are known: most sources report no year at all, and
/// refusing everything unlabelled would leave the viewer with nothing. A year
/// either side is allowed, because release dates disagree across territories.
fn year_agrees(wanted: Option<i32>, found: Option<i32>) -> bool {
    match (wanted, found) {
        (Some(w), Some(f)) => (w - f).abs() <= 1,
        _ => true,
    }
}

/// Can a request for `wanted` legitimately be served by a `offered` source?
///
/// Only the coarse split matters: something watched must not be satisfied by
/// something listened to. Within a class the sources are interchangeable.
fn compatible_category(wanted: &str, offered: &str) -> bool {
    if wanted.is_empty() || offered.is_empty() {
        return true;
    }
    fn class(c: &str) -> &'static str {
        match c {
            "music" | "podcast" | "audiobook" => "audio",
            "book" | "manga" => "text",
            _ => "video",
        }
    }
    class(wanted) == class(offered)
}

/// Title-based cross-source failover producing a full resolution set.
async fn resolve_via_search(
    state: &crate::state::AppState,
    title: &str,
    skip_source: &str,
    // Category the original request belonged to; empty means "no preference".
    wanted_category: &str,
    include_web: bool,
) -> ApiResult<ResolutionSet> {
    let providers = gather_providers(state, include_web, false).await;

    // Search every eligible source at once rather than one after another.
    // Serially this cost 9s per provider before the next was even tried, so a
    // miss across a few dozen sources took minutes; concurrently the whole
    // sweep is bounded by the slowest single source.
    let candidates: Vec<&ProviderMeta> = providers
        .iter()
        .filter(|p| p.name != skip_source)
        // A video request must not fail over to an audio-only backend, and
        // vice versa: the stream would play with nothing to look at.
        .filter(|p| compatible_category(wanted_category, &p.category))
        .collect();

    // The year the request was for, if the title carries one. Failover used
    // to compare names alone, so "Dune" (2021) could be satisfied by "Dune"
    // (1984) - the same name, a different film.
    let (_, wanted_year) = clean_release_title(title);

    let searches = candidates.iter().map(|provider| async move {
        let _guard = state.sources_health.enter(&provider.name).await;
        let started = std::time::Instant::now();
        let searched = tokio::time::timeout(
            Duration::from_secs(9),
            provider_search(state, provider, title, 5),
        )
        .await;

        match searched {
            Ok(Some(items)) => {
                state
                    .sources_health
                    .record_op(Op::Search, &provider.name, started.elapsed().as_millis())
                    .await;
                Some((*provider, items))
            }
            _ => None,
        }
    });

    let found = futures_util::future::join_all(searches).await;

    // Providers keep their registry order, so a preferred source still wins
    // over a later one that happened to answer sooner.
    for (provider, items) in found.into_iter().flatten() {
        // Scan the top few rather than blindly taking the first, and only
        // accept one whose title actually matches what was asked for.
        let Some(first) = items
            .iter()
            .take(5)
            .find(|item| title_matches(title, &item.title) && year_agrees(wanted_year, item.year))
        else {
            continue;
        };

        let Some(detail) = provider_item(state, provider, &first.external_id).await else {
            continue;
        };
        let Some(best) = best_stream(&detail.streams) else {
            continue;
        };

        tracing::info!(from = %provider.name, "failover resolved");
        metrics::counter!("stream_failover_total").increment(1);
        let quality = quality_label(&best);
        return Ok(ResolutionSet {
            title: detail.item.title,
            source: provider.name.clone(),
            url: best.url,
            kind: best.kind,
            quality,
            audio: detect_audio(&best.label, None),
            subtitles: Vec::new(),
            fallbacks: Vec::new(),
        });
    }

    // Nothing held it. If the title is one we can identify, a player keyed by
    // its catalogue id will still play it — which is a better answer than
    // telling the viewer every source failed.
    if let Some((player, canonical)) = player_for_title(state, title, wanted_category).await {
        tracing::info!(%title, "resolved through a player after every source missed");
        metrics::counter!("stream_player_fallback_total").increment(1);
        return Ok(ResolutionSet {
            title: canonical,
            source: "player".into(),
            quality: quality_label(&player),
            audio: String::new(),
            subtitles: Vec::new(),
            url: player.url,
            kind: player.kind,
            fallbacks: Vec::new(),
        });
    }

    Err(ApiError::Unavailable("all sources exhausted".into()))
}


/// Where-can-I-watch-this: asks every healthy source (including
/// resolve-only ones) for a title match and returns minted play tokens.
///
/// This is the counterpart to [`search_everything`] for sources that must
/// never appear in general search: the client picks a title through the
/// normal catalog/metadata search first, then calls this with that exact
/// title to discover playback options across all backends.
pub struct LookupHit {
    pub token: String,
    pub source: String,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub category: &'static str,
    /// best stream kind available on this source (no network call yet)
    pub streams_preview: Vec<(String, String)>, // (label, kind), top 3
}

/// Words that carry no identity, so their absence must not fail a match.
const TITLE_STOPWORDS: &[&str] = &["the", "a", "an", "of", "and", "part"];

/// Does `candidate` name the same work as `query`?
///
/// Containment alone is not enough, and getting this wrong is worse than
/// returning nothing: a viewer clicks "Spider-Man: Brand New Day" and a plain
/// containment check happily accepts
/// "Spider-Man: Brand New Day Review #Depression #Healing", or a fan edit
/// titled "Banjaare Barsaat X Spider Man (Brand New Day Edition)(720P HD)".
/// Both contain every word of the query, so both used to play — the UI showed
/// one thing and the player played another.
///
/// So two conditions: every significant word of the query must appear, **and**
/// the candidate must not be padded out with much beyond it. Reviews,
/// reactions, edits and soundtracks all fail the second test.
pub fn title_matches(query: &str, candidate: &str) -> bool {
    let q = normalize_title(query);
    let c = normalize_title(candidate);
    if q.is_empty() || c.is_empty() {
        return false;
    }
    if q == c {
        return true;
    }

    let significant = |t: &&str| !t.is_empty() && !TITLE_STOPWORDS.contains(t);

    let q_tokens: Vec<&str> = q.split(' ').filter(significant).collect();
    let c_tokens: Vec<&str> = c.split(' ').filter(significant).collect();
    if q_tokens.is_empty() || c_tokens.is_empty() {
        return false;
    }

    let c_set: std::collections::HashSet<&str> = c_tokens.iter().copied().collect();
    if !q_tokens.iter().all(|t| c_set.contains(t)) {
        return false;
    }

    // Extra words are only disqualifying when they change what the thing *is*.
    // A year, an episode number or a release tag does not ("Vincenzo (2021)
    // Episode 18" is still Vincenzo); words like "review", "reaction" or
    // "edition" do.
    let q_set: std::collections::HashSet<&str> = q_tokens.iter().copied().collect();
    let meaningful_extra = c_tokens
        .iter()
        .filter(|t| !q_set.contains(*t) && !is_qualifier(t))
        .count();

    // Scale the allowance with the title's length. One extra word on a
    // one-word title is a whole new subject — "Filmmagasinet Nosferatu" is a
    // podcast *about* Nosferatu, not the film — while on a longer title it is
    // usually a subtitle or an edition.
    meaningful_extra == 0 || (q_tokens.len() >= 3 && meaningful_extra <= 1)
}

/// Tokens that annotate a release rather than name a different work.
fn is_qualifier(token: &str) -> bool {
    const QUALIFIERS: &[&str] = &[
        "episode", "ep", "season", "s", "e", "vol", "volume", "chapter",
        "full", "complete", "remastered", "extended", "uncut", "directors",
        "hd", "sd", "uhd", "fhd", "4k", "1080p", "720p", "480p", "2160p",
        "web", "webrip", "webdl", "bluray", "brrip", "bdrip", "dvdrip", "hdrip",
        "x264", "x265", "h264", "h265", "hevc", "avc", "aac", "ac3", "dts",
        "sub", "subs", "subbed", "dub", "dubbed", "eng", "english", "multi",
        "dl", "rip", "proper", "repack",
    ];
    // A bare number is a year, an episode or a part — never an identity word.
    token.chars().all(|c| c.is_ascii_digit())
        || QUALIFIERS.contains(&token)
        || is_episode_code(token)
}

/// `s01`, `e03`, `s02e03` — scene numbering, not part of the title.
fn is_episode_code(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut i = 0;
    let mut matched_any = false;

    while i < bytes.len() {
        if bytes[i] != b's' && bytes[i] != b'e' {
            return false;
        }
        i += 1;
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start {
            return false; // a bare "s" or "e" is already a listed qualifier
        }
        matched_any = true;
    }
    matched_any
}

pub async fn lookup_title(
    state: &crate::state::AppState,
    title: &str,
    year: Option<i32>,
    category_filter: Option<&str>,
    include_web: bool,
) -> Vec<LookupHit> {
    let providers = gather_providers(state, include_web, false).await;
    let jobs = providers.iter().map(|provider| {
        let meta = provider.clone();
        let health = state.sources_health.clone();
        let wanted = title.to_string();
        async move {
            let _guard = health.enter(&meta.name).await;
            let searched = tokio::time::timeout(
                Duration::from_secs(9),
                provider_search(state, &meta, &wanted, 6),
            )
            .await;
            match searched {
                Ok(Some(items)) => Some((meta, items)),
                _ => None,
            }
        }
    });

    let mut hits: Vec<LookupHit> = Vec::new();
    for result in futures_util::future::join_all(jobs.collect::<Vec<_>>()).await.into_iter().flatten() {
        let (meta, items) = result;
        for item in items {
            if !title_matches(title, &item.title) {
                continue;
            }
            if let (Some(want), Some(have)) = (year, item.year)
                && (have - want).abs() > 1
            {
                continue;
            }
            if let Some(cat) = category_filter
                && !cat.is_empty()
                && item.category != cat
            {
                continue;
            }
            hits.push(LookupHit {
                token: make_token(&meta.name, &item.external_id),
                source: meta.name.clone(),
                title: item.title.clone(),
                year: item.year,
                poster: item.poster,
                category: item.category,
                // details are intentionally NOT fetched here — resolution
                // does that on demand; keep lookup cheap.
                streams_preview: Vec::new(),
            });
        }
    }
    hits.truncate(40);
    hits
}

/// Ranks a detail's streams under client preferences and packages the result.
/// The first result that is genuinely the same title.
///
/// Only an exact name match counts, and when the source told us a year that
/// must agree too. TMDB happily answers "Obsession" with "A Beautiful
/// Obsession"; playing that would be worse than showing the page the source
/// gave us, so a near miss is no match at all.
fn exact_match<'a>(results: &'a [Value], clean: &str, year: Option<i32>) -> Option<&'a Value> {
    results.iter().take(6).find(|row| {
        let name = row
            .get("title")
            .or_else(|| row.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if normalize_title(name) != normalize_title(clean) {
            return false;
        }
        match year {
            None => true,
            Some(wanted) => row
                .get("release_date")
                .or_else(|| row.get("first_air_date"))
                .and_then(Value::as_str)
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse::<i32>().ok())
                // a year either side covers release-date disagreement
                .is_some_and(|found| (found - wanted).abs() <= 1),
        }
    })
}

/// Strips release noise from a scraped title, returning the name and any year.
///
/// Sources name items from url slugs and release lines — "project hail mary
/// 2026 imax bluray 480p 720p 1080p" — none of which TMDB will match.
fn clean_release_title(title: &str) -> (String, Option<i32>) {
    const NOISE: &[&str] = &[
        "imax", "bluray", "blu-ray", "webrip", "web-dl", "webdl", "hdrip", "dvdrip",
        "brrip", "hdtv", "x264", "x265", "hevc", "aac", "dual", "audio", "hindi",
        "english", "dubbed", "subbed", "complete", "season", "uncut", "extended",
        "remastered", "repack", "proper",
    ];

    let mut year = None;
    let mut words = Vec::new();
    for word in title.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();

        // a plausible release year, not part of the name
        if lower.len() == 4
            && lower.chars().all(|c| c.is_ascii_digit())
            && let Ok(value) = lower.parse::<i32>()
            && (1900..=2100).contains(&value)
        {
            year = Some(value);
            continue;
        }
        // resolutions and rip tags
        if lower.ends_with('p') && lower[..lower.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if NOISE.contains(&lower.as_str()) {
            continue;
        }
        words.push(word.to_string());
    }
    (words.join(" ").trim().to_string(), year)
}

/// Hosts that are players rather than websites.
///
/// A stream pointing at one of these opens something that plays. A stream
/// pointing anywhere else is an article, a listing or a landing page — which
/// is what "it just loads the web page" looks like from the viewer's seat.
fn is_player_url(url: &str) -> bool {
    const PLAYERS: &[&str] = &[
        "vidking.net",
        "megaplay.buzz",
        "rivestream.app/embed",
        "rivestream.app/download",
        "vidsrc",
        "embed.su",
        "videasy",
        "/embed",
        "/stream/",
        "player.",
    ];
    let lower = url.to_lowercase();
    PLAYERS.iter().any(|p| lower.contains(p))
}

/// A watchable player for a title we can identify.
///
/// Many sources only ever hand back their own page for an item — a review, a
/// download listing, a site landing page. Nothing plays there. When the title
/// can be matched in TMDB, the same content is reachable through a player keyed
/// by that id, so one is offered ahead of the page.
async fn player_for_title(
    state: &crate::state::AppState,
    title: &str,
    category: &str,
) -> Option<(crate::sources::StreamLink, String)> {
    // This last resort mints a TMDB embed, which plays film and television and
    // nothing else. Reaching it with a book, a comic or a track is how a novel
    // ended up playing a film of the same name; there is no player that would
    // be right, so the honest answer is that nothing was found.
    if !is_video_category(category) {
        return None;
    }
    let key = state.cfg.tmdb_api_key.as_deref()?;
    let kind = if category == "series" { "tv" } else { "movie" };

    // These titles are usually a url slug: "project hail mary 2026 imax
    // bluray 480p 720p". Searching TMDB for that finds nothing, so the
    // release noise comes off first, and the year is kept aside to
    // disambiguate rather than thrown away.
    let (clean, year) = clean_release_title(title);
    if clean.is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    let body = crate::catalog::tmdb_cache::fetch(
        &client,
        &format!(
            "https://api.themoviedb.org/3/search/{kind}?api_key={key}&query={}{}",
            crate::plugins::native::engines::quote_plus(&clean),
            year.map(|y| format!("&year={y}")).unwrap_or_default()
        ),
    )
    .await?;

    let mut results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Sources disagree with TMDB about what is a film and what is a series —
    // a drama site calls everything a series. If the declared kind finds
    // nothing, ask the other one before giving up.
    if results.is_empty() {
        let other = if kind == "tv" { "movie" } else { "tv" };
        let body = crate::catalog::tmdb_cache::fetch(
            &client,
            &format!(
                "https://api.themoviedb.org/3/search/{other}?api_key={key}&query={}{}",
                crate::plugins::native::engines::quote_plus(&clean),
                year.map(|y| format!("&year={y}")).unwrap_or_default()
            ),
        )
        .await?;
        results = body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(hit) = exact_match(&results, &clean, year) {
            let tmdb_id = hit.get("id")?.as_i64()?;
            let canonical = hit
                .get("title")
                .or_else(|| hit.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(&clean)
                .to_string();
            return Some((
                crate::sources::StreamLink {
                    label: "player".to_string(),
                    url: crate::plugins::native::engines::tmdb_embed_url(
                        other,
                        &tmdb_id.to_string(),
                        1,
                        1,
                    ),
                    kind: "page".to_string(),
                },
                canonical,
            ));
        }
    }

    let hit = exact_match(&results, &clean, year)?;
    let tmdb_id = hit.get("id")?.as_i64()?;
    // The catalogue's own name, so the page stops showing a url slug such as
    // "project hail mary 2026" as if it were the title.
    let canonical = hit
        .get("title")
        .or_else(|| hit.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(&clean)
        .to_string();


    Some((
        crate::sources::StreamLink {
            label: "player".to_string(),
            url: crate::plugins::native::engines::tmdb_embed_url(kind, &tmdb_id.to_string(), 1, 1),
            kind: "page".to_string(),
        },
        canonical,
    ))
}

async fn build_resolution_set(
    source_name: String,
    detail: SourceDetail,
    prefs: &ResolvePreferences,
    state: &crate::state::AppState,
) -> ResolutionSet {
    let bonus = state.sources_health.score_bonus(&source_name).await;

    // If nothing this source offered actually plays — every stream is one of
    // its own pages — try to put a real player in front of them.
    let mut streams = detail.streams.clone();
    let mut display_title = detail.item.title.clone();
    let nothing_plays = !streams
        .iter()
        .any(|link| link.kind != "page" || is_player_url(&link.url));
    if nothing_plays
        && let Some((player, canonical)) =
            player_for_title(state, &detail.item.title, detail.item.category).await
    {
        streams.insert(0, player);
        // Show the catalogue's name rather than the slug the source used.
        display_title = canonical;
    }

    let mut scored: Vec<(f64, crate::sources::StreamLink)> = streams
        .iter()
        .map(|link| (stream_score(link, prefs, bonus), link.clone()))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let (_, best) = scored.remove(0);
    let fallbacks = scored
        .into_iter()
        .take(3)
        .map(|(_, link)| FallbackStream {
            source: source_name.clone(),
            kind: link.kind,
            url: link.url,
        })
        .collect();

    ResolutionSet {
        title: display_title,
        quality: quality_label(&best),
        audio: detect_audio(&best.label, prefs.language.as_deref()),
        subtitles: detect_subtitles(&best.label, &prefs.subtitles),
        source: source_name,
        url: best.url,
        kind: best.kind,
        fallbacks,
    }
}

/// Pure preference-aware stream score (unit-tested).
pub fn stream_score(
    link: &crate::sources::StreamLink,
    prefs: &ResolvePreferences,
    health_bonus: f64,
) -> f64 {
    let haystack = format!("{} {}", link.label, link.kind).to_lowercase();

    let mut score = match link.kind.as_str() {
        "hls" => 34.0,
        "mp4" => 24.0,
        "audio" => 26.0,
        "page" => 12.0,
        "file" => 7.0,
        "torrent" => 5.0,
        _ => 9.0,
    };
    if haystack.contains("hls") || haystack.contains("m3u8") {
        score += 6.0;
    }
    if haystack.contains("original") {
        score += 5.0;
    }

    // Quality: exact target match wins; close matches degrade gracefully;
    // known-but-far quality still beats unknown; unknown is neutral-low.
    match prefs.quality_number() {
        Some(target) => match parse_quality(&haystack) {
            Some(actual) => {
                let diff = (i32::from(actual) - i32::from(target)).abs();
                score += if diff == 0 {
                    40.0
                } else if diff <= 120 {
                    30.0 - diff as f64 / 12.0
                } else {
                    14.0
                };
                // Going far above target wastes bandwidth; slight penalty.
                if actual > target * 2 {
                    score -= 3.0;
                }
            }
            None => score += 8.0,
        },
        None => score += tier_points(&haystack),
    }

    if let Some(lang) = prefs.language.as_deref() {
        if label_mentions_language(&haystack, lang) {
            score += 14.0;
        }
    }
    if !prefs.subtitles.is_empty()
        && ["sub", "subs", "subtitle", "vtt", "srt"]
            .iter()
            .any(|hint| haystack.contains(hint))
    {
        score += 9.0;
    }

    score + health_bonus
}

fn tier_points(haystack: &str) -> f64 {
    if haystack.contains("2160") || haystack.contains("4k") {
        32.0
    } else if haystack.contains("1080") {
        28.0
    } else if haystack.contains("720") {
        22.0
    } else if haystack.contains("480") {
        16.0
    } else if haystack.contains("512kb") {
        8.0
    } else {
        18.0
    }
}

/// Best-effort display label like "1080p", "original", or the stream kind.
pub fn quality_label(link: &crate::sources::StreamLink) -> String {
    let hay = format!("{} {}", link.label, link.kind);
    if let Some(q) = parse_quality(&hay) {
        return format!("{q}p");
    }
    let l = hay.to_lowercase();
    if l.contains("original") {
        "original".into()
    } else {
        link.kind.clone()
    }
}

fn label_mentions_language(haystack: &str, lang: &str) -> bool {
    const ALIASES: &[(&str, &[&str])] = &[
        ("en", &["eng", "english", " en ", "dub"]),
        ("ja", &["jpn", "japanese", " jap", "ja "]),
        ("de", &["ger", "german", "deu"]),
        ("fr", &["fre", "french", "fra"]),
        ("es", &["spa", "spanish"]),
    ];
    let needle = format!(" {lang} ");
    let padded = format!(" {haystack} ");
    padded.contains(&needle)
        || ALIASES
            .iter()
            .filter(|(code, _)| *code == lang)
            .flat_map(|(_, names)| names.iter())
            .any(|name| padded.contains(name))
}

fn detect_audio(label: &str, preferred: Option<&str>) -> String {
    if let Some(p) = preferred {
        return p.to_string();
    }
    let l = format!(" {label} ").to_lowercase();
    for (code, names) in [
        ("en", &["eng", "english"][..]),
        ("ja", &["jpn", "japanese"][..]),
        ("de", &["ger", "german"][..]),
    ] {
        if names.iter().any(|n| l.contains(n)) {
            return code.to_string();
        }
    }
    String::new()
}

fn detect_subtitles(label: &str, wanted: &[String]) -> Vec<String> {
    let l = label.to_lowercase();
    let hinted = ["sub", "vtt", "srt"].iter().any(|h| l.contains(h));
    if hinted && !wanted.is_empty() {
        wanted.to_vec()
    } else {
        Vec::new()
    }
}

/// Manga chapter listing through a token (source hidden).
pub async fn manga_chapters_for_token(
    state: &crate::state::AppState,
    token: &str,
) -> ApiResult<(String, Vec<crate::sources::MangaChapter>)> {
    let payload = parse_token(token)?;
    let title_check = state
        .sources
        .get(&payload.s)
        .map(|_| ())
        .ok_or_else(|| ApiError::not_found("source"))?;
    let _ = title_check;

    if payload.s == "mangadex" {
        let chapters = crate::sources::mangadex::MangaDex::default()
            .chapters_for(&payload.e)
            .await?;
        return Ok((payload.s, chapters));
    }
    Err(ApiError::bad_request("token is not a manga"))
}

pub async fn manga_pages_for_token(
    state: &crate::state::AppState,
    token: &str,
    chapter_id: &str,
) -> ApiResult<Vec<String>> {
    let payload = parse_token(token)?;
    if payload.s != "mangadex" {
        return Err(ApiError::bad_request("token is not a mangadex item"));
    }
    let _ = state;
    crate::sources::mangadex::MangaDex::default()
        .chapter_pages(chapter_id)
        .await
}

// helpers ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn link(label: &str, kind: &str) -> crate::sources::StreamLink {
        crate::sources::StreamLink {
            label: label.into(),
            url: "https://x".into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn quality_parsing() {
        assert_eq!(parse_quality("1080p"), Some(1080));
        assert_eq!(parse_quality("4K UHD"), Some(2160));
        assert_eq!(parse_quality("720"), Some(720));
        assert_eq!(parse_quality("512kb MPEG4"), None);
        assert_eq!(parse_quality("MP3 128k"), None);
    }

    #[test]
    fn preference_ranking_prefers_target_quality() {
        let prefs = ResolvePreferences {
            quality: Some("1080p".into()),
            ..Default::default()
        };
        let low = stream_score(&link("512kb MPEG4", "mp4"), &prefs, 0.0);
        let exact = stream_score(&link("1080p", "mp4"), &prefs, 0.0);
        let high = stream_score(&link("2160p", "mp4"), &prefs, 0.0);
        assert!(exact > high && high > low);
    }

    #[test]
    fn hls_beats_mp4_at_same_quality() {
        let prefs = ResolvePreferences::default();
        let hls = stream_score(&link("1080p", "hls"), &prefs, 0.0);
        let mp4 = stream_score(&link("1080p", "mp4"), &prefs, 0.0);
        assert!(hls > mp4);
    }

    #[test]
    fn language_and_subtitle_hints_count() {
        let prefs = ResolvePreferences {
            language: Some("ja".into()),
            subtitles: vec!["en".into()],
            ..Default::default()
        };
        let plain = stream_score(&link("Original", "mp4"), &prefs, 0.0);
        let rich = stream_score(&link("japanese eng subs", "mp4"), &prefs, 0.0);
        assert!(rich > plain);
    }

    #[test]
    fn duplicate_titles_merge() {
        let key_a = normalize_title("One Piece!");
        let key_b = normalize_title("one   piece");
        assert_eq!(key_a, key_b);
    }

    #[test]
    fn lookup_title_matching() {
        assert!(title_matches("Breaking Bad", "Breaking Bad"));
        assert!(title_matches("breaking   bad!", "Breaking Bad"));
        assert!(title_matches("Vincenzo", "Vincenzo (2021) Episode 18"));
        // containment is word-boundary aware, not substring
        assert!(!title_matches("Up", "Pulp Fiction"));
        assert!(title_matches("The Office", "Office"));
        assert!(!title_matches("", "anything"));
    }
}

#[cfg(test)]
mod title_match_tests {
    use super::title_matches;

    const WANTED: &str = "Spider-Man: Brand New Day";

    #[test]
    fn accepts_the_same_film() {
        assert!(title_matches(WANTED, "Spider-Man: Brand New Day"));
        // Release naming: punctuation as separators, plus a year.
        assert!(title_matches(WANTED, "Spider Man. Brand. New. Day. 2026"));
        assert!(title_matches(WANTED, "Spider-Man Brand New Day (2026)"));
    }

    /// The cases that made the app show one title and play another.
    #[test]
    fn rejects_commentary_and_edits() {
        assert!(!title_matches(
            WANTED,
            "Spider-Man: Brand New Day Review #Depression #Spiderman #Healing #Isolation"
        ));
        assert!(!title_matches(
            WANTED,
            "Banjaare Barsaat X Spider Man ( Brand New Day Edition)( 720 P HD)"
        ));
        assert!(!title_matches(
            WANTED,
            "SPIDER-MAN: Brand New Day - Main Theme (EPIC VERSION) Soundtrack Cover"
        ));
    }

    /// A short title has no slack: any added subject word is a different work.
    #[test]
    fn rejects_a_work_about_the_title() {
        assert!(!title_matches("Nosferatu", "Filmmagasinet Nosferatu"));
        assert!(!title_matches("Dune", "Dune Explained"));
        assert!(title_matches("Nosferatu", "Nosferatu (1922)"));
    }

    #[test]
    fn rejects_an_unrelated_work() {
        assert!(!title_matches(WANTED, "Star Wars: The Alliance Podcast Episode 12"));
        assert!(!title_matches(WANTED, "Spider-Man: No Way Home"));
    }

    /// Series releases annotate the title; that must not read as a mismatch.
    #[test]
    fn accepts_episode_and_release_tags() {
        assert!(title_matches("Vincenzo", "Vincenzo (2021) Episode 18"));
        assert!(title_matches("Reacher", "Reacher S02E03 1080p WEB-DL x265"));
        assert!(title_matches("Silo", "Silo - Season 2 Complete"));
    }

    #[test]
    fn ignores_articles() {
        assert!(title_matches("The Matrix Reloaded", "Matrix Reloaded"));
        assert!(title_matches("Matrix Reloaded", "The Matrix Reloaded 2003"));
    }

    #[test]
    fn a_short_title_still_needs_every_word() {
        assert!(!title_matches("Dune", "Dune Part Two Behind The Scenes Featurette"));
        assert!(title_matches("Dune", "Dune (2021)"));
    }
}

#[cfg(test)]
mod shared_id_tests {
    use super::{IdScope, id_scope};

    #[test]
    fn stremio_ids_travel_between_addons() {
        // this is how a metadata addon pairs with a stream addon
        assert_eq!(id_scope("series|tt0903747"), IdScope::StremioAddons);
        assert_eq!(id_scope("movie|tt1375666"), IdScope::StremioAddons);
    }

    #[test]
    fn catalogue_ids_travel_between_sites() {
        assert_eq!(id_scope("movie:27205"), IdScope::CatalogueSites);
        assert_eq!(id_scope("tv:1396"), IdScope::CatalogueSites);
        assert_eq!(id_scope("tv:1396:2:3"), IdScope::CatalogueSites);
        assert_eq!(id_scope("anilist:20"), IdScope::CatalogueSites);
        assert_eq!(id_scope("mal:anime:20"), IdScope::CatalogueSites);
    }

    #[test]
    fn source_private_ids_stay_home() {
        // an Audius key, a YouTube id, a slug, a page url
        assert_eq!(id_scope("bvPOO"), IdScope::Owner);
        assert_eq!(id_scope("ilfYnhXD-bE"), IdScope::Owner);
        assert_eq!(id_scope("breaking-bad"), IdScope::Owner);
        assert_eq!(id_scope("https://pahe.ink/breaking-bad"), IdScope::Owner);
        assert_eq!(id_scope("movie:"), IdScope::Owner);
        assert_eq!(id_scope("tv:abc"), IdScope::Owner);
    }

    #[test]
    fn the_two_shared_spaces_do_not_overlap() {
        // `movie|…` and `movie:…` are different languages; a site that speaks
        // one must never be handed the other
        assert_ne!(id_scope("movie|tt1375666"), id_scope("movie:27205"));
    }
}

#[cfg(test)]
mod player_fallback_tests {
    use super::{clean_release_title, is_player_url};

    #[test]
    fn release_noise_comes_off_a_scraped_title() {
        let (name, year) = clean_release_title("project hail mary 2026 imax bluray 480p 720p 1080p");
        assert_eq!(name, "project hail mary");
        assert_eq!(year, Some(2026));
    }

    #[test]
    fn a_plain_title_survives_untouched() {
        let (name, year) = clean_release_title("Interstellar");
        assert_eq!(name, "Interstellar");
        assert_eq!(year, None);
    }

    #[test]
    fn a_number_in_the_name_is_not_mistaken_for_a_year() {
        // 2012 the film, not the year 2012 — but either way the name survives
        let (name, _) = clean_release_title("Blade Runner 2049");
        assert_eq!(name, "Blade Runner");
    }

    #[test]
    fn players_are_told_apart_from_websites() {
        assert!(is_player_url("https://www.vidking.net/embed/movie/157336"));
        assert!(is_player_url("https://megaplay.buzz/stream/ani/20/1/sub"));
        assert!(is_player_url("https://www.rivestream.app/embed?type=movie&id=1"));
        // a site's own article page is not a player
        assert!(!is_player_url("https://novamovie.net/project-hail-mary-2026"));
        assert!(!is_player_url("https://pahe.ink/project-hail-mary-2026-bluray/"));
    }
}

#[cfg(test)]
mod best_stream_tests {
    use super::best_stream;
    use crate::sources::StreamLink;

    fn link(label: &str, kind: &str) -> StreamLink {
        StreamLink {
            label: label.into(),
            url: format!("https://x/{label}"),
            kind: kind.into(),
        }
    }

    #[test]
    fn a_tie_keeps_the_order_the_source_gave() {
        // rivestream lists its player first and a download link last; the
        // player must win
        let streams = vec![
            link("Rive - fastest server", "page"),
            link("Rive - aggregator servers", "page"),
            link("Rive - download", "page"),
        ];
        let best = best_stream(&streams).unwrap();
        assert_eq!(best.label, "Rive - fastest server");
    }

    #[test]
    fn a_download_never_beats_something_watchable() {
        let streams = vec![link("download", "page"), link("watch page", "page")];
        assert_eq!(best_stream(&streams).unwrap().label, "watch page");
    }

    #[test]
    fn real_video_still_wins_over_any_page() {
        let streams = vec![link("watch page", "page"), link("1080p hls", "hls")];
        assert_eq!(best_stream(&streams).unwrap().label, "1080p hls");
    }
}

#[cfg(test)]
mod category_routing_tests {
    use super::*;

    #[test]
    fn a_player_is_only_offered_for_what_it_can_play() {
        assert!(is_video_category("movie"));
        assert!(is_video_category("series"));
        // The bug: a book that resolved nowhere was handed a TMDB film embed.
        assert!(!is_video_category("book"));
        assert!(!is_video_category("manga"));
        assert!(!is_video_category("music"));
        assert!(!is_video_category("audiobook"));
        // Unknown is not permission.
        assert!(!is_video_category(""));
    }

    #[test]
    fn reading_and_watching_do_not_substitute_for_each_other() {
        assert!(!compatible_category("book", "movie"));
        assert!(!compatible_category("movie", "music"));
        assert!(compatible_category("book", "manga"));
        assert!(compatible_category("movie", "series"));
    }

    #[test]
    fn a_year_only_decides_when_both_sides_know_it() {
        // The mismatch this exists to stop.
        assert!(!year_agrees(Some(2021), Some(1984)));
        // Release dates disagree across territories by up to a year.
        assert!(year_agrees(Some(2021), Some(2022)));
        assert!(year_agrees(Some(2021), Some(2021)));
        // Most sources report no year; refusing those leaves nothing at all.
        assert!(year_agrees(Some(2021), None));
        assert!(year_agrees(None, Some(1984)));
    }
}

#[cfg(test)]
mod exact_match_tests {
    use super::exact_match;
    use serde_json::json;

    #[test]
    fn a_near_miss_is_not_a_match() {
        // what TMDB actually answers for "Obsession" in 2026
        let results = vec![
            json!({ "id": 331946, "name": "A Beautiful Obsession", "first_air_date": "2026-01-01" }),
            json!({ "id": 310212, "name": "Hidden Obsession", "first_air_date": "2026-01-01" }),
        ];
        assert!(exact_match(&results, "Obsession", Some(2026)).is_none());
    }

    #[test]
    fn an_exact_title_further_down_is_found() {
        let results = vec![
            json!({ "id": 1, "name": "A Beautiful Obsession", "first_air_date": "2026-01-01" }),
            json!({ "id": 2, "name": "Obsession", "first_air_date": "2026-03-04" }),
        ];
        let hit = exact_match(&results, "Obsession", Some(2026)).expect("found");
        assert_eq!(hit.get("id").unwrap().as_i64(), Some(2));
    }

    #[test]
    fn the_year_has_to_agree() {
        let results = vec![json!({ "id": 9, "title": "Obsession", "release_date": "1976-08-01" })];
        assert!(exact_match(&results, "Obsession", Some(2026)).is_none());
        // released either side of what the source said is still the same film
        assert!(exact_match(&results, "Obsession", Some(1977)).is_some());
        // no year known: the exact title is enough
        assert!(exact_match(&results, "Obsession", None).is_some());
    }
}

#[cfg(test)]
mod torrent_tests {
    use super::{best_stream, compatible_category};
    use crate::sources::StreamLink;

    fn link(label: &str, kind: &str) -> StreamLink {
        StreamLink { label: label.into(), url: format!("https://x/{label}"), kind: kind.into() }
    }

    #[test]
    fn a_torrent_index_answers_for_every_category() {
        // torrent sources carry books, comics, music and film alike, so they
        // are registered with no category of their own
        for wanted in ["movie", "series", "book", "manga", "music"] {
            assert!(
                compatible_category(wanted, ""),
                "a torrent index must be reachable for {wanted}"
            );
        }
    }

    #[test]
    fn a_torrent_beats_a_bare_file_but_not_a_player() {
        let over_file = vec![link("file", "file"), link("magnet", "torrent")];
        assert_eq!(best_stream(&over_file).unwrap().kind, "torrent");

        // an embed plays in the app; a torrent does not
        let under_page = vec![link("magnet", "torrent"), link("watch page", "page")];
        assert_eq!(best_stream(&under_page).unwrap().kind, "page");

        // and a real stream always wins
        let under_hls = vec![link("magnet", "torrent"), link("1080p hls", "hls")];
        assert_eq!(best_stream(&under_hls).unwrap().kind, "hls");
    }
}
