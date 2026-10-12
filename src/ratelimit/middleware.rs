use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct LimitSpec {
    pub limit: u32,
    pub window: Duration,
}

enum Store {
    Memory(Mutex<HashMap<String, (u32, Instant)>>),
    Redis(deadpool_redis::Pool),
}

impl Clone for Store {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(_) => Self::Memory(Mutex::new(HashMap::new())),
            Self::Redis(pool) => Self::Redis(pool.clone()),
        }
    }
}

/// Fixed-window rate limiter with a Redis backend and in-memory fallback.
#[derive(Clone)]
pub struct RateLimiter {
    enabled: bool,
    store: Option<Store>,
}

impl RateLimiter {
    pub async fn new(cfg: &crate::config::Config) -> Self {
        let store = if cfg.redis_url.is_some() {
            match deadpool_redis::Config::from_url(cfg.redis_url.clone().unwrap())
                .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            {
                Ok(pool) => Some(Store::Redis(pool)),
                Err(err) => {
                    tracing::warn!(error = %err, "rate limiter redis pool unavailable; using memory");
                    None
                }
            }
        } else {
            None
        };

        Self {
            enabled: cfg.rate_limit_enabled,
            store,
        }
    }

    fn memory_hit(&self, key: &str, spec: LimitSpec) -> (bool, u32) {
        // Returns (allowed, remaining)
        static FALLBACK: std::sync::OnceLock<Mutex<HashMap<String, (u32, Instant)>>> =
            std::sync::OnceLock::new();
        let map = FALLBACK.get_or_init(|| Mutex::new(HashMap::new()));
        let mut guard = map.lock().expect("rate limit lock");

        let now = Instant::now();
        let entry = guard.entry(key.to_string());
        match entry {
            std::collections::hash_map::Entry::Occupied(mut occupied) => {
                let (count, started_at) = occupied.get_mut();
                if now.duration_since(*started_at) > spec.window {
                    *count = 1;
                    *started_at = now;
                    return (true, spec.limit.saturating_sub(1));
                }
                *count += 1;
                ((*count <= spec.limit), spec.limit.saturating_sub(*count))
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                vacant.insert((1, now));
                (true, spec.limit.saturating_sub(1))
            }
        }
    }

    async fn hit(&self, key: &str, spec: LimitSpec) -> (bool, u32) {
        if !self.enabled {
            return (true, spec.limit);
        }

        match &self.store {
            Some(Store::Redis(pool)) => {
                let bucket = chrono::Utc::now().timestamp() / spec.window.as_secs().max(1) as i64;
                let full_key = format!("rl:{key}:{bucket}");
                match redis_incr_with_ttl(pool, &full_key, spec.window).await {
                    Ok(count) => (
                        count <= spec.limit,
                        spec.limit.saturating_sub(count.min(spec.limit)),
                    ),
                    Err(err) => {
                        tracing::debug!(error = %err, "redis rate limit failed; falling back");
                        self.memory_hit(key, spec)
                    }
                }
            }
            _ => self.memory_hit(key, spec),
        }
    }
}

async fn redis_incr_with_ttl(
    pool: &deadpool_redis::Pool,
    key: &str,
    ttl: Duration,
) -> Result<u32, deadpool_redis::PoolError> {
    use deadpool_redis::redis::AsyncCommands;
    let mut conn = pool.get().await?;
    let count: i64 = conn.incr(key, 1i64).await?;
    if count == 1 {
        let _: () = conn.expire(key, ttl.as_secs().max(1) as i64).await?;
    }
    Ok(count.clamp(0, u32::MAX as i64) as u32)
}

fn is_auth_path(path: &str) -> bool {
    path.starts_with("/api/auth/login")
        || path.starts_with("/api/auth/register")
        || path.starts_with("/api/auth/refresh")
        || path.starts_with("/api/auth/password")
}

/// Who this request is billed to.
///
/// Bucketing purely by IP has two problems for a media app: every viewer
/// behind one address shares a single budget, and the app's own front page -
/// which loads a rail per source, then polls them - can spend it by itself.
///
/// When a request carries a session, the bucket follows the session instead,
/// so one busy viewer cannot starve the others. The token is hashed and never
/// used for authorisation here; this only decides which counter to increment.
fn rate_subject(request: &Request<axum::body::Body>, ip: &str) -> String {
    let bearer = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    match bearer {
        Some(token) => {
            let digest = crate::utils::sha256_hex(token);
            format!("session:{}", &digest[..16])
        }
        None => format!("ip:{ip}"),
    }
}

/// The server's own calls to the built-in site plugins.
///
/// Native sources are served over loopback so the rest of the pipeline can
/// treat them like any other HTTP source. That makes the health prober a
/// client of this server: it probes every source once a minute, which is far
/// more than the per-minute budget, so the probes 429, get recorded as
/// failures, and the circuit breaker drops sources that are perfectly
/// healthy - while also spending the budget the real user's browser shares,
/// since both arrive from 127.0.0.1.
///
/// Only loopback callers are exempt: from anywhere else these routes reach
/// out to third-party sites and stay rate limited.
fn is_self_call(path: &str, ip: &str) -> bool {
    path.starts_with("/api/native-plugins")
        && (ip == "127.0.0.1" || ip == "::1" || ip == "localhost")
}

pub async fn enforce(
    State(state): State<Arc<AppState>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if !request.uri().path().starts_with("/api") {
        return next.run(request).await;
    }

    let ip = client_ip(&state, &request);
    let path = request.uri().path();

    if is_self_call(path, &ip) {
        return next.run(request).await;
    }

    let subject = rate_subject(&request, &ip);

    let specs: Vec<(String, LimitSpec)> = if is_auth_path(path) {
        vec![
            (
                // sign-in attempts stay pinned to the address: a caller
                // guessing passwords has no session yet
                format!("auth:{ip}"),
                LimitSpec {
                    limit: state.cfg.rate_auth_per_min,
                    window: Duration::from_secs(60),
                },
            ),
            (
                format!("global:{subject}"),
                LimitSpec {
                    limit: state.cfg.rate_global_per_min,
                    window: Duration::from_secs(60),
                },
            ),
        ]
    } else {
        vec![(
            format!("global:{subject}"),
            LimitSpec {
                limit: state.cfg.rate_global_per_min,
                window: Duration::from_secs(60),
            },
        )]
    };

    for (key, spec) in specs {
        let (allowed, remaining) = state.limiter.hit(&key, spec).await;
        if !allowed {
            metrics::counter!("rate_limit_rejected_total").increment(1);
            return ApiError::TooManyRequests { retry_after: 60 }.into_response();
        }
        let _ = remaining;
    }

    next.run(request).await
}

fn client_ip(state: &AppState, request: &Request<axum::body::Body>) -> String {
    if state.cfg.trust_proxy {
        if let Some(forwarded) = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            return forwarded.trim().to_string();
        }
    }
    request
        .extensions()
        .get::<std::net::SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_plugin_probes_are_exempt() {
        assert!(is_self_call("/api/native-plugins/0xdb/items", "127.0.0.1"));
        assert!(is_self_call("/api/native-plugins/0xdb/search", "::1"));
    }

    #[test]
    fn sessions_get_their_own_bucket() {
        let with_token = Request::builder()
            .uri("/api/media")
            .header(axum::http::header::AUTHORIZATION, "Bearer abc123")
            .body(axum::body::Body::empty())
            .unwrap();
        let anonymous = Request::builder()
            .uri("/api/media")
            .body(axum::body::Body::empty())
            .unwrap();

        let a = rate_subject(&with_token, "10.0.0.1");
        let b = rate_subject(&anonymous, "10.0.0.1");
        assert!(a.starts_with("session:"), "signed-in requests bill to the session");
        assert!(b.starts_with("ip:"), "anonymous requests bill to the address");
        assert_ne!(a, b, "one viewer must not spend another's budget");
    }

    #[test]
    fn remote_callers_and_other_paths_are_not() {
        // same path, different origin: still limited
        assert!(!is_self_call("/api/native-plugins/0xdb/items", "203.0.113.9"));
        // loopback, but not a self call
        assert!(!is_self_call("/api/search/all", "127.0.0.1"));
    }
}
