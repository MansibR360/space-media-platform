use axum::Router;
use axum::routing::get;
use std::sync::Arc;

use crate::recommendations::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/recommendations", get(handlers::for_me))
        .route(
            "/api/media/{media_id}/similar-scores",
            get(handlers::similar_scores),
        )
}
