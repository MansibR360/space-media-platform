//! First-class torrent search.
//!
//! Torrents are how books, comics and long-tail media are actually available:
//! the public book sources contribute almost nothing, and 1337x sits behind
//! anti-bot so a server cannot read it. This index answers a plain JSON API
//! with no key and no protection, and it carries every category — ebooks,
//! comics, audiobooks, music, film and television — which is what makes it
//! useful for the things nothing else covers.
//!
//! A result is self-contained: the magnet is built from the info hash and
//! carried in the id, so resolving one costs no second request.

use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use super::torrent_cache::{self, CachedTorrent};
use crate::plugins::native::engines::{EngineCtx, quote_plus};

#[derive(Debug, Clone)]
pub struct TorrentIndex {
    pub base: String,
}

/// Trackers to attach to every magnet. Without them a client has only the
/// DHT to work with, which makes a healthy torrent look dead.
const TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.tracker.cl:1337/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://exodus.desync.com:6969/announce",
];

impl TorrentIndex {
    pub fn new() -> Self {
        Self {
            base: std::env::var("TORRENT_API_BASE")
                .ok()
                .map(|v| v.trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "https://apibay.org".to_string()),
        }
    }

    /// What SPACE calls the index's numeric category.
    ///
    /// The ranges are the index's own: 100s audio, 200s video, 600s other —
    /// where the ebooks and comics live, which is the whole point of having
    /// this source.
    fn category_of(code: i64) -> &'static str {
        match code {
            102 | 199 => "audiobook",
            100..=199 => "music",
            201 | 207 => "movie",
            205 | 208 => "series",
            200..=299 => "movie",
            601 => "book",
            602 => "manga",
            _ => "movie",
        }
    }

    /// Adult categories are excluded: nobody searching for a novel wants them
    /// appearing beside it.
    fn is_excluded(code: i64) -> bool {
        (500..=599).contains(&code)
    }

    fn magnet(info_hash: &str, name: &str) -> String {
        let trackers: String = TRACKERS
            .iter()
            .map(|t| format!("&tr={}", quote_plus(t)))
            .collect();
        format!(
            "magnet:?xt=urn:btih:{info_hash}&dn={}{trackers}",
            quote_plus(name)
        )
    }

    /// Bytes as a person reads them.
    fn human_size(bytes: i64) -> String {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut size = bytes as f64;
        let mut unit = 0;
        while size >= 1024.0 && unit < UNITS.len() - 1 {
            size /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{bytes} B")
        } else {
            format!("{size:.1} {}", UNITS[unit])
        }
    }

    /// Ask every index, fold in everything already known, and answer with the
    /// best copy of each release.
    ///
    /// The cache is consulted first and always contributes: an index that is
    /// down today cannot take away a magnet found yesterday.
    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }

        let mut found = torrent_cache::lookup(query).await;
        let known = found.len();

        // Indexes are asked together; one being slow or down must not decide
        // the whole answer.
        let (apibay, knaben) = tokio::join!(
            self.search_apibay(ctx, query),
            self.search_knaben(ctx, query)
        );
        let fresh: Vec<CachedTorrent> = apibay.into_iter().chain(knaben).collect();

        if !fresh.is_empty() {
            torrent_cache::store(query, &fresh).await;
            found.extend(fresh);
        }

        if found.is_empty() {
            return Ok(json!({
                "results": [],
                "note": "no index answered, and nothing was already known",
            }));
        }

        tracing::debug!(query, known, total = found.len(), "torrent search");

        let results: Vec<Value> = torrent_cache::best_per_release(found)
            .into_iter()
            .take(limit.clamp(1, 40))
            .map(|t| {
                // Self-contained id: resolving costs no request at all.
                let id = serde_json::to_string(&json!({
                    "magnet": t.magnet,
                    "title": t.title,
                    "size": Self::human_size(t.size_bytes),
                    "seeders": t.seeders,
                }))
                .unwrap_or_default();

                json!({
                    "id": id,
                    "title": t.title,
                    "overview": format!(
                        "S:{} \u{b7} {} \u{b7} {}",
                        t.seeders,
                        Self::human_size(t.size_bytes),
                        t.source
                    ),
                    "year": Value::Null,
                    "poster": "",
                    "type": t.category,
                })
            })
            .collect();

        Ok(json!({ "results": results }))
    }

    /// The plain JSON index: no key, no protection, every category.
    async fn search_apibay(&self, ctx: &EngineCtx, query: &str) -> Vec<CachedTorrent> {
        let url = format!("{}/q.php?q={}&cat=0", self.base, quote_plus(query));
        let Ok(body) = ctx.get_json(&url).await else {
            return Vec::new();
        };

        body.as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                let name = row.get("name").and_then(Value::as_str)?;
                let info_hash = row.get("info_hash").and_then(Value::as_str)?;
                // "no results" comes back as one row with a zero hash
                if name.is_empty() || info_hash.chars().all(|c| c == '0') {
                    return None;
                }
                let number = |key: &str| -> i64 {
                    row.get(key)
                        .and_then(|v| {
                            v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_i64())
                        })
                        .unwrap_or(0)
                };
                let category = number("category");
                if Self::is_excluded(category) {
                    return None;
                }
                Some(CachedTorrent {
                    info_hash: info_hash.to_uppercase(),
                    title: name.to_string(),
                    magnet: Self::magnet(info_hash, name),
                    seeders: number("seeders"),
                    size_bytes: number("size"),
                    category: Self::category_of(category).to_string(),
                    source: "apibay".into(),
                })
            })
            .collect()
    }

    /// Knaben aggregates many trackers behind one JSON API, which is what
    /// makes it worth asking alongside the others: one call, many indexes.
    async fn search_knaben(&self, ctx: &EngineCtx, query: &str) -> Vec<CachedTorrent> {
        let body = json!({
            "search_type": "score",
            "search_field": "title",
            "query": query,
            "order_by": "seeders",
            "order_direction": "desc",
            "size": 40,
            "hide_unsafe": true,
        });

        let Ok(response) = ctx
            .post_json("https://api.knaben.org/v1", &body)
            .await
        else {
            return Vec::new();
        };

        response
            .get("hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                let title = row.get("title").and_then(Value::as_str)?;
                let hash = row.get("hash").and_then(Value::as_str)?;
                if title.is_empty() || hash.is_empty() {
                    return None;
                }
                let magnet = row
                    .get("magnetUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| Self::magnet(hash, title));

                // Knaben's category ids are its own; the leading digits say
                // what kind of thing it is.
                let category_id = row
                    .get("categoryId")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let category = match category_id / 1_000_000 {
                    1 => "music",
                    2 => "movie",
                    3 => "series",
                    6 => "book",
                    _ => "movie",
                };

                Some(CachedTorrent {
                    info_hash: hash.to_uppercase(),
                    title: title.to_string(),
                    magnet,
                    seeders: row.get("seeders").and_then(Value::as_i64).unwrap_or(0),
                    size_bytes: row.get("bytes").and_then(Value::as_i64).unwrap_or(0),
                    category: category.to_string(),
                    source: "knaben".into(),
                })
            })
            .collect()
    }

    pub async fn details(&self, _ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let row: Value = serde_json::from_str(id)
            .map_err(|_| ApiError::bad_request("id must be a torrent search result"))?;

        let magnet = row
            .get("magnet")
            .and_then(Value::as_str)
            .filter(|m| m.starts_with("magnet:?xt=urn:btih:"))
            .ok_or_else(|| ApiError::bad_request("id carries no magnet"))?;

        let size = row.get("size").and_then(Value::as_str).unwrap_or("");
        let seeders = row
            .get("seeders")
            .and_then(|v| v.as_i64())
            .unwrap_or_default();
        let title = row
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Torrent");

        Ok(json!({
            "title": title,
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "movie",
            "streams": [{
                "label": format!("magnet \u{b7} {size} \u{b7} S:{seeders}"),
                "url": magnet,
                "kind": "torrent",
            }],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_map_to_what_the_thing_actually_is() {
        // the reason this source exists: books and comics
        assert_eq!(TorrentIndex::category_of(601), "book");
        assert_eq!(TorrentIndex::category_of(602), "manga");
        assert_eq!(TorrentIndex::category_of(102), "audiobook");
        assert_eq!(TorrentIndex::category_of(101), "music");
        assert_eq!(TorrentIndex::category_of(207), "movie");
        assert_eq!(TorrentIndex::category_of(205), "series");
    }

    #[test]
    fn adult_categories_are_left_out() {
        assert!(TorrentIndex::is_excluded(501));
        assert!(!TorrentIndex::is_excluded(601));
        assert!(!TorrentIndex::is_excluded(201));
    }

    #[test]
    fn a_magnet_carries_its_name_and_trackers() {
        let magnet = TorrentIndex::magnet("ABC123", "Dune by Frank Herbert");
        assert!(magnet.starts_with("magnet:?xt=urn:btih:ABC123"));
        assert!(magnet.contains("dn=Dune"), "the name travels with it");
        assert!(
            magnet.contains("tr=udp"),
            "trackers matter: DHT alone makes a healthy torrent look dead"
        );
    }

    #[test]
    fn sizes_read_the_way_people_write_them() {
        assert_eq!(TorrentIndex::human_size(512), "512 B");
        assert_eq!(TorrentIndex::human_size(1024), "1.0 KiB");
        assert_eq!(TorrentIndex::human_size(1_073_741_824), "1.0 GiB");
    }
}
