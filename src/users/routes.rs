use axum::Router;
use axum::routing::get;
use std::sync::Arc;

use crate::state::AppState;
use crate::users::handlers;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/users/me",
            get(handlers::me).patch(handlers::update_me),
        )
        .route("/api/users/{username}", get(handlers::public_profile))
        .route("/api/admin/users", get(handlers::admin_list_users))
        .route(
            "/api/admin/users/{user_id}/permissions",
            axum::routing::post(handlers::grant_permission)
                .get(handlers::list_user_permissions),
        )
        .route(
            "/api/admin/users/{user_id}/permissions/{permission}",
            axum::routing::delete(handlers::revoke_permission),
        )
}
