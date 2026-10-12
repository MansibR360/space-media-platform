//! A permanent home for magnets, and the place duplicates get resolved.
//!
//! Two things make this worth having:
//!
//! * A magnet never changes — the info hash *is* the content — so a torrent
//!   seen once never needs fetching again, and a repeat search is answered
//!   from here rather than from an index.
//! * Every index lists the same film several times over. Seeders are the only
//!   thing that decides whether a torrent actually downloads, so when the same
//!   release shows up twice only the best-seeded copy is kept.
//!
//! Seeder counts are the exception to "written once": they are refreshed
//! whenever an index is consulted, because a torrent with no seeders today is
//! a dead link regardless of how healthy it was when first seen.

use std::sync::OnceLock;

use sqlx::PgPool;

static POOL: OnceLock<PgPool> = OnceLock::new();

pub fn init(pool: PgPool) {
    let _ = POOL.set(pool);
}

fn pool() -> Option<&'static PgPool> {
    POOL.get()
}

/// One torrent, from an index or from the cache.
#[derive(Debug, Clone)]
pub struct CachedTorrent {
    pub info_hash: String,
    pub title: String,
    pub magnet: String,
    pub seeders: i64,
    pub size_bytes: i64,
    pub category: String,
    pub source: String,
}

/// The query as a cache key: the same search however it was typed.
pub fn query_key(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A release identity that survives the noise indexes wrap around it.
///
/// "Dune 2021 1080p BluRay x265" and "Dune.2021.2160p.WEB-DL" are the same
/// film, and the viewer wants the one that will actually download — not both.
/// Quality tags are deliberately *kept* out of the key so different cuts of the
/// same title collapse together and compete on seeders.
pub fn title_key(title: &str) -> String {
    const NOISE: &[&str] = &[
        "bluray", "blu", "ray", "webrip", "web", "dl", "webdl", "hdrip", "dvdrip", "brrip",
        "hdtv", "x264", "x265", "hevc", "avc", "aac", "ac3", "dts", "hdr", "sdr", "imax",
        "remux", "repack", "proper", "extended", "uncut", "remastered", "dual", "audio",
        "subs", "esub", "esubs", "multi", "10bit", "8bit",
    ];

    let mut words: Vec<String> = Vec::new();
    for word in title.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let lower = word.to_lowercase();
        // resolutions
        if lower.ends_with('p')
            && lower.len() > 1
            && lower[..lower.len() - 1].chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        if NOISE.contains(&lower.as_str()) {
            continue;
        }
        words.push(lower);
    }
    words.join(" ")
}

/// Everything already known for a query.
pub async fn lookup(query: &str) -> Vec<CachedTorrent> {
    let Some(pool) = pool() else {
        return Vec::new();
    };
    let rows: Vec<(String, String, String, i32, i64, String, String)> = sqlx::query_as(
        "SELECT info_hash, title, magnet, seeders, size_bytes, category, source
         FROM torrent_cache
         WHERE query_key = $1
         ORDER BY seeders DESC
         LIMIT 200",
    )
    .bind(query_key(query))
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(
            |(info_hash, title, magnet, seeders, size_bytes, category, source)| CachedTorrent {
                info_hash,
                title,
                magnet,
                seeders: seeders as i64,
                size_bytes,
                category,
                source,
            },
        )
        .collect()
}

/// Remember what an index returned.
///
/// The magnet is written once; seeders and the query that found it are kept
/// current, so a torrent found again under a different search is reachable
/// from both.
pub async fn store(query: &str, torrents: &[CachedTorrent]) {
    let Some(pool) = pool() else { return };
    let key = query_key(query);

    for torrent in torrents {
        let _ = sqlx::query(
            "INSERT INTO torrent_cache
                (info_hash, title, title_key, magnet, seeders, size_bytes, category, source, query_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (info_hash) DO UPDATE SET
                seeders = EXCLUDED.seeders,
                query_key = EXCLUDED.query_key,
                refreshed_at = now()",
        )
        .bind(&torrent.info_hash)
        .bind(&torrent.title)
        .bind(title_key(&torrent.title))
        .bind(&torrent.magnet)
        .bind(torrent.seeders as i32)
        .bind(torrent.size_bytes)
        .bind(&torrent.category)
        .bind(&torrent.source)
        .bind(&key)
        .execute(pool)
        .await;
    }
}

/// Collapse duplicates, keeping the copy most likely to download.
///
/// Called with everything known about a query — freshly fetched and
/// remembered — so the answer improves as more indexes are consulted rather
/// than being whatever the last one said.
pub fn best_per_release(mut torrents: Vec<CachedTorrent>) -> Vec<CachedTorrent> {
    // Best-seeded first, so the first of each release is the one to keep.
    torrents.sort_by(|a, b| b.seeders.cmp(&a.seeders));

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for torrent in torrents {
        // A dead torrent is not a result.
        if torrent.seeders <= 0 {
            continue;
        }
        let key = title_key(&torrent.title);
        if key.is_empty() || seen.insert(key) {
            out.push(torrent);
        }
    }
    out
}

/// What the cache is holding.
pub async fn stats() -> serde_json::Value {
    let Some(pool) = pool() else {
        return serde_json::json!({ "entries": 0 });
    };
    let row: Result<(i64,), _> = sqlx::query_as("SELECT count(*)::bigint FROM torrent_cache")
        .fetch_one(pool)
        .await;
    match row {
        Ok((entries,)) => serde_json::json!({ "entries": entries }),
        Err(err) => {
            tracing::warn!(error = %err, "torrent cache stats unavailable");
            serde_json::json!({ "entries": 0 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torrent(title: &str, seeders: i64, hash: &str) -> CachedTorrent {
        CachedTorrent {
            info_hash: hash.into(),
            title: title.into(),
            magnet: format!("magnet:?xt=urn:btih:{hash}"),
            seeders,
            size_bytes: 1024,
            category: "movie".into(),
            source: "test".into(),
        }
    }

    #[test]
    fn the_same_release_collapses_to_its_best_seeded_copy() {
        let found = vec![
            torrent("Dune 2021 1080p BluRay x265", 12, "AAA"),
            torrent("Dune.2021.2160p.WEB-DL.HDR", 480, "BBB"),
            torrent("Dune 2021 720p WEBRip", 30, "CCC"),
        ];
        let best = best_per_release(found);
        assert_eq!(best.len(), 1, "one film, one result");
        assert_eq!(best[0].seeders, 480, "the copy that will actually download");
    }

    #[test]
    fn different_films_are_not_merged() {
        let found = vec![
            torrent("Dune 2021 1080p", 100, "AAA"),
            torrent("Dune Part Two 2024 1080p", 90, "BBB"),
        ];
        assert_eq!(best_per_release(found).len(), 2);
    }

    #[test]
    fn dead_torrents_are_dropped() {
        let found = vec![torrent("Dune 2021", 0, "AAA"), torrent("Arrival 2016", 5, "BBB")];
        let best = best_per_release(found);
        assert_eq!(best.len(), 1);
        assert_eq!(best[0].title, "Arrival 2016");
    }

    #[test]
    fn release_noise_does_not_split_a_title() {
        assert_eq!(
            title_key("Dune.2021.2160p.WEB-DL.x265.HDR"),
            title_key("Dune 2021 1080p BluRay x264")
        );
    }

    #[test]
    fn the_same_search_shares_a_key() {
        assert_eq!(query_key("  Dune   FRANK herbert "), query_key("dune frank herbert"));
    }
}
