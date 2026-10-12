use axum::Router;
use axum::routing::{delete, get, post, put};
use std::sync::Arc;

use crate::state::AppState;
use crate::library::handlers;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/library/playlists",
            post(handlers::create_playlist).get(handlers::my_playlists),
        )
        .route(
            "/api/library/playlists/{playlist_id}",
            get(handlers::get_playlist)
                .patch(handlers::update_playlist)
                .delete(handlers::delete_playlist),
        )
        .route(
            "/api/library/playlists/{playlist_id}/items",
            get(handlers::list_items)
                .post(handlers::add_item)
                .put(handlers::replace_items),
        )
        .route(
            "/api/library/playlists/{playlist_id}/items/{media_id}",
            delete(handlers::remove_item),
        )
        .route(
            "/api/library/playlists/{playlist_id}/order",
            put(handlers::reorder),
        )
        .route("/api/library/storage", get(handlers::storage_info))
}
