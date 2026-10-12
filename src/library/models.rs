use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Playlist flavor.
///
/// - `Multimedia`: mixed media types allowed (movie + track + book ...).
/// - `Standard`: pinned to a single catalog media type; enforced on insert —
///   the pin is taken from the first added item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize, utoipa::ToSchema)]
// Column is TEXT with a CHECK constraint, not a Postgres enum. Without
// type_name, sqlx infers an enum type that was never created.
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PlaylistKind {
    Multimedia,
    Standard,
}

impl PlaylistKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Multimedia => "multimedia",
            Self::Standard => "standard",
        }
    }
}

/// A saved playlist. Content is media references only — see
/// [`crate::library::store`] for the media-only text guard.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct LibraryPlaylist {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub kind: PlaylistKind,
    /// Pinned catalog type for `standard` playlists (null until first item).
    pub media_type: Option<String>,
    pub visibility: String,
    /// Frontend-owned presentation config (sections, grid/theme hints).
    /// Structural JSON only: prose/text blobs are rejected by validation.
    #[sqlx(json)]
    pub layout: serde_json::Value,
    pub item_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct LibraryPlaylistItem {
    pub playlist_id: Uuid,
    pub media_id: Uuid,
    pub position: f64,
    pub added_at: DateTime<Utc>,
}

/// Item joined with its catalog media so frontends can render immediately.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct PlaylistItemWithMedia {
    pub playlist_id: Uuid,
    pub media_id: Uuid,
    pub position: f64,
    pub added_at: DateTime<Utc>,
    pub title: String,
    pub media_type: String,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub release_date: Option<chrono::NaiveDate>,
    pub runtime_minutes: Option<i32>,
    pub rating_avg: Option<f64>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreatePlaylistRequest {
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: PlaylistKind,
    #[serde(default = "default_visibility")]
    pub visibility: String,
    /// Free-form frontend presentation config. Validated: no text content.
    #[serde(default)]
    pub layout: serde_json::Value,
}

fn default_kind() -> PlaylistKind {
    PlaylistKind::Multimedia
}

fn default_visibility() -> String {
    "private".into()
}

#[derive(Debug, Deserialize, Default, utoipa::ToSchema)]
pub struct UpdatePlaylistRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub layout: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AddItemRequest {
    pub media_id: Uuid,
    /// Fractional rank; omit to append at the end of the playlist.
    #[serde(default)]
    pub position: Option<f64>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReplaceItemsRequest {
    pub items: Vec<AddItemRequest>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ReorderRequest {
    /// Media ids in playback order; ranks are rewritten to match.
    pub media_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PlaylistIdPath {
    pub playlist_id: Uuid,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct PlaylistMediaPath {
    pub playlist_id: Uuid,
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListPlaylistsQuery {
    /// Filter by flavor (`multimedia` | `standard`).
    pub kind: Option<crate::library::models::PlaylistKind>,
}
