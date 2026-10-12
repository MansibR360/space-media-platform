use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

/// Exponential time-decay half-life (days): older activity counts half as much.
pub const HALF_LIFE_DAYS: f64 = 21.0;
/// Gap (minutes) after which consecutive plays are considered new sessions.
pub const SESSION_GAP_MINUTES: i64 = 45;

#[derive(Debug, Clone)]
pub struct PlaySignal {
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub episode_id: Option<Uuid>,
    pub action: &'static str,
    pub percent: Option<f64>,
    pub rating: Option<f64>,
    pub effective_seconds: i32,
    pub dedupe_key: Option<String>,
}

// ---------- recording ----------

pub async fn record(pool: &PgPool, signal: &PlaySignal) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO heuristic_events
            (id, user_id, media_id, episode_id, action, weight, percent, rating,
             effective_seconds, dedupe_key, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())
         ON CONFLICT DO NOTHING",
    )
    .bind(crate::utils::new_id())
    .bind(signal.user_id)
    .bind(signal.media_id)
    .bind(signal.episode_id)
    .bind(signal.action)
    .bind(crate::heuristic::models::action_weight(signal.action))
    .bind(signal.percent)
    .bind(signal.rating)
    .bind(signal.effective_seconds)
    .bind(&signal.dedupe_key)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn wipe_user(pool: &PgPool, user_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM heuristic_events WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- shared fragments ----------

fn decay_sql(half_life_days: f64) -> String {
    format!(
        "(e.weight * EXP(-LN(2) * GREATEST(EXTRACT(EPOCH FROM (now() - e.occurred_at)), 0) / {}))",
        86400.0 * half_life_days
    )
}

fn window_sql(days: Option<i32>) -> String {
    match days {
        Some(d) => format!(" AND e.occurred_at > now() - make_interval(days => {d})"),
        None => String::new(),
    }
}

// ---------- profile aggregates ----------

pub struct Totals {
    pub total_plays: i64,
    pub unique_titles: i64,
    pub total_seconds: i64,
    pub window_first: Option<DateTime<Utc>>,
    pub window_last: Option<DateTime<Utc>>,
}

pub async fn totals(pool: &PgPool, user_id: Uuid) -> anyhow::Result<Totals> {
    let row = sqlx::query_as::<_, (i64, i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
        "SELECT COUNT(*) FILTER (WHERE e.action IN ('play','complete','rewatch'))::bigint,
                COUNT(DISTINCT e.media_id)::bigint,
                COALESCE(SUM(e.effective_seconds), 0)::bigint,
                MIN(e.occurred_at), MAX(e.occurred_at)
         FROM heuristic_events e WHERE e.user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(Totals {
        total_plays: row.0,
        unique_titles: row.1,
        total_seconds: row.2,
        window_first: row.3,
        window_last: row.4,
    })
}

pub struct ItemRow {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub plays: i64,
    pub weight: f64,
    pub last_played_at: DateTime<Utc>,
}

pub async fn top_items(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
    limit: i64,
    min_plays: i64,
) -> anyhow::Result<Vec<ItemRow>> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, String, i64, f64, DateTime<Utc>)>(
        &format!(
            "SELECT m.id, m.title, m.poster_url, m.media_type,
                    COUNT(*) FILTER (WHERE e.action IN ('play','complete','rewatch'))::bigint AS plays,
                    SUM({decay})::float8 AS weight,
                    MAX(e.occurred_at) AS last_played_at
             FROM heuristic_events e
             JOIN media m ON m.id = e.media_id
             WHERE e.user_id = $1{window}
             GROUP BY m.id, m.title, m.poster_url, m.media_type
             HAVING COUNT(*) FILTER (WHERE e.action IN ('play','complete','rewatch')) >= $3
             ORDER BY weight DESC LIMIT $2",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .bind(limit)
    .bind(min_plays)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ItemRow {
            media_id: r.0,
            title: r.1,
            poster_url: r.2,
            media_type: r.3,
            plays: r.4,
            weight: r.5,
            last_played_at: r.6,
        })
        .collect())
}

pub async fn genre_affinity(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
    limit: i64,
) -> anyhow::Result<Vec<(String, f64, i64)>> {
    let rows = sqlx::query_as::<_, (String, f64, i64)>(
        &format!(
            "SELECT g.name, SUM({decay})::float8 AS weight, COUNT(*)::bigint AS plays
             FROM heuristic_events e
             JOIN media_genres mg ON mg.media_id = e.media_id
             JOIN genres g ON g.id = mg.genre_id
             WHERE e.user_id = $1{window}
             GROUP BY g.name ORDER BY weight DESC LIMIT $2",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn decade_affinity(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
    limit: i64,
) -> anyhow::Result<Vec<(String, f64, i64)>> {
    let rows = sqlx::query_as::<_, (String, f64, i64)>(
        &format!(
            "SELECT (FLOOR(EXTRACT(YEAR FROM m.release_date) / 10) * 10)::text AS decade,
                    SUM({decay})::float8 AS weight, COUNT(*)::bigint AS plays
             FROM heuristic_events e
             JOIN media m ON m.id = e.media_id
             WHERE e.user_id = $1 AND m.release_date IS NOT NULL{window}
             GROUP BY decade ORDER BY weight DESC LIMIT $2",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(d, w, p)| (format!("{d}s"), w, p)).collect())
}

pub async fn type_mix(
    pool: &PgPool,
    user_id: Uuid,
    days: Option<i32>,
) -> anyhow::Result<Vec<(String, i64)>> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        &format!(
            "SELECT m.media_type, COUNT(*)::bigint AS plays
             FROM heuristic_events e JOIN media m ON m.id = e.media_id
             WHERE e.user_id = $1{window}
             GROUP BY m.media_type ORDER BY plays DESC",
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn hour_histogram(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
) -> anyhow::Result<[f64; 24]> {
    let rows = sqlx::query_as::<_, (i32, f64)>(
        &format!(
            "SELECT EXTRACT(HOUR FROM e.occurred_at)::int AS h, SUM({decay})::float8 AS weight
             FROM heuristic_events e WHERE e.user_id = $1{window} GROUP BY h",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut hist = [0.0f64; 24];
    for (h, w) in rows {
        if (0..24).contains(&h) {
            hist[h as usize] = w;
        }
    }
    Ok(hist)
}

pub async fn weekday_histogram(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
) -> anyhow::Result<[f64; 7]> {
    let rows = sqlx::query_as::<_, (i32, f64)>(
        &format!(
            "SELECT EXTRACT(DOW FROM e.occurred_at)::int AS d, SUM({decay})::float8 AS weight
             FROM heuristic_events e WHERE e.user_id = $1{window} GROUP BY d",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let mut hist = [0.0f64; 7];
    for (d, w) in rows {
        if (0..7).contains(&d) {
            hist[d as usize] = w;
        }
    }
    Ok(hist)
}

pub async fn completion_rate(pool: &PgPool, user_id: Uuid) -> anyhow::Result<f64> {
    let row: (Option<f64>,) = sqlx::query_as(
        "SELECT AVG(CASE WHEN e.action = 'complete' THEN 100.0 ELSE COALESCE(e.percent, 0) END)::float8
         FROM heuristic_events e
         WHERE e.user_id = $1 AND e.action IN ('play', 'complete')",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0.unwrap_or(0.0))
}

/// Chronological (occurred_at, media_type) pairs for session reconstruction.
pub async fn timeline(
    pool: &PgPool,
    user_id: Uuid,
    days: Option<i32>,
) -> anyhow::Result<Vec<(DateTime<Utc>, String)>> {
    let rows = sqlx::query_as::<_, (DateTime<Utc>, String)>(
        &format!(
            "SELECT e.occurred_at, m.media_type
             FROM heuristic_events e JOIN media m ON m.id = e.media_id
             WHERE e.user_id = $1{window}
             ORDER BY e.occurred_at ASC",
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn active_days(pool: &PgPool, user_id: Uuid, days: Option<i32>) -> anyhow::Result<Vec<NaiveDate>> {
    let rows = sqlx::query_as::<_, (NaiveDate,)>(
        &format!(
            "SELECT DISTINCT (e.occurred_at AT TIME ZONE 'UTC')::date AS day
             FROM heuristic_events e WHERE e.user_id = $1{window} ORDER BY day ASC",
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(d,)| d).collect())
}

/// Biggest single day inside the window: (date, plays).
pub async fn biggest_day(
    pool: &PgPool,
    user_id: Uuid,
    days: Option<i32>,
) -> anyhow::Result<Option<(NaiveDate, i64)>> {
    let row = sqlx::query_as::<_, (NaiveDate, i64)>(
        &format!(
            "SELECT (e.occurred_at AT TIME ZONE 'UTC')::date AS day, COUNT(*)::bigint AS plays
             FROM heuristic_events e
             WHERE e.user_id = $1 AND e.action IN ('play','complete','rewatch'){window}
             GROUP BY day ORDER BY plays DESC LIMIT 1",
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Titles engaged for the very first time inside the window ("discoveries").
pub async fn new_discoveries(pool: &PgPool, user_id: Uuid, days: Option<i32>) -> anyhow::Result<i64> {
    let Some(d) = days else { return Ok(0) };
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM (
            SELECT e.media_id FROM heuristic_events e
            WHERE e.user_id = $1
            GROUP BY e.media_id
            HAVING MIN(e.occurred_at) > now() - make_interval(days => $2)
         ) firsts",
    )
    .bind(user_id)
    .bind(d)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn momentum(pool: &PgPool, user_id: Uuid) -> anyhow::Result<(i64, i64)> {
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT
            COUNT(*) FILTER (WHERE e.occurred_at > now() - interval '7 days'
                              AND e.action IN ('play','complete','rewatch'))::bigint,
            COUNT(*) FILTER (WHERE e.occurred_at <= now() - interval '7 days'
                              AND e.occurred_at > now() - interval '14 days'
                              AND e.action IN ('play','complete','rewatch'))::bigint
         FROM heuristic_events e WHERE e.user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok((row.0, row.1))
}

// ---------- prediction inputs ----------

pub struct CatalogCandidate {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub genres: Vec<String>,
    pub popularity: f64,
    /// minutes since the user's most recent engagement with this title (NULL = never)
    pub minutes_since_last: Option<i64>,
    /// true when the user already finished it
    pub completed: bool,
}

pub async fn catalog_candidates(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<CatalogCandidate>> {
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, String, Vec<String>, f64, Option<i64>, bool)>(
        "SELECT m.id, m.title, m.poster_url, m.media_type,
                COALESCE((SELECT array_agg(g.name) FROM media_genres mg
                          JOIN genres g ON g.id = mg.genre_id WHERE mg.media_id = m.id), '{}'),
                m.popularity,
                (SELECT ROUND(EXTRACT(EPOCH FROM (now() - MAX(e.occurred_at))) / 60)::bigint
                 FROM heuristic_events e WHERE e.user_id = $1 AND e.media_id = m.id),
                EXISTS (SELECT 1 FROM watch_history wh
                        WHERE wh.user_id = $1 AND wh.media_id = m.id AND wh.completed)
         FROM media m
         ORDER BY m.popularity DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| CatalogCandidate {
        media_id: r.0,
        title: r.1,
        poster_url: r.2,
        media_type: r.3,
        genres: r.4,
        popularity: r.5,
        minutes_since_last: r.6,
        completed: r.7,
    })
    .collect();
    Ok(rows)
}

// ---------- collaborative + sequential signals ----------

/// Item-item co-watch strength across the whole server: how many distinct
/// users engaged with BOTH the seed title (one of yours) and the partner
/// title, over a bounded window.
pub async fn co_watch(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<(Uuid, Uuid, i64)>> {
    const PLAY_ACTIONS: &str = "('play','complete','rewatch')";
    let rows = sqlx::query_as::<_, (Uuid, Uuid, i64)>(
        &format!(
            "WITH seeds AS (
                SELECT DISTINCT media_id FROM heuristic_events
                WHERE user_id = $1 AND action IN {PLAY_ACTIONS}
             ),
             audience AS (
                SELECT user_id, media_id FROM heuristic_events
                WHERE action IN {PLAY_ACTIONS}
                  AND occurred_at > now() - interval '180 days'
                  AND user_id IN (
                      SELECT DISTINCT user_id FROM heuristic_events
                      WHERE media_id IN (SELECT media_id FROM seeds)
                        AND action IN {PLAY_ACTIONS}
                  )
             )
             SELECT a.media_id AS src, b.media_id AS dst,
                    COUNT(DISTINCT a.user_id)::bigint AS strength
             FROM audience a
             JOIN audience b ON b.user_id = a.user_id AND b.media_id <> a.media_id
             WHERE a.media_id IN (SELECT media_id FROM seeds)
             GROUP BY a.media_id, b.media_id
             HAVING COUNT(DISTINCT a.user_id) >= 2
             ORDER BY strength DESC LIMIT $2"
        ),
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Server-wide sequential signal: titles commonly played right after another
/// within one session (LEAD window over the behavior log).
pub async fn transitions(pool: &PgPool, limit: i64) -> anyhow::Result<Vec<(Uuid, Uuid, i64)>> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, i64)>(
        "WITH seq AS (
            SELECT media_id,
                   LEAD(media_id) OVER (PARTITION BY user_id ORDER BY occurred_at) AS next_media,
                   LEAD(occurred_at) OVER (PARTITION BY user_id ORDER BY occurred_at) AS next_at,
                   occurred_at
            FROM heuristic_events
            WHERE action IN ('play','complete','rewatch')
              AND occurred_at > now() - interval '120 days'
         )
         SELECT media_id AS from_media, next_media AS to_media, COUNT(*)::bigint AS n
         FROM seq
         WHERE next_media IS NOT NULL
           AND next_at - occurred_at < interval '45 minutes'
           AND next_media <> media_id
         GROUP BY media_id, next_media
         HAVING COUNT(*) >= 2
         ORDER BY n DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Play counts bucketed by day (windows ≤ 31d) or month (longer windows).
pub async fn activity_buckets(
    pool: &PgPool,
    user_id: Uuid,
    days: Option<i32>,
) -> anyhow::Result<Vec<(String, i64)>> {
    let trunc = match days {
        Some(d) if d <= 31 => "day",
        _ => "month",
    };
    let rows = sqlx::query_as::<_, (String, i64)>(
        &format!(
            "SELECT to_char(date_trunc('{trunc}', e.occurred_at), 'YYYY-MM-DD') AS bucket,
                    COUNT(*) FILTER (WHERE e.action IN ('play','complete','rewatch'))::bigint AS plays
             FROM heuristic_events e
             WHERE e.user_id = $1{window}
             GROUP BY bucket ORDER BY bucket ASC",
            trunc = trunc,
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(b, p)| (if trunc == "month" { b[..7].to_string() } else { b }, p)).collect())
}

/// Most-engaged cast/directors across the period's titles.
pub async fn top_people(
    pool: &PgPool,
    user_id: Uuid,
    half_life: f64,
    days: Option<i32>,
    limit: i64,
) -> anyhow::Result<Vec<(String, f64, i64)>> {
    let rows = sqlx::query_as::<_, (String, f64, i64)>(
        &format!(
            "SELECT p.name, SUM({decay})::float8 AS weight,
                    COUNT(DISTINCT e.media_id)::bigint AS plays
             FROM heuristic_events e
             JOIN credits c ON c.media_id = e.media_id AND c.credit_type IN ('cast', 'director')
             JOIN people p ON p.id = c.person_id
             WHERE e.user_id = $1{window}
             GROUP BY p.name ORDER BY weight DESC LIMIT $2",
            decay = decay_sql(half_life),
            window = window_sql(days),
        ),
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Share of plays abandoned early (< 15% progress).
pub async fn abandonment_rate(pool: &PgPool, user_id: Uuid) -> anyhow::Result<f64> {
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*) FILTER (WHERE COALESCE(e.percent, 100) < 15.0)::bigint,
                COUNT(*)::bigint
         FROM heuristic_events e
         WHERE e.user_id = $1 AND e.action = 'play'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(if row.1 == 0 { 0.0 } else { row.0 as f64 / row.1 as f64 })
}

// keep ApiError import meaningful for callers mapping errors upstream
#[allow(dead_code)]
fn _err_shape(_: ApiError) {}
