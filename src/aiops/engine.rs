//! AI copilot engine: persisted governance, context building, action
//! execution behind a strict whitelist, and the autonomous actor loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::aiops::provider::{AiProvider, HeuristicProvider, LlmProvider, Recommendation};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct AiConfig {
    pub mode: String,
    pub autonomy: String,
    pub model: String,
    pub api_url: String,
    pub security_integration: bool,
    pub scopes: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_analysis: Option<serde_json::Value>,
    pub last_analyzed_at: Option<DateTime<Utc>>,
}

pub async fn load_config(pool: &PgPool) -> ApiResult<AiConfig> {
    let row = sqlx::query_as::<_, AiConfig>(
        "SELECT mode, autonomy, model, api_url, security_integration, scopes,
                last_analysis, last_analyzed_at
         FROM ai_config WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ConfigPatch {
    pub mode: Option<String>,
    pub autonomy: Option<String>,
    pub model: Option<String>,
    pub api_url: Option<String>,
    pub security_integration: Option<bool>,
    pub scopes: Option<Vec<String>>,
}

const MODES: &[&str] = &["off", "heuristic", "llm"];
const AUTONOMY: &[&str] = &["observe", "suggest", "act"];
const KNOWN_SCOPES: &[&str] = &[
    "sources",
    "security",
    "library",
    "notifications",
    "grants",
    "graph",
];

pub async fn save_config(pool: &PgPool, patch: &ConfigPatch) -> ApiResult<AiConfig> {
    if let Some(mode) = &patch.mode {
        if !MODES.contains(&mode.as_str()) {
            return Err(ApiError::bad_request("mode must be off | heuristic | llm"));
        }
    }
    if let Some(a) = &patch.autonomy {
        if !AUTONOMY.contains(&a.as_str()) {
            return Err(ApiError::bad_request(
                "autonomy must be observe | suggest | act",
            ));
        }
    }
    if let Some(scopes) = &patch.scopes {
        for scope in scopes {
            if !KNOWN_SCOPES.contains(&scope.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown scope '{scope}' (allowed: {KNOWN_SCOPES:?})"
                )));
            }
        }
    }

    let mut sets: Vec<String> = Vec::new();
    macro_rules! push_set {
        ($field:expr, $bound:expr) => {
            if $bound.is_some() {
                sets.push(format!("{} = ${}", $field, sets.len() + 1));
            }
        };
    }
    push_set!("mode", patch.mode);
    push_set!("autonomy", patch.autonomy);
    push_set!("model", patch.model);
    push_set!("api_url", patch.api_url);
    push_set!("security_integration", patch.security_integration);
    if patch.scopes.is_some() {
        sets.push(format!("scopes = ${}::jsonb", sets.len() + 1));
    }

    if !sets.is_empty() {
        let query = format!(
            "UPDATE ai_config SET {}, updated_at = now() WHERE id = 1",
            sets.join(", ")
        );
        let mut q = sqlx::query(&query);
        if let Some(v) = &patch.mode {
            q = q.bind(v);
        }
        if let Some(v) = &patch.autonomy {
            q = q.bind(v);
        }
        if let Some(v) = &patch.model {
            q = q.bind(v);
        }
        if let Some(v) = &patch.api_url {
            q = q.bind(v);
        }
        if let Some(v) = patch.security_integration {
            q = q.bind(v);
        }
        if let Some(scopes) = &patch.scopes {
            q = q.bind(serde_json::to_value(scopes).unwrap_or(serde_json::json!([])));
        }
        q.execute(pool).await?;
    }
    load_config(pool).await
}

pub struct AiEngine {
    pool: PgPool,
    cfg: Arc<Config>,
    hub: Arc<crate::chat::hub::Hub>,
}

/// Whitelisted action → governing scope.
fn scope_of(action_type: &str) -> Option<&'static str> {
    match action_type {
        "disable_source_plugin" | "enable_source_plugin" => Some("sources"),
        "set_risk_policy" => Some("security"),
        "revoke_user_grants" => Some("grants"),
        "notify_admins" => Some("notifications"),
        "run_library_scan" => Some("library"),
        "add_media_relationship" => Some("graph"),
        _ => None,
    }
}

impl AiEngine {
    pub fn new(pool: PgPool, cfg: Arc<Config>, hub: Arc<crate::chat::hub::Hub>) -> Arc<Self> {
        Arc::new(Self { pool, cfg, hub })
    }

    fn provider_for(&self, config: &AiConfig) -> Box<dyn AiProvider> {
        match config.mode.as_str() {
            "llm" => Box::new(LlmProvider::new(
                if config.api_url.is_empty() {
                    self.cfg.ai_api_url.clone()
                } else {
                    config.api_url.clone()
                },
                self.cfg.ai_api_key.clone(),
                if config.model.is_empty() {
                    self.cfg.ai_model.clone()
                } else {
                    config.model.clone()
                },
            )),
            _ => Box::new(HeuristicProvider),
        }
    }

    /// Live snapshot handed to whichever brain is configured.
    pub async fn build_context(&self, security_integration: bool) -> ApiResult<serde_json::Value> {
        let stats = system_stats_snapshot(&self.pool).await;
        let sources = self.pool_sources_context().await;

        let mut ctx = serde_json::json!({
            "stats": stats,
            "sources": sources,
        });

        // Failure-analysis context: worst offending route over 24 h.
        let top_route: Option<(String, i64)> = sqlx::query_as(
            "SELECT route, COUNT(*) AS count FROM failure_events
             WHERE created_at > now() - INTERVAL '24 hours'
             GROUP BY route ORDER BY count DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        if let Some((route, count)) = top_route {
            ctx["failures"] = serde_json::json!({
                "top_route": { "route": route, "count": count }
            });
        }

        if security_integration {
            let policy = crate::security::risk::load_policy(&self.pool)
                .await
                .ok();
            let enabled = policy.map(|p| p.enabled).unwrap_or(false);
            let critical: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM security_events \
                 WHERE severity = 'critical' AND created_at > now() - INTERVAL '24 hours'",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
            let offender: Option<(Option<Uuid>, i64)> = sqlx::query_as(
                "SELECT user_id, COUNT(*) AS events FROM security_events \
                 WHERE decision <> 'allow' AND created_at > now() - INTERVAL '24 hours' \
                   AND user_id IS NOT NULL \
                 GROUP BY user_id ORDER BY events DESC LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();

            ctx["risk"] = serde_json::json!({
                "enabled": enabled,
                "critical_events_24h": critical,
                "top_offender": offender.map(|(uid, events)| serde_json::json!({
                    "user_id": uid.map(|u| u.to_string()),
                    "events": events,
                })),
            });
        }
        Ok(ctx)
    }

    async fn pool_sources_context(&self) -> serde_json::Value {
        // Source health lives in-memory on AppState; the engine only has the
        // pool, so plugins + a note are surfaced from DB. The full health
        // snapshot is injected by `analyze_with_health` when called via state.
        match crate::plugins::repo::list(&self.pool).await {
            Ok(plugins) => serde_json::json!(plugins
                .iter()
                .filter(|p| p.kind == "source")
                .map(|p| serde_json::json!({
                    "name": p.name, "kind": "plugin", "enabled": p.enabled,
                }))
                .collect::<Vec<_>>()),
            Err(_) => serde_json::json!([]),
        }
    }

    /// Analyze using AppState-provided health snapshot (preferred entry).
    pub async fn analyze(
        &self,
        health_snapshot: serde_json::Value,
    ) -> ApiResult<serde_json::Value> {
        let config = load_config(&self.pool).await?;
        if config.mode == "off" {
            return Err(ApiError::bad_request(
                "AI layer is off — enable it in /api/admin/ai/config",
            ));
        }

        let mut ctx = self.build_context(config.security_integration).await?;
        if !health_snapshot.is_null() {
            ctx["sources"] = health_snapshot;
        }

        let provider = self.provider_for(&config);
        let (recommendations, provider_note) = match provider.analyze(&ctx).await {
            Ok(recs) => (recs, format!("via {}", provider.kind())),
            Err(err) => {
                tracing::warn!(error = %err, "AI provider failed; falling back to heuristic");
                (
                    HeuristicProvider.analyze(&ctx).await.unwrap_or_default(),
                    format!("heuristic fallback ({err})"),
                )
            }
        };

        let payload = serde_json::json!({
            "mode": config.mode,
            "autonomy": config.autonomy,
            "provider": provider_note,
            "context_summary": {
                "media_count": ctx.pointer("/stats/media_count"),
                "active_streams": ctx.pointer("/stats/active_playback_sessions"),
                "running_transcodes": ctx.pointer("/stats/running_transcodes"),
                "sources": ctx.get("sources"),
                "risk_enabled": ctx.pointer("/risk/enabled"),
            },
            "recommendations": recommendations,
            "generated_at": Utc::now(),
        });

        let _ = sqlx::query(
            "UPDATE ai_config SET last_analysis = $2, last_analyzed_at = now() WHERE id = 1",
        )
        .bind(&payload)
        .execute(&self.pool)
        .await;

        log_event(
            &self.pool,
            "analysis",
            &format!(
                "{} recommendation(s) generated {}",
                recommendations.len(),
                provider_note
            ),
            &serde_json::json!({ "count": recommendations.len(), "provider": provider_note }),
        )
        .await;

        Ok(payload)
    }

    /// Executes one recommendation after governance gates. Admin-initiated.
    pub async fn apply(
        &self,
        rec: &Recommendation,
        origin: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let config = load_config(&self.pool).await?;
        if config.mode == "off" {
            return Err(ApiError::bad_request("AI layer is off"));
        }

        let Some(action_type) = rec.action.get("type").and_then(|t| t.as_str()) else {
            return Err(ApiError::bad_request("action.type missing"));
        };
        let Some(scope) = scope_of(action_type) else {
            log_rejected(&self.pool, rec, "action type not whitelisted").await;
            return Err(ApiError::forbidden("action type not whitelisted"));
        };

        // Scope gate.
        let scopes: Vec<String> = config
            .scopes
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !scopes.iter().any(|s| s == scope) {
            log_rejected(&self.pool, rec, &format!("scope '{scope}' not granted")).await;
            return Err(ApiError::forbidden(format!(
                "scope '{scope}' not granted to the AI layer"
            )));
        }

        // Security-domain gate.
        if scope == "security" && !config.security_integration {
            log_rejected(&self.pool, rec, "security integration disabled").await;
            return Err(ApiError::forbidden(
                "security integration is disabled for the AI layer",
            ));
        }

        let outcome = self.execute(action_type, &rec.action).await;
        match outcome {
            Ok(result) => {
                log_event(
                    &self.pool,
                    "action_applied",
                    &format!("{origin}: {}", rec.title),
                    &serde_json::json!({
                        "origin": origin,
                        "scope": scope,
                        "action": rec.action,
                        "result": result,
                    }),
                )
                .await;
                Ok(serde_json::json!({
                    "applied": true,
                    "action": rec.action,
                    "result": result,
                }))
            }
            Err(err) => {
                log_rejected(&self.pool, rec, &format!("execution failed: {err}")).await;
                Err(err)
            }
        }
    }

    async fn execute(&self, action_type: &str, action: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        match action_type {
            "disable_source_plugin" | "enable_source_plugin" => {
                let name = action
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| ApiError::bad_request("name required"))?;
                let enable = action_type == "enable_source_plugin";
                let result =
                    sqlx::query("UPDATE plugins SET enabled = $2 WHERE name = $1 AND kind = 'source'")
                        .bind(name)
                        .bind(enable)
                        .execute(&self.pool)
                        .await?;
                if result.rows_affected() == 0 {
                    // Compiled-in natives cannot be toggled at runtime.
                    return Err(ApiError::bad_request(
                        "unknown source plugin (builtin sources can't be toggled)",
                    ));
                }
                Ok(serde_json::json!({ "plugin": name, "enabled": enable }))
            }
            "set_risk_policy" => {
                let patch: crate::security::risk::PolicyPatch = serde_json::from_value(
                    action.get("patch").cloned().unwrap_or(serde_json::json!({})),
                )
                .map_err(|e| ApiError::bad_request(format!("invalid policy patch: {e}")))?;
                let policy = crate::security::risk::update_policy(&self.pool, &patch).await?;
                Ok(serde_json::to_value(policy).unwrap_or(serde_json::json!({})))
            }
            "revoke_user_grants" => {
                let uid = action
                    .get("user_id")
                    .and_then(|u| u.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or_else(|| ApiError::bad_request("user_id required"))?;
                let count = crate::security::grants::revoke_for_user(&self.pool, uid).await?;
                Ok(serde_json::json!({ "user_id": uid.to_string(), "revoked": count }))
            }
            "notify_admins" => {
                let title = action
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("AI notice");
                let body = action.get("body").and_then(|b| b.as_str()).unwrap_or("");
                let admins: Vec<(Uuid,)> = sqlx::query_as(
                    "SELECT id FROM users WHERE role = 'admin' AND is_active",
                )
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
                for (uid,) in &admins {
                    let _ = crate::platform::repo::notify(
                        &self.pool,
                        *uid,
                        &self.hub,
                        "ai_copilot",
                        title,
                        body,
                        serde_json::json!({}),
                    )
                    .await;
                }
                Ok(serde_json::json!({ "notified": admins.len() }))
            }
            "run_library_scan" => {
                let roots = self.cfg.library_roots.clone();
                let reports = crate::streaming::scanner::scan_all_configured(&self.pool, &roots).await;
                Ok(serde_json::json!({ "scans": reports.len() }))
            }
            "add_media_relationship" => {
                let parse_id = |key: &str| -> Result<Uuid, ApiError> {
                    action
                        .get(key)
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .ok_or_else(|| ApiError::bad_request(format!("{key} must be a uuid")))
                };
                let from = parse_id("from_media_id")?;
                let to = parse_id("to_media_id")?;
                let relation = action
                    .get("relation")
                    .and_then(|r| r.as_str())
                    .ok_or_else(|| ApiError::bad_request("relation required"))?;
                let rel =
                    crate::graph::repo::add(&self.pool, from, to, relation, "ai").await?;
                Ok(serde_json::to_value(rel).unwrap_or(serde_json::json!({})))
            }
            other => Err(ApiError::forbidden(format!(
                "action '{other}' not whitelisted"
            ))),
        }
    }

    /// Autonomous loop: only runs meaningful work when mode != off AND
    /// autonomy == act; otherwise it idles cheaply.
    pub fn spawn_actor(self: Arc<Self>, cancel: CancellationToken) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(900)) => {
                        let Ok(config) = load_config(&self.pool).await else { continue };
                        if config.mode == "off" || config.autonomy != "act" {
                            continue;
                        }
                        match self.analyze(serde_json::Value::Null).await {
                            Ok(report) => {
                                let recs: Vec<Recommendation> = serde_json::from_value(
                                    report.get("recommendations").cloned().unwrap_or(serde_json::json!([])),
                                )
                                .unwrap_or_default();
                                for rec in recs {
                                    if let Err(err) = self.apply(&rec, "auto-act").await {
                                        tracing::debug!(error = %err, "auto-apply rejected");
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::debug!(error = %err, "actor analysis skipped");
                            }
                        }
                    }
                }
            }
        });
    }
}

async fn log_event(pool: &PgPool, kind: &str, summary: &str, payload: &serde_json::Value) {
    let _ = sqlx::query(
        "INSERT INTO ai_events (id, kind, summary, payload) VALUES ($1, $2, $3, $4)",
    )
    .bind(new_id())
    .bind(kind)
    .bind(summary)
    .bind(payload)
    .execute(pool)
    .await;
}

async fn log_rejected(pool: &PgPool, rec: &Recommendation, reason: &str) {
    log_event(
        pool,
        "action_rejected",
        reason,
        &serde_json::json!({ "recommendation": rec }),
    )
    .await;
}

/// Compact stats snapshot mirroring /api/stats, without needing AppState.
async fn system_stats_snapshot(pool: &PgPool) -> serde_json::Value {
    let media_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let file_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_files")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let active_playback_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_sessions \
         WHERE ended_at IS NULL AND last_active_at > now() - INTERVAL '30 minutes'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let running_transcodes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transcode_jobs WHERE status IN ('queued','running')",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    serde_json::json!({
        "media_count": media_count,
        "file_count": file_count,
        "user_count": user_count,
        "active_playback_sessions": active_playback_sessions,
        "running_transcodes": running_transcodes,
    })
}
