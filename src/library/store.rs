//! Playlist persistence SPI (Service Provider Interface).
//!
//! The HTTP layer only talks to the [`PlaylistStore`] trait — never to SQL
//! directly. That keeps playlist storage swappable: implement the trait for a
//! new backend (Redis, S3, encrypted vault, another database engine ...) and
//! register it in one place via [`build_store`].
//!
//! Invariants every backend MUST enforce (see [`ensure_media_only`]):
//! 1. Items are references to real catalog media only.
//! 2. No outside text: free-form prose/content blobs are rejected at the API
//!    boundary before they ever reach a backend.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::library::models::{
    AddItemRequest, CreatePlaylistRequest, LibraryPlaylist, PlaylistItemWithMedia,
    ReplaceItemsRequest, ReorderRequest, UpdatePlaylistRequest,
};

#[async_trait]
pub trait PlaylistStore: Send + Sync {
    /// Backend identifier surfaced to clients (e.g. `"postgres"`).
    fn backend(&self) -> &'static str;

    async fn create_playlist(
        &self,
        owner: Uuid,
        req: &CreatePlaylistRequest,
    ) -> ApiResult<LibraryPlaylist>;

    async fn list_playlists(&self, owner: Uuid) -> ApiResult<Vec<LibraryPlaylist>>;

    /// Owner or public visibility; anything else is `NotFound`.
    async fn get_playlist(&self, playlist_id: Uuid, viewer: Uuid)
        -> ApiResult<LibraryPlaylist>;

    async fn update_playlist(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        patch: &UpdatePlaylistRequest,
    ) -> ApiResult<LibraryPlaylist>;

    async fn delete_playlist(&self, playlist_id: Uuid, owner: Uuid) -> ApiResult<()>;

    /// Appends/positions a single catalog-media reference. Returns the
    /// refreshed playlist (item count + standard-kind type pin may change).
    async fn add_item(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &AddItemRequest,
    ) -> ApiResult<LibraryPlaylist>;

    async fn remove_item(&self, playlist_id: Uuid, owner: Uuid, media_id: Uuid)
        -> ApiResult<()>;

    async fn list_items(
        &self,
        playlist_id: Uuid,
        viewer: Uuid,
    ) -> ApiResult<Vec<PlaylistItemWithMedia>>;

    /// Wholesale replacement of the item list (import / full reorder).
    async fn replace_items(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &ReplaceItemsRequest,
    ) -> ApiResult<Vec<PlaylistItemWithMedia>>;

    /// Rewrites ranks so items play back in the given order.
    async fn reorder(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &ReorderRequest,
    ) -> ApiResult<()>;
}

/// Storage backend selection (`PLAYLIST_STORE_BACKEND`, default `postgres`).
pub fn build_store(cfg: &Config, pool: sqlx::PgPool) -> ApiResult<Arc<dyn PlaylistStore>> {
    match cfg.playlist_store_backend.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "" => {
            Ok(Arc::new(super::repo::PgPlaylistStore::new(pool)))
        }
        other => Err(ApiError::bad_request(format!(
            "unknown PLAYLIST_STORE_BACKEND '{other}' (supported: postgres)"
        ))),
    }
}

// ------------------------- media-only text guard -------------------------

/// Longest string allowed anywhere inside user-supplied JSON. Enough for short
/// labels/tokens the frontend needs for rendering; far too short for prose.
pub const MAX_EMBEDDED_STRING: usize = 120;

/// Keys that carry written content. Playlists are media containers, not
/// documents — these are rejected wherever they appear in the JSON tree.
const TEXT_KEYS: &[&str] = &[
    "text",
    "content",
    "body",
    "note",
    "notes",
    "description",
    "caption",
    "transcript",
    "lyrics",
    "script",
    "article",
    "essay",
    "comment",
    "comments",
    "subtitle_text",
];

/// Rejects outside text in frontend-supplied JSON (`layout` payloads).
///
/// Rules:
/// - any key in [`TEXT_KEYS`] (case-insensitive) → rejected
/// - any string longer than [`MAX_EMBEDDED_STRING`] chars → rejected
/// - arrays/objects/nested structures are walked recursively
///
/// Structural config ("columns", "#ff0050", "grid", section ids) passes; prose
/// does not. This is what makes "playlists hold media, nothing else" hold even
/// though frontends can shape their layout however they want.
pub fn ensure_media_only(value: &serde_json::Value) -> ApiResult<()> {
    if let Err(path) = walk(value, "$") {
        return Err(ApiError::bad_request(format!(
            "playlists support media only, no outside text (rejected at {path})"
        )));
    }
    Ok(())
}

fn walk(value: &serde_json::Value, path: &str) -> Result<(), String> {
    match value {
        serde_json::Value::String(s) => {
            if s.chars().count() > MAX_EMBEDDED_STRING {
                return Err(format!(
                    "{path}: string exceeds {MAX_EMBEDDED_STRING} chars"
                ));
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, format!("{path}[{i}]").as_str())?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if TEXT_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                    return Err(format!("{path}.{key}: text content key"));
                }
                walk(item, format!("{path}.{key}").as_str())?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structural_layout_passes() {
        let layout = json!({
            "view": "grid",
            "theme": { "accent": "#ff0050", "density": "compact" },
            "sections": [
                { "id": "s1", "match": { "media_type": ["movie"] }, "span": 2 },
                { "id": "s2", "order": "added_desc" }
            ],
            "tags": ["roadtrip", "2026"]
        });
        assert!(ensure_media_only(&layout).is_ok());
    }

    #[test]
    fn rejects_prose_blobs() {
        let essay = "a".repeat(MAX_EMBEDDED_STRING + 1);
        assert!(ensure_media_only(&json!({ "summary": essay })).is_err());
    }

    #[test]
    fn rejects_text_keys_anywhere() {
        assert!(ensure_media_only(&json!({ "meta": { "Notes": "hello" } })).is_err());
        assert!(ensure_media_only(&json!([{ "transcript": "line" }])).is_err());
        assert!(ensure_media_only(&json!({ "lyrics": "" })).is_err());
    }

    #[test]
    fn allows_short_strings_and_non_strings() {
        assert!(ensure_media_only(&json!({ "n": 1, "ok": true, "s": "short" })).is_ok());
    }
}
