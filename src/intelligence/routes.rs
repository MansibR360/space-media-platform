use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::intelligence::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/intelligence/vibe", get(handlers::vibe_search))
        .route("/api/intelligence/moods", get(handlers::moods))
        .route("/api/me/taste", get(handlers::my_taste))
        .route("/api/home", get(handlers::home))
        .route("/api/collections", post(handlers::create_collection))
        .route(
            "/api/collections/{collection_id}/items",
            get(handlers::collection_items),
        )
        .route(
            "/api/collections/{collection_id}",
            delete(handlers::delete_collection),
        )
}
