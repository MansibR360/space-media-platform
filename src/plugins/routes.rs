use axum::Router;
use axum::routing::{get, post};
use std::sync::Arc;

use crate::plugins::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/plugins",
            get(handlers::list_plugins).post(handlers::create_plugin),
        )
        .route("/api/metadata/sources", get(handlers::list_enabled_sources))
        .route(
            "/api/admin/plugins/{plugin_id}",
            axum::routing::patch(handlers::update_plugin).delete(handlers::delete_plugin),
        )
        .route(
            "/api/admin/plugins/{plugin_id}/test",
            post(handlers::test_plugin),
        )
}
