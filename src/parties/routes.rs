use axum::Router;
use axum::routing::{get, post};
use std::sync::Arc;

use crate::parties::handlers;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/parties", post(handlers::create_party))
        .route("/api/parties/join-by-code", post(handlers::join_by_code))
        .route("/api/me/parties", get(handlers::my_parties))
        .route(
            "/api/parties/{party_id}",
            get(handlers::get_party).delete(handlers::end_party),
        )
        .route("/api/parties/{party_id}/join", post(handlers::join_party))
        .route("/api/parties/{party_id}/leave", post(handlers::leave_party))
        .route(
            "/api/parties/{party_id}/members",
            get(handlers::list_members),
        )
        .route("/api/parties/{party_id}/control", post(handlers::control))
        .route(
            "/api/parties/{party_id}/latency",
            post(handlers::report_latency),
        )
        .route(
            "/api/parties/{party_id}/state",
            get(handlers::current_state),
        )
        .route("/api/parties/{party_id}/events", get(handlers::events))
        .route(
            "/api/parties/{party_id}/reactions",
            post(handlers::add_reaction),
        )
        .route(
            "/api/parties/{party_id}/reactions/heatmap",
            get(handlers::reaction_heatmap),
        )
        .route(
            "/api/parties/{party_id}/transfer",
            post(handlers::transfer_host),
        )
        .route(
            "/api/parties/{party_id}/promote",
            post(handlers::promote_cohost),
        )
}
