use crate::error::{ApiError, ApiResult};
use crate::parties::models::{Party, PartyEvent, PartyMember};
use crate::utils::new_id;
use sqlx::PgPool;
use uuid::Uuid;

const PARTY_SELECT: &str = "SELECT p.* FROM parties p";

pub async fn create(
    pool: &PgPool,
    host_id: Uuid,
    title: &str,
    media_id: Option<Uuid>,
    episode_id: Option<Uuid>,
    visibility: &str,
    max_participants: i32,
    password: Option<&str>,
) -> ApiResult<Party> {
    if !matches!(visibility, "public" | "private" | "friends") {
        return Err(ApiError::bad_request(
            "visibility must be public, private or friends",
        ));
    }
    if !(1..=500).contains(&max_participants) {
        return Err(ApiError::bad_request("max_participants must be 1-500"));
    }

    let mut join_code;
    loop {
        join_code = crate::utils::random_hex(4).to_ascii_uppercase();
        let clash: Option<(bool,)> =
            sqlx::query_as("SELECT TRUE FROM parties WHERE join_code = $1")
                .bind(&join_code)
                .fetch_optional(pool)
                .await?;
        if clash.is_none() {
            break;
        }
    }

    let id = new_id();
    let password_hash = password.map(|p| crate::utils::sha256_hex(p));
    sqlx::query(
        "INSERT INTO parties (id, host_id, media_id, episode_id, join_code, title, visibility, max_participants, password_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(host_id)
    .bind(media_id)
    .bind(episode_id)
    .bind(&join_code)
    .bind(title.trim())
    .bind(visibility)
    .bind(max_participants)
    .bind(password_hash)
    .execute(pool)
    .await?;

    sqlx::query("INSERT INTO party_members (party_id, user_id, role) VALUES ($1, $2, 'host') ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(host_id)
        .execute(pool)
        .await?;

    get(pool, id).await
}

pub async fn get(pool: &PgPool, party_id: Uuid) -> ApiResult<Party> {
    sqlx::query_as::<_, Party>(&format!("{PARTY_SELECT} WHERE p.id = $1"))
        .bind(party_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("party"))
}

pub async fn get_by_code(pool: &PgPool, code: &str) -> ApiResult<Party> {
    sqlx::query_as::<_, Party>(&format!(
        "{PARTY_SELECT} WHERE p.join_code = UPPER($1) AND p.ended_at IS NULL"
    ))
    .bind(code.trim())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("party for code"))
}

pub async fn is_member(pool: &PgPool, party_id: Uuid, user_id: Uuid) -> ApiResult<bool> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT TRUE FROM party_members WHERE party_id = $1 AND user_id = $2")
            .bind(party_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.is_some())
}

pub async fn member_role(
    pool: &PgPool,
    party_id: Uuid,
    user_id: Uuid,
) -> ApiResult<Option<String>> {
    let role: Option<(String,)> =
        sqlx::query_as("SELECT role FROM party_members WHERE party_id = $1 AND user_id = $2")
            .bind(party_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    Ok(role.map(|(r,)| r))
}

pub async fn members(pool: &PgPool, party_id: Uuid) -> ApiResult<Vec<PartyMember>> {
    Ok(sqlx::query_as::<_, PartyMember>(
        "SELECT pm.user_id, u.username, u.display_name, u.avatar_url, pm.role, pm.joined_at
         FROM party_members pm JOIN users u ON u.id = pm.user_id
         WHERE pm.party_id = $1 ORDER BY pm.joined_at",
    )
    .bind(party_id)
    .fetch_all(pool)
    .await?)
}

pub async fn member_count(pool: &PgPool, party_id: Uuid) -> ApiResult<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM party_members WHERE party_id = $1")
        .bind(party_id)
        .fetch_one(pool)
        .await?;
    Ok(count)
}

pub async fn join(pool: &PgPool, party_id: Uuid, user_id: Uuid) -> ApiResult<PartyMember> {
    sqlx::query(
        "INSERT INTO party_members (party_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(party_id)
    .bind(user_id)
    .execute(pool)
    .await?;

    sqlx::query_as::<_, PartyMember>(
        "SELECT pm.user_id, u.username, u.display_name, u.avatar_url, pm.role, pm.joined_at
         FROM party_members pm JOIN users u ON u.id = pm.user_id
         WHERE pm.party_id = $1 AND pm.user_id = $2",
    )
    .bind(party_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

pub async fn leave(pool: &PgPool, party_id: Uuid, user_id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("DELETE FROM party_members WHERE party_id = $1 AND user_id = $2")
        .bind(party_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("membership"));
    }
    Ok(())
}

/// Applies a canonical playback-state change and bumps the event sequence.
pub async fn apply_state_change(
    pool: &PgPool,
    party_id: Uuid,
    is_playing: bool,
    position_seconds: f64,
) -> ApiResult<Party> {
    let party = sqlx::query_as::<_, Party>(
        "UPDATE parties SET is_playing = $2, position_seconds = GREATEST($3, 0), \
         state_updated_at = now(), last_event_seq = last_event_seq + 1
         WHERE id = $1 RETURNING *",
    )
    .bind(party_id)
    .bind(is_playing)
    .bind(position_seconds.max(0.0))
    .fetch_one(pool)
    .await?;
    Ok(party)
}

pub async fn record_event(
    pool: &PgPool,
    party_id: Uuid,
    actor_id: Option<Uuid>,
    event_kind: &str,
    payload: serde_json::Value,
) -> ApiResult<i64> {
    let seq: (i64,) = sqlx::query_as(
        "INSERT INTO party_events (party_id, actor_id, event_kind, payload)
         VALUES ($1, $2, $3, $4) RETURNING seq",
    )
    .bind(party_id)
    .bind(actor_id)
    .bind(event_kind)
    .bind(payload)
    .fetch_one(pool)
    .await?;
    Ok(seq.0)
}

pub async fn events_after(
    pool: &PgPool,
    party_id: Uuid,
    after_seq: i64,
    limit: i64,
) -> ApiResult<Vec<PartyEvent>> {
    Ok(sqlx::query_as::<_, PartyEvent>(
        "SELECT * FROM party_events WHERE party_id = $1 AND seq > $2 ORDER BY seq LIMIT $3",
    )
    .bind(party_id)
    .bind(after_seq.max(0))
    .bind(limit.clamp(1, 200))
    .fetch_all(pool)
    .await?)
}

pub async fn my_parties(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<Party>> {
    Ok(sqlx::query_as::<_, Party>(&format!(
        "{PARTY_SELECT} JOIN party_members pm ON pm.party_id = p.id \
         WHERE pm.user_id = $1 AND p.ended_at IS NULL ORDER BY p.created_at DESC LIMIT 50"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub async fn end(pool: &PgPool, host_id: Uuid, party_id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("UPDATE parties SET ended_at = now() WHERE id = $1 AND host_id = $2")
        .bind(party_id)
        .bind(host_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("active party you host"));
    }
    Ok(())
}

pub async fn add_reaction(
    pool: &PgPool,
    party_id: Uuid,
    user_id: Uuid,
    second_offset: i32,
    emoji: &str,
) -> ApiResult<()> {
    if emoji.is_empty() || emoji.chars().count() > 8 {
        return Err(ApiError::bad_request("emoji must be 1-8 characters"));
    }
    sqlx::query("INSERT INTO party_reactions (id, party_id, user_id, second_offset, emoji) VALUES ($1, $2, $3, $4, $5)")
        .bind(new_id())
        .bind(party_id)
        .bind(user_id)
        .bind(second_offset.clamp(0, 86_400 * 2))
        .bind(emoji)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reaction_heatmap(pool: &PgPool, party_id: Uuid) -> ApiResult<Vec<(i32, String, i64)>> {
    let rows: Vec<(i32, String, i64)> = sqlx::query_as(
        "SELECT (second_offset / 10) * 10 AS bucket, emoji, COUNT(*)::bigint AS count
         FROM party_reactions WHERE party_id = $1
         GROUP BY bucket, emoji ORDER BY bucket, count DESC",
    )
    .bind(party_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
