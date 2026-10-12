//! Admin surface hardening — the diamond gets the vault.
//!
//! Layers, evaluated before anything else on `/api/admin/*` and `/admin.html`:
//!
//! 1. **Watchdog lockout** — when tripped, non-loopback peers are rejected
//!    outright for 15 min. Localhost always passes (recovery path).
//! 2. **Bans table** — honeypot victims and escalated lockouts are permanent
//!    or timed; checked by IP-hash and by identifier once authenticated.
//! 3. **Loopback policy** — admin surface is localhost-only unless
//!    `ADMIN_LOCAL_ONLY=false`.
//! 4. Role checks remain in the auth extractors downstream.
//! 5. **Tokenless hammering** — every 401 (no/invalid login token) on
//!    `/api/admin/*` feeds the watchdog; sustained calls lock the surface.

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

pub fn ip_hash(ip: &str) -> String {
    crate::utils::sha256_hex(ip)[..32].to_string()
}

pub async fn enforce(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let local = is_loopback(peer.ip());

    // ---- HONEYPOT: intercept before any other policy. Remote or local,
    // authenticated or not — touching this path is hostile by definition. ----
    if path == "/api/admin/debug/dump-secrets" {
        let ip = peer.ip().to_string();
        crate::security::routes::honeypot_trip(&state, &ip).await;
        return ApiError::forbidden("banned from the admin surface").into_response();
    }

    // ---- console page gating (server-authoritative) ----
    // The raw asset must never be served; only the gated /console path is.
    if path == "/admin-console.html" || path == "/console.html" {
        metrics::counter!("admin_console_probe_total").increment(1);
        return (
            axum::http::StatusCode::NOT_FOUND,
            "not found",
        )
            .into_response();
    }
    if path == "/console" {
        // 1. watchdog lockout + bans apply before anything else
        let iph = ip_hash(&peer.ip().to_string());
        if !local && crate::watchdog::is_locked() {
            metrics::counter!("admin_watchdog_blocked_total").increment(1);
            return ApiError::forbidden("admin surface locked to localhost").into_response();
        }
        if banned(&state, &iph).await {
            return ApiError::forbidden("banned from the admin surface").into_response();
        }
        // 2. authenticated admin/moderator session required
        let token = extract_session_token(&request);
        let authed = match token {
            Some(t) => state
                .auth
                .resolve_session(&t)
                .await
                .ok()
                .filter(|ctx| {
                    matches!(
                        ctx.user.role,
                        crate::users::models::Role::Admin | crate::users::models::Role::Moderator
                    )
                })
                .is_some(),
            None => false,
        };
        if !authed {
            metrics::counter!("admin_console_unauthed_total").increment(1);
            return axum::response::Redirect::temporary("/admin.html").into_response();
        }
        // 3. loopback policy for the console itself
        if state.cfg.admin_local_only && !local {
            metrics::counter!("admin_remote_blocked_total").increment(1);
            return ApiError::forbidden("admin console is localhost-only").into_response();
        }
        return next.run(request).await;
    }

    // ---- site-wide lockdown ----
    if path != "/admin.html" && !path.starts_with("/api/health") && !path.starts_with("/health") && !path.starts_with("/api/auth") {
        if crate::security::lockdown::is_enabled(&state).await {
            metrics::counter!("lockdown_rejected_total").increment(1);
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "code": "locked_down",
                    "message": crate::security::lockdown::message(&state).await,
                })),
            )
                .into_response();
        }
    }

    let admin_surface = path.starts_with("/api/admin") || path == "/docs/admin";

    // ---- watchdog lockout (localhost exempt) ----
    if admin_surface && !local && crate::watchdog::is_locked() {
        metrics::counter!("admin_watchdog_blocked_total").increment(1);
        return ApiError::forbidden(
            "admin surface locked to localhost — security event in progress",
        )
        .into_response();
    }

    // ---- bans ----
    if admin_surface {
        let iph = ip_hash(&peer.ip().to_string());
        if banned(&state, &iph).await {
            metrics::counter!("admin_ban_blocked_total").increment(1);
            return ApiError::forbidden("banned from the admin surface").into_response();
        }
    }

    if !admin_surface {
        return next.run(request).await;
    }

    let iph = ip_hash(&peer.ip().to_string());

    // ---- loopback policy ----
    if state.cfg.admin_local_only {
        // Behind a proxy we additionally refuse forwarded headers claiming a
        // remote client — loopback means the process itself, not a proxy hop.
        if state.cfg.trust_proxy && request.headers().contains_key("x-forwarded-for") {
            metrics::counter!("admin_remote_blocked_total").increment(1);
            crate::watchdog::record_failure(&iph);
            return ApiError::forbidden("admin console is localhost-only").into_response();
        }

        if !local {
            tracing::warn!(%peer, %path, "blocked remote admin access");
            metrics::counter!("admin_remote_blocked_total").increment(1);
            crate::watchdog::record_failure(&iph);
            return ApiError::forbidden("admin console is localhost-only").into_response();
        }
    }

    let response = next.run(request).await;

    // ---- tokenless hammering feeds the watchdog ----
    // 401 here means no valid session/API key. Sustained calls without a
    // proper login token are an attack pattern: after PER_IP_LIMIT failures
    // in the window the whole admin surface locks to localhost.
    if response.status() == axum::http::StatusCode::UNAUTHORIZED {
        metrics::counter!("admin_auth_failures_total").increment(1);
        crate::watchdog::record_failure(&iph);
    }

    response
}

/// Session token from `Authorization: Bearer …` or the session cookie.
fn extract_session_token(request: &Request) -> Option<String> {
    use axum::http::header;
    if let Some(v) = request.headers().get(header::AUTHORIZATION) {
        if let Ok(raw) = v.to_str() {
            if let Some(tok) = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer ")) {
                let tok = tok.trim();
                if !tok.is_empty() {
                    return Some(tok.to_string());
                }
            }
        }
    }
    let cookie = request.headers().get(header::COOKIE)?.to_str().ok()?;
    crate::utils::parse_cookies(cookie)
        .into_iter()
        .find(|(k, v)| k == crate::auth::middleware::SESSION_COOKIE && !v.is_empty())
        .map(|(_, v)| v)
}

/// Bans check: identifier may be an IP hash, a username hash, or both.
pub async fn banned(state: &AppState, identifier: &str) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM admin_bans
             WHERE identifier = $1
               AND (until IS NULL OR until > now())
         )",
    )
    .bind(identifier)
    .fetch_one(&state.pool)
    .await
    {
        Ok(hit) => hit,
        Err(_) => false,
    }
}

