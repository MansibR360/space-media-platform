use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::playback::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/playback/sessions",
            post(handlers::create_session).get(handlers::my_sessions),
        )
        .route(
            "/api/playback/sessions/{session_id}/heartbeat",
            post(handlers::heartbeat),
        )
        .route(
            "/api/playback/sessions/{session_id}",
            delete(handlers::end_session),
        )
        .route("/api/playback/next-hint", get(handlers::next_hint))
        .route("/api/playback/resume/{media_id}", get(handlers::resume))
        .route(
            "/api/playback/resume/episode/{episode_id}",
            get(handlers::resume_episode),
        )
        .route(
            "/api/transcode-jobs/{job_id}",
            get(handlers::job_status).delete(handlers::cancel_job),
        )
        .route(
            "/api/admin/files/{file_id}/transcode",
            post(handlers::enqueue_transcode),
        )
        .route(
            "/api/admin/files/{file_id}/thumbnails",
            post(handlers::enqueue_thumbnails),
        )
        .route(
            "/api/files/{file_id}/master.m3u8",
            get(handlers::hls_master),
        )
        .route("/api/hls/{job_id}/{*asset}", get(handlers::hls_asset))
        .route("/api/playback/handoff", post(handlers::create_handoff))
        .route(
            "/api/playback/handoff/redeem",
            post(handlers::redeem_handoff),
        )
        .route(
            "/api/files/{file_id}/markers",
            get(handlers::file_markers).put(handlers::set_marker),
        )
        .route(
            "/api/admin/files/{file_id}/markers/detect",
            post(handlers::detect_markers_endpoint),
        )
        .route(
            "/api/files/{file_id}/chapters",
            get(handlers::file_chapters),
        )
}
