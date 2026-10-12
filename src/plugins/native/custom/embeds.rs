//! Turning a site's watch page into something that actually plays.
//!
//! None of these sites host video themselves. A watch page embeds a
//! third-party player in an iframe, and that player holds the real stream
//! behind its own small API. The important consequence: there are only a
//! handful of embed providers behind hundreds of sites, so playback is solved
//! per *provider*, not per site.
//!
//! Every extractor here returns SPACE stream entries - `hls`/`mp4` urls the
//! player can open directly, plus any subtitle tracks the provider exposes.
//!
//! These endpoints check `Referer` and `X-Requested-With`, which is why this
//! runs server-side: a browser cannot set them cross-origin, and the Python
//! plugins never attempted it (they all handed back a page link instead).

use serde_json::{Value, json};

use crate::error::ApiResult;
use crate::plugins::native::engines::EngineCtx;

/// Iframe sources on a watch page, absolutised.
pub fn iframe_urls(html: &str, base: &str) -> Vec<String> {
    static IFRAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = IFRAME
        .get_or_init(|| regex::Regex::new(r#"(?i)<iframe[^>]+src=["']([^"']+)["']"#).unwrap());

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
    out
}

/// Every embed a watch page leads to, whether or not the iframe is in the
/// server-rendered HTML.
///
/// These sites are Next.js apps: the player iframe is injected client-side, so
/// fetching the page server-side finds no `<iframe>` at all. The page payload
/// still carries the id the player is built from (`"anilistId":20` becomes
/// `megaplay.buzz/stream/ani/20/{ep}/sub`), so the embed url is reconstructed
/// from that rather than needing a browser.
pub fn discover(html: &str, base: &str, episode: &str) -> Vec<String> {
    let mut out = iframe_urls(html, base);

    static ANILIST_ID: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = ANILIST_ID
        .get_or_init(|| regex::Regex::new(r#"["\\]*anilistId["\\]*\s*:\s*(\d+)"#).unwrap());
    if let Some(id) = re.captures(html).and_then(|c| c.get(1)) {
        for category in ["sub", "dub"] {
            let url = format!(
                "https://megaplay.buzz/stream/ani/{}/{episode}/{category}",
                id.as_str()
            );
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

/// Resolve one embed url to playable streams, if its provider is known.
pub async fn resolve(ctx: &EngineCtx, embed_url: &str, referer: &str) -> ApiResult<Vec<Value>> {
    let host = url::Url::parse(embed_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();

    if host.contains("megaplay") {
        return megaplay(ctx, embed_url, referer).await;
    }
    Ok(Vec::new())
}

/// True when the embed host has an extractor here.
pub fn is_known_provider(embed_url: &str) -> bool {
    url::Url::parse(embed_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|host| host.contains("megaplay"))
}

/// MegaPlay: the embed page carries an internal id, and `/stream/getSources`
/// trades that id for an HLS master playlist plus subtitle tracks. The
/// endpoint refuses anything that does not look like the player's own AJAX
/// call, so both headers are required.
async fn megaplay(ctx: &EngineCtx, embed_url: &str, referer: &str) -> ApiResult<Vec<Value>> {
    let page = ctx.get_text_with(embed_url, &[("referer", referer)]).await?;

    static DATA_ID: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = DATA_ID.get_or_init(|| regex::Regex::new(r#"data-id=["'](\d+)["']"#).unwrap());
    let Some(id) = re
        .captures(&page)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
    else {
        return Ok(Vec::new());
    };

    let origin = url::Url::parse(embed_url)
        .ok()
        .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default()))
        .unwrap_or_default();

    let sources = ctx
        .get_json_with(
            &format!("{origin}/stream/getSources?id={id}"),
            &[
                ("referer", embed_url),
                ("x-requested-with", "XMLHttpRequest"),
            ],
        )
        .await?;

    let mut streams = Vec::new();
    if let Some(file) = sources.pointer("/sources/file").and_then(Value::as_str) {
        streams.push(json!({
            "label": "MegaPlay \u{b7} HLS",
            "url": file,
            "kind": "hls",
        }));
    }
    for track in sources
        .get("tracks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        // captions only; the thumbnail track is not a subtitle
        if track.get("kind").and_then(Value::as_str) != Some("captions") {
            continue;
        }
        let (Some(file), Some(label)) = (
            track.get("file").and_then(Value::as_str),
            track.get("label").and_then(Value::as_str),
        ) else {
            continue;
        };
        streams.push(json!({
            "label": format!("subtitles \u{b7} {label}"),
            "url": file,
            "kind": "subtitle",
        }));
    }
    Ok(streams)
}
