use axum::Router;
use axum::routing::{delete, get, post};
use std::sync::Arc;

use crate::chat::{handlers, ws};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/ws", get(ws::gateway))
        .route(
            "/api/chat/conversations",
            get(handlers::list_conversations).post(handlers::create_conversation),
        )
        .route(
            "/api/chat/conversations/{conversation_id}/messages",
            get(handlers::list_messages).post(handlers::send_message),
        )
        .route(
            "/api/chat/conversations/{conversation_id}/members/{user_id}",
            post(handlers::add_member),
        )
        .route(
            "/api/chat/messages/{message_id}",
            axum::routing::patch(handlers::edit_message)
                .delete(handlers::delete_message),
        )
        .route(
            "/api/chat/conversations/{conversation_id}/members/me",
            delete(handlers::leave_conversation),
        )
        .route(
            "/api/chat/conversations/{conversation_id}/read",
            post(handlers::mark_read),
        )
}
