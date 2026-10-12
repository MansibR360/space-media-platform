use axum::Router;
use axum::routing::{get, post};
use std::sync::Arc;

use crate::state::AppState;
use crate::streaming::{download_handlers, handlers};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/files", get(handlers::list_files))
        .route("/api/files/{file_id}", get(handlers::get_file))
        .route("/api/files/{file_id}/raw", get(handlers::stream_file))
        .route(
            "/api/files/{file_id}/subtitles",
            get(handlers::get_subtitle),
        )
        .route(
            "/api/files/{file_id}/download-url",
            post(download_handlers::request_download_url),
        )
        .route("/api/dl/{token}", get(download_handlers::redeem_download))
        .route(
            "/api/admin/files/{file_id}/probe",
            post(handlers::probe_file_endpoint),
        )
        .route(
            "/api/admin/plugins/{plugin_id}/sync",
            post(download_handlers::sync_source_plugin),
        )
}
