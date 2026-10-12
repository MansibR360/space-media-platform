use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Party {
    pub id: Uuid,
    pub host_id: Uuid,
    pub media_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    pub join_code: String,
    pub title: String,
    pub visibility: String,
    pub max_participants: i32,
    pub is_playing: bool,
    pub position_seconds: f64,
    pub state_updated_at: DateTime<Utc>,
    pub last_event_seq: i64,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct PartyMember {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct PartyEvent {
    pub seq: i64,
    pub party_id: Uuid,
    pub actor_id: Option<Uuid>,
    pub event_kind: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

impl Party {
    /// Projected playback position right now, accounting for elapsed wall-clock time.
    pub fn projected_position(&self) -> f64 {
        if !self.is_playing {
            return self.position_seconds;
        }
        let elapsed = (Utc::now() - self.state_updated_at).num_milliseconds() as f64 / 1000.0;
        self.position_seconds + elapsed.max(0.0)
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreatePartyRequest {
    #[serde(default)]
    pub title: String,
    pub media_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default = "default_max")]
    pub max_participants: i32,
    /// optional shared password for joining
    #[serde(default)]
    pub password: Option<String>,
}

fn default_visibility() -> String {
    "public".into()
}

fn default_max() -> i32 {
    20
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct JoinPartyRequest {
    #[serde(default)]
    pub join_code: Option<String>,
    /// required when the party has a password set
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PartyControlRequest {
    /// play | pause | seek | sync_tracks
    pub action: String,
    /// target position for seek actions
    #[serde(default)]
    pub position_seconds: Option<f64>,
    /// for sync_tracks: preferred tracks broadcast to all viewers
    #[serde(default)]
    pub audio_stream_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub subtitle_stream_id: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PartyReactionRequest {
    pub emoji: String,
    pub second_offset: i32,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ReactionBucket {
    pub bucket_start: i32,
    pub emoji: String,
    pub count: i64,
}
