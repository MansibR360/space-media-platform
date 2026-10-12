use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize, utoipa::ToSchema,
)]
// Column is TEXT with a CHECK constraint, not a Postgres enum. Without
// type_name, sqlx infers an enum type that was never created.
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ListStatus {
    Watching,
    Completed,
    OnHold,
    Dropped,
    Planned,
}

impl ListStatus {
    #[allow(dead_code)]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "watching" => Some(Self::Watching),
            "completed" => Some(Self::Completed),
            "on_hold" => Some(Self::OnHold),
            "dropped" => Some(Self::Dropped),
            "planned" => Some(Self::Planned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct WatchEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub episode_id: Option<Uuid>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ListEntry {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub status: ListStatus,
    pub score: Option<f64>,
    pub notes: String,
    pub episodes_watched: i32,
    pub rewatch_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ProgressUpdate {
    pub media_id: Uuid,
    pub episode_id: Option<Uuid>,
    #[serde(default)]
    pub position_seconds: f64,
    #[serde(default)]
    pub duration_seconds: f64,
    /// Mark as fully watched
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct StatusUpdate {
    pub status: ListStatus,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub episodes_watched: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ContinueWatchingItem {
    #[sqlx(flatten)]
    pub watch: WatchEntry,
    pub title: String,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub episode_label: Option<String>,
    pub percent_watched: f64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ReadingProgress {
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub position_percent: f64,
    pub page: Option<i32>,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReadingProgressUpdate {
    pub media_id: Uuid,
    pub position_percent: f64,
    #[serde(default)]
    pub page: Option<i32>,
}

/// list_entries joined with the media title.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ListEntryWithTitle {
    #[sqlx(flatten)]
    pub entry: ListEntry,
    pub title: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct HistoryRow {
    pub id: Uuid,
    pub media_id: Uuid,
    pub episode_id: Option<Uuid>,
    pub title: String,
    pub episode_title: Option<String>,
    pub position_seconds: f64,
    pub duration_seconds: f64,
    pub completed: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct RatingRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub title: String,
    pub score: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct UserListItem {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub visibility: String,
    pub item_count: i64,
    pub created_at: DateTime<Utc>,
}

/// Watch-history recording toggle for the caller (pause/resume).
#[derive(Debug, Clone, Copy, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct HistorySettings {
    /// false → progress writes are accepted but not persisted
    pub enabled: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct HistorySettingsUpdate {
    pub enabled: bool,
}
