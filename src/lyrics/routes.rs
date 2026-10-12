use axum::Router;
use std::sync::Arc;

use crate::lyrics::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/lyrics",
            axum::routing::get(handlers::get_lyrics),
        )
        .route(
            "/api/lyrics/providers",
            axum::routing::get(handlers::providers),
        )
        .route(
            "/api/media/{media_id}/lyrics",
            axum::routing::get(handlers::media_lyrics),
        )
}
