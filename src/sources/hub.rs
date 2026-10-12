//! Remote source hubs.
//!
//! A hub is any SPACE instance (or compatible service) exposing
//! `/api/hub/*`. Two directions:
//!
//! * **client** — this server registers hubs and queries them when it needs
//!   more sources: search results flow into unified search and, crucially,
//!   resolution falls back to a hub when local providers cannot produce a
//!   playable stream. Learned recipes can be pushed/pulled between servers.
//! * **server** — this server answers `/api/hub/*` from its own learned web
//!   sources (see `hub_server.rs`).
//!
//! Auth is a shared secret in the `X-Hub-Token` header. Inbound: unset token
//! means loopback-only access (homelab safe). Outbound: optional per-hub
//! token stored alongside the registration.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct HubRow {
    pub id: Uuid,
    pub name: String,
    pub base_url: String,
    #[serde(skip_serializing)]
    pub token: Option<String>,
    pub enabled: bool,
    pub last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing)]
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub mod repo {
    use super::HubRow;
    use crate::error::{ApiError, ApiResult};
    use sqlx::PgPool;
    use uuid::Uuid;

    pub async fn list(pool: &PgPool) -> ApiResult<Vec<HubRow>> {
        Ok(sqlx::query_as::<_, HubRow>(
            "SELECT * FROM source_hubs ORDER BY created_at",
        )
        .fetch_all(pool)
        .await?)
    }

    pub async fn get(pool: &PgPool, id: Uuid) -> ApiResult<HubRow> {
        sqlx::query_as::<_, HubRow>("SELECT * FROM source_hubs WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("hub"))
    }

    pub async fn insert(
        pool: &PgPool,
        row: &HubRow,
        created_by: Option<Uuid>,
    ) -> ApiResult<HubRow> {
        sqlx::query_as::<_, HubRow>(
            "INSERT INTO source_hubs (id, name, base_url, token, enabled, created_by)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
        )
        .bind(row.id)
        .bind(&row.name)
        .bind(&row.base_url)
        .bind(&row.token)
        .bind(row.enabled)
        .bind(created_by)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn patch(
        pool: &PgPool,
        id: Uuid,
        base_url: Option<&str>,
        token: Option<Option<&str>>,
        enabled: Option<bool>,
    ) -> ApiResult<HubRow> {
        let token_value = match token {
            // COALESCE semantics: only overwrite when Some(new value)
            Some(t) => t.map(str::to_string),
            None => None,
        };
        sqlx::query_as::<_, HubRow>(
            "UPDATE source_hubs SET
                base_url = COALESCE($2, base_url),
                token = CASE WHEN $5::bool THEN $3 ELSE token END,
                enabled = COALESCE($4, enabled),
                updated_at = now()
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(base_url)
        .bind(token_value)
        .bind(enabled)
        .bind(token.is_some())
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn touch_sync(pool: &PgPool, id: Uuid) -> ApiResult<()> {
        sqlx::query("UPDATE source_hubs SET last_sync_at = now() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    pub async fn delete(pool: &PgPool, id: Uuid) -> ApiResult<()> {
        let result = sqlx::query("DELETE FROM source_hubs WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("hub"));
        }
        Ok(())
    }
}

// -------------------------------------------------------------- outbound

/// Lightweight reference so the aggregator can query hubs without loading
/// full rows.
pub struct HubRef<'a> {
    pub name: &'a str,
    pub base_url: &'a str,
    pub token: Option<&'a str>,
}

impl HubRow {
    pub fn r#ref(&self) -> HubRef<'_> {
        HubRef {
            name: &self.name,
            base_url: &self.base_url,
            token: self.token.as_deref(),
        }
    }
}

fn hub_client(cfg: &crate::config::Config) -> ApiResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.hub_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("space-media-server/0.1")
        .build()
        .map_err(|e| ApiError::Unavailable(format!("http client: {e}")))
}

async fn hub_get_json(
    cfg: &crate::config::Config,
    hub: HubRef<'_>,
    path: &str,
    query: &[(&str, String)],
) -> ApiResult<serde_json::Value> {
    let mut req = hub_client(cfg)?.get(format!(
        "{}{}",
        hub.base_url.trim_end_matches('/'),
        path
    ));
    if let Some(tok) = hub.token {
        req = req.header("X-Hub-Token", tok);
    }
    if !query.is_empty() {
        req = req.query(query);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("hub '{}' unreachable: {e}", hub.name)))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ApiError::Unavailable(format!(
            "hub '{}' answered {}",
            hub.name, status
        )));
    }
    resp.json()
        .await
        .map_err(|e| ApiError::Unavailable(format!("hub '{}' sent bad json: {e}", hub.name)))
}

/// One result row from a remote hub.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HubHit {
    /// remote learned-source name
    pub source: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub overview: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
}

pub async fn find(
    cfg: &crate::config::Config,
    hub: HubRef<'_>,
    query: &str,
    category: Option<&str>,
    limit: i32,
) -> ApiResult<Vec<HubHit>> {
    let mut q: Vec<(&str, String)> = vec![
        ("q", query.to_string()),
        ("limit", limit.clamp(1, 60).to_string()),
    ];
    if let Some(cat) = category {
        q.push(("category", cat.to_string()));
    }
    let body = hub_get_json(cfg, hub, "/api/hub/find", &q).await?;
    let hits = body
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(hits
        .into_iter()
        .filter_map(|h| serde_json::from_value::<HubHit>(h).ok())
        .collect())
}

pub async fn item_detail(
    cfg: &crate::config::Config,
    hub: HubRef<'_>,
    source: &str,
    id: &str,
) -> ApiResult<serde_json::Value> {
    hub_get_json(
        cfg,
        hub,
        "/api/hub/item",
        &[("source", source.to_string()), ("id", id.to_string())],
    )
    .await
}

/// Recipe listing used by pull + reachability probes.
pub async fn sources(cfg: &crate::config::Config, hub: HubRef<'_>) -> ApiResult<Vec<serde_json::Value>> {
    let body = hub_get_json(cfg, hub, "/api/hub/sources", &[]).await?;
    Ok(body
        .get("sources")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default())
}

pub async fn upload_recipe(
    cfg: &crate::config::Config,
    hub: HubRef<'_>,
    payload: serde_json::Value,
) -> ApiResult<()> {
    let mut req = hub_client(cfg)?
        .post(format!(
            "{}/api/hub/sources",
            hub.base_url.trim_end_matches('/')
        ))
        .json(&payload);
    if let Some(tok) = hub.token {
        req = req.header("X-Hub-Token", tok);
    }
    let resp = req.send().await.map_err(|e| {
        ApiError::Unavailable(format!("hub '{}' unreachable: {e}", hub.name))
    })?;
    if !resp.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "hub '{}' rejected recipe ({})",
            hub.name,
            resp.status()
        )));
    }
    Ok(())
}
