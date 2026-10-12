use axum::Router;
use axum::routing::get;
use std::sync::Arc;

use crate::settings::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/users/me/settings",
        get(handlers::get_settings)
            .put(handlers::replace_settings)
            .patch(handlers::patch_settings),
    )
}
