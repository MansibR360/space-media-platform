//! Risk assessment engine.
//!
//! Signals are cheap SQL aggregates over tables the server already maintains;
//! scoring is a transparent weighted sum so admins can reason about decisions.
//! Every non-allow decision is persisted to `security_events` for audit and
//! for the AI layer's context.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::utils::new_id;

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct RiskPolicy {
    pub enabled: bool,
    pub challenge_threshold: f64,
    pub revoke_threshold: f64,
    pub max_concurrent_sessions: i32,
    pub max_devices_per_day: i32,
    pub max_resolves_per_hour: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Challenge,
    Revoke,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Signal {
    pub name: &'static str,
    pub value: f64,
    pub points: f64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Assessment {
    /// false when the risk layer is disabled — decision is always allow.
    pub enabled: bool,
    pub score: f64,
    pub decision: Decision,
    pub signals: Vec<Signal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<RiskPolicy>,
}

impl Assessment {
    fn passthrough() -> Self {
        Self {
            enabled: false,
            score: 0.0,
            decision: Decision::Allow,
            signals: vec![],
            policy: None,
        }
    }
}

pub async fn load_policy(pool: &PgPool) -> ApiResult<RiskPolicy> {
    let policy = sqlx::query_as::<_, RiskPolicy>(
        "SELECT enabled, challenge_threshold, revoke_threshold,
                max_concurrent_sessions, max_devices_per_day, max_resolves_per_hour
         FROM risk_policies WHERE id = 1",
    )
    .fetch_one(pool)
    .await?;
    Ok(policy)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PolicyPatch {
    pub enabled: Option<bool>,
    pub challenge_threshold: Option<f64>,
    pub revoke_threshold: Option<f64>,
    pub max_concurrent_sessions: Option<i32>,
    pub max_devices_per_day: Option<i32>,
    pub max_resolves_per_hour: Option<i32>,
}

const FIELDS: &[&str] = &[
    "enabled",
    "challenge_threshold",
    "revoke_threshold",
    "max_concurrent_sessions",
    "max_devices_per_day",
    "max_resolves_per_hour",
];

pub async fn update_policy(pool: &PgPool, patch: &PolicyPatch) -> ApiResult<RiskPolicy> {
    let _ = FIELDS;
    if patch.challenge_threshold.map_or(false, |c| !(0.0..=100.0).contains(&c))
        || patch.revoke_threshold.map_or(false, |r| !(0.0..=100.0).contains(&r))
    {
        return Err(crate::error::ApiError::bad_request(
            "thresholds must be within 0..=100",
        ));
    }
    if let (Some(c), Some(r)) = (patch.challenge_threshold, patch.revoke_threshold) {
        if c >= r {
            return Err(crate::error::ApiError::bad_request(
                "challenge threshold must be below revoke threshold",
            ));
        }
    }
    // Dynamic SET clause from whitelisted fields only.
    let mut sets: Vec<String> = Vec::new();
    macro_rules! push_set {
        ($field:expr, $bound:expr) => {
            if $bound.is_some() {
                sets.push(format!("{} = ${}", $field, sets.len() + 1));
            }
        };
    }
    push_set!("enabled", patch.enabled);
    push_set!("challenge_threshold", patch.challenge_threshold);
    push_set!("revoke_threshold", patch.revoke_threshold);
    push_set!("max_concurrent_sessions", patch.max_concurrent_sessions);
    push_set!("max_devices_per_day", patch.max_devices_per_day);
    push_set!("max_resolves_per_hour", patch.max_resolves_per_hour);

    if !sets.is_empty() {
        let query = format!(
            "UPDATE risk_policies SET {}, updated_at = now() WHERE id = 1",
            sets.join(", ")
        );
        let mut q = sqlx::query(&query);
        if let Some(v) = patch.enabled {
            q = q.bind(v);
        }
        if let Some(v) = patch.challenge_threshold {
            q = q.bind(v);
        }
        if let Some(v) = patch.revoke_threshold {
            q = q.bind(v);
        }
        if let Some(v) = patch.max_concurrent_sessions {
            q = q.bind(v);
        }
        if let Some(v) = patch.max_devices_per_day {
            q = q.bind(v);
        }
        if let Some(v) = patch.max_resolves_per_hour {
            q = q.bind(v);
        }
        q.execute(pool).await?;
    }
    load_policy(pool).await
}

/// Scores one playback authorization attempt for `user_id`.
pub async fn assess(pool: &PgPool, user_id: Uuid) -> ApiResult<Assessment> {
    let policy = load_policy(pool).await?;
    if !policy.enabled {
        return Ok(Assessment::passthrough());
    }

    let signals = collect_signals(pool, user_id, &policy).await?;
    let score: f64 = signals.iter().map(|s| s.points).sum();
    let score = score.min(100.0);

    let decision = if score >= policy.revoke_threshold {
        Decision::Revoke
    } else if score >= policy.challenge_threshold {
        Decision::Challenge
    } else {
        Decision::Allow
    };

    Ok(Assessment {
        enabled: true,
        score,
        decision,
        signals,
        policy: Some(policy),
    })
}

async fn collect_signals(
    pool: &PgPool,
    user_id: Uuid,
    policy: &RiskPolicy,
) -> ApiResult<Vec<Signal>> {
    let mut signals = Vec::new();

    let (concurrent,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM playback_sessions
         WHERE user_id = $1 AND ended_at IS NULL AND last_active_at > now() - INTERVAL '30 minutes'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if concurrent > policy.max_concurrent_sessions as i64 {
        let over = (concurrent - policy.max_concurrent_sessions as i64) as f64;
        signals.push(Signal {
            name: "concurrent_sessions",
            value: concurrent as f64,
            points: (25.0 + over * 12.0).min(50.0),
            detail: format!("limit {}", policy.max_concurrent_sessions),
        });
    }

    let (devices,): (i64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT device_name) FROM playback_sessions
         WHERE user_id = $1 AND started_at > now() - INTERVAL '24 hours'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if devices > policy.max_devices_per_day as i64 {
        let over = (devices - policy.max_devices_per_day as i64) as f64;
        signals.push(Signal {
            name: "distinct_devices_24h",
            value: devices as f64,
            points: (15.0 + over * 8.0).min(40.0),
            detail: format!("limit {}", policy.max_devices_per_day),
        });
    }

    let (resolves,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM playback_grants
         WHERE user_id = $1 AND created_at > now() - INTERVAL '1 hour'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if resolves > policy.max_resolves_per_hour as i64 {
        let over = (resolves - policy.max_resolves_per_hour as i64) as f64;
        signals.push(Signal {
            name: "resolves_per_hour",
            value: resolves as f64,
            points: (20.0 + over * 4.0).min(45.0),
            detail: format!("limit {}", policy.max_resolves_per_hour),
        });
    }

    let (prior,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM security_events
         WHERE user_id = $1 AND decision <> 'allow' AND created_at > now() - INTERVAL '24 hours'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if prior > 0 {
        signals.push(Signal {
            name: "prior_flags_24h",
            value: prior as f64,
            points: (prior as f64 * 10.0).min(30.0),
            detail: String::new(),
        });
    }

    // Distinct client contexts (TLS/device fingerprints bound to grants).
    // One user presenting many contexts smells like credential/grant sharing.
    let (contexts,): (i64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT device_hash) FROM playback_grants \
         WHERE user_id = $1 AND device_hash <> '' AND device_hash NOT LIKE 'anon:%' \
           AND created_at > now() - INTERVAL '24 hours'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    if contexts > policy.max_devices_per_day as i64 {
        let over = (contexts - policy.max_devices_per_day as i64) as f64;
        signals.push(Signal {
            name: "distinct_client_contexts_24h",
            value: contexts as f64,
            points: (18.0 + over * 7.0).min(35.0),
            detail: format!("limit {}", policy.max_devices_per_day),
        });
    }

    Ok(signals)
}

/// Persists an auditable event; never blocks the caller.
pub async fn record_event(
    pool: &PgPool,
    user_id: Option<Uuid>,
    signal_type: &str,
    severity: &str,
    score: f64,
    decision: Decision,
    details: serde_json::Value,
) {
    let decision_str = match decision {
        Decision::Allow => "allow",
        Decision::Challenge => "challenge",
        Decision::Revoke => "revoke",
    };
    let result = sqlx::query(
        "INSERT INTO security_events (id, user_id, signal_type, severity, score, decision, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(signal_type)
    .bind(severity)
    .bind(score)
    .bind(decision_str)
    .bind(details)
    .execute(pool)
    .await;
    if let Err(err) = result {
        tracing::warn!(error = %err, "failed to record security event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_bands() {
        let policy = RiskPolicy {
            enabled: true,
            challenge_threshold: 40.0,
            revoke_threshold: 75.0,
            max_concurrent_sessions: 5,
            max_devices_per_day: 8,
            max_resolves_per_hour: 60,
        };
        let band = |score: f64| {
            if score >= policy.revoke_threshold {
                Decision::Revoke
            } else if score >= policy.challenge_threshold {
                Decision::Challenge
            } else {
                Decision::Allow
            }
        };
        assert_eq!(band(10.0), Decision::Allow);
        assert_eq!(band(40.0), Decision::Challenge);
        assert_eq!(band(74.9), Decision::Challenge);
        assert_eq!(band(75.0), Decision::Revoke);
    }
}
