use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::{CurrentUser, StaffUser};
use crate::error::ApiResult;
use crate::pagination::{Page, PageQuery};
use crate::state::AppState;
use crate::users::models::{PrivateProfile, PublicProfile, UpdateProfileRequest};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct UsernamePath {
    pub username: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserRow {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub display_name: String,
    pub role: crate::users::models::Role,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantPermissionRequest {
    /// manage_library | moderate_content | download | admin_all |
    /// admin_billing | admin_users | admin_merchants | admin_cards | admin_view
    pub permission: String,
}

fn parse_permission(raw: &str) -> Result<crate::auth::permissions::Permission, crate::error::ApiError> {
    crate::auth::permissions::Permission::parse(raw).ok_or_else(|| {
        crate::error::ApiError::bad_request(format!(
            "unknown permission '{}'; valid: {}",
            raw,
            crate::auth::permissions::Permission::ALL.join(", ")
        ))
    })
}

/// Grants a capability to a user (sub-admin provisioning). Granting
/// `admin_all` is reserved for root administrators.
#[utoipa::path(post, path = "/api/admin/users/{user_id}/permissions", tag = "admin",
    security(("bearer" = [])),
    request_body = GrantPermissionRequest,
    responses((status = 200, body = MessageResponse), (status = 400), (status = 403))
)]
pub async fn grant_permission(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    axum::extract::Path(user_id): axum::extract::Path<Uuid>,
    Json(req): Json<GrantPermissionRequest>,
) -> ApiResult<Json<MessageResponse>> {
    use crate::auth::permissions::Permission;

    staff.require(Permission::AdminUsers.as_str())?;
    let permission = parse_permission(&req.permission)?;
    if permission == Permission::AdminAll && !staff.is_root() {
        return Err(crate::error::ApiError::forbidden(
            "only a full administrator can grant 'admin_all'",
        ));
    }
    crate::auth::permissions::grant(&state.pool, user_id, permission).await?;
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        if permission == Permission::AdminAll {
            "admin.promoted_root"
        } else {
            "admin.permission_granted"
        },
        &user_id.to_string(),
        serde_json::json!({ "permission": permission.as_str() }),
        "",
    )
    .await;
    Ok(Json(MessageResponse {
        message: format!("granted {}", permission.as_str()),
    }))
}

/// Revokes a capability from a user. Revoking `admin_all` is reserved for
/// root administrators.
#[utoipa::path(delete, path = "/api/admin/users/{user_id}/permissions/{permission}", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = MessageResponse), (status = 404))
)]
pub async fn revoke_permission(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    axum::extract::Path((user_id, permission)): axum::extract::Path<(Uuid, String)>,
) -> ApiResult<Json<MessageResponse>> {
    use crate::auth::permissions::Permission;

    staff.require(Permission::AdminUsers.as_str())?;
    let permission = parse_permission(&permission)?;
    if permission == Permission::AdminAll && !staff.is_root() {
        return Err(crate::error::ApiError::forbidden(
            "only a full administrator can revoke 'admin_all'",
        ));
    }
    // A root admin's own role cannot be stripped via permissions.
    if let Some(target) = crate::users::repo::find_by_id(&state.pool, user_id).await? {
        if target.role == crate::users::models::Role::Admin {
            return Err(crate::error::ApiError::forbidden(
                "role-based administrators cannot be downgraded here",
            ));
        }
    }
    let removed = crate::auth::permissions::revoke(&state.pool, user_id, permission).await?;
    if !removed {
        return Err(crate::error::ApiError::not_found("permission grant"));
    }
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        "admin.permission_revoked",
        &user_id.to_string(),
        serde_json::json!({ "permission": permission.as_str() }),
        "",
    )
    .await;
    Ok(Json(MessageResponse {
        message: format!("revoked {}", permission.as_str()),
    }))
}

/// Lists every capability currently granted to a user.
#[utoipa::path(get, path = "/api/admin/users/{user_id}/permissions", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_user_permissions(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    axum::extract::Path(user_id): axum::extract::Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminUsers.as_str())?;
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT permission FROM user_permissions WHERE user_id = $1 ORDER BY permission")
            .bind(user_id)
            .fetch_all(&state.pool)
            .await?;
    let is_root = crate::users::repo::find_by_id(&state.pool, user_id)
        .await?
        .map(|u| u.role == crate::users::models::Role::Admin)
        .unwrap_or(false);
    let permissions: Vec<String> = rows.into_iter().map(|(p,)| p).collect();
    Ok(Json(serde_json::json!({
        "user_id": user_id,
        "is_root_admin": is_root,
        "permissions": permissions,
        "all_valid": crate::auth::permissions::Permission::ALL,
    })))
}

#[utoipa::path(get, path = "/api/users/me", tag = "users",
    security(("bearer" = [])),
    responses((status = 200, body = PrivateProfile))
)]
pub async fn me(auth: CurrentUser) -> Json<PrivateProfile> {
    Json(auth.user().private())
}

#[utoipa::path(patch, path = "/api/users/me", tag = "users",
    security(("bearer" = [])),
    request_body = UpdateProfileRequest,
    responses((status = 200, body = PrivateProfile))
)]
pub async fn update_me(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<UpdateProfileRequest>,
) -> ApiResult<Json<PrivateProfile>> {
    if let Some(bio) = &req.bio {
        if bio.len() > 2000 {
            return Err(crate::error::ApiError::bad_request(
                "bio too long (max 2000)",
            ));
        }
    }
    let user = crate::users::repo::update_profile(&state.pool, auth.id(), &req).await?;
    Ok(Json(user.private()))
}

#[utoipa::path(get, path = "/api/users/{username}", tag = "users",
    params(UsernamePath),
    responses((status = 200, body = PublicProfile), (status = 404))
)]
pub async fn public_profile(
    State(state): State<Arc<AppState>>,
    maybe_user: Option<CurrentUser>,
    Path(path): Path<UsernamePath>,
) -> ApiResult<Json<PublicProfile>> {
    let user = crate::users::repo::find_by_username(&state.pool, &path.username)
        .await?
        .ok_or_else(|| crate::error::ApiError::not_found("user"))?;

    // Privacy gate (changeset 25): 'friends' / 'private' profiles are hidden
    // from non-friends. The 404 is identical to "user does not exist" so
    // private accounts are not enumerable.
    let viewer = maybe_user.as_ref().map(|u| u.id());
    let is_staff = maybe_user
        .as_ref()
        .is_some_and(|u| {
            matches!(
                u.user().role,
                crate::users::models::Role::Moderator | crate::users::models::Role::Admin
            )
        });
    let visible = is_staff
        || crate::privacy::can_view_profile(&state.pool, viewer, user.id)
            .await
            .unwrap_or(false);
    if !visible {
        return Err(crate::error::ApiError::not_found("user"));
    }

    Ok(Json(user.public()))
}

#[utoipa::path(get, path = "/api/admin/users", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = [AdminUserRow]))
)]
pub async fn admin_list_users(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    axum::extract::Query(query): axum::extract::Query<PageQuery>,
) -> ApiResult<Json<Page<AdminUserRow>>> {
    use crate::auth::permissions::Permission;
    staff.require(Permission::AdminUsers.as_str())
        .or_else(|_| staff.require(Permission::AdminView.as_str()))?;
    let limit = query.clamp_limit();
    let offset = query
        .cursor
        .as_deref()
        .and_then(|c| crate::pagination::decode_cursor(c).ok())
        .and_then(|v| v.get("o").and_then(|o| o.as_i64()))
        .unwrap_or(0);

    let users = crate::users::repo::list(&state.pool, limit, offset).await?;
    let items: Vec<AdminUserRow> = users
        .into_iter()
        .map(|u| AdminUserRow {
            id: u.id,
            username: u.username,
            email: u.email,
            display_name: u.display_name,
            role: u.role,
            is_active: u.is_active,
            created_at: u.created_at,
        })
        .collect();

    let next = (items.len() as i64 == limit)
        .then(|| crate::pagination::encode_cursor(&serde_json::json!({ "o": offset + limit })));

    Ok(Json(Page::with_cursor(items, next)))
}

#[allow(dead_code)]
fn _assert_send<T: Send>(_: T) {}
