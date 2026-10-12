//! Persistent search cache.
//!
//! A unified search fans out to every enabled source: seconds of latency and a
//! few hundred upstream requests for one query. The result rarely differs
//! between one viewer and the next, so it is written to the database and
//! reused — the first person to search a term pays for it, everyone after is
//! served from Postgres.
//!
//! The cache is deliberately unbounded: nothing is evicted, entries are
//! refreshed in place, and the table grows with the number of distinct queries
//! ever asked. That is the point — a query answered once stays answered.
//!
//! Staleness is handled without ever making a caller wait: a hit is always
//! served immediately, and if the entry is older than the refresh window the
//! fan-out is re-run in the background so the *next* caller gets fresher data.

use serde_json::Value;
use sqlx::PgPool;

/// How old an entry may get before a hit also triggers a background refresh.
/// The stale answer is still served — nobody waits on this.
pub const REFRESH_AFTER: chrono::Duration = chrono::Duration::hours(24);

pub struct CachedSearch {
    pub payload: Value,
    pub refreshed_at: chrono::DateTime<chrono::Utc>,
}

impl CachedSearch {
    pub fn is_stale(&self) -> bool {
        chrono::Utc::now() - self.refreshed_at > REFRESH_AFTER
    }
}

/// Cache key: the same question asked with different spacing or casing is the
/// same question.
pub fn key(query: &str, category: Option<&str>, limit: i64) -> String {
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    format!("{normalized}\u{1f}{}\u{1f}{limit}", category.unwrap_or(""))
}

/// A previously answered search, if this query has ever been asked.
pub async fn get(pool: &PgPool, key: &str) -> Option<CachedSearch> {
    let row: Option<(Value, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT payload, refreshed_at FROM search_cache WHERE query_key = $1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    row.map(|(payload, refreshed_at)| CachedSearch {
        payload,
        refreshed_at,
    })
}

/// Store or refresh an answer.
///
/// `created_at` and `hits` survive a refresh, so an entry keeps its history and
/// the popularity ranking stays meaningful across refreshes.
pub async fn put(
    pool: &PgPool,
    key: &str,
    query: &str,
    category: Option<&str>,
    limit: i64,
    payload: &Value,
    result_count: usize,
) {
    let _ = sqlx::query(
        "INSERT INTO search_cache
            (query_key, query, category, result_limit, payload, result_count)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (query_key) DO UPDATE SET
            payload = EXCLUDED.payload,
            result_count = EXCLUDED.result_count,
            refreshed_at = now()",
    )
    .bind(key)
    .bind(query)
    .bind(category)
    .bind(limit as i32)
    .bind(payload)
    .bind(result_count as i32)
    .execute(pool)
    .await;
}

/// Count a reuse. Fire-and-forget: a failure here must never affect a search.
pub async fn record_hit(pool: &PgPool, key: &str) {
    let _ = sqlx::query(
        "UPDATE search_cache SET hits = hits + 1, last_hit_at = now() WHERE query_key = $1",
    )
    .bind(key)
    .execute(pool)
    .await;
}

/// What the cache is holding and how much work it has saved.
pub async fn stats(pool: &PgPool) -> Value {
    // `sum(bigint)` comes back as numeric in Postgres, which will not decode
    // into i64 — cast it, or the whole row silently fails to read.
    let row: Result<(i64, i64, Option<chrono::DateTime<chrono::Utc>>), _> = sqlx::query_as(
        "SELECT count(*)::bigint,
                COALESCE(sum(hits), 0)::bigint,
                min(created_at)
         FROM search_cache",
    )
    .fetch_one(pool)
    .await;

    let (entries, hits, since) = match row {
        Ok(values) => values,
        Err(err) => {
            tracing::warn!(error = %err, "search cache stats unavailable");
            (0, 0, None)
        }
    };
    serde_json::json!({
        "entries": entries,
        "hits": hits,
        "caching_since": since,
        "refresh_after_hours": REFRESH_AFTER.num_hours(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_question_shares_a_key() {
        let a = key("The  MATRIX ", None, 30);
        let b = key("the matrix", None, 30);
        assert_eq!(a, b, "spacing and case must not split the cache");
    }

    #[test]
    fn different_questions_do_not_collide() {
        assert_ne!(key("matrix", None, 30), key("matrix", Some("movie"), 30));
        assert_ne!(key("matrix", None, 30), key("matrix", None, 60));
        assert_ne!(key("matrix", None, 30), key("inception", None, 30));
        // a category must not be confusable with part of the query text
        assert_ne!(key("matrix movie", None, 30), key("matrix", Some("movie"), 30));
    }

    #[test]
    fn freshness_is_measured_from_the_last_refresh() {
        let fresh = CachedSearch {
            payload: Value::Null,
            refreshed_at: chrono::Utc::now(),
        };
        let old = CachedSearch {
            payload: Value::Null,
            refreshed_at: chrono::Utc::now() - REFRESH_AFTER - chrono::Duration::minutes(1),
        };
        assert!(!fresh.is_stale());
        assert!(old.is_stale());
    }
}
