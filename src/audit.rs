//! Global security/admin audit trail.
//!
//! One helper, called from every sensitive action: policy changes, key ops,
//! bans, lockdown toggles, card decisions, merchant actions. Never blocks the
//! caller.

use sqlx::PgPool;
use uuid::Uuid;

pub async fn log(
    pool: &PgPool,
    actor: Option<Uuid>,
    action: &str,
    target: &str,
    details: serde_json::Value,
    ip: &str,
) {
    if let Err(err) = sqlx::query(
        "INSERT INTO audit_logs (id, actor, action, target, details, ip)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(crate::utils::new_id())
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(details)
    .bind(ip)
    .execute(pool)
    .await
    {
        tracing::warn!(error = %err, action, "audit write failed");
    }
}

/// Fire-and-forget convenience for request contexts without an IP handy.
#[allow(dead_code)]
pub async fn log_simple(pool: &PgPool, actor: Option<Uuid>, action: &str, target: &str) {
    log(pool, actor, action, target, serde_json::json!({}), "").await;
}
