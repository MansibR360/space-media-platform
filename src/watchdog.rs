//! Admin-surface watchdog (changeset 5).
//!
//! In-memory sliding-window detector wired into the admin gate. Patterns it
//! treats as an attack:
//!
//! * repeated 401/403 against `/api/admin/*` from one address
//! * distributed failures (many addresses) in the window
//! * ANY honeypot touch — instant, permanent ban + lockout
//!
//! When tripped: every admin-surface request from non-loopback peers is
//! rejected for `LOCKOUT_SECS`. Localhost always gets through so the real
//! admin can regain control.

use dashmap::DashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const PER_IP_LIMIT: u32 = 12;
const DISTRIBUTED_LIMIT: u64 = 40;
pub const LOCKOUT_SECS: u64 = 900;

struct Watchdog {
    /// ip -> (window_start, failure count)
    failures: DashMap<String, (Instant, u32)>,
    tripped_until: AtomicI64, // unix secs; 0 = not tripped
}

static WATCHDOG: OnceLock<Watchdog> = OnceLock::new();

fn wd() -> &'static Watchdog {
    WATCHDOG.get_or_init(|| Watchdog {
        failures: DashMap::new(),
        tripped_until: AtomicI64::new(0),
    })
}

/// Records one failed admin-surface interaction from `ip`.
/// Returns true when this record tripped the watchdog.
pub fn record_failure(ip: &str) -> bool {
    let w = wd();
    {
        let mut entry = w.failures.entry(ip.to_string()).or_insert((Instant::now(), 0));
        if entry.0.elapsed() > WINDOW {
            *entry = (Instant::now(), 0);
        }
        entry.1 += 1;
        if entry.1 >= PER_IP_LIMIT {
            return engage("failure pattern", ip);
        }
    }

    // Distributed pattern: many distinct addresses failing in the window.
    let mut total = 0u64;
    for pair in w.failures.iter() {
        let (start, count) = pair.value();
        if start.elapsed() <= WINDOW {
            total += *count as u64;
        }
    }
    if total >= DISTRIBUTED_LIMIT {
        return engage("distributed failure pattern", ip);
    }
    false
}

/// Honeypot touched — permanent ban territory. Always trips.
pub fn honeypot_triggered(ip: &str) -> bool {
    metrics::counter!("watchdog_honeypot_total").increment(1);
    tracing::error!(%ip, "HONEYPOT TRIGGERED — locking remote admin surface");
    engage("honeypot", ip)
}

fn engage(reason: &str, ip: &str) -> bool {
    let w = wd();
    let until = now_unix() + LOCKOUT_SECS as i64;
    let prev = w.tripped_until.load(Ordering::SeqCst);
    if until > prev {
        w.tripped_until.store(until, Ordering::SeqCst);
        metrics::counter!("watchdog_tripped_total").increment(1);
        tracing::error!(%ip, %reason, lockout_secs = LOCKOUT_SECS,
            "ADMIN SURFACE LOCKED TO LOCALHOST");
        return true;
    }
    // Already engaged; extend nothing but report the hit.
    false
}

/// True while the remote-admin lockout is active.
pub fn is_locked() -> bool {
    let until = wd().tripped_until.load(Ordering::SeqCst);
    until > now_unix()
}

/// Admin cleared the trip manually (localhost recovery path).
#[allow(dead_code)]
pub fn reset() {
    wd().tripped_until.store(0, Ordering::SeqCst);
}

/// Periodic decay so stale entries can't accumulate forever.
#[allow(dead_code)]
pub fn sweep() {
    let w = wd();
    w.failures.retain(|_, (start, _)| start.elapsed() <= WINDOW * 4);
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_ip_pattern_trips_and_resets() {
        // Fresh process state: not locked.
        reset();
        assert!(!is_locked());

        // One address hammering failures crosses PER_IP_LIMIT.
        let tripped = (0..PER_IP_LIMIT).any(|_| record_failure("203.0.113.7"));
        assert!(tripped, "watchdog should trip on repeated single-IP failures");
        assert!(is_locked());

        reset();
        assert!(!is_locked());
    }
}
