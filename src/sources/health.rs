//! Source health tracking + circuit breaker + telemetry (Phase 44 / A).
//!
//! Every provider interaction feeds this registry. After 3 consecutive
//! failures a source trips to `down` and the aggregator skips it entirely
//! (fast paths stay fast); the background prober half-openly probes it every
//! minute until it recovers.
//!
//! On top of the breaker this now records per-operation telemetry — search vs
//! resolve success rates, an in-flight request gauge — so `/api/sources/status`
//! can drive dynamic source preference.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Unknown,
    Healthy,
    Degraded,
    Down,
}

/// Which kind of provider call produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Search,
    Resolve,
    /// Background liveness probe — refreshes breaker state, not public rates.
    Probe,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthSnapshot {
    pub name: String,
    pub status: Status,
    pub latency_ms: Option<u128>,
    pub last_ok: Option<DateTime<Utc>>,
    pub last_checked: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    // ---- telemetry (Phase A) ----
    pub success_rate: Option<f64>,
    pub search_success_rate: Option<f64>,
    pub resolve_success_rate: Option<f64>,
    pub active_requests: usize,
    /// closed | open | half-open
    pub circuit: &'static str,
}

#[derive(Debug, Default, Clone)]
struct Record {
    status: Status,
    latency_ms: Option<u128>,
    last_ok: Option<DateTime<Utc>>,
    last_checked: Option<DateTime<Utc>>,
    consecutive_failures: u32,
    last_error: Option<String>,
    search_total: u64,
    search_ok: u64,
    resolve_total: u64,
    resolve_ok: u64,
}

impl Record {
    fn circuit(&self) -> &'static str {
        match self.status {
            Status::Down => "open",
            Status::Degraded if self.consecutive_failures >= OPEN_AFTER.saturating_sub(1) => {
                "half-open"
            }
            _ => "closed",
        }
    }

    fn rate(ok: u64, total: u64) -> Option<f64> {
        (total > 0).then(|| ((ok as f64 / total as f64) * 1000.0).round() / 1000.0)
    }

    fn bump(&mut self, op: Op, ok: bool) {
        match op {
            Op::Search => {
                self.search_total += 1;
                if ok {
                    self.search_ok += 1;
                }
            }
            Op::Resolve => {
                self.resolve_total += 1;
                if ok {
                    self.resolve_ok += 1;
                }
            }
            // Probes only refresh breaker state, not the public success rates.
            Op::Probe => {}
        }
    }
}

/// RAII guard for the in-flight request gauge.
pub struct ActiveGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
pub struct HealthRegistry {
    inner: RwLock<std::collections::HashMap<String, Record>>,
    active: DashMap<String, Arc<AtomicUsize>>,
}

const OPEN_AFTER: u32 = 3;

impl HealthRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    async fn mutate(&self, name: &str, f: impl FnOnce(&mut Record)) {
        let mut guard = self.inner.write().await;
        f(guard.entry(name.to_string()).or_default());
    }

    pub async fn enter(&self, name: &str) -> ActiveGuard {
        // Seed a record so the source appears in /api/sources/status with
        // `active_requests` even before its first outcome is recorded.
        self.inner.write().await.entry(name.to_string()).or_default();
        let counter = self
            .active
            .entry(name.to_string())
            .or_default()
            .value()
            .clone();
        counter.fetch_add(1, Ordering::Relaxed);
        ActiveGuard { counter }
    }

    pub async fn record_op(&self, op: Op, name: &str, latency_ms: u128) {
        self.mutate(name, |rec| {
            rec.status = Status::Healthy;
            rec.latency_ms = Some(latency_ms);
            rec.last_ok = Some(Utc::now());
            rec.last_checked = Some(Utc::now());
            rec.consecutive_failures = 0;
            rec.last_error = None;
            rec.bump(op, true);
        })
        .await;
    }

    pub async fn record_op_failure(&self, op: Op, name: &str, error: &str) {
        let error = error.chars().take(200).collect::<String>();
        self.mutate(name, |rec| {
            rec.consecutive_failures += 1;
            rec.last_checked = Some(Utc::now());
            rec.last_error = Some(error);
            rec.status = if rec.consecutive_failures >= OPEN_AFTER {
                Status::Down
            } else {
                Status::Degraded
            };
            rec.bump(op, false);
        })
        .await;
    }

    /// Legacy shim kept for the background prober path.
    pub async fn record_success(&self, name: &str, latency_ms: u128) {
        self.record_op(Op::Probe, name, latency_ms).await;
    }

    /// Legacy shim kept for the background prober path.
    pub async fn record_failure(&self, name: &str, error: &str) {
        self.record_op_failure(Op::Probe, name, error).await;
    }

    /// Circuit breaker: `down` sources are excluded from fan-out.
    pub async fn is_available(&self, name: &str) -> bool {
        match self.inner.read().await.get(name) {
            Some(rec) => rec.status != Status::Down,
            None => true,
        }
    }

    /// Health bonus for resolver ranking: healthy+fast sources win ties.
    pub async fn score_bonus(&self, name: &str) -> f64 {
        let guard = self.inner.read().await;
        let Some(rec) = guard.get(name) else {
            return 0.0;
        };
        let mut bonus = match rec.status {
            Status::Healthy => 8.0,
            Status::Unknown => 3.0,
            Status::Degraded => -6.0,
            Status::Down => -50.0,
        };
        if let Some(rate) = Record::rate(rec.resolve_ok, rec.resolve_total)
            .or_else(|| Record::rate(rec.search_ok, rec.search_total))
        {
            bonus += (rate - 0.9).min(0.1) * 20.0; // up to ±2 for extreme rates
        }
        bonus
    }

    /// Used by the background prober to force a half-open probe.
    pub async fn reset_for_probe(&self, name: &str) {
        let mut guard = self.inner.write().await;
        if let Some(rec) = guard.get_mut(name) {
            if rec.status == Status::Down {
                rec.status = Status::Degraded; // allow one probe through
                rec.consecutive_failures = OPEN_AFTER.saturating_sub(1);
            }
        } else {
            guard.insert(name.to_string(), Record::default());
        }
    }

    fn snapshot(name: &str, rec: &Record, active: usize) -> HealthSnapshot {
        HealthSnapshot {
            name: name.to_string(),
            status: rec.status,
            latency_ms: rec.latency_ms,
            last_ok: rec.last_ok,
            last_checked: rec.last_checked,
            consecutive_failures: rec.consecutive_failures,
            last_error: rec.last_error.clone(),
            success_rate: Record::rate(
                rec.search_ok + rec.resolve_ok,
                rec.search_total + rec.resolve_total,
            ),
            search_success_rate: Record::rate(rec.search_ok, rec.search_total),
            resolve_success_rate: Record::rate(rec.resolve_ok, rec.resolve_total),
            active_requests: active,
            circuit: rec.circuit(),
        }
    }

    fn active_count(&self, name: &str) -> usize {
        self.active
            .get(name)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub async fn snapshot_all(&self) -> Vec<HealthSnapshot> {
        let guard = self.inner.read().await;
        guard
            .iter()
            .map(|(name, rec)| Self::snapshot(name, rec, self.active_count(name)))
            .collect()
    }

    pub async fn snapshot_missing(&self, names: &[String]) -> Vec<HealthSnapshot> {
        let guard = self.inner.read().await;
        names
            .iter()
            .filter(|n| !guard.contains_key(*n))
            .map(|n| HealthSnapshot {
                name: n.clone(),
                status: Status::Unknown,
                latency_ms: None,
                last_ok: None,
                last_checked: None,
                consecutive_failures: 0,
                last_error: None,
                success_rate: None,
                search_success_rate: None,
                resolve_success_rate: None,
                active_requests: 0,
                circuit: "closed",
            })
            .collect()
    }

    /// Names currently tripped open — used by the AI copilot context builder.
    #[allow(dead_code)]
    pub async fn down_sources(&self) -> Vec<String> {
        self.inner
            .read()
            .await
            .iter()
            .filter(|(_, rec)| rec.status == Status::Down)
            .map(|(name, _)| name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn breaker_trips_and_reports_rates() {
        let reg = HealthRegistry::new();
        reg.record_op(Op::Search, "s", 10).await;
        reg.record_op(Op::Resolve, "s", 12).await;
        reg.record_op_failure(Op::Resolve, "s", "boom").await;

        let snap = reg
            .snapshot_all()
            .await
            .into_iter()
            .find(|s| s.name == "s")
            .unwrap();
        assert_eq!(snap.search_success_rate, Some(1.0));
        assert_eq!(snap.resolve_success_rate, Some(0.5));
        assert_eq!(snap.circuit, "closed");

        reg.record_op_failure(Op::Search, "s", "x").await;
        reg.record_op_failure(Op::Search, "s", "x").await;
        let snap = reg.snapshot_all().await.remove(0);
        assert_eq!(snap.status, Status::Down);
        assert_eq!(snap.circuit, "open");
        assert!(!reg.is_available("s").await);
    }

    #[tokio::test]
    async fn active_gauge_tracks_guard_lifetimes() {
        let reg = HealthRegistry::new();
        {
            let _g = reg.enter("s").await;
            let snap = reg.snapshot_all().await.remove(0);
            assert_eq!(snap.active_requests, 1);
            assert_eq!(snap.status, Status::Unknown);
        }
        let snap = reg.snapshot_all().await.remove(0);
        assert_eq!(snap.active_requests, 0);
    }
}
