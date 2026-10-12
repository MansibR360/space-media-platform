use axum::Router;
use axum::routing::{get, post};
use std::sync::Arc;

use crate::catalog::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/media", get(handlers::list_media))
        .route("/api/media/{media_id}", get(handlers::get_media))
        .route(
            "/api/media/{media_id}/episodes",
            get(handlers::list_episodes),
        )
        .route("/api/admin/media", post(handlers::create_media))
        .route(
            "/api/admin/media/{media_id}",
            axum::routing::patch(handlers::update_media).delete(handlers::delete_media),
        )
        .route(
            "/api/admin/media/{media_id}/seasons",
            post(handlers::create_season),
        )
        .route(
            "/api/admin/media/{media_id}/seasons/{season_id}/episodes",
            post(handlers::create_episode),
        )
        .route("/api/metadata/search", get(handlers::metadata_search))
        .route("/api/media/{media_id}/imdb", get(handlers::imdb_ratings))
        .route(
            "/api/admin/media/{media_id}/imdb/sync",
            post(handlers::imdb_sync_one),
        )
        .route("/api/admin/imdb/sync", post(handlers::imdb_sync_stale))
        .route(
            "/api/admin/quality/duplicates",
            get(handlers::quality_duplicates),
        )
        .route(
            "/api/admin/quality/duplicates/auto-resolve",
            post(handlers::auto_resolve_duplicates),
        )
        .route(
            "/api/admin/storage/analytics",
            get(handlers::storage_analytics),
        )
        .route(
            "/api/admin/files/{file_id}/events",
            get(handlers::file_events),
        )
        .route("/api/metadata/import", post(handlers::import_metadata))
}
