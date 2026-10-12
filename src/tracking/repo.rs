use crate::error::{ApiError, ApiResult};
use crate::tracking::models::{
    ContinueWatchingItem, HistoryRow, ListEntry, ListEntryWithTitle, ListStatus, ProgressUpdate,
    RatingRow, StatusUpdate, UserListItem, WatchEntry,
};
use crate::utils::{new_id, now};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn upsert_progress(
    pool: &PgPool,
    user_id: Uuid,
    req: &ProgressUpdate,
) -> ApiResult<WatchEntry> {
    if req.position_seconds < 0.0 || req.duration_seconds < 0.0 {
        return Err(ApiError::bad_request(
            "progress values must be non-negative",
        ));
    }
    if !req.completed
        && req.duration_seconds > 0.0
        && req.position_seconds > req.duration_seconds * 1.2
    {
        return Err(ApiError::bad_request("position exceeds duration"));
    }

    let completed = req.completed
        || (req.duration_seconds > 60.0 && req.position_seconds >= req.duration_seconds * 0.92);

    let mut tx = pool.begin().await?;

    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM watch_history
         WHERE user_id = $1 AND media_id = $2
           AND ((episode_id IS NULL AND $3 IS NULL) OR episode_id = $3)
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(req.media_id)
    .bind(req.episode_id)
    .fetch_optional(&mut *tx)
    .await?;

    let entry = match existing {
        Some((id,)) => {
            sqlx::query_as::<_, WatchEntry>(
                "UPDATE watch_history SET position_seconds = $2, duration_seconds = $3, \
                 completed = completed OR $4, updated_at = now() WHERE id = $1 RETURNING *",
            )
            .bind(id)
            .bind(req.position_seconds.max(0.0))
            .bind(req.duration_seconds)
            .bind(completed)
            .fetch_one(&mut *tx)
            .await?
        }
        None => {
            sqlx::query_as::<_, WatchEntry>(
                "INSERT INTO watch_history (id, user_id, media_id, episode_id, position_seconds, duration_seconds, completed)
                 VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
            )
            .bind(new_id())
            .bind(user_id)
            .bind(req.media_id)
            .bind(req.episode_id)
            .bind(req.position_seconds)
            .bind(req.duration_seconds)
            .bind(completed)
            .fetch_one(&mut *tx)
            .await?
        }
    };

    if entry.completed {
        sqlx::query(
            "INSERT INTO list_entries (id, user_id, media_id, status, episodes_watched)
             VALUES ($1, $2, $3, 'watching', 0)
             ON CONFLICT (user_id, media_id) DO NOTHING",
        )
        .bind(new_id())
        .bind(user_id)
        .bind(req.media_id)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(session_id) = req.session_id {
        sqlx::query(
            "UPDATE playback_sessions SET position_seconds = $3, last_active_at = now()
             WHERE id = $1 AND user_id = $2",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(entry.position_seconds)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(entry)
}

fn continue_watching_row(row: &ContinueWatchingItem) -> bool {
    row.percent_watched >= 0.5 && row.percent_watched < 96.0 && !row.watch.completed
}

pub async fn continue_watching(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> ApiResult<Vec<ContinueWatchingItem>> {
    let rows = sqlx::query_as::<_, ContinueWatchingItem>(
        "SELECT wh.*, m.title, m.poster_url, m.backdrop_url,
                CASE WHEN e.season_id IS NOT NULL
                     THEN CONCAT('S', s.number, ' · E', e.number, ' ', e.title)
                     ELSE NULL END AS episode_label,
                CASE WHEN wh.duration_seconds > 0
                     THEN wh.position_seconds / wh.duration_seconds * 100.0
                     ELSE 0.0 END AS percent_watched
         FROM watch_history wh
         JOIN media m ON m.id = wh.media_id
         LEFT JOIN episodes e ON e.id = wh.episode_id
         LEFT JOIN seasons s ON s.id = e.season_id
         WHERE wh.user_id = $1 AND wh.completed = FALSE AND wh.position_seconds > 15.0
         ORDER BY wh.updated_at DESC
         LIMIT $2",
    )
    .bind(user_id)
    .bind(limit * 3)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter(continue_watching_row)
        .take(limit as usize)
        .collect())
}

pub async fn history(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
    cursor_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    cursor_id: Option<Uuid>,
) -> ApiResult<(
    Vec<HistoryRow>,
    Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
)> {
    let rows = sqlx::query_as::<_, HistoryRow>(
        "SELECT wh.id, wh.media_id, wh.episode_id, m.title, e.title AS episode_title,
                wh.position_seconds, wh.duration_seconds, wh.completed, wh.updated_at
         FROM watch_history wh
         JOIN media m ON m.id = wh.media_id
         LEFT JOIN episodes e ON e.id = wh.episode_id
         WHERE wh.user_id = $1
           AND (($2 IS NULL) OR (wh.updated_at, wh.id) < ($2, COALESCE($3, '00000000-0000-0000-0000-000000000000'::uuid)))
         ORDER BY wh.updated_at DESC, wh.id DESC
         LIMIT $4",
    )
    .bind(user_id)
    .bind(cursor_updated_at)
    .bind(cursor_id)
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;

    let mut next = None;
    if rows.len() as i64 > limit {
        let last = rows.last();
        next = last.map(|r| (r.updated_at, r.id));
    }
    Ok((rows, next))
}

pub async fn set_status(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    req: &StatusUpdate,
) -> ApiResult<ListEntry> {
    let score = req.score.map(|s| crate::utils::clamp_f64(s, 0.0, 10.0));
    let entry = sqlx::query_as::<_, ListEntry>(
        "INSERT INTO list_entries (id, user_id, media_id, status, score, notes, episodes_watched)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (user_id, media_id) DO UPDATE SET
            status = EXCLUDED.status,
            score = EXCLUDED.score,
            notes = EXCLUDED.notes,
            episodes_watched = EXCLUDED.episodes_watched,
            updated_at = now()
         RETURNING *",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(media_id)
    .bind(req.status)
    .bind(score)
    .bind(req.notes.as_deref().unwrap_or(""))
    .bind(req.episodes_watched.unwrap_or(0).clamp(0, 100_000))
    .fetch_one(pool)
    .await?;
    Ok(entry)
}

pub async fn remove_status(pool: &PgPool, user_id: Uuid, media_id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("DELETE FROM list_entries WHERE user_id = $1 AND media_id = $2")
        .bind(user_id)
        .bind(media_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("list entry"));
    }
    Ok(())
}

pub async fn my_list(
    pool: &PgPool,
    user_id: Uuid,
    status: Option<ListStatus>,
) -> ApiResult<Vec<ListEntryWithTitle>> {
    let entries = match status {
        Some(status) => sqlx::query_as::<_, ListEntryWithTitle>(
            "SELECT le.*, m.title AS title FROM list_entries le JOIN media m ON m.id = le.media_id \
                 WHERE le.user_id = $1 AND le.status = $2 ORDER BY le.updated_at DESC",
        )
        .bind(user_id)
        .bind(status)
        .fetch_all(pool)
        .await?,
        None => sqlx::query_as::<_, ListEntryWithTitle>(
            "SELECT le.*, m.title AS title FROM list_entries le JOIN media m ON m.id = le.media_id \
                 WHERE le.user_id = $1 ORDER BY le.updated_at DESC",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await?,
    };
    Ok(entries)
}

pub async fn rate_media(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    score: f64,
) -> ApiResult<f64> {
    if !(0.5..=10.0).contains(&score) {
        return Err(ApiError::bad_request("rating must be between 0.5 and 10"));
    }

    let mut tx = pool.begin().await?;
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM reviews WHERE user_id = $1 AND media_id = $2")
            .bind(user_id)
            .bind(media_id)
            .fetch_optional(&mut *tx)
            .await?;

    match existing {
        Some(review_id) => {
            sqlx::query("UPDATE reviews SET score = $2, updated_at = now() WHERE id = $1")
                .bind(review_id)
                .bind(score)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            sqlx::query("INSERT INTO reviews (id, user_id, media_id, body, score) VALUES ($1, $2, $3, '', $4)")
                .bind(new_id())
                .bind(user_id)
                .bind(media_id)
                .bind(score)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;

    crate::catalog::repo::recompute_rating(pool, media_id).await?;
    Ok(score)
}

pub async fn delete_rating(pool: &PgPool, user_id: Uuid, media_id: Uuid) -> ApiResult<()> {
    let result =
        sqlx::query("DELETE FROM reviews WHERE user_id = $1 AND media_id = $2 AND body = ''")
            .bind(user_id)
            .bind(media_id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("rating"));
    }
    crate::catalog::repo::recompute_rating(pool, media_id).await?;
    Ok(())
}

pub async fn my_ratings(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<RatingRow>> {
    let rows = sqlx::query_as::<_, RatingRow>(
        "SELECT r.id, r.user_id, r.media_id, m.title, r.score, r.created_at
         FROM reviews r JOIN media m ON m.id = r.media_id
         WHERE r.user_id = $1 AND r.score IS NOT NULL
         ORDER BY r.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn toggle_favorite(pool: &PgPool, user_id: Uuid, media_id: Uuid) -> ApiResult<bool> {
    let deleted = sqlx::query("DELETE FROM favorites WHERE user_id = $1 AND media_id = $2")
        .bind(user_id)
        .bind(media_id)
        .execute(pool)
        .await?
        .rows_affected()
        > 0;
    if deleted {
        return Ok(false);
    }
    sqlx::query("INSERT INTO favorites (user_id, media_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user_id)
        .bind(media_id)
        .execute(pool)
        .await?;
    Ok(true)
}

pub async fn favorites(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<(uuid::Uuid, String)>> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT f.media_id, m.title FROM favorites f JOIN media m ON m.id = f.media_id \
         WHERE f.user_id = $1 ORDER BY f.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn create_list(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    description: &str,
    visibility: &str,
) -> ApiResult<UserListItem> {
    if name.trim().is_empty() || name.len() > 120 {
        return Err(ApiError::bad_request("list name must be 1-120 characters"));
    }
    let list = sqlx::query_as::<_, UserListItem>(
        "INSERT INTO user_lists (id, user_id, name, description, visibility)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name, description, visibility, 0::bigint AS item_count, created_at",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(name.trim())
    .bind(description.trim())
    .bind(visibility)
    .fetch_one(pool)
    .await?;
    Ok(list)
}

pub async fn my_lists(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<UserListItem>> {
    let lists = sqlx::query_as::<_, UserListItem>(
        "SELECT ul.id, ul.name, ul.description, ul.visibility,
                COUNT(li.media_id)::bigint AS item_count, ul.created_at
         FROM user_lists ul
         LEFT JOIN user_list_items li ON li.list_id = ul.id
         WHERE ul.user_id = $1
         GROUP BY ul.id
         ORDER BY ul.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(lists)
}

pub async fn add_to_list(
    pool: &PgPool,
    user_id: Uuid,
    list_id: Uuid,
    media_id: Uuid,
) -> ApiResult<()> {
    let owner: (Uuid,) = sqlx::query_scalar("SELECT user_id FROM user_lists WHERE id = $1")
        .bind(list_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("list"))?;
    if owner.0 != user_id {
        return Err(ApiError::forbidden("not your list"));
    }
    sqlx::query(
        "INSERT INTO user_list_items (list_id, media_id, position)
         SELECT $1, $2, COALESCE(MAX(position), -1) + 1 FROM user_list_items WHERE list_id = $1
         ON CONFLICT DO NOTHING",
    )
    .bind(list_id)
    .bind(media_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_from_list(
    pool: &PgPool,
    user_id: Uuid,
    list_id: Uuid,
    media_id: Uuid,
) -> ApiResult<()> {
    let owner: (Uuid,) = sqlx::query_scalar("SELECT user_id FROM user_lists WHERE id = $1")
        .bind(list_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("list"))?;
    if owner.0 != user_id {
        return Err(ApiError::forbidden("not your list"));
    }
    let result = sqlx::query("DELETE FROM user_list_items WHERE list_id = $1 AND media_id = $2")
        .bind(list_id)
        .bind(media_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("list item"));
    }
    Ok(())
}

pub async fn list_items(pool: &PgPool, list_id: Uuid) -> ApiResult<Vec<(uuid::Uuid, String, i32)>> {
    let rows: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT li.media_id, m.title, li.position
         FROM user_list_items li JOIN media m ON m.id = li.media_id
         WHERE li.list_id = $1 ORDER BY li.position, li.added_at",
    )
    .bind(list_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[allow(dead_code)]
pub fn touch_now() -> chrono::DateTime<chrono::Utc> {
    now()
}

// ---------------- history recording toggle ----------------

/// Whether the caller's watch activity is being recorded. A missing
/// privacy_settings row means recording is on.
pub async fn history_recording_enabled(pool: &PgPool, user_id: Uuid) -> ApiResult<bool> {
    let enabled: Option<bool> =
        sqlx::query_scalar("SELECT record_history FROM privacy_settings WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(enabled.unwrap_or(true))
}

pub async fn set_history_recording(pool: &PgPool, user_id: Uuid, enabled: bool) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO privacy_settings (user_id, record_history) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET
            record_history = EXCLUDED.record_history,
            updated_at = now()",
    )
    .bind(user_id)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(())
}

/// WatchEntry-shaped echo for callers with recording disabled: clients keep
/// their local UI state while the server persists nothing.
pub fn ephemeral_progress(user_id: Uuid, req: &ProgressUpdate) -> WatchEntry {
    let now = crate::utils::now();
    let completed = req.completed
        || (req.duration_seconds > 60.0 && req.position_seconds >= req.duration_seconds * 0.92);
    WatchEntry {
        id: crate::utils::new_id(),
        user_id,
        media_id: req.media_id,
        episode_id: req.episode_id,
        position_seconds: req.position_seconds.max(0.0),
        duration_seconds: req.duration_seconds,
        completed,
        created_at: now,
        updated_at: now,
    }
}

// ---------------- reading progress (books) ----------------

use crate::tracking::models::ReadingProgress;

pub async fn upsert_reading_progress(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    position_percent: f64,
    page: Option<i32>,
) -> ApiResult<ReadingProgress> {
    if !(0.0..=100.0).contains(&position_percent) {
        return Err(ApiError::bad_request("position_percent must be 0-100"));
    }
    let completed = position_percent >= 99.0;
    let progress = sqlx::query_as::<_, ReadingProgress>(
        "INSERT INTO reading_progress (user_id, media_id, position_percent, page, completed)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id, media_id) DO UPDATE SET
            position_percent = EXCLUDED.position_percent,
            page = EXCLUDED.page,
            completed = EXCLUDED.completed OR reading_progress.completed,
            updated_at = now()
         RETURNING *",
    )
    .bind(user_id)
    .bind(media_id)
    .bind(position_percent)
    .bind(page.map(|p| p.clamp(0, 100_000)))
    .bind(completed)
    .fetch_one(pool)
    .await?;
    Ok(progress)
}

pub async fn get_reading_progress(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
) -> ApiResult<ReadingProgress> {
    sqlx::query_as::<_, ReadingProgress>(
        "SELECT * FROM reading_progress WHERE user_id = $1 AND media_id = $2",
    )
    .bind(user_id)
    .bind(media_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("reading progress"))
}
