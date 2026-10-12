use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Review {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
    pub media_id: Uuid,
    pub media_title: String,
    pub title: String,
    pub body: String,
    pub score: Option<f64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Review {
    #[allow(dead_code)]
    pub fn event(&self) -> serde_json::Value {
        serde_json::json!({ "type": "review.new", "data": self })
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Comment {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_name: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub body: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ReactionCount {
    pub emoji: String,
    pub count: i64,
    pub mine: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct FollowInfo {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub followed_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateReviewRequest {
    #[serde(default)]
    pub title: Option<String>,
    pub body: String,
    #[serde(default)]
    pub score: Option<f64>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateCommentRequest {
    /// review | media | party
    pub target_type: String,
    pub target_id: Uuid,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ToggleReactionRequest {
    /// review | comment | message
    pub target_type: String,
    pub target_id: Uuid,
    pub emoji: String,
}
