use axum::Router;
use axum::routing::{get, post, put};
use std::sync::Arc;

use crate::sources::{handlers, hub_handlers, hub_server, unified_handlers, web_handlers};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/sources", get(handlers::list_sources))
        .route("/api/sources/status", get(handlers::status_all))
        .route(
            "/api/sources/rankings",
            get(crate::sources::benchmark::rankings),
        )
        .route("/api/sources/{name}/browse", get(handlers::browse_source))
        .route(
            "/api/admin/sources/{name}/debug-search",
            get(handlers::search_source),
        )
        .route(
            "/api/admin/sources/{name}/debug-item/{external_id}",
            get(handlers::source_item),
        )
        .route(
            "/api/admin/mangadex/chapters/{manga_id}",
            get(handlers::manga_chapters),
        )
        .route(
            "/api/admin/mangadex/pages/{chapter_id}",
            get(handlers::manga_pages),
        )
        .route(
            "/api/admin/sources/{name}/sync",
            post(handlers::sync_source),
        )
        .route(
            "/api/admin/sources/register-url",
            post(handlers::register_source_url),
        )
}

/// Learned web sources admin surface.
pub fn web_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/websources",
            get(web_handlers::list_web_sources).post(web_handlers::create_web_source),
        )
        .route(
            "/api/admin/websources/{id}",
            put(web_handlers::patch_web_source).delete(web_handlers::delete_web_source),
        )
        .route(
            "/api/admin/websources/{id}/relearn",
            post(web_handlers::relearn_web_source),
        )
        .route(
            "/api/admin/websources/{id}/test",
            post(web_handlers::test_web_source),
        )
        .route(
            "/api/admin/websources/{id}/recipe",
            get(web_handlers::get_recipe),
        )
}

/// Remote source hubs: admin management + the machine-facing `/api/hub/*`.
pub fn hub_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/hubs",
            get(hub_handlers::list_hubs).post(hub_handlers::create_hub),
        )
        .route(
            "/api/admin/hubs/{id}",
            put(hub_handlers::patch_hub).delete(hub_handlers::delete_hub),
        )
        .route("/api/admin/hubs/{id}/pull", post(hub_handlers::pull_hub))
        .route("/api/admin/hubs/{id}/push", post(hub_handlers::push_hub))
        .route("/api/admin/hubs/{id}/test", post(hub_handlers::test_hub))
        // machine surface (token/loopback gated in the handlers)
        .route(
            "/api/hub/find",
            get(hub_server::find),
        )
        .route("/api/hub/item", get(hub_server::item))
        .route(
            "/api/hub/sources",
            get(hub_server::list_sources).post(hub_server::upload_source),
        )
}

pub fn unified_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/search/all", get(unified_handlers::search_everything))
        .route(
            "/api/playback/resolve",
            post(unified_handlers::resolve_playback),
        )
        .route(
            "/api/playback/lookup",
            post(unified_handlers::watch_lookup),
        )
        .route("/api/resolve", post(unified_handlers::resolve_v2))
        .route(
            "/api/playback/manga/chapters",
            post(unified_handlers::manga_chapters),
        )
        .route(
            "/api/playback/episodes",
            get(unified_handlers::source_episodes),
        )
        .route(
            "/api/playback/manga/pages",
            post(unified_handlers::manga_pages),
        )
        .route(
            "/api/playback/manga/open",
            post(unified_handlers::manga_open),
        )
}

/// YouTube resilience surface: per-strategy telemetry + manual heal trigger.
pub fn youtube_admin_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/youtube/status",
            get(crate::sources::youtube::admin::status),
        )
        .route(
            "/api/admin/youtube/heal",
            post(crate::sources::youtube::admin::heal),
        )
        // Public: the YouTube video section, used by the home page and by
        // search. Not admin - it is part of what a visitor sees.
        .route(
            "/api/youtube/search",
            get(crate::sources::youtube::search_videos),
        )
        .route(
            "/api/youtube/trending",
            get(crate::sources::youtube::trending_videos),
        )
}
