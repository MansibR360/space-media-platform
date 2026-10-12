use crate::chat::models::{ChatMessage, Conversation, ConversationSummary};
use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, now};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn list_conversations(
    pool: &PgPool,
    user_id: Uuid,
) -> ApiResult<Vec<ConversationSummary>> {
    let rows = sqlx::query_as::<_, ConversationSummary>(
        "SELECT c.id, c.kind, c.title, c.created_by, c.party_id, c.created_at,
                lm.body AS last_message, lm.created_at AS last_message_at,
                (SELECT COUNT(*)::bigint FROM conversation_members cm WHERE cm.conversation_id = c.id) AS member_count
         FROM conversations c
         JOIN conversation_members me ON me.conversation_id = c.id AND me.user_id = $1
         LEFT JOIN LATERAL (
             SELECT msg.body, msg.created_at FROM messages msg
             WHERE msg.conversation_id = c.id ORDER BY msg.created_at DESC LIMIT 1
         ) lm ON TRUE
         ORDER BY COALESCE(lm.created_at, c.created_at) DESC
         LIMIT 100",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn is_member(pool: &PgPool, conversation_id: Uuid, user_id: Uuid) -> ApiResult<bool> {
    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT TRUE FROM conversation_members WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

pub async fn get(pool: &PgPool, conversation_id: Uuid) -> ApiResult<Conversation> {
    sqlx::query_as::<_, Conversation>("SELECT * FROM conversations WHERE id = $1")
        .bind(conversation_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::not_found("conversation"))
}

async fn find_direct(pool: &PgPool, a: Uuid, b: Uuid) -> ApiResult<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT c.id FROM conversations c
         JOIN conversation_members m1 ON m1.conversation_id = c.id AND m1.user_id = $1
         JOIN conversation_members m2 ON m2.conversation_id = c.id AND m2.user_id = $2
         WHERE c.kind = 'direct'
           AND (SELECT COUNT(*) FROM conversation_members cm WHERE cm.conversation_id = c.id) = 2
         LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id,)| id))
}

pub async fn create_direct(pool: &PgPool, a: Uuid, b: Uuid) -> ApiResult<Conversation> {
    if a == b {
        return Err(ApiError::bad_request(
            "cannot open a direct chat with yourself",
        ));
    }

    if let Some(existing) = find_direct(pool, a, b).await? {
        return get(pool, existing).await;
    }

    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query("INSERT INTO conversations (id, kind, created_by) VALUES ($1, 'direct', $2)")
        .bind(id)
        .bind(a)
        .execute(&mut *tx)
        .await?;
    for member in [a, b] {
        sqlx::query(
            "INSERT INTO conversation_members (conversation_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(member)
        .execute(&mut *tx)
        .await?;
    }
    let conversation =
        sqlx::query_as::<_, Conversation>("SELECT * FROM conversations WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(conversation)
}

pub async fn create_group(
    pool: &PgPool,
    creator: Uuid,
    title: &str,
    member_ids: &[Uuid],
) -> ApiResult<Conversation> {
    if title.trim().is_empty() {
        return Err(ApiError::bad_request("group title required"));
    }
    if member_ids.len() > 200 {
        return Err(ApiError::bad_request("too many members (max 200)"));
    }

    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query(
        "INSERT INTO conversations (id, kind, title, created_by) VALUES ($1, 'group', $2, $3)",
    )
    .bind(id)
    .bind(title.trim())
    .bind(creator)
    .execute(&mut *tx)
    .await?;

    let mut members: Vec<Uuid> = member_ids.to_vec();
    members.push(creator);
    members.sort();
    members.dedup();
    for member in members {
        sqlx::query(
            "INSERT INTO conversation_members (conversation_id, user_id, role) VALUES ($1, $2, CASE WHEN $2 = $3 THEN 'owner' ELSE 'member' END) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(member)
        .bind(creator)
        .execute(&mut *tx)
        .await?;
    }
    let conversation =
        sqlx::query_as::<_, Conversation>("SELECT * FROM conversations WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(conversation)
}

pub async fn create_for_party(
    pool: &PgPool,
    party_id: Uuid,
    host_id: Uuid,
    title: &str,
) -> ApiResult<Conversation> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query("INSERT INTO conversations (id, kind, title, created_by, party_id) VALUES ($1, 'party', $2, $3, $4)")
        .bind(id)
        .bind(title)
        .bind(host_id)
        .bind(party_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO conversation_members (conversation_id, user_id, role) VALUES ($1, $2, 'owner')")
        .bind(id)
        .bind(host_id)
        .execute(&mut *tx)
        .await?;
    let conversation =
        sqlx::query_as::<_, Conversation>("SELECT * FROM conversations WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(conversation)
}

pub async fn add_member(
    pool: &PgPool,
    conversation_id: Uuid,
    actor: Uuid,
    new_member: Uuid,
) -> ApiResult<()> {
    let conversation = get(pool, conversation_id).await?;
    match conversation.kind.as_str() {
        "direct" => return Err(ApiError::bad_request("cannot invite into a direct chat")),
        "party" => return Err(ApiError::bad_request("party membership manages this room")),
        _ => {}
    }
    let is_member_now = is_member(pool, conversation_id, actor).await?;
    let is_owner = conversation.created_by == Some(actor);
    if !is_member_now && !is_owner {
        return Err(ApiError::Forbidden(
            "not a member of this conversation".into(),
        ));
    }
    sqlx::query("INSERT INTO conversation_members (conversation_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(conversation_id)
        .bind(new_member)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn remove_member(
    pool: &PgPool,
    conversation_id: Uuid,
    actor: Uuid,
    target: Uuid,
) -> ApiResult<()> {
    let conversation = get(pool, conversation_id).await?;
    if conversation.created_by != Some(actor) && actor != target {
        return Err(ApiError::forbidden("only the owner can remove others"));
    }
    let result =
        sqlx::query("DELETE FROM conversation_members WHERE conversation_id = $1 AND user_id = $2")
            .bind(conversation_id)
            .bind(target)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("member"));
    }
    Ok(())
}

const MESSAGE_SELECT: &str = "SELECT msg.id, msg.conversation_id, msg.sender_id, u.display_name AS sender_name, \
     msg.body, msg.created_at, msg.edited_at \
     FROM messages msg JOIN users u ON u.id = msg.sender_id";

pub async fn messages_page(
    pool: &PgPool,
    conversation_id: Uuid,
    viewer_id: Uuid,
    before: Option<DateTime<Utc>>,
    limit: i64,
) -> ApiResult<Vec<ChatMessage>> {
    let rows = sqlx::query_as::<_, ChatMessage>(&format!(
        "{MESSAGE_SELECT}
         WHERE msg.conversation_id = $1
           AND msg.deleted_at IS NULL
           AND msg.sender_id NOT IN (SELECT blocked FROM user_blocks WHERE blocker = $4)
           AND msg.sender_id NOT IN (SELECT muted FROM user_mutes WHERE muter = $4 AND (until IS NULL OR until > now()))
           AND (($2 IS NULL) OR (msg.created_at, msg.id) < ($2, 'ffffffff-ffff-ffff-ffff-ffffffffffff'::uuid))
         ORDER BY msg.created_at DESC, msg.id DESC
         LIMIT $3"
    ))
    .bind(conversation_id)
    .bind(before)
    .bind(limit)
    .bind(viewer_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Owner-only edit. Returns the updated message.
pub async fn edit_message(
    pool: &PgPool,
    message_id: Uuid,
    sender_id: Uuid,
    new_body: &str,
) -> ApiResult<ChatMessage> {
    let trimmed = new_body.trim();
    if trimmed.is_empty() || trimmed.len() > 4000 {
        return Err(ApiError::bad_request("message must be 1-4000 characters"));
    }
    let result = sqlx::query(
        "UPDATE messages SET body = $3, edited_at = now() \
         WHERE id = $1 AND sender_id = $2 AND deleted_at IS NULL",
    )
    .bind(message_id)
    .bind(sender_id)
    .bind(trimmed)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("editable message"));
    }
    sqlx::query_as::<_, ChatMessage>(&format!(
        "{MESSAGE_SELECT} WHERE msg.id = $1"
    ))
    .bind(message_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("message"))
}

/// Owner soft-deletes; moderators/admins can delete anyone's.
/// Returns true when a row was hidden.
pub async fn delete_message(
    pool: &PgPool,
    message_id: Uuid,
    actor_id: Uuid,
    is_staff: bool,
) -> ApiResult<bool> {
    let result = sqlx::query(
        "UPDATE messages SET deleted_at = now() \
         WHERE id = $1 AND deleted_at IS NULL \
           AND ($3 OR sender_id = $2)",
    )
    .bind(message_id)
    .bind(actor_id)
    .bind(is_staff)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn send_message(
    pool: &PgPool,
    conversation_id: Uuid,
    sender_id: Uuid,
    sender_name: &str,
    body: &str,
) -> ApiResult<ChatMessage> {
    let trimmed = body.trim();
    if trimmed.is_empty() || trimmed.len() > 4000 {
        return Err(ApiError::bad_request("message must be 1-4000 characters"));
    }
    // Block enforcement: a blocked pair cannot DM each other.
    let blocked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_blocks ub \
         JOIN conversation_members cm ON cm.conversation_id = $1 \
             AND cm.user_id <> $2 \
         WHERE (ub.blocker = cm.user_id AND ub.blocked = $2) \
            OR (ub.blocker = $2 AND ub.blocked = cm.user_id))",
    )
    .bind(conversation_id)
    .bind(sender_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    if blocked {
        return Err(ApiError::forbidden("message blocked between these users"));
    }

    let message_id = new_id();
    let message = sqlx::query_as::<_, ChatMessage>(
        "WITH inserted AS (
            INSERT INTO messages (id, conversation_id, sender_id, body)
            VALUES ($1, $2, $3, $4)
            RETURNING *
         )
         SELECT i.id, i.conversation_id, i.sender_id, $5 AS sender_name, i.body, i.created_at, i.edited_at
         FROM inserted i",
    )
    .bind(message_id)
    .bind(conversation_id)
    .bind(sender_id)
    .bind(trimmed)
    .bind(sender_name)
    .fetch_one(pool)
    .await?;

    sqlx::query(
        "INSERT INTO message_receipts (message_id, user_id, delivered_at) VALUES ($1, $2, now()) ON CONFLICT DO NOTHING",
    )
    .bind(message_id)
    .bind(sender_id)
    .execute(pool)
    .await?;

    Ok(message)
}

pub async fn mark_read(pool: &PgPool, conversation_id: Uuid, user_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        "UPDATE message_receipts r SET read_at = now()
         FROM messages m
         WHERE r.message_id = m.id AND m.conversation_id = $1 AND r.user_id = $2 AND r.read_at IS NULL",
    )
    .bind(conversation_id)
    .bind(user_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE conversation_members SET last_read_at = $3 WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(user_id)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn unread_count(pool: &PgPool, user_id: Uuid) -> ApiResult<i64> {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM message_receipts r
         JOIN messages m ON m.id = r.message_id
         WHERE r.user_id = $1 AND r.read_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(count.0)
}
