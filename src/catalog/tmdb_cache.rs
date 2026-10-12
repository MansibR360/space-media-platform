//! A permanent cache in front of TMDB.
//!
//! Every TMDB-keyed source asks the same questions — the same search for a
//! popular title, the same detail lookup for the same id — once per source, per
//! request. That is a lot of identical round trips against a rate limit, for
//! answers that do not change.
//!
//! So an answer is fetched once and kept. Nothing is evicted: TMDB's reply for
//! "movie 157336" is the same next year as today, which is exactly the kind of
//! thing worth remembering forever.
//!
//! The pool is held here rather than threaded through every engine, because the
//! callers are deep in code that has no business knowing about a database. A
//! server that has not started yet simply misses the cache and fetches.

use std::sync::OnceLock;

use serde_json::Value;
use sqlx::PgPool;

static POOL: OnceLock<PgPool> = OnceLock::new();

/// Called once at startup. Until it is, lookups fall through to the network.
pub fn init(pool: PgPool) {
    let _ = POOL.set(pool);
}

fn pool() -> Option<&'static PgPool> {
    POOL.get()
}

/// The cache key for a TMDB request: the path and query with the credential
/// removed, so one account's key does not fragment the cache — and so a key
/// is never written to the database.
pub fn request_key(url: &str) -> String {
    let without_host = url.strip_prefix("https://api.themoviedb.org/3").unwrap_or(url);
    let mut parts: Vec<&str> = Vec::new();
    let (path, query) = match without_host.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (without_host, None),
    };
    if let Some(query) = query {
        for pair in query.split('&') {
            let name = pair.split('=').next().unwrap_or("");
            // the credential, and the cache-buster some callers add
            if name == "api_key" || name == "_t" {
                continue;
            }
            parts.push(pair);
        }
    }
    parts.sort_unstable();
    if parts.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parts.join("&"))
    }
}

/// A previously fetched answer, if this exact request has ever been made.
pub async fn get(url: &str) -> Option<Value> {
    let pool = pool()?;
    let key = request_key(url);

    let row: Option<(Value,)> = sqlx::query_as("SELECT payload FROM tmdb_cache WHERE request_key = $1")
        .bind(&key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    let payload = row.map(|(payload,)| payload)?;

    // Counting the hit must never delay the answer.
    let pool = pool.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            "UPDATE tmdb_cache SET hits = hits + 1, last_hit_at = now() WHERE request_key = $1",
        )
        .bind(&key)
        .execute(&pool)
        .await;
    });

    Some(payload)
}

/// Keep an answer. Fire-and-forget: a cache write must never fail a lookup.
pub fn put(url: &str, payload: &Value) {
    let Some(pool) = pool() else { return };
    // An error response is not worth remembering forever.
    if payload.get("success").and_then(Value::as_bool) == Some(false)
        || payload.get("status_code").is_some()
    {
        return;
    }

    let key = request_key(url);
    let payload = payload.clone();
    let pool = pool.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO tmdb_cache (request_key, payload) VALUES ($1, $2)
             ON CONFLICT (request_key) DO UPDATE SET
                payload = EXCLUDED.payload,
                refreshed_at = now()",
        )
        .bind(&key)
        .bind(&payload)
        .execute(&pool)
        .await;
    });
}

/// Fetch a TMDB url, answering from the cache when it can.
pub async fn fetch(client: &reqwest::Client, url: &str) -> Option<Value> {
    if let Some(cached) = get(url).await {
        metrics::counter!("tmdb_cache_hits_total").increment(1);
        return Some(cached);
    }
    metrics::counter!("tmdb_cache_misses_total").increment(1);

    let body: Value = client.get(url).send().await.ok()?.json().await.ok()?;
    put(url, &body);
    Some(body)
}

/// What the cache holds and how much fetching it has saved.
pub async fn stats() -> Value {
    let Some(pool) = pool() else {
        return serde_json::json!({ "entries": 0, "hits": 0 });
    };
    // `sum(bigint)` is numeric in Postgres and will not decode into i64.
    let row: Result<(i64, i64), _> = sqlx::query_as(
        "SELECT count(*)::bigint, COALESCE(sum(hits), 0)::bigint FROM tmdb_cache",
    )
    .fetch_one(pool)
    .await;

    match row {
        Ok((entries, hits)) => serde_json::json!({ "entries": entries, "hits": hits }),
        Err(err) => {
            tracing::warn!(error = %err, "tmdb cache stats unavailable");
            serde_json::json!({ "entries": 0, "hits": 0 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credential_never_reaches_the_key() {
        let key = request_key("https://api.themoviedb.org/3/movie/157336?api_key=secret123");
        assert!(!key.contains("secret123"), "an api key must never be stored");
        assert_eq!(key, "/movie/157336");
    }

    #[test]
    fn the_same_question_shares_an_entry() {
        // different keys, different parameter order, a cache-buster: one entry
        let a = request_key("https://api.themoviedb.org/3/search/movie?api_key=aaa&query=dune&year=2021");
        let b = request_key("https://api.themoviedb.org/3/search/movie?year=2021&api_key=bbb&query=dune&_t=999");
        assert_eq!(a, b);
    }

    #[test]
    fn different_questions_stay_apart() {
        assert_ne!(
            request_key("https://api.themoviedb.org/3/movie/1"),
            request_key("https://api.themoviedb.org/3/movie/2")
        );
        assert_ne!(
            request_key("https://api.themoviedb.org/3/movie/1"),
            request_key("https://api.themoviedb.org/3/tv/1")
        );
    }
}
