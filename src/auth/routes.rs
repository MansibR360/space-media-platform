use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::auth::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auth/register", post(handlers::register))
        .route("/api/auth/login", post(handlers::login))
        .route("/api/auth/refresh", post(handlers::refresh))
        .route("/api/auth/password/forgot", post(handlers::forgot_password))
        .route("/api/auth/password/reset", post(handlers::reset_password))
        .route(
            "/api/auth/email/request-code",
            post(handlers::request_email_code),
        )
        .route("/api/auth/email/verify", post(handlers::verify_email))
        .route("/api/auth/logout", post(handlers::logout))
        .route("/api/auth/logout-all", post(handlers::logout_all))
        .route("/api/users/me/password", post(handlers::change_password))
        .route("/api/auth/sessions", get(handlers::list_sessions))
        .route(
            "/api/auth/sessions/{session_id}",
            delete(handlers::revoke_session),
        )
}
