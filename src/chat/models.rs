use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Conversation {
    pub id: Uuid,
    pub kind: String,
    pub title: Option<String>,
    pub created_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub party_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ConversationSummary {
    #[sqlx(flatten)]
    pub conversation: Conversation,
    pub last_message: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub member_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ChatMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub sender_name: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub edited_at: Option<DateTime<Utc>>,
}

impl ChatMessage {
    pub fn event(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "message.new",
            "data": self,
        })
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateConversationRequest {
    /// direct | group
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    /// for direct conversations supply the other user's id; for groups all members
    pub member_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SendMessageRequest {
    pub body: String,
}
