use std::sync::Arc;

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;

use crate::auth::service::SessionContext;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::utils::parse_cookies;

pub const SESSION_COOKIE: &str = "space_session";

#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub ctx: Arc<SessionContext>,
}

impl CurrentUser {
    pub fn id(&self) -> uuid::Uuid {
        self.ctx.user.id
    }

    pub fn user(&self) -> &crate::users::models::User {
        &self.ctx.user
    }

    pub fn session_id(&self) -> uuid::Uuid {
        self.ctx.session_id
    }
}

impl std::ops::Deref for CurrentUser {
    type Target = SessionContext;

    fn deref(&self) -> &Self::Target {
        &self.ctx
    }
}

pub fn extract_token(parts: &Parts, cookie_name: &str) -> Option<String> {
    if let Some(value) = parts.headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(raw) = value.to_str() {
            if let Some(token) = raw
                .strip_prefix("Bearer ")
                .or_else(|| raw.strip_prefix("bearer "))
            {
                return Some(token.trim().to_string());
            }
        }
    }
    let cookie_header = parts
        .headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?;
    parse_cookies(cookie_header)
        .into_iter()
        .find(|(name, value)| name == cookie_name && !value.is_empty())
        .map(|(_, value)| value)
}

impl FromRequestParts<Arc<AppState>> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        resolve(parts, state).await?.ok_or(ApiError::Unauthorized)
    }
}

impl OptionalFromRequestParts<Arc<AppState>> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Option<Self>, Self::Rejection> {
        resolve(parts, state).await
    }
}

async fn resolve(parts: &Parts, state: &Arc<AppState>) -> ApiResult<Option<CurrentUser>> {
    // Machine clients: `X-API-Key: sk_<prefix>_<secret>` with scoped access.
    if let Some(raw) = parts
        .headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with("sk_"))
    {
        let Some(auth) = crate::platform::repo::authenticate_api_key(&state.pool, raw).await?
        else {
            return Err(ApiError::Unauthorized);
        };

        let mutating = !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS");
        if (mutating && !auth.scopes.iter().any(|s| s == "write"))
            || (!auth.scopes.iter().any(|s| s == "read"))
        {
            return Err(ApiError::forbidden(
                "api key missing required scope for this request",
            ));
        }
        if parts.uri.path().starts_with("/api/admin")
            && auth.role != crate::users::models::Role::Admin
        {
            return Err(ApiError::forbidden("admin scope required"));
        }

        let Some(user) = crate::users::repo::find_by_id(&state.pool, auth.user_id).await? else {
            return Err(ApiError::Unauthorized);
        };
        return Ok(Some(CurrentUser {
            ctx: Arc::new(SessionContext {
                user,
                session_id: auth.key_id,
                permissions: Default::default(),
            }),
        }));
    }

    let Some(token) = extract_token(parts, SESSION_COOKIE) else {
        return Ok(None);
    };
    let ctx = state
        .auth
        .resolve_session(&token)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    Ok(Some(CurrentUser { ctx: Arc::new(ctx) }))
}

#[derive(Clone, Debug)]
pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<Arc<AppState>> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let user =
            <CurrentUser as FromRequestParts<Arc<AppState>>>::from_request_parts(parts, state)
                .await?;
        if user.user().role != crate::users::models::Role::Admin {
            return Err(ApiError::forbidden("administrator access required"));
        }
        Ok(AdminUser(user))
    }
}

/// Staff access: a full admin **or** a sub-admin holding scoped `admin_*`
/// capabilities (changeset 26). The permission set is loaded once per
/// request; routes then assert exactly the capability they need via
/// [`StaffUser::require`], so an admin page can be built purely on the API
/// with least-privilege accounts.
#[derive(Clone, Debug)]
pub struct StaffUser {
    inner: CurrentUser,
    perms: std::collections::HashSet<String>,
}

impl std::ops::Deref for StaffUser {
    type Target = CurrentUser;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl StaffUser {
    /// Root administrator (role-based, cannot be revoked via permissions).
    pub fn is_root(&self) -> bool {
        self.inner.user().role == crate::users::models::Role::Admin
    }

    pub fn can(&self, capability: &str) -> bool {
        self.is_root()
            || self.perms.contains(crate::auth::permissions::Permission::AdminAll.as_str())
            || self.perms.contains(capability)
    }

    pub fn require(&self, capability: &str) -> ApiResult<()> {
        if self.can(capability) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!(
                "missing '{capability}' administration capability"
            )))
        }
    }
}

impl FromRequestParts<Arc<AppState>> for StaffUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let inner =
            <CurrentUser as FromRequestParts<Arc<AppState>>>::from_request_parts(parts, state)
                .await?;
        let perms =
            crate::auth::permissions::permission_strings_of(&state.pool, inner.id()).await?;
        // Anyone without any staff capability is rejected outright — same
        // opaque error as AdminUser so probes learn nothing.
        let is_staff = inner.user().role == crate::users::models::Role::Admin
            || perms.iter().any(|p| p == "admin_all" || p.starts_with("admin_"));
        if !is_staff {
            return Err(ApiError::forbidden("administrator access required"));
        }
        Ok(StaffUser { inner, perms })
    }
}
