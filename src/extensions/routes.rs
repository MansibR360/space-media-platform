use axum::Router;
use axum::routing::{delete, get, patch, post};
use std::sync::Arc;
use crate::extensions::handlers;
use crate::state::AppState;

/// Extension system routes — separate namespace from the plugin system.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/extensions",
            get(handlers::list_extensions).post(handlers::install_extension),
        )
        .route(
            "/api/admin/extensions/{id}",
            patch(handlers::patch_extension).delete(handlers::delete_extension),
        )
        .route(
            "/api/admin/extensions/discover",
            get(handlers::discover_extensions),
        )
        // 1:1 stremio resource surface (auth: any user)
        .route(
            "/api/extensions/{ext_id}/catalog/{stype}/{catalog_id}",
            get(handlers::catalog_resource),
        )
        .route(
            "/api/extensions/{ext_id}/stream/{stype}/{sid}",
            get(handlers::stream_resource),
        )
        .route(
            "/api/extensions/{ext_id}/meta/{stype}/{mid}",
            get(handlers::meta_resource),
        )
        .route(
            "/api/extensions/subtitles/{stype}/{sid}",
            get(handlers::subtitles_all),
        )
}
