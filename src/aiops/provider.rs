//! AI provider abstraction + the two built-in implementations.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One concrete, machine-applicable suggestion.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Recommendation {
    /// Stable id derived from the action content.
    pub id: String,
    /// Governance scope: sources | security | library | notifications | grants
    pub scope: String,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    /// 0.0 – 1.0
    #[serde(default)]
    pub confidence: f64,
    /// The whitelisted action envelope: {"type": "...", ...}
    pub action: serde_json::Value,
}

pub fn recommendation_id(action: &serde_json::Value, salt: &str) -> String {
    let digest = crate::utils::sha256_hex(&format!("{action}{salt}"));
    format!("rec-{}", &digest[..12])
}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn analyze(&self, context: &serde_json::Value) -> anyhow::Result<Vec<Recommendation>>;
}

// ---------------------------------------------------------------- heuristic

/// Deterministic rules over the live snapshot. This is the default brain and
/// the fallback when an LLM is configured but unreachable.
pub struct HeuristicProvider;

#[async_trait]
impl AiProvider for HeuristicProvider {
    fn kind(&self) -> &'static str {
        "heuristic"
    }

    async fn analyze(&self, ctx: &serde_json::Value) -> anyhow::Result<Vec<Recommendation>> {
        let mut recs = Vec::new();
        let mut n = 0usize;
        let mut push = |scope: &str, title: &str, detail: &str, confidence: f64, action: serde_json::Value| {
            n += 1;
            let id = recommendation_id(&action, &format!("heuristic-{n}"));
            recs.push(Recommendation {
                id,
                scope: scope.to_string(),
                title: title.to_string(),
                detail: detail.to_string(),
                confidence,
                action,
            });
        };

        let stats = ctx.get("stats").cloned().unwrap_or(serde_json::json!({}));

        // 1. Dead or unreliable sources → suggest disabling remote plugins
        //    (compiled-ins can't be toggled at runtime; they get a report).
        if let Some(sources) = ctx.get("sources").and_then(|s| s.as_array()) {
            for src in sources {
                let name = src.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let status = src.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                if status == "down" {
                    push(
                        "sources",
                        &format!("Source '{name}' is down"),
                        "Circuit breaker tripped. Disable the plugin while it recovers to stop wasted fan-out latency.",
                        0.8,
                        serde_json::json!({ "type": "disable_source_plugin", "name": name }),
                    );
                } else if let Some(rate) = src.get("success_rate").and_then(|v| v.as_f64()) {
                    if rate < 0.5 && status != "unknown" {
                        push(
                            "sources",
                            &format!("Source '{name}' success rate is {rate:.0}%"),
                            "Below 50% success — consider disabling until healthy.",
                            0.65,
                            serde_json::json!({ "type": "disable_source_plugin", "name": name }),
                        );
                    }
                }
            }
        }

        // 2. Stuck transcode backlog.
        let stuck = stats.get("running_transcodes").and_then(|v| v.as_i64()).unwrap_or(0);
        if stuck > 3 {
            push(
                "notifications",
                "Transcode backlog building up",
                &format!("{stuck} jobs running/queued. Workers may be saturated — alert the admins."),
                0.7,
                serde_json::json!({
                    "type": "notify_admins",
                    "title": "Transcode backlog",
                    "body": format!("{stuck} transcode jobs are queued or running."),
                }),
            );
        }

        // 2b. Error spike on a single route (failure analysis feed).
        if let Some(top) = ctx.pointer("/failures/top_route") {
            if let (Some(route), Some(count)) = (
                top.get("route").and_then(|v| v.as_str()),
                top.get("count").and_then(|v| v.as_i64()),
            ) {
                if count >= 10 {
                    push(
                        "notifications",
                        "Error spike detected",
                        &format!("'{route}' returned 5xx {count} times in 24h — check server logs and the failure timeline."),
                        0.75,
                        serde_json::json!({
                            "type": "notify_admins",
                            "title": "Server error spike",
                            "body": format!("{route} failed {count} times in the last 24 hours."),
                        }),
                    );
                }
            }
        }

        // 3. Empty library with roots configured → scan.
        let media = stats.get("media_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let files = stats.get("file_count").and_then(|v| v.as_i64()).unwrap_or(0);
        if media == 0 || (files == 0 && media == 0) {
            push(
                "library",
                "Library looks empty",
                "No catalog items found — run a library scan to import from LIBRARY_ROOTS.",
                0.9,
                serde_json::json!({ "type": "run_library_scan" }),
            );
        }

        // 4. Risk layer: repeated challenges on one user → tighten concurrency;
        //    abusive user → revoke grants. Only when security integration is on.
        if ctx.get("security_integration").and_then(|v| v.as_bool()).unwrap_or(false) {
            if let Some(risk) = ctx.get("risk") {
                let enabled = risk.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                let critical = risk
                    .get("critical_events_24h")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if !enabled && critical > 0 {
                    push(
                        "security",
                        "Security events but risk layer disabled",
                        &format!("{critical} critical security events in 24h while the risk layer is off."),
                        0.75,
                        serde_json::json!({ "type": "set_risk_policy", "patch": { "enabled": true } }),
                    );
                }
                if enabled {
                    if let Some(offender) = risk.get("top_offender") {
                        if let (Some(uid), Some(events)) = (
                            offender.get("user_id").and_then(|v| v.as_str()),
                            offender.get("events").and_then(|v| v.as_i64()),
                        ) {
                            if events >= 5 {
                                push(
                                    "grants",
                                    "Repeat playback-abuse suspect",
                                    &format!("{events} flagged events in 24h — revoke their active playback grants."),
                                    0.7,
                                    serde_json::json!({ "type": "revoke_user_grants", "user_id": uid }),
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(recs)
    }
}

// ---------------------------------------------------------------- llm

/// OpenAI-compatible chat-completions provider. The API key never enters the
/// database — it comes from `AI_API_KEY` at boot.
pub struct LlmProvider {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    client: reqwest::Client,
}

const SYSTEM_PROMPT: &str = "\
You are the operations copilot of a self-hosted media platform. You receive a \
JSON snapshot of system health. Respond ONLY with JSON: \
{\"recommendations\":[{\"scope\":\"sources|security|library|notifications|grants\",\
\"title\":\"...\",\"detail\":\"...\",\"confidence\":0.0..1.0,\
\"action\":{\"type\":\"disable_source_plugin|set_risk_policy|revoke_user_grants|notify_admins|run_library_scan|add_media_relationship\", ...}}]} \
Maximum 6 recommendations. Only propose actions from that whitelist.";

impl LlmProvider {
    pub fn new(base_url: String, api_key: Option<String>, model: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            client,
        }
    }
}

#[async_trait]
impl AiProvider for LlmProvider {
    fn kind(&self) -> &'static str {
        "llm"
    }

    async fn analyze(&self, context: &serde_json::Value) -> anyhow::Result<Vec<Recommendation>> {
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.1,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": context.to_string() },
            ],
        });

        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request.send().await?.error_for_status()?;
        let payload: serde_json::Value = response.json().await?;
        let content = payload
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow::anyhow!("llm response missing message content"))?;

        let parsed: serde_json::Value = serde_json::from_str(content)?;
        let mut recs: Vec<Recommendation> = Vec::new();
        if let Some(items) = parsed.get("recommendations").and_then(|r| r.as_array()) {
            for item in items {
                let action = item.get("action").cloned().unwrap_or(serde_json::Value::Null);
                if !action.is_object() {
                    continue;
                }
                let scope = item
                    .get("scope")
                    .and_then(|s| s.as_str())
                    .unwrap_or("notifications")
                    .to_string();
                recs.push(Recommendation {
                    id: recommendation_id(&action, "llm"),
                    scope,
                    title: item
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Suggestion")
                        .to_string(),
                    detail: item
                        .get("detail")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    confidence: item
                        .get("confidence")
                        .and_then(|c| c.as_f64())
                        .unwrap_or(0.5)
                        .clamp(0.0, 1.0),
                    action,
                });
            }
        }
        recs.truncate(6);
        Ok(recs)
    }
}
