//! Routes for the Playback Security Controller.
//!
//! Public:
//!   POST /api/playback/session   — controller entry: risk gate + signed grant
//! Admin (role-gated by the global admin gate):
//!   GET|PATCH /api/admin/security/policy
//!   GET       /api/admin/security/events
//!   GET       /api/admin/security/grants
//!   POST      /api/admin/security/grants/{id}/revoke
//!   POST      /api/admin/security/users/{user_id}/revoke-grants

use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Path as AxPath, Query as AxQuery, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::security::{grants, keys, lockdown, offline, risk};
use crate::state::AppState;

/// True when ANY of the identifiers carries a live admin ban.
pub async fn admin_banned_any(
    pool: &sqlx::PgPool,
    identifiers: &[&str],
) -> bool {
    for id in identifiers {
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM admin_bans \
             WHERE identifier = $1 AND (until IS NULL OR until > now()))",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

pub fn security_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/playback/session", post(authorize_session))
        .route(
            "/api/playback/keys/{file_id}",
            get(hls_key),
        )
        .route(
            "/api/playback/grant/segment",
            post(segment_grant),
        )
        .route(
            "/api/playback/offline-keys",
            post(issue_offline_key).get(my_offline_keys),
        )
        .route(
            "/api/playback/offline-keys/{license_id}/validate",
            get(validate_offline_key),
        )
        .route(
            "/api/playback/offline-keys/{license_id}",
            axum::routing::delete(revoke_offline_key),
        )
        .route(
            "/api/admin/security/policy",
            get(get_policy).patch(patch_policy),
        )
        .route("/api/admin/security/events", get(list_events))
        .route(
            "/api/admin/security/keys/{media_ref}/rotate",
            post(rotate_key),
        )
        .route(
            "/api/admin/security/keys/{media_ref}/revoke",
            post(revoke_keys),
        )
        .route("/api/admin/security/grants", get(list_grants))
        .route(
            "/api/admin/security/grants/{id}/revoke",
            post(revoke_grant),
        )
        .route(
            "/api/admin/security/users/{user_id}/revoke-grants",
            post(revoke_user_grants),
        )
        .route(
            "/api/admin/security/totp/setup",
            post(totp_setup),
        )
        .route(
            "/api/admin/security/totp/enable",
            post(totp_enable),
        )
        .route(
            "/api/admin/security/totp/disable",
            post(totp_disable),
        )
        .route(
            "/api/admin/security/lockdown",
            get(lockdown_status).post(lockdown_toggle),
        )
        // Honeypot: plausible-looking, never legitimate. Any touch bans.
        .route(
            "/api/admin/debug/dump-secrets",
            get(honeypot).post(honeypot),
        )
}

// ------------------------------------------------- media protection (Phase J)

/// Shared-secret gate for private origins: when `ORIGIN_INTERNAL_TOKEN` is
/// configured, the CDN/proxy must forward `X-Origin-Token`. Callers behind the
/// edge never see this requirement because the proxy injects the header.
pub fn ensure_origin(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> ApiResult<()> {
    let Some(expected) = state.cfg.origin_internal_token.as_deref() else {
        return Ok(());
    };
    if expected.is_empty() {
        return Ok(());
    }
    let provided = headers
        .get("x-origin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    use subtle::ConstantTimeEq;
    let ok = expected.len() == provided.len()
        && bool::from(expected.as_bytes().ct_eq(provided.as_bytes()));
    if ok {
        Ok(())
    } else {
        Err(ApiError::forbidden("origin token required"))
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HlsKeyQuery {
    /// optional explicit grant; session cookie auth also accepted
    #[serde(default)]
    pub grant: Option<String>,
}

/// HLS AES-128 content key delivery. Only a valid, unexpired, non-revoked
/// playback grant bound to *this* file (or an authenticated session cookie for
/// the owner) releases bytes. Pairs with `EXT-X-KEY` lines injected at
/// transcode time when HLS_ENCRYPTION=true.
#[utoipa::path(get, path = "/api/playback/keys/{file_id}", tag = "security",
    params(("file_id" = Uuid, Path), HlsKeyQuery),
    responses((status = 200, content_type = "application/octet-stream"), (status = 403), (status = 404))
)]
pub async fn hls_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    AxPath(file_id): AxPath<Uuid>,
    auth: Result<CurrentUser, ApiError>,
    AxQuery(q): AxQuery<HlsKeyQuery>,
) -> ApiResult<axum::response::Response> {
    ensure_origin(&state, &headers)?;

    let presented = super::presented_fingerprint(&headers, None);

    // Authorize: explicit grant bound to this file + client context, or an
    // admin/moderator session.
    let mut authorized = false;
    if let Some(grant) = q.grant.as_deref().filter(|g| !g.is_empty()) {
        // Fingerprint binding: a stolen grant replayed from another TLS/client
        // context is rejected even when the signature is valid.
        if let Ok(verified) =
            grants::verify_with_fingerprint(&state.pool, grant, presented.as_deref()).await
        {
            if verified.media_ref == file_id.to_string() {
                authorized = true;
            }
        }
    }
    if !authorized {
        if let Ok(user) = auth {
            let exists: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM media_files WHERE id = $1",
            )
            .bind(file_id)
            .fetch_optional(&state.pool)
            .await?;
            if exists.is_some() {
                if matches!(
                    user.ctx.user.role,
                    crate::users::models::Role::Admin | crate::users::models::Role::Moderator
                ) {
                    authorized = true;
                } else {
                    // Any authenticated user with a live grant on this file.
                    let granted: Option<(Uuid,)> = sqlx::query_as(
                        "SELECT id FROM playback_grants \
                         WHERE user_id = $1 AND media_ref = $2 AND revoked_at IS NULL \
                           AND expires_at > now() LIMIT 1",
                    )
                    .bind(user.id())
                    .bind(file_id.to_string())
                    .fetch_optional(&state.pool)
                    .await?;
                    authorized = granted.is_some();
                }
            }
        }
    }

    if !authorized {
        metrics::counter!("hls_key_denied_total").increment(1);
        return Err(ApiError::forbidden("no valid playback authorization"));
    }

    let media_ref = file_id.to_string();
    let key_pair =
        match crate::security::keys::active_media_key(&state.pool, &media_ref).await {
            Ok(pair) => pair,
            Err(_) => {
                // First encrypted playback before any rotation: mint v1.
                crate::security::keys::issue_media_key(&state.pool, &media_ref).await?;
                crate::security::keys::active_media_key(&state.pool, &media_ref).await?
            }
        };
    let (_, media_key) = key_pair;
    let content_key = crate::security::keys::hls_content_key(&media_key);

    metrics::counter!("hls_key_served_total").increment(1);
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        content_key.to_vec(),
    )
        .into_response())
}

// ------------------------------------------------- segment-scoped grants

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SegmentGrantRequest {
    /// a valid full-access parent grant
    pub authorization: String,
    /// HLS segment index (or byte-range window id) to scope to
    pub segment: u32,
}

/// Mints a short-lived child ticket that authorizes exactly one segment.
/// The child can never outlive its parent and is useless for any other
/// segment — initial access no longer implies the whole file.
#[utoipa::path(post, path = "/api/playback/grant/segment", tag = "security",
    security(("bearer" = [])),
    request_body = SegmentGrantRequest,
    responses((status = 200), (status = 403), (status = 400))
)]
pub async fn segment_grant(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    auth: CurrentUser,
    Json(req): Json<SegmentGrantRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_origin(&state, &headers)?;
    let presented = super::presented_fingerprint(&headers, None);

    // Parent must be a full-access grant owned by the caller and still live.
    let parent = grants::verify_with_fingerprint(&state.pool, &req.authorization, presented.as_deref())
        .await?;
    if !parent.scope.is_empty() {
        return Err(ApiError::bad_request("parent grant is already segment-scoped"));
    }
    if parent.user_id != auth.id() {
        return Err(ApiError::forbidden("grant belongs to another user"));
    }

    // Child TTL: min(parent remaining, 120 s), clamped 30..=300.
    let remaining: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
        "SELECT expires_at FROM playback_grants WHERE id = $1",
    )
    .bind(parent.grant_id)
    .fetch_optional(&state.pool)
    .await?;
    let parent_exp = remaining.map(|(e,)| e).unwrap_or_else(chrono::Utc::now);
    let child_ttl = std::cmp::min(
        (parent_exp - chrono::Utc::now()).to_std().unwrap_or(std::time::Duration::from_secs(30)),
        std::time::Duration::from_secs(120),
    );

    let ticket = grants::issue_scoped(
        &state.pool,
        auth.id(),
        &parent.media_ref,
        &parent.quality,
        None,
        "",
        "",
        child_ttl,
        &format!("seg:{}", req.segment),
    )
    .await?;

    metrics::counter!("segment_grants_issued_total").increment(1);
    Ok(Json(json!({
        "authorization": ticket.authorization,
        "grant_id": ticket.grant_id,
        "scope": format!("seg:{}", req.segment),
        "expires_at_unix": ticket.expires_at_unix,
        "media_ref": parent.media_ref,
    })))
}

// ------------------------------------------------- offline licenses

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct OfflineKeyRequest {
    /// media/file reference to license for offline playback
    pub media_ref: String,
    /// stable per-device identifier (kept alongside the fingerprint hash)
    #[serde(default)]
    pub device_id: Option<String>,
    /// client-context fingerprint — bound into the license
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// Issues a 24 h offline license: the AES content key is delivered once over
/// TLS, bound to user + media + device. Clients must store it in OS-protected
/// storage (Keychain / DPAPI / Keystore). Idempotent until expiry; revocable;
/// rotation of the underlying media key kills re-delivery after expiry.
#[utoipa::path(post, path = "/api/playback/offline-keys", tag = "security",
    security(("bearer" = [])),
    request_body = OfflineKeyRequest,
    responses((status = 200, body = offline::LicenseIssued), (status = 403))
)]
pub async fn issue_offline_key(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    auth: CurrentUser,
    Json(req): Json<OfflineKeyRequest>,
) -> ApiResult<Json<offline::LicenseIssued>> {
    ensure_origin(&state, &headers)?;
    if req.media_ref.trim().is_empty() {
        return Err(ApiError::bad_request("media_ref is required"));
    }
    crate::cards::ensure_card_access(&state.pool, auth.id()).await?;

    // Same risk gate as streaming: suspicious sessions don't get keys.
    let assessment = risk::assess(&state.pool, auth.id()).await?;
    match assessment.decision {
        risk::Decision::Revoke => {
            risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_revoke",
                "critical",
                assessment.score,
                risk::Decision::Revoke,
                json!({ "endpoint": "offline-keys", "media_ref": req.media_ref }),
            )
            .await;
            return Err(ApiError::forbidden("blocked by risk policy"));
        }
        risk::Decision::Challenge => {
            return Err(ApiError::forbidden(
                "challenge_required: offline keys require a clean risk decision",
            ));
        }
        risk::Decision::Allow => {}
    }

    let fp = super::presented_fingerprint(&headers, req.fingerprint.as_deref());
    let device_hash = match (&fp, &req.device_id) {
        (Some(f), _) => format!("ja4:{}", super::hash_fingerprint(f)),
        (None, Some(d)) => format!("dev:{}", super::hash_fingerprint(d)),
        (None, None) => format!("anon:{}", crate::utils::random_hex(8)),
    };

    let license = offline::issue(
        &state.pool,
        auth.id(),
        req.media_ref.trim(),
        &device_hash,
        state.cfg.offline_license_ttl,
    )
    .await?;

    crate::events::emit(
        &state.pool,
        &state.hub,
        "offline.license.issued",
        json!({
            "license_id": license.license_id,
            "media_ref": license.media_ref,
            "expires_at_unix": license.expires_at_unix,
        }),
        Some(auth.id()),
    )
    .await;

    Ok(Json(license))
}

#[utoipa::path(get, path = "/api/playback/offline-keys", tag = "security",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_offline_keys(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<offline::OfflineLicense>>> {
    Ok(Json(offline::list_for_user(&state.pool, auth.id()).await?))
}

/// License window re-check for offline players: call before each playback
/// session; a revoked or expired license must stop playback immediately.
#[utoipa::path(get, path = "/api/playback/offline-keys/{license_id}/validate", tag = "security",
    security(("bearer" = [])),
    responses((status = 200), (status = 403), (status = 404))
)]
pub async fn validate_offline_key(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(license_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let row: Option<(Uuid, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT user_id, revoked_at, expires_at FROM offline_licenses WHERE id = $1",
        )
        .bind(license_id)
        .fetch_optional(&state.pool)
        .await?;
    let Some((owner, revoked_at, expires_at)) = row else {
        return Err(ApiError::not_found("offline license"));
    };
    if owner != auth.id() {
        return Err(ApiError::forbidden("not your license"));
    }
    if revoked_at.is_some() {
        return Err(ApiError::forbidden("offline license revoked"));
    }
    offline::ensure_not_expired(expires_at)?;
    Ok(Json(json!({
        "valid": true,
        "expires_at_unix": expires_at.timestamp(),
        "remaining_secs": (expires_at - chrono::Utc::now()).num_seconds().max(0),
    })))
}

/// Revoke an offline license (owner or staff). Takes effect immediately.
#[utoipa::path(delete, path = "/api/playback/offline-keys/{license_id}", tag = "security",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn revoke_offline_key(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(license_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    // Owners revoke their own; admins may revoke anyone's.
    let owner: Option<(Uuid,)> =
        sqlx::query_as("SELECT user_id FROM offline_licenses WHERE id = $1")
            .bind(license_id)
            .fetch_optional(&state.pool)
            .await?
            .filter(|(uid,)| *uid == auth.id());
    if owner.is_none()
        && !matches!(
            auth.ctx.user.role,
            crate::users::models::Role::Admin | crate::users::models::Role::Moderator
        )
    {
        return Err(ApiError::not_found("offline license"));
    }
    if !offline::revoke_by_id(&state.pool, license_id).await? {
        return Err(ApiError::not_found("active offline license"));
    }
    tracing::info!(%license_id, "offline license revoked");
    Ok(Json(json!({ "revoked": true })))
}

// ------------------------------------------------- admin fortress

/// Generates (but does not enable) a TOTP secret for the calling admin.
#[utoipa::path(post, path = "/api/admin/security/totp/setup", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn totp_setup(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    let account = &auth.0.ctx.user.username;
    let (secret, uri) = crate::auth::totp::generate(account);
    sqlx::query("UPDATE users SET totp_secret = $2, totp_enabled = FALSE WHERE id = $1")
        .bind(auth.0.id())
        .bind(&secret)
        .execute(&state.pool)
        .await?;
    crate::audit::log_simple(&state.pool, Some(auth.0.id()), "admin.totp.setup", account).await;
    Ok(Json(json!({ "secret": secret, "otpauth_uri": uri })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TotpCodeRequest {
    pub code: String,
}

/// Verifies one code and switches TOTP on.
#[utoipa::path(post, path = "/api/admin/security/totp/enable", tag = "admin",
    security(("bearer" = [])),
    request_body = TotpCodeRequest,
    responses((status = 200), (status = 400))
)]
pub async fn totp_enable(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    Json(req): Json<TotpCodeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let secret: Option<String> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = $1")
            .bind(auth.0.id())
            .fetch_one(&state.pool)
            .await?;
    let Some(secret) = secret else {
        return Err(ApiError::bad_request("run setup first"));
    };
    let code = req.code.trim().parse::<u32>().map_err(|_| ApiError::bad_request("code must be 6 digits"))?;
    if !crate::auth::totp::verify(&secret, code, chrono::Utc::now().timestamp()) {
        return Err(ApiError::forbidden("invalid code"));
    }
    sqlx::query("UPDATE users SET totp_enabled = TRUE WHERE id = $1")
        .bind(auth.0.id())
        .execute(&state.pool)
        .await?;
    crate::audit::log_simple(&state.pool, Some(auth.0.id()), "admin.totp.enabled", "").await;
    Ok(Json(json!({ "totp_enabled": true })))
}

#[utoipa::path(post, path = "/api/admin/security/totp/disable", tag = "admin",
    security(("bearer" = [])),
    request_body = TotpCodeRequest,
    responses((status = 200), (status = 400))
)]
pub async fn totp_disable(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    Json(req): Json<TotpCodeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let secret: Option<String> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = $1")
            .bind(auth.0.id())
            .fetch_one(&state.pool)
            .await?;
    let code = req.code.trim().parse::<u32>().map_err(|_| ApiError::bad_request("code must be 6 digits"))?;
    match secret.as_deref() {
        Some(s)
            if sqlx::query_scalar::<_, bool>(
                "SELECT COALESCE(totp_enabled, FALSE) FROM users WHERE id = $1",
            )
            .bind(auth.0.id())
            .fetch_one(&state.pool)
            .await
            .unwrap_or(false)
            && crate::auth::totp::verify(s, code, chrono::Utc::now().timestamp()) => {}
        _ => return Err(ApiError::forbidden("invalid code")),
    }
    sqlx::query("UPDATE users SET totp_enabled = FALSE, totp_secret = NULL WHERE id = $1")
        .bind(auth.0.id())
        .execute(&state.pool)
        .await?;
    crate::audit::log_simple(&state.pool, Some(auth.0.id()), "admin.totp.disabled", "").await;
    Ok(Json(json!({ "totp_enabled": false })))
}

// ---------------- honeypot ----------------

/// Looks like a juicy debug endpoint; touching it at all is an attack signal.
/// The caller is permanently banned from the admin surface (IP hash + username
/// when authenticated) and the watchdog locks remote admin to localhost.
/// Shared trip logic — called from the admin gate (pre-auth intercept, so
/// remote probers get banned too) and from the route handler.
pub async fn honeypot_trip(state: &AppState, ip: &str) {
    let iph = crate::admin_gate::ip_hash(ip);
    let _ = sqlx::query(
        "INSERT INTO admin_bans (id, identifier, kind, reason) \
         VALUES ($1, $2, 'honeypot', 'touched admin honeypot') \
         ON CONFLICT (identifier, kind) DO NOTHING",
    )
    .bind(crate::utils::new_id())
    .bind(iph)
    .execute(&state.pool);
    crate::watchdog::honeypot_triggered(ip);
}

#[utoipa::path(get, path = "/api/admin/debug/dump-secrets", tag = "admin",
    responses((status = 403))
)]
pub async fn honeypot(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    auth: Option<crate::auth::middleware::CurrentUser>,
) -> ApiResult<Json<serde_json::Value>> {
    let ip = peer.ip().to_string();
    // Must be awaited: without this the future is dropped and the ban is never
    // written, so the honeypot silently does nothing.
    honeypot_trip(&state, &ip).await;

    if let Some(a) = auth {
        // Also ban the account itself and wipe every session it holds.
        let uid = format!("user:{}", a.id());
        let _ = sqlx::query(
            "INSERT INTO admin_bans (id, identifier, kind, reason) \
             VALUES ($1, $2, 'honeypot', 'authenticated honeypot touch') \
             ON CONFLICT (identifier, kind) DO NOTHING",
        )
        .bind(crate::utils::new_id())
        .bind(crate::utils::sha256_hex(&uid)[..32].to_string())
        .execute(&state.pool)
        .await;
        crate::audit::log(
            &state.pool,
            Some(a.id()),
            "security.honeypot_triggered",
            &ip,
            json!({ "user": a.ctx.user.username }),
            &ip,
        )
        .await;
        let _ = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(a.id())
            .execute(&state.pool)
            .await;
    }
    Err(ApiError::forbidden("banned from the admin surface"))
}

// ---------------- site lockdown ----------------

#[utoipa::path(get, path = "/api/admin/security/lockdown", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn lockdown_status(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({
        "enabled": lockdown::is_enabled(&state).await,
        "message": lockdown::message(&state).await,
        "watchdog_locked": crate::watchdog::is_locked(),
    })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LockdownToggle {
    pub enabled: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// Kill switch: blocks all public routes (503) while admin + health stay up.
#[utoipa::path(post, path = "/api/admin/security/lockdown", tag = "admin",
    security(("bearer" = [])),
    request_body = LockdownToggle,
    responses((status = 200))
)]
pub async fn lockdown_toggle(
    State(state): State<Arc<AppState>>,
    auth: AdminUser,
    headers: axum::http::HeaderMap,
    ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<LockdownToggle>,
) -> ApiResult<Json<serde_json::Value>> {
    lockdown::set_enabled(
        &state.pool,
        req.enabled,
        req.message.as_deref().unwrap_or("service temporarily unavailable"),
        Some(auth.0.id()),
    )
    .await?;
    crate::audit::log(
        &state.pool,
        Some(auth.0.id()),
        if req.enabled { "security.lockdown_on" } else { "security.lockdown_off" },
        "",
        json!({ "message": req.message }),
        &peer.ip().to_string(),
    )
    .await;
    let _ = headers;
    Ok(Json(json!({
        "enabled": req.enabled,
        "watchdog_locked": crate::watchdog::is_locked(),
    })))
}

#[utoipa::path(post, path = "/api/admin/security/keys/{media_ref}/rotate", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = keys::MediaKeyRecord))
)]
pub async fn rotate_key(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(media_ref): AxPath<String>,
) -> ApiResult<Json<keys::MediaKeyRecord>> {
    Ok(Json(keys::issue_media_key(&state.pool, &media_ref).await?))
}

#[utoipa::path(post, path = "/api/admin/security/keys/{media_ref}/revoke", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn revoke_keys(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(media_ref): AxPath<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let count = keys::revoke_media_keys(&state.pool, &media_ref).await?;
    if count == 0 {
        return Err(ApiError::not_found("media key"));
    }
    tracing::warn!(%media_ref, count, "media keys revoked");
    Ok(Json(json!({ "revoked": count })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AuthorizeRequest {
    /// Opaque reference of what is being played (resolver token, media uuid,
    /// or any stable string). Bound into the signed grant.
    pub media_ref: String,
    #[serde(default)]
    pub quality: Option<String>,
    /// Clients may accept a challenge once; the acknowledgement is logged.
    #[serde(default)]
    pub acknowledge_challenge: bool,
    /// Client-context fingerprint (JA4 hash or stable device hash). Bound
    /// into the grant; later fetches must present the same context.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// Playback Security Controller entry point: authenticates the user, runs the
/// risk layer (when enabled), then issues a short-lived signed playback grant.
#[utoipa::path(post, path = "/api/playback/session", tag = "security",
    security(("bearer" = [])),
    request_body = AuthorizeRequest,
    responses((status = 200), (status = 403))
)]
pub async fn authorize_session(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    auth: CurrentUser,
    Json(req): Json<AuthorizeRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.media_ref.trim().is_empty() {
        return Err(ApiError::bad_request("media_ref is required"));
    }
    crate::cards::ensure_card_access(&state.pool, auth.id()).await?;

    let assessment = risk::assess(&state.pool, auth.id()).await?;
    match assessment.decision {
        risk::Decision::Revoke => {
            risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_revoke",
                "critical",
                assessment.score,
                risk::Decision::Revoke,
                json!({ "media_ref": req.media_ref, "signals": assessment.signals }),
            )
            .await;
            metrics::counter!("risk_decisions_total", "decision" => "revoke").increment(1);
            state.hub.publish(
                &crate::chat::hub::user_room(&auth.id()),
                json!({
                    "type": "security.flag",
                    "data": { "decision": "revoke", "score": assessment.score }
                })
                .to_string(),
            );
            return Err(ApiError::forbidden(
                "playback blocked by risk policy (revoked)",
            ));
        }
        risk::Decision::Challenge if !req.acknowledge_challenge => {
            risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_challenge",
                "warning",
                assessment.score,
                risk::Decision::Challenge,
                json!({ "media_ref": req.media_ref, "signals": assessment.signals }),
            )
            .await;
            metrics::counter!("risk_decisions_total", "decision" => "challenge").increment(1);
            return Err(ApiError::forbidden(
                "challenge_required: re-send with acknowledge_challenge=true after verifying the device",
            ));
        }
        risk::Decision::Challenge => {
            risk::record_event(
                &state.pool,
                Some(auth.id()),
                "risk_challenge_acknowledged",
                "info",
                assessment.score,
                risk::Decision::Allow,
                json!({ "media_ref": req.media_ref }),
            )
            .await;
        }
        risk::Decision::Allow => {}
    }

    // Bind the grant to the presenting client context (JA4 from the edge,
    // client fingerprint, or a coarse per-session value when none supplied).
    let device_hash = super::presented_fingerprint(&headers, req.fingerprint.as_deref())
        .map(|fp| format!("ja4:{}", super::hash_fingerprint(&fp)))
        .unwrap_or_else(|| format!("anon:{}", crate::utils::random_hex(8)));
    let ticket = grants::issue(
        &state.pool,
        auth.id(),
        req.media_ref.trim(),
        req.quality.as_deref().unwrap_or(""),
        None,
        &device_hash,
        "",
        state.cfg.playback_grant_ttl,
    )
    .await?;

    metrics::counter!("risk_decisions_total", "decision" => "allow").increment(1);
    crate::events::emit(
        &state.pool,
        &state.hub,
        "playback.authorized",
        serde_json::json!({
            "grant_id": ticket.grant_id,
            "media_ref": req.media_ref,
            "risk_score": assessment.score,
        }),
        Some(auth.id()),
    )
    .await;
    Ok(Json(json!({
        "grant_id": ticket.grant_id,
        "authorization": ticket.authorization,
        "expires_at_unix": ticket.expires_at_unix,
        "ttl_secs": state.cfg.playback_grant_ttl.as_secs(),
        "risk": {
            "enabled": assessment.enabled,
            "score": assessment.score,
            "decision": assessment.decision,
        },
    })))
}

/// Cheap fallback when no client context was presented.
///
/// Deliberately coarse: a random per-session value.

// ---------------------------------------------------------------- admin

#[utoipa::path(get, path = "/api/admin/security/policy", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = risk::RiskPolicy))
)]
pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
) -> ApiResult<Json<risk::RiskPolicy>> {
    Ok(Json(risk::load_policy(&state.pool).await?))
}

#[utoipa::path(patch, path = "/api/admin/security/policy", tag = "admin",
    security(("bearer" = [])),
    request_body = risk::PolicyPatch,
    responses((status = 200, body = risk::RiskPolicy), (status = 400))
)]
pub async fn patch_policy(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    Json(patch): Json<risk::PolicyPatch>,
) -> ApiResult<Json<risk::RiskPolicy>> {
    Ok(Json(risk::update_policy(&state.pool, &patch).await?))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct SecurityEvent {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub signal_type: String,
    pub severity: String,
    pub score: f64,
    pub decision: String,
    pub details: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct EventQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    100
}

#[utoipa::path(get, path = "/api/admin/security/events", tag = "admin",
    security(("bearer" = [])),
    params(EventQuery),
    responses((status = 200))
)]
pub async fn list_events(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<EventQuery>,
) -> ApiResult<Json<Vec<SecurityEvent>>> {
    let rows = sqlx::query_as::<_, SecurityEvent>(
        "SELECT * FROM security_events ORDER BY created_at DESC LIMIT $1",
    )
    .bind(q.limit.clamp(1, 500))
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct GrantQuery {
    /// only non-revoked, unexpired grants
    #[serde(default)]
    pub active_only: bool,
}

#[utoipa::path(get, path = "/api/admin/security/grants", tag = "admin",
    security(("bearer" = [])),
    params(GrantQuery),
    responses((status = 200))
)]
pub async fn list_grants(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(q): AxQuery<GrantQuery>,
) -> ApiResult<Json<Vec<grants::PlaybackGrant>>> {
    let sql = if q.active_only {
        "SELECT * FROM playback_grants WHERE revoked_at IS NULL AND expires_at > now() \
         ORDER BY created_at DESC LIMIT 200"
    } else {
        "SELECT * FROM playback_grants ORDER BY created_at DESC LIMIT 200"
    };
    let rows = sqlx::query_as::<_, grants::PlaybackGrant>(sql)
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/security/grants/{id}/revoke", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn revoke_grant(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let revoked = grants::revoke(&state.pool, id).await?;
    if !revoked {
        return Err(ApiError::not_found("active playback grant"));
    }
    tracing::info!(grant = %id, "playback grant revoked by admin");
    Ok(Json(json!({ "revoked": true })))
}

#[utoipa::path(post, path = "/api/admin/security/users/{user_id}/revoke-grants", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn revoke_user_grants(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(user_id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let count = grants::revoke_for_user(&state.pool, user_id).await?;
    tracing::info!(user = %user_id, count, "user playback grants revoked");
    Ok(Json(json!({ "revoked": count })))
}





