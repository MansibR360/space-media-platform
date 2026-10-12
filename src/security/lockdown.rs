//! Site-wide lockdown switch (changeset 5).
//!
//! Admin flips it; every public route answers `503 locked_down` while admin
//! surface + health checks keep working. State is cached for a few seconds so
//! the check stays cheap on hot paths.

use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
struct Cached {
    enabled: bool,
    message: String,
    at: std::time::Instant,
}

static CACHE: RwLock<Option<Cached>> = RwLock::const_new(None);

const TTL: std::time::Duration = std::time::Duration::from_secs(5);

async fn load(state: &crate::state::AppState) -> Cached {
    {
        let guard = CACHE.read().await;
        if let Some(c) = guard.as_ref() {
            if c.at.elapsed() < TTL {
                return c.clone();
            }
        }
    }
    let row: Option<(bool, String)> = sqlx::query_as(
        "SELECT enabled, message FROM site_lockdown WHERE id = 1",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();
    let fresh = match row {
        Some((enabled, message)) => Cached {
            enabled,
            message,
            at: std::time::Instant::now(),
        },
        None => Cached {
            enabled: false,
            message: "service temporarily unavailable".into(),
            at: std::time::Instant::now(),
        },
    };
    *CACHE.write().await = Some(fresh.clone());
    fresh
}

pub async fn is_enabled(state: &Arc<crate::state::AppState>) -> bool {
    load(state).await.enabled
}

pub async fn message(state: &Arc<crate::state::AppState>) -> String {
    load(state).await.message
}

/// Admin toggle. Audit the flip.
pub async fn set_enabled(
    pool: &PgPool,
    enabled: bool,
    message: &str,
    actor: Option<Uuid>,
) -> crate::error::ApiResult<()> {
    sqlx::query(
        "UPDATE site_lockdown SET enabled = $2, message = $3,
                enabled_at = CASE WHEN $2 THEN now() ELSE NULL END, enabled_by = $4
         WHERE id = 1",
    )
    .bind(actor)
    .bind(enabled)
    .bind(message)
    .bind(actor)
    .execute(pool)
    .await?;
    *CACHE.write().await = None; // invalidate
    Ok(())
}

use uuid::Uuid;
