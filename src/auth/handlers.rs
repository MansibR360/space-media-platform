use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::auth::password;
use crate::auth::service::{SessionInfo, SessionTokens};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::users::models::{ChangePasswordRequest, PrivateProfile};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LoginRequest {
    pub identifier: String,
    pub password: String,
    /// Required when the admin account has TOTP enabled.
    #[serde(default)]
    pub totp_code: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AuthResponse {
    pub user: PrivateProfile,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SessionIdPath {
    pub session_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

/// Always returns 200 regardless of account existence (no user enumeration).
#[utoipa::path(post, path = "/api/auth/password/forgot", tag = "auth",
    request_body = ForgotPasswordRequest,
    responses((status = 200, body = MessageResponse))
)]
pub async fn forgot_password(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ForgotPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    let ip = peer_ip(&state, &headers, addr);
    let token = state.auth.request_password_reset(&req.email, &ip).await?;

    if let Some(token) = token {
        // Delivery adapter hook: plug an SMTP/webhook provider here.
        tracing::info!(
            email_domain = req.email.split('@').nth(1).unwrap_or("?"),
            reset_link = format!(
                "{}/{}",
                state.cfg.reset_link_base.trim_end_matches('/'),
                token
            ),
            "password reset requested"
        );
        metrics::counter!("password_reset_requested_total").increment(1);
    }

    Ok(Json(MessageResponse {
        message: "if that address exists, a reset link has been sent".into(),
    }))
}

#[utoipa::path(post, path = "/api/auth/password/reset", tag = "auth",
    request_body = ResetPasswordRequest,
    responses((status = 200, body = MessageResponse), (status = 400))
)]
pub async fn reset_password(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<Json<MessageResponse>> {
    if req.token.trim().is_empty() {
        return Err(ApiError::bad_request("token required"));
    }
    let user = state
        .auth
        .confirm_password_reset(req.token.trim(), &req.new_password)
        .await?;
    tracing::info!(user_id = %user.id, "password reset completed; all sessions revoked");
    metrics::counter!("password_reset_completed_total").increment(1);
    Ok(Json(MessageResponse {
        message: "password updated, please sign in again".into(),
    }))
}

// ---------------- email verification ----------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EmailCodeResponse {
    pub sent: bool,
    /// Only populated when EMAIL_DEV_ECHO=true (localhost convenience).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_code: Option<String>,
    pub already_verified: bool,
}

#[utoipa::path(post, path = "/api/auth/email/request-code", tag = "auth",
    security(("bearer" = [])),
    responses((status = 200, body = EmailCodeResponse))
)]
pub async fn request_email_code(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<EmailCodeResponse>> {
    if auth.user().email_verified {
        return Ok(Json(EmailCodeResponse {
            sent: false,
            dev_code: None,
            already_verified: true,
        }));
    }
    let code = state.auth.request_email_code(auth.id()).await?;
    // Delivery adapter hook: wire SMTP / SES / webhook here. Dev echo only.
    let dev_code = state.cfg.email_dev_echo.then_some(code.clone());
    tracing::info!(user_id = %auth.id(), "email verification code generated");
    Ok(Json(EmailCodeResponse {
        sent: true,
        dev_code,
        already_verified: false,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    pub code: String,
}

#[utoipa::path(post, path = "/api/auth/email/verify", tag = "auth",
    security(("bearer" = [])),
    request_body = VerifyEmailRequest,
    responses((status = 200, body = MessageResponse), (status = 400))
)]
pub async fn verify_email(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<VerifyEmailRequest>,
) -> ApiResult<Json<MessageResponse>> {
    state.auth.verify_email_code(auth.id(), &req.code).await?;
    metrics::counter!("email_verified_total").increment(1);
    Ok(Json(MessageResponse {
        message: "email verified".into(),
    }))
}

fn peer_ip(state: &AppState, headers: &HeaderMap, addr: SocketAddr) -> String {
    if state.cfg.trust_proxy {
        if let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            return first.trim().to_string();
        }
    }
    addr.ip().to_string()
}

fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(256)
        .collect()
}

fn auth_response(user: &crate::users::models::User, tokens: &SessionTokens) -> AuthResponse {
    AuthResponse {
        user: user.private(),
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        token_type: "Bearer",
        expires_in: tokens.access_ttl_secs,
        expires_at: tokens.expires_at,
    }
}

fn set_cookie(state: &AppState, response: &mut Response, token: &str, max_age: i64) {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let value = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        crate::auth::middleware::SESSION_COOKIE,
        token,
        max_age,
        secure
    );
    if let Ok(header) = HeaderValue::from_str(&value) {
        response.headers_mut().append(SET_COOKIE, header);
    }
}

fn clear_cookie(state: &AppState, response: &mut Response) {
    let secure = if state.cfg.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let value = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        crate::auth::middleware::SESSION_COOKIE,
        secure
    );
    if let Ok(header) = HeaderValue::from_str(&value) {
        response.headers_mut().append(SET_COOKIE, header);
    }
}

fn cookie_from_headers(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    crate::utils::parse_cookies(raw)
        .into_iter()
        .find(|(name, _)| *name == state.cfg.session_cookie)
        .map(|(_, value)| value)
}

#[utoipa::path(post, path = "/api/auth/register", tag = "auth",
    request_body = RegisterRequest,
    responses((status = 201, body = AuthResponse), (status = 400, description = "validation failed"), (status = 409, description = "conflict"))
)]
pub async fn register(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<axum::response::Response> {
    let ip = peer_ip(&state, &headers, addr);
    let ua = user_agent(&headers);
    let (user, tokens) = state.auth.register(&req, &ip, &ua).await?;
    // Welcome gift: one 7-day trial card per account (unique-index protected).
    if crate::cards::issue_trial(&state.pool, user.id).await.unwrap_or(false) {
        tracing::info!(user = %user.id, "trial card granted on registration");
    }
    let mut response: Response =
        (StatusCode::CREATED, Json(auth_response(&user, &tokens))).into_response();
    set_cookie(
        &state,
        &mut response,
        &tokens.access_token,
        tokens.access_ttl_secs,
    );
    Ok(response)
}

#[utoipa::path(post, path = "/api/auth/login", tag = "auth",
    request_body = LoginRequest,
    responses((status = 200, body = AuthResponse), (status = 401, description = "invalid credentials"))
)]
pub async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> ApiResult<axum::response::Response> {
    let ip = peer_ip(&state, &headers, addr);
    let ua = user_agent(&headers);
    let (user, tokens) = state.auth.secure_login(&req, &ip, &ua).await?;
    let mut response: Response = Json(auth_response(&user, &tokens)).into_response();
    set_cookie(
        &state,
        &mut response,
        &tokens.access_token,
        tokens.access_ttl_secs,
    );
    Ok(response)
}

#[utoipa::path(post, path = "/api/auth/refresh", tag = "auth",
    request_body = RefreshRequest,
    responses((status = 200, body = AuthResponse), (status = 401, description = "refresh token invalid"))
)]
pub async fn refresh(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Option<Json<RefreshRequest>>,
) -> ApiResult<axum::response::Response> {
    let ip = peer_ip(&state, &headers, addr);
    let ua = user_agent(&headers);
    let refresh_token = match body {
        Some(Json(req)) => req.refresh_token,
        None => cookie_from_headers(&state, &headers)
            .ok_or_else(|| ApiError::bad_request("refresh_token required"))?,
    };
    let (user, tokens) = state.auth.refresh(&refresh_token, &ip, &ua).await?;
    let mut response: Response = Json(auth_response(&user, &tokens)).into_response();
    set_cookie(
        &state,
        &mut response,
        &tokens.access_token,
        tokens.access_ttl_secs,
    );
    Ok(response)
}

#[utoipa::path(post, path = "/api/auth/logout", tag = "auth",
    security(("bearer" = [])),
    responses((status = 200, body = MessageResponse))
)]
pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    _auth: CurrentUser,
) -> ApiResult<axum::response::Response> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .map(str::trim);
    let token = bearer
        .map(str::to_string)
        .or_else(|| cookie_from_headers(&state, &headers));
    if let Some(token) = token {
        state.auth.logout(&token).await?;
    }
    let mut response: Response = Json(MessageResponse {
        message: "logged out".into(),
    })
    .into_response();
    clear_cookie(&state, &mut response);
    Ok(response)
}

#[utoipa::path(post, path = "/api/auth/logout-all", tag = "auth",
    security(("bearer" = [])),
    responses((status = 200, body = MessageResponse))
)]
pub async fn logout_all(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<MessageResponse>> {
    let revoked = state.auth.revoke_all_for_user(auth.id()).await?;
    Ok(Json(MessageResponse {
        message: format!("revoked {revoked} sessions"),
    }))
}

#[utoipa::path(get, path = "/api/auth/sessions", tag = "auth",
    security(("bearer" = [])),
    responses((status = 200, body = [SessionInfo]))
)]
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<SessionInfo>>> {
    let sessions = state
        .auth
        .list_sessions(auth.id(), auth.session_id())
        .await?;
    Ok(Json(sessions))
}

#[utoipa::path(delete, path = "/api/auth/sessions/{session_id}", tag = "auth",
    security(("bearer" = [])),
    params(SessionIdPath),
    responses((status = 200, body = MessageResponse))
)]
pub async fn revoke_session(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<SessionIdPath>,
) -> ApiResult<Json<MessageResponse>> {
    state
        .auth
        .revoke_session(auth.id(), path.session_id)
        .await?;
    Ok(Json(MessageResponse {
        message: "session revoked".into(),
    }))
}

#[utoipa::path(post, path = "/api/users/me/password", tag = "users",
    request_body = ChangePasswordRequest,
    responses((status = 200, body = MessageResponse), (status = 400), (status = 401))
)]
pub async fn change_password(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<(StatusCode, Json<MessageResponse>)> {
    if req.new_password.len() < 8 || req.new_password.len() > 128 {
        return Err(ApiError::bad_request(
            "new password must be between 8 and 128 characters",
        ));
    }
    state.auth.password_strength_ok(&req.new_password).await?;
    if !password::verify_password(&req.current_password, &auth.user().password_hash) {
        return Err(ApiError::Unauthorized);
    }
    let salt = password::new_salt();
    let hash = password::hash_password(&req.new_password, &salt)?;
    crate::users::repo::update_password(&state.pool, auth.id(), &hash).await?;
    state
        .auth
        .revoke_others(auth.id(), auth.session_id())
        .await?;
    let _ = headers;
    Ok((
        StatusCode::OK,
        Json(MessageResponse {
            message: "password updated, other sessions revoked".into(),
        }),
    ))
}
