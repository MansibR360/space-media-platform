use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type, Serialize, utoipa::ToSchema)]
// The schema stores this as TEXT with a CHECK constraint, not a Postgres
// enum. Without type_name, sqlx infers a nonexistent enum type "role".
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Moderator,
    Admin,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub display_name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: Role,
    pub avatar_url: Option<String>,
    pub bio: String,
    pub is_active: bool,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    pub fn public(&self) -> PublicProfile {
        PublicProfile {
            id: self.id,
            username: self.username.clone(),
            display_name: self.display_name.clone(),
            role: self.role,
            avatar_url: self.avatar_url.clone(),
            bio: self.bio.clone(),
            created_at: self.created_at,
        }
    }

    pub fn private(&self) -> PrivateProfile {
        PrivateProfile {
            id: self.id,
            username: self.username.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
            role: self.role,
            avatar_url: self.avatar_url.clone(),
            bio: self.bio.clone(),
            is_active: self.is_active,
            email_verified: self.email_verified,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PublicProfile {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub avatar_url: Option<String>,
    pub bio: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PrivateProfile {
    pub id: Uuid,
    pub username: String,
    pub email: String,
    pub display_name: String,
    pub role: Role,
    pub avatar_url: Option<String>,
    pub bio: String,
    pub is_active: bool,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}
