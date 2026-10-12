use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::social::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/media/{media_id}/reviews",
            get(handlers::list_reviews).post(handlers::create_review),
        )
        .route("/api/reviews/{review_id}", delete(handlers::delete_review))
        .route("/api/social/feed", get(handlers::feed))
        .route(
            "/api/comments",
            get(handlers::list_comments).post(handlers::create_comment),
        )
        .route(
            "/api/comments/{comment_id}",
            delete(handlers::delete_comment),
        )
        .route(
            "/api/reactions",
            post(handlers::toggle_reaction).get(handlers::reactions_summary),
        )
        .route("/api/users/{user_id}/follow", post(handlers::follow_user))
        .route("/api/me/following", get(handlers::my_following))
        .route("/api/me/followers", get(handlers::my_followers))
}
