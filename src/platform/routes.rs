use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::platform::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/me/keys",
            post(handlers::create_key).get(handlers::list_keys),
        )
        .route("/api/me/keys/{key_id}", delete(handlers::revoke_key))
        .route(
            "/api/me/devices",
            post(handlers::register_device).get(handlers::list_devices),
        )
        .route(
            "/api/me/devices/{device_id}",
            delete(handlers::revoke_device),
        )
        .route("/api/me/notifications", get(handlers::notifications))
        .route(
            "/api/me/notifications/unread-count",
            get(handlers::unread_count),
        )
        .route(
            "/api/me/notifications/{notification_id}/read",
            post(handlers::mark_read),
        )
        .route(
            "/api/me/search-history",
            get(handlers::search_history).delete(handlers::clear_search_history),
        )
        .route("/api/me/behavior", get(handlers::behavior))
}
