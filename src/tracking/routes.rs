use axum::Router;
use axum::routing::{get, post, put};
use std::sync::Arc;

use crate::state::AppState;
use crate::tracking::handlers;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tracking/progress", put(handlers::update_progress))
        .route(
            "/api/tracking/reading",
            put(handlers::update_reading_progress),
        )
        .route(
            "/api/me/media/{media_id}/reading",
            get(handlers::get_reading_progress),
        )
        .route(
            "/api/me/continue-watching",
            get(handlers::continue_watching),
        )
        .route("/api/me/history", get(handlers::history))
        .route(
            "/api/me/history/settings",
            get(handlers::get_history_settings).put(handlers::set_history_settings),
        )
        .route("/api/me/ratings", get(handlers::my_ratings))
        .route("/api/me/favorites", get(handlers::favorites))
        .route("/api/me/list", get(handlers::my_list))
        .route(
            "/api/me/lists",
            post(handlers::create_list).get(handlers::my_lists),
        )
        .route(
            "/api/me/media/{media_id}/status",
            put(handlers::set_status).delete(handlers::remove_status),
        )
        .route(
            "/api/media/{media_id}/rating",
            put(handlers::rate_media).delete(handlers::delete_rating),
        )
        .route(
            "/api/media/{media_id}/favorite",
            post(handlers::toggle_favorite),
        )
        .route("/api/lists/{list_id}/items", get(handlers::list_items))
        .route(
            "/api/lists/{list_id}/items/{media_id}",
            post(handlers::add_list_item).delete(handlers::remove_list_item),
        )
}
