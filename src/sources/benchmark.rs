//! Daily source benchmark & ratings engine.
//!
//! Once every 24 h (and once shortly after boot) every registered source â€”
//! built-ins, learned web sources, hubs, stremio extensions, HTTP plugins â€”
//! gets a *real* end-to-end test:
//!
//!   1. timed search call          â†’ latency_ms, search_ok
//!   2. resolve its first hit      â†’ streams_found, resolve_ok
//!   3. ranged download (256 KB)   â†’ speed_kbps (only for direct media)
//!
//! Results land in `source_ratings`, are smoothed against the previous
//! score, and feed two consumer surfaces:
//!
//!   â€¢ provider ordering â€” fastest sources are consulted first during
//!     fan-out, failover and lookup (`gather_providers` sorts by score)
//!   â€¢ stream selection â€” `stream_score` already blends a per-source bonus
//!
//! `GET /api/sources/rankings` exposes the leaderboard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use axum::extract::State as AxumState;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::error::ApiResult;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RatingRow {
    pub source: String,
    pub last_checked: chrono::DateTime<chrono::Utc>,
    pub last_ok: Option<chrono::DateTime<chrono::Utc>>,
    pub latency_ms: Option<i32>,
    pub search_ok: bool,
    pub resolve_ok: bool,
    pub streams_found: i32,
    pub speed_kbps: Option<i32>,
    pub score: f64,
}

/// In-memory snapshot for hot-path lookups (aggregator ordering/bonus).
#[derive(Default)]
pub struct Ratings {
    inner: RwLock<HashMap<String, RatingRow>>,
}

impl Ratings {
    pub async fn get(&self, name: &str) -> Option<RatingRow> {
        self.inner.read().await.get(name).cloned()
    }

    pub async fn snapshot(&self) -> Vec<RatingRow> {
        let mut v: Vec<RatingRow> = self.inner.read().await.values().cloned().collect();
        v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    async fn put(&self, row: RatingRow) {
        self.inner.write().await.insert(row.source.clone(), row);
    }

    pub async fn reload(&self, pool: &PgPool) {
        match sqlx::query_as::<_, RatingRow>("SELECT * FROM source_ratings")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => {
                let mut map = self.inner.write().await;
                map.clear();
                for r in rows {
                    map.insert(r.source.clone(), r);
                }
            }
            Err(err) => tracing::warn!(error = %err, "ratings reload failed"),
        }
    }
}

// ------------------------------------------------------------ score model

/// Composite 0..100. Weights: search works (35) · resolvable (25) ·
/// latency tier (max 15) · throughput tier (max 20) · streams richness (5).
fn compute_score(latency_ms: Option<i64>, resolve_ok: bool, streams: usize, kbps: Option<i64>) -> f64 {
    let mut s = 35.0f64; // search answered
    if resolve_ok && streams > 0 {
        s += 25.0;
    } else if resolve_ok {
        s += 5.0;
    }
    s += match latency_ms {
        Some(ms) if ms <= 800 => 15.0,
        Some(ms) if ms <= 1500 => 12.0,
        Some(ms) if ms <= 3000 => 8.0,
        Some(_) => 4.0,
        None => 0.0,
    };
    s += match kbps {
        Some(k) if k >= 3000 => 20.0,
        Some(k) if k >= 1500 => 16.0,
        Some(k) if k >= 700 => 12.0,
        Some(k) if k >= 250 => 8.0,
        Some(k) if k > 0 => 4.0,
        _ => 0.0,
    };
    if streams > 3 {
        s += 5.0;
    }
    s.clamp(0.0, 100.0)
}

fn blend(previous: Option<f64>, fresh: f64) -> f64 {
    match previous {
        Some(old) => ((old * 0.4) + (fresh * 0.6)).clamp(0.0, 100.0),
        None => fresh,
    }
}

// -------------------------------------------------------------- benchmark

struct BenchOutcome {
    latency_ms: i64,
    search_ok: bool,
    resolve_ok: bool,
    streams: usize,
    kbps: Option<i64>,
    sample_url: Option<String>,
}

async fn bench_one(state: std::sync::Arc<crate::state::AppState>, name: String) -> BenchOutcome {
    // probe word tuned per category family; "" exercises browse rails
    let words = ["star", "love", "one"];
    let name = name.as_str();
    let mut result = BenchOutcome {
        latency_ms: 0,
        search_ok: false,
        resolve_ok: false,
        streams: 0,
        kbps: None,
        sample_url: None,
    };

    let started = Instant::now();
    let mut hits: Vec<crate::sources::SourceItem> = Vec::new();

    if state.web_sources.get(name).await.is_some() {
        hits = state.web_sources.search(&state, name, words[0], 3).await.unwrap_or_default();
    } else if let Some(ext) = state.extensions.find_by_prefix(name).await {
        // extensions: exercise the stream resource for a well-known id shape
        let sid = "tt0468569"; // constant, measures reachability+latency
        let url = ext.target.stream_url("movie", sid);
        result.latency_ms = 0;
        let started2 = Instant::now();
        match crate::extensions::http_get_json_cfg(&state.cfg, &url).await {
            Ok(body) => {
                result.search_ok = true;
                result.latency_ms = started2.elapsed().as_millis() as i64;
                let links = crate::extensions::convert_streams(
                    body.get("streams").unwrap_or(&serde_json::Value::Null),
                    sid,
                );
                result.streams = links.len();
                result.resolve_ok = !links.is_empty();
                if let Some(best) = links.iter().find(|l| matches!(l.kind.as_str(), "mp4" | "hls" | "audio")) {
                    result.sample_url = Some(best.url.clone());
                }
            }
            Err(e) => tracing::debug!(ext = %name, error = %e, "ext bench failed"),
        }
        let _ = started; // search-equivalent timing captured above
        return result;
    } else {
        // natives + everything else share the generic search surface
        for w in words {
            let found = tokio::time::timeout(
                Duration::from_secs(9),
                search_generic(&state, name, w, 3),
            )
            .await;
            match found {
                Ok(Ok(items)) => {
                    hits = items;
                    break;
                }
                Ok(Err(_)) => continue,
                Err(_) => continue,
            }
        }
    }

    result.latency_ms = started.elapsed().as_millis() as i64;
    result.search_ok = !hits.is_empty() || result.search_ok;

    // resolve first hit â†’ streams (+ sample url for speed test)
    if let Some(first) = hits.first() {
        if let Ok(detail) = tokio::time::timeout(
            Duration::from_secs(9),
            item_generic(&state, name, &first.external_id),
        )
        .await
        {
            if let Ok(detail) = detail {
                result.resolve_ok = true;
                result.streams = detail.streams.len();
                if let Some(best) = crate::sources::aggregator::best_stream(&detail.streams) {
                    if matches!(best.kind.as_str(), "mp4" | "hls" | "audio") {
                        result.sample_url = Some(best.url.clone());
                    }
                }
            }
        }
    }

    // ranged throughput probe (256 KB cap, 4 s budget)
    if let Some(url) = &result.sample_url {
        if let Some(kbps) = speed_probe(url).await {
            result.kbps = Some(kbps);
        }
    }

    result
}

/// Generic search across natives / web / hubs / http-plugins by name.
async fn search_generic(
    state: &crate::state::AppState,
    name: &str,
    query: &str,
    limit: i32,
) -> ApiResult<Vec<crate::sources::SourceItem>> {
    if state.sources.get(name).is_some() {
        return state.sources.get(name).unwrap().search(query, limit).await;
    }
    if state.web_sources.get(name).await.is_some() {
        return state.web_sources.search(state, name, query, limit).await;
    }
    if let Ok(Some(hub_row)) = sqlx::query_as::<_, crate::sources::hub::HubRow>(
        "SELECT * FROM source_hubs WHERE name = $1 AND enabled",
    )
    .bind(name)
    .fetch_optional(&state.pool)
    .await
    {
        let hits =
            crate::sources::hub::find(&state.cfg, hub_row.r#ref(), query, None, limit).await?;
        return Ok(hits
            .into_iter()
            .map(|h| crate::sources::SourceItem {
                external_id: format!("{}|{}", h.source, h.id),
                title: h.title,
                overview: h.overview,
                year: h.year,
                poster: h.poster,
                category: "movie",
            })
            .collect());
    }
    if let Ok(Some(plugin)) = sqlx::query_as::<_, crate::plugins::models::PluginRecord>(
        "SELECT * FROM plugins WHERE name = $1 AND enabled AND kind IN ('source','metadata')",
    )
    .bind(name)
    .fetch_optional(&state.pool)
    .await
    {
        if let Some(base) = &plugin.base_url {
            let resp = reqwest::Client::builder()
                .timeout(Duration::from_secs(9))
                .build()
                .map_err(|e| crate::error::ApiError::Unavailable(format!("{e}")))?
                .get(format!("{}/search", base.trim_end_matches('/')))
                .query(&[("q", query)])
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            let rows = body.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            return Ok(rows
                .into_iter()
                .filter_map(|r| {
                    Some(crate::sources::SourceItem {
                        external_id: r.get("id")?.as_str()?.to_string(),
                        title: r.get("title")?.as_str()?.to_string(),
                        overview: String::new(),
                        year: None,
                        poster: None,
                        category: "movie",
                    })
                })
                .collect());
        }
    }
    Err(crate::error::ApiError::not_found("provider"))
}

/// Generic item resolve across natives / web / hubs / http-plugins.
async fn item_generic(
    state: &crate::state::AppState,
    name: &str,
    external_id: &str,
) -> ApiResult<crate::sources::SourceDetail> {
    if state.sources.get(name).is_some() {
        return state.sources.get(name).unwrap().item(external_id).await;
    }
    if state.web_sources.get(name).await.is_some() {
        return state.web_sources.item(state, name, external_id).await;
    }
    if let Ok(Some(hub_row)) = sqlx::query_as::<_, crate::sources::hub::HubRow>(
        "SELECT * FROM source_hubs WHERE name = $1 AND enabled",
    )
    .bind(name)
    .fetch_optional(&state.pool)
    .await
    {
        let (remote_source, remote_id) = external_id.split_once('|').ok_or_else(|| {
            crate::error::ApiError::bad_request("bad hub id")
        })?;
        let body = crate::sources::hub::item_detail(
            &state.cfg,
            hub_row.r#ref(),
            remote_source,
            remote_id,
        )
        .await?;
        let streams: Vec<crate::sources::StreamLink> = body
            .get("streams")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(crate::sources::StreamLink {
                            label: s.get("label").and_then(|v| v.as_str()).map(str::to_string)?,
                            url: s.get("url").and_then(|v| v.as_str()).map(str::to_string)?,
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
        return Ok(crate::sources::SourceDetail {
            item: crate::sources::SourceItem {
                external_id: external_id.to_string(),
                title: body.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
                overview: String::new(),
                year: None,
                poster: None,
                category: "movie",
            },
            streams,
            chapters: None,
        });
    }
    if let Ok(Some(plugin)) = sqlx::query_as::<_, crate::plugins::models::PluginRecord>(
        "SELECT * FROM plugins WHERE name = $1 AND enabled",
    )
    .bind(name)
    .fetch_optional(&state.pool)
    .await
    {
        if let Some(base) = &plugin.base_url {
            let resp = reqwest::Client::builder()
                .timeout(Duration::from_secs(9))
                .build()
                .map_err(|e| crate::error::ApiError::Unavailable(format!("{e}")))?
                .get(format!("{}/details", base.trim_end_matches('/')))
                .query(&[("id", external_id)])
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            let streams: Vec<crate::sources::StreamLink> = body
                .get("streams")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            Some(crate::sources::StreamLink {
                                label: s.get("label").and_then(|v| v.as_str()).map(str::to_string)?,
                                url: s.get("url").and_then(|v| v.as_str()).map(str::to_string)?,
                                kind: s.get("kind").and_then(|v| v.as_str()).map(str::to_string)?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Ok(crate::sources::SourceDetail {
                item: crate::sources::SourceItem {
                    external_id: external_id.to_string(),
                    title: body.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
                    overview: body.get("overview").and_then(|v| v.as_str()).unwrap_or("").into(),
                    year: body.get("year").and_then(|v| v.as_i64()).map(|v| v as i32),
                    poster: body.get("poster").and_then(|v| v.as_str()).map(str::to_string),
                    category: "movie",
                },
                streams,
                chapters: None,
            });
        }
    }
    Err(crate::error::ApiError::not_found("provider"))
}

/// Ranged throughput probe: pulls â‰¤256 KB and returns KB/s.
async fn speed_probe(url: &str) -> Option<i64> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("space-media-server/0.1")
        .build()
        .ok()?;
    let started = Instant::now();
    let resp = client
        .get(url)
        .header("Range", "bytes=0-262143")
        .send()
        .await
        .ok()?;
    let bytes = resp.bytes().await.ok()?.len() as f64;
    let secs = started.elapsed().as_secs_f64().max(0.001);
    Some((bytes / 1024.0 / secs) as i64)
}

/// Runs one full benchmark sweep and persists blended scores.
pub async fn run_sweep(state: &std::sync::Arc<crate::state::AppState>) -> Vec<RatingRow> {
    let mut names: Vec<String> = state.sources.list().iter().map(|s| s.to_string()).collect();
    for e in state.web_sources.list().await {
        names.push(e.source.name().to_string());
    }
    for e in state.extensions.list().await {
        names.push(format!("ext:{}", e.addon_id()));
    }
    if let Ok(hubs) = crate::sources::hub::repo::list(&state.pool).await {
        for h in hubs.into_iter().filter(|h| h.enabled) {
            names.push(h.name);
        }
    }
    if let Ok(plugins) = crate::plugins::repo::list(&state.pool).await {
        for p in plugins.into_iter().filter(|p| p.enabled) {
            names.push(p.name);
        }
    }
    names.dedup();

    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let mut jobs = Vec::new();
    for name in names {
        let permit = sem.clone().acquire_owned().await.ok();
        let st = state.clone();
        jobs.push(tokio::spawn(async move {
            let _permit = permit;
            (name.clone(), bench_one(st, name).await)
        }));
    }

    let mut rows = Vec::new();
    for job in futures_util::future::join_all(jobs).await {
        let (name, outcome) = match job {
            Ok(v) => v,
            Err(_) => continue,
        };
        let prev = state.source_ratings.get(&name).await.map(|r| r.score);
        let score = blend(prev, compute_score(
            Some(outcome.latency_ms),
            outcome.resolve_ok,
            outcome.streams,
            outcome.kbps,
        ));
        let row = RatingRow {
            source: name.clone(),
            last_checked: chrono::Utc::now(),
            last_ok: outcome.search_ok.then(chrono::Utc::now),
            latency_ms: Some(outcome.latency_ms as i32),
            search_ok: outcome.search_ok,
            resolve_ok: outcome.resolve_ok,
            streams_found: outcome.streams as i32,
            speed_kbps: outcome.kbps.map(|k| k as i32),
            score,
        };
        let _ = sqlx::query(
            "INSERT INTO source_ratings (source, last_checked, last_ok, latency_ms,
                search_ok, resolve_ok, streams_found, speed_kbps, score)
             VALUES ($1, now(), $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (source) DO UPDATE SET
                last_checked = now(), last_ok = $2, latency_ms = $3, search_ok = $4,
                resolve_ok = $5, streams_found = $6, speed_kbps = $7, score = $8",
        )
        .bind(&row.source)
        .bind(row.last_ok)
        .bind(row.latency_ms)
        .bind(row.search_ok)
        .bind(row.resolve_ok)
        .bind(row.streams_found)
        .bind(row.speed_kbps)
        .bind(row.score)
        .execute(&state.pool)
        .await;
        state.source_ratings.put(row.clone()).await;
        rows.push(row);
    }
    rows.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    metrics::gauge!("sources_benchmarked").set(rows.len() as f64);
    rows
}

/// Daily worker: first sweep after 30 s, then every 24 h.
pub fn spawn_worker(state: std::sync::Arc<crate::state::AppState>, cancel: tokio_util::sync::CancellationToken) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let st = state.clone();
            let started = std::time::Instant::now();
            let rows = run_sweep(&st).await;
            tracing::info!(count = rows.len(), elapsed_s = started.elapsed().as_secs(), "daily source benchmark complete");
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(24 * 3600)) => {}
            }
        }
    });
}

// -------------------------------------------------------------- endpoints

/// Source leaderboard — daily benchmark scores, fastest first.
#[utoipa::path(get, path = "/api/sources/rankings", tag = "sources",
    responses((status = 200))
)]
pub async fn rankings(
    AxumState(state): AxumState<std::sync::Arc<crate::state::AppState>>,
) -> ApiResult<axum::Json<Value>> {
    let mut rows = state.source_ratings.snapshot().await;
    // keep only sources that currently exist
    let known: std::collections::HashSet<String> = {
        let mut set: std::collections::HashSet<String> =
            state.sources.list().iter().map(|s| s.to_string()).collect();
        for e in state.web_sources.list().await {
            set.insert(e.source.name().to_string());
        }
        for e in state.extensions.list().await {
            set.insert(format!("ext:{}", e.addon_id()));
        }
        if let Ok(hubs) = crate::sources::hub::repo::list(&state.pool).await {
            for h in hubs.into_iter().filter(|h| h.enabled) {
                set.insert(h.name);
            }
        }
        if let Ok(plugins) = crate::plugins::repo::list(&state.pool).await {
            for p in plugins.into_iter().filter(|p| p.enabled) {
                set.insert(p.name);
            }
        }
        set
    };
    rows.retain(|r| known.contains(&r.source));
    Ok(axum::Json(Value::Array(
        rows.into_iter()
            .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::{blend, compute_score};

    #[test]
    fn score_tiers() {
        // search only
        let base = compute_score(Some(5000), false, 0, None);
        assert!((base - 39.0).abs() < f64::EPSILON);
        // resolvable with streams beats resolvable-empty
        assert!(compute_score(Some(500), true, 4, Some(4000)) > 95.0);
        assert!(compute_score(Some(500), true, 1, None) < 80.0);
        // latency tiers
        assert!(compute_score(Some(700), true, 0, None) > compute_score(Some(2500), true, 0, None));
        // throughput tiers
        assert!(compute_score(Some(300), true, 5, Some(3200)) > compute_score(Some(300), true, 5, Some(600)));
    }

    #[test]
    fn blend_pulls_toward_fresh() {
        assert_eq!(blend(None, 70.0), 70.0);
        let blended = blend(Some(50.0), 90.0);
        assert!(blended > 50.0 && blended < 90.0, "{}", blended);
        assert_eq!(blend(Some(100.0), 100.0), 100.0);
    }
}
