//! Universal event bus (Phase H).

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::chat::hub::{GLOBAL_ROOM, Hub};

/// Fire-and-forget publish: never blocks or fails the caller.
pub async fn emit(
    pool: &PgPool,
    hub: &Hub,
    event: &str,
    payload: serde_json::Value,
    actor: Option<Uuid>,
) {
    metrics::counter!("platform_events_total", "event" => event.to_string()).increment(1);

    if let Err(err) = sqlx::query(
        "INSERT INTO events (id, event, payload, actor) VALUES ($1, $2, $3, $4)",
    )
    .bind(crate::utils::new_id())
    .bind(event)
    .bind(&payload)
    .bind(actor)
    .execute(pool)
    .await
    {
        tracing::debug!(error = %err, event, "event persist failed");
    }

    hub.publish(
        GLOBAL_ROOM,
        serde_json::json!({
            "type": "platform.event",
            "event": event,
            "data": payload,
        })
        .to_string(),
    );
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct EventRow {
    pub id: Uuid,
    pub event: String,
    pub payload: serde_json::Value,
    pub actor: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Recent events, optionally filtered — feeds the ops timeline.
pub async fn recent(pool: &PgPool, event: Option<&str>, limit: i64) -> anyhow::Result<Vec<EventRow>> {
    let rows = match event {
        Some(name) => {
            sqlx::query_as::<_, EventRow>(
                "SELECT * FROM events WHERE event = $1 ORDER BY created_at DESC LIMIT $2",
            )
            .bind(name)
            .bind(limit.clamp(1, 500))
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, EventRow>(
                "SELECT * FROM events ORDER BY created_at DESC LIMIT $1",
            )
            .bind(limit.clamp(1, 500))
            .fetch_all(pool)
            .await?
        }
    };
    Ok(rows)
}
