use axum::Router;
use axum::routing::{get, post};
use std::sync::Arc;

use crate::music::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- per user ----
        .route("/api/music/listen", post(handlers::record_listen))
        .route("/api/music/taste", get(handlers::taste))
        .route("/api/music/recommendations", get(handlers::recommendations))
        .route("/api/music/history", get(handlers::history))
        // ---- global: same answer for every caller ----
        .route("/api/music/charts", get(handlers::charts))
        .route("/api/music/{media_id}/analysis", get(handlers::get_analysis))
        .route("/api/music/{media_id}/stats", get(handlers::get_stats))
        .route("/api/music/{media_id}/similar", get(handlers::similar))
        // ---- admin ----
        .route(
            "/api/admin/music/{media_id}/analyze",
            post(handlers::analyze_one),
        )
        .route(
            "/api/admin/music/analyze-pending",
            post(handlers::analyze_pending),
        )
        .route("/api/admin/music/dataset", get(handlers::dataset))
        .route(
            "/api/admin/music/dataset/summary",
            get(handlers::dataset_summary),
        )
}
