//! Individual lyrics backends. Each provider returns `Ok(None)` on a clean
//! miss and `Err` on transport/parse failure so the chain can continue.

use serde_json::Value;

use super::{
    html_to_text, normalize, parse_lrc, parse_ttml, LyricsLine, LyricsPayload, LyricsQuery,
};

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("space-media-server/0.1 (+lyrics)")
        .build()
        .expect("reqwest client")
}

fn clean_query(query: &LyricsQuery) -> Option<(String, String)> {
    let title = query.title.as_deref().map(str::trim).filter(|t| !t.is_empty())?;
    let artist = query.artist.as_deref().map(str::trim).unwrap_or("");
    Some((title.to_string(), artist.to_string()))
}

pub mod betterlyrics {
    //! betterlyrics.org — the API behind the Better Lyrics extension for
    //! YouTube Music. Word-synced TTML via `GET /getLyrics?s=&a=`.

    use super::*;

    const BASE: &str = "https://lyrics-api.boidu.dev";

    pub async fn fetch(query: &LyricsQuery) -> crate::error::ApiResult<Option<LyricsPayload>> {
        let Some((song, artist)) = clean_query(query) else {
            return Ok(None);
        };
        let body: Value = client()
            .get(format!("{BASE}/getLyrics"))
            .query(&[("s", song.as_str()), ("a", artist.as_str())])
            .send()
            .await
            .map_err(|e| crate::error::ApiError::Unavailable(format!("betterlyrics: {e}")))?
            .json()
            .await
            .map_err(crate::error::ApiError::from)?;

        let ttml = body.get("ttml").and_then(Value::as_str).unwrap_or("").trim();
        if ttml.is_empty() || !ttml.contains('<') {
            return Ok(None);
        }
        let lines = parse_ttml(ttml);
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(LyricsPayload {
            source: "betterlyrics".into(),
            title: song,
            artist,
            synced: true,
            format: "ttml".into(),
            instrumental: false,
            lines,
            plain: None,
            synced_lrc: None,
            ttml: Some(ttml.to_string()),
        }))
    }
}

pub mod lrclib {
    //! lrclib.net — open lyrics database with synced LRC + plain text.

    use super::*;

    const BASE: &str = "https://lrclib.net/api";

    pub async fn fetch(query: &LyricsQuery) -> crate::error::ApiResult<Option<LyricsPayload>> {
        let Some((title, artist)) = clean_query(query) else {
            return Ok(None);
        };

        // Exact match first (duration sharpens it), then ranked search.
        let mut request = client()
            .get(format!("{BASE}/get"))
            .query(&[
                ("track_name", title.as_str()),
                ("artist_name", artist.as_str()),
            ]);
        if let Some(dur) = query.duration {
            request = request.query(&[("duration", format!("{dur:.0}"))]);
        }
        let exact: Option<Value> = match request.send().await {
            Ok(resp) => resp.json::<Value>().await.ok(),
            Err(_) => None,
        };

        let best = match exact {
            Some(v) if v.get("syncedLyrics").is_some() || v.get("plainLyrics").is_some() => Some(v),
            _ => {
                let rows: Value = client()
                    .get(format!("{BASE}/search"))
                    .query(&[("track_name", title.as_str()), ("artist_name", artist.as_str())])
                    .send()
                    .await
                    .map_err(|e| crate::error::ApiError::Unavailable(format!("lrclib: {e}")))?
                    .json()
                    .await
                    .unwrap_or(Value::Null);
                pick_best(rows.as_array(), query.duration)
            }
        };

        let Some(row) = best else { return Ok(None) };
        let synced_raw = row.get("syncedLyrics").and_then(Value::as_str).unwrap_or("");
        let plain = row.get("plainLyrics").and_then(Value::as_str);
        if synced_raw.is_empty() && plain.map(str::trim).unwrap_or("").is_empty() {
            return Ok(None);
        }
        let instrumental = row
            .get("instrumental")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let lines: Vec<LyricsLine> = if synced_raw.is_empty() {
            Vec::new()
        } else {
            parse_lrc(synced_raw)
        };
        let synced = !lines.is_empty();
        Ok(Some(LyricsPayload {
            source: "lrclib".into(),
            title: row
                .get("trackName")
                .and_then(Value::as_str)
                .unwrap_or(title.as_str())
                .to_string(),
            artist: row
                .get("artistName")
                .and_then(Value::as_str)
                .unwrap_or(artist.as_str())
                .to_string(),
            synced,
            format: if synced { "lrc".into() } else { "plain".into() },
            instrumental,
            plain: plain.map(str::to_string),
            synced_lrc: (!synced_raw.is_empty()).then(|| synced_raw.to_string()),
            ttml: None,
            lines,
        }))
    }

    /// Prefers synced results whose duration is closest to the request.
    fn pick_best(rows: Option<&Vec<Value>>, want: Option<f64>) -> Option<Value> {
        let rows = rows?;
        let scored: Vec<(f64, usize, &Value)> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let dur = row.get("duration").and_then(Value::as_f64);
                let has_sync = row
                    .get("syncedLyrics")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty());
                let mut score = if has_sync { 100.0 } else { 0.0 };
                if let (Some(want), Some(dur)) = (want, dur) {
                    score -= (dur - want).abs().min(120.0);
                }
                (score, i, row)
            })
            .collect();
        scored
            .into_iter()
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, _, row)| row.clone())
    }
}

pub mod netease {
    //! NetEase Cloud Music (`music.163.com`) — LRC lyrics, strong CJK
    //! coverage. Search then lyric fetch; both endpoints are public.

    use super::*;

    const BASE: &str = "https://music.163.com";

    pub async fn fetch(query: &LyricsQuery) -> crate::error::ApiResult<Option<LyricsPayload>> {
        let Some((title, artist)) = clean_query(query) else {
            return Ok(None);
        };
        let keyword = format!("{artist} {title}");

        let search: Value = client()
            .post(format!("{BASE}/api/search/pc"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!(
                "s={}&type=1&limit=8",
                urlencoding_form(&keyword)
            ))
            .send()
            .await
            .map_err(|e| crate::error::ApiError::Unavailable(format!("netease: {e}")))?
            .json()
            .await
            .unwrap_or(Value::Null);

        let songs = search
            .pointer("/result/songs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some(song) = pick_song(&songs, query.duration, &title) else {
            return Ok(None);
        };
        let id = song
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| crate::error::ApiError::Unavailable("netease: song id missing".into()))?;

        let lyric: Value = client()
            .get(format!("{BASE}/api/song/lyric"))
            .query(&[("id", id), ("lv", -1), ("kv", -1), ("tv", -1)])
            .send()
            .await
            .map_err(|e| crate::error::ApiError::Unavailable(format!("netease: {e}")))?
            .json()
            .await
            .unwrap_or(Value::Null);

        let lrc = lyric
            .pointer("/lrc/lyric")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if lrc.is_empty() {
            return Ok(None);
        }
        let lines = parse_lrc(lrc);
        let synced = !lines.is_empty();
        Ok(Some(LyricsPayload {
            source: "netease".into(),
            title,
            artist,
            synced,
            format: if synced { "lrc".into() } else { "plain".into() },
            instrumental: false,
            plain: (!synced).then(|| lrc.to_string()),
            synced_lrc: synced.then(|| lrc.to_string()),
            ttml: None,
            lines,
        }))
    }

    fn pick_song(songs: &[Value], want: Option<f64>, title: &str) -> Option<Value> {
        let wanted = normalize(title);
        songs
            .iter()
            .map(|s| {
                let name = s.get("name").and_then(Value::as_str).unwrap_or("");
                let closeness =
                    if normalize(name).contains(&wanted) { 50.0 } else { 0.0 };
                let dur = s
                    .get("duration")
                    .and_then(Value::as_i64)
                    .map(|ms| ms as f64 / 1000.0);
                let near = match (want, dur) {
                    (Some(w), Some(d)) => -(d - w).abs().min(60.0),
                    _ => 0.0,
                };
                (closeness + near, s)
            })
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, s)| s.clone())
    }
}

pub mod genius {
    //! Genius — unofficial search endpoint plus page extraction for plain
    //! text lyrics. Widest western catalog; last in priority order.

    use super::*;

    pub async fn fetch(query: &LyricsQuery) -> crate::error::ApiResult<Option<LyricsPayload>> {
        let Some((title, artist)) = clean_query(query) else {
            return Ok(None);
        };
        let q = format!("{artist} {title}");

        let search: Value = client()
            .get("https://genius.com/api/search/multi")
            .query(&[("q", q.as_str())])
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|e| crate::error::ApiError::Unavailable(format!("genius: {e}")))?
            .json()
            .await
            .map_err(crate::error::ApiError::from)?;

        let path = find_song_path(search.pointer("/response"), &title);
        let Some(path) = path else { return Ok(None) };

        let html = client()
            .get(format!("https://genius.com{path}"))
            .header("accept", "text/html")
            .send()
            .await
            .map_err(|e| crate::error::ApiError::Unavailable(format!("genius page: {e}")))?
            .text()
            .await
            .map_err(crate::error::ApiError::from)?;

        let text = extract_lyrics_html(&html);
        if text.lines().count() < 3 {
            return Ok(None);
        }
        Ok(Some(LyricsPayload {
            source: "genius".into(),
            title,
            artist,
            synced: false,
            format: "plain".into(),
            instrumental: false,
            plain: Some(text),
            synced_lrc: None,
            ttml: None,
            lines: Vec::new(),
        }))
    }

    /// Walks response.sections[].items[] for the first song hit matching the
    /// requested title (falls back to the first song of any name).
    fn find_song_path(response: Option<&Value>, title: &str) -> Option<String> {
        let sections = response?.get("sections")?.as_array()?;
        let wanted = normalize(title);
        let mut fallback = None;
        for section in sections {
            for item in section.get("items")?.as_array()?.iter() {
                if item.get("type").and_then(Value::as_str) != Some("song") {
                    continue;
                }
                let result_path = item
                    .pointer("/result/path")
                    .and_then(Value::as_str)?
                    .to_string();
                let full_title =
                    item.pointer("/result/full_title").and_then(Value::as_str).unwrap_or("");
                if fallback.is_none() {
                    fallback = Some(result_path.clone());
                }
                if normalize(full_title).contains(&wanted)
                    || wanted.contains(&normalize(
                        full_title.split(" by ").next().unwrap_or(full_title),
                    ))
                {
                    return Some(result_path);
                }
            }
        }
        fallback
    }

    /// Pulls `<div data-lyrics-container="true">…</div>` blocks and converts
    /// to readable text.
    fn extract_lyrics_html(html: &str) -> String {
        static CONTAINER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r#"(?is)<div[^>]*data-lyrics-container="true"[^>]*>(.*?)</div>"#)
                .expect("container regex")
        });
        static BREAKS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(?i)<br\s*/?>|</p>").expect("break regex")
        });
        let mut out = String::new();
        for cap in CONTAINER.captures_iter(html) {
            let chunk = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let with_breaks = BREAKS.replace_all(chunk, "\n");
            out.push_str(&html_to_text(&with_breaks));
            out.push('\n');
        }
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Minimal percent-encoding for POST bodies (spaces + reserved chars).
fn urlencoding_form(input: &str) -> String {
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
