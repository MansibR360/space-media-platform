use axum::Router;
use axum::routing::get;
use std::sync::Arc;

use crate::search::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/search", get(handlers::search))
        .route("/api/search/parse", get(handlers::parse_query))
        .route("/api/search/suggest", get(handlers::suggest))
        .route("/api/admin/search-cache", get(handlers::search_cache_stats))
        .route("/api/discover", get(handlers::discover))
        .route("/api/trending", get(handlers::trending))
        .route(
            "/api/media/{media_id}/similar",
            get(handlers::similar_media),
        )
}
