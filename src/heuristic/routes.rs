use axum::Router;
use axum::routing::get;
use std::sync::Arc;

use crate::heuristic::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/heuristic/status", get(handlers::engine_status))
        .route(
            "/api/me/heuristic",
            get(handlers::my_profile).delete(handlers::wipe_my_data),
        )
        .route("/api/me/heuristic/wrapped", get(handlers::my_wrapped))
        .route("/api/me/heuristic/insights", get(handlers::my_insights))
        .route("/api/me/heuristic/predictions", get(handlers::my_predictions))
        .route("/api/me/heuristic/similar/{media_id}", get(handlers::my_similar))
}
