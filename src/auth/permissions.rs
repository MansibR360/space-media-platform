use std::collections::HashSet;

use crate::auth::service::SessionContext;
use crate::users::models::Role;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    ManageLibrary,
    ModerateContent,
    AdminAll,
    /// Allows issuing signed download URLs (Phase 21 capability gate).
    Download,
    /// Scoped administration: billing queue + card pricing config.
    AdminBilling,
    /// Scoped administration: user listing + capability grants.
    AdminUsers,
    /// Scoped administration: merchants (list, ban, card-dispense lockout).
    AdminMerchants,
    /// Scoped administration: cards (mint/grant/hold/chain/search).
    AdminCards,
    /// Read-only cross-cutting admin visibility (stats, chains, queues).
    AdminView,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ManageLibrary => "manage_library",
            Self::ModerateContent => "moderate_content",
            Self::AdminAll => "admin_all",
            Self::Download => "download",
            Self::AdminBilling => "admin_billing",
            Self::AdminUsers => "admin_users",
            Self::AdminMerchants => "admin_merchants",
            Self::AdminCards => "admin_cards",
            Self::AdminView => "admin_view",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "manage_library" => Some(Self::ManageLibrary),
            "moderate_content" => Some(Self::ModerateContent),
            "admin_all" => Some(Self::AdminAll),
            "download" => Some(Self::Download),
            "admin_billing" => Some(Self::AdminBilling),
            "admin_users" => Some(Self::AdminUsers),
            "admin_merchants" => Some(Self::AdminMerchants),
            "admin_cards" => Some(Self::AdminCards),
            "admin_view" => Some(Self::AdminView),
            _ => None,
        }
    }

    /// All grantable permission names (for docs + validation messages).
    pub const ALL: &'static [&'static str] = &[
        "manage_library",
        "moderate_content",
        "download",
        "admin_all",
        "admin_billing",
        "admin_users",
        "admin_merchants",
        "admin_cards",
        "admin_view",
    ];
}

pub fn has(ctx: &SessionContext, permission: Permission) -> bool {
    ctx.user.role == Role::Admin
        || ctx.permissions.contains(&Permission::AdminAll)
        || ctx.permissions.contains(&permission)
}

pub fn require(ctx: &SessionContext, permission: Permission) -> Result<(), crate::error::ApiError> {
    if has(ctx, permission) {
        Ok(())
    } else {
        Err(crate::error::ApiError::forbidden(format!(
            "missing permission: {}",
            permission.as_str()
        )))
    }
}

#[allow(dead_code)]
pub fn require_role(
    user: &crate::users::models::User,
    min: Role,
) -> Result<(), crate::error::ApiError> {
    let rank = |r: Role| match r {
        Role::User => 0,
        Role::Moderator => 1,
        Role::Admin => 2,
    };
    if rank(user.role) >= rank(min) {
        Ok(())
    } else {
        Err(crate::error::ApiError::forbidden("insufficient role"))
    }
}

#[allow(dead_code)]
pub async fn grant(
    pool: &PgPool,
    user_id: Uuid,
    permission: Permission,
) -> Result<(), crate::error::ApiError> {
    sqlx::query(
        "INSERT INTO user_permissions (user_id, permission) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(permission.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn permissions_of(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<HashSet<Permission>, crate::error::ApiError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT permission FROM user_permissions WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .iter()
        .filter_map(|row| Permission::parse(&row.0))
        .collect())
}

/// Raw permission strings (tolerant of unknown/legacy rows) for extractor use.
pub async fn permission_strings_of(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<std::collections::HashSet<String>, crate::error::ApiError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT permission FROM user_permissions WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

pub async fn revoke(
    pool: &PgPool,
    user_id: Uuid,
    permission: Permission,
) -> Result<bool, crate::error::ApiError> {
    let result =
        sqlx::query("DELETE FROM user_permissions WHERE user_id = $1 AND permission = $2")
            .bind(user_id)
            .bind(permission.as_str())
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}
