//! Default [`PlaylistStore`] backend: PostgreSQL.
//!
//! All playlist SQL lives here behind the SPI — handlers never touch queries,
//! so alternative backends can replace this file wholesale.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::library::models::{
    AddItemRequest, CreatePlaylistRequest, LibraryPlaylist, PlaylistItemWithMedia,
    ReplaceItemsRequest, ReorderRequest, UpdatePlaylistRequest,
};
use crate::library::store::{PlaylistStore, ensure_media_only};
use crate::utils::{new_id, now};

pub struct PgPlaylistStore {
    pool: PgPool,
}

impl PgPlaylistStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Playlist row with live item count.
    ///
    /// When `viewer` differs from the owner only public playlists resolve;
    /// internal callers pass the owner id they already authenticated against.
    async fn fetch(&self, playlist_id: Uuid, viewer: Uuid) -> ApiResult<LibraryPlaylist> {
        sqlx::query_as::<_, LibraryPlaylist>(
            "SELECT p.id AS id, p.user_id AS user_id, p.name AS name, p.kind AS kind,
                    p.media_type AS media_type, p.visibility AS visibility,
                    p.layout AS layout, p.created_at AS created_at,
                    p.updated_at AS updated_at,
                    COUNT(i.media_id)::bigint AS item_count
             FROM library_playlists p
             LEFT JOIN library_playlist_items i ON i.playlist_id = p.id
             WHERE p.id = $1 AND (p.user_id = $2 OR p.visibility = 'public')
             GROUP BY p.id",
        )
        .bind(playlist_id)
        .bind(viewer)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("playlist"))
    }

    async fn items(&self, playlist_id: Uuid) -> ApiResult<Vec<PlaylistItemWithMedia>> {
        sqlx::query_as::<_, PlaylistItemWithMedia>(
            "SELECT i.playlist_id AS playlist_id, i.media_id AS media_id,
                    i.position AS position, i.added_at AS added_at,
                    m.title AS title, m.media_type AS media_type,
                    m.poster_url AS poster_url, m.backdrop_url AS backdrop_url,
                    m.release_date AS release_date, m.runtime_minutes AS runtime_minutes,
                    m.rating_avg AS rating_avg
             FROM library_playlist_items i
             JOIN media m ON m.id = i.media_id
             WHERE i.playlist_id = $1
             ORDER BY i.position, i.added_at",
        )
        .bind(playlist_id)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    async fn require_owned(&self, playlist_id: Uuid, owner: Uuid) -> ApiResult<()> {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM library_playlists WHERE id = $1 AND user_id = $2",
        )
        .bind(playlist_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?
        .map(|_| ())
        .ok_or_else(|| ApiError::not_found("playlist"))
    }
}

#[async_trait]
impl PlaylistStore for PgPlaylistStore {
    fn backend(&self) -> &'static str {
        "postgres"
    }

    async fn create_playlist(
        &self,
        owner: Uuid,
        req: &CreatePlaylistRequest,
    ) -> ApiResult<LibraryPlaylist> {
        let name = validate_name(&req.name)?;
        validate_visibility(&req.visibility)?;
        ensure_media_only(&req.layout)?;

        let mut tx = self.pool.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO library_playlists (id, user_id, name, kind, visibility, layout)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(new_id())
        .bind(owner)
        .bind(&name)
        .bind(req.kind)
        .bind(&req.visibility)
        .bind(&req.layout)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;

        self.fetch(id, owner).await
    }

    async fn list_playlists(&self, owner: Uuid) -> ApiResult<Vec<LibraryPlaylist>> {
        sqlx::query_as::<_, LibraryPlaylist>(
            "SELECT p.id AS id, p.user_id AS user_id, p.name AS name, p.kind AS kind,
                    p.media_type AS media_type, p.visibility AS visibility,
                    p.layout AS layout, p.created_at AS created_at,
                    p.updated_at AS updated_at,
                    COUNT(i.media_id)::bigint AS item_count
             FROM library_playlists p
             LEFT JOIN library_playlist_items i ON i.playlist_id = p.id
             WHERE p.user_id = $1
             GROUP BY p.id
             ORDER BY p.updated_at DESC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await
        .map_err(Into::into)
    }

    async fn get_playlist(&self, playlist_id: Uuid, viewer: Uuid) -> ApiResult<LibraryPlaylist> {
        self.fetch(playlist_id, viewer).await
    }

    async fn update_playlist(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        patch: &UpdatePlaylistRequest,
    ) -> ApiResult<LibraryPlaylist> {
        self.require_owned(playlist_id, owner).await?;
        if let Some(name) = &patch.name {
            validate_name(name)?;
        }
        if let Some(visibility) = &patch.visibility {
            validate_visibility(visibility)?;
        }
        if let Some(layout) = &patch.layout {
            ensure_media_only(layout)?;
        }

        sqlx::query(
            "UPDATE library_playlists SET
                name = COALESCE($3, name),
                visibility = COALESCE($4, visibility),
                layout = COALESCE($5, layout),
                updated_at = $6
             WHERE id = $1 AND user_id = $2",
        )
        .bind(playlist_id)
        .bind(owner)
        .bind(&patch.name)
        .bind(&patch.visibility)
        .bind(&patch.layout)
        .bind(now())
        .execute(&self.pool)
        .await?;

        self.fetch(playlist_id, owner).await
    }

    async fn delete_playlist(&self, playlist_id: Uuid, owner: Uuid) -> ApiResult<()> {
        self.require_owned(playlist_id, owner).await?;
        sqlx::query("DELETE FROM library_playlists WHERE id = $1")
            .bind(playlist_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn add_item(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &AddItemRequest,
    ) -> ApiResult<LibraryPlaylist> {
        let mut tx = self.pool.begin().await?;

        let pinned: Option<Option<String>> = sqlx::query_scalar(
            "SELECT media_type FROM library_playlists
             WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(playlist_id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        let pinned = pinned.ok_or_else(|| ApiError::not_found("playlist"))?;

        // Media-only rule: the reference must resolve to real catalog media.
        let media_type: String =
            sqlx::query_scalar("SELECT media_type FROM media WHERE id = $1")
                .bind(req.media_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| ApiError::not_found("media"))?;

        if let Some(expected) = &pinned {
            if expected != &media_type {
                return Err(ApiError::bad_request(format!(
                    "standard playlist is pinned to '{expected}', cannot add '{media_type}'"
                )));
            }
        }

        let position = match req.position {
            Some(p) => p.max(0.0),
            None => {
                let max: Option<f64> = sqlx::query_scalar(
                    "SELECT MAX(position) FROM library_playlist_items WHERE playlist_id = $1",
                )
                .bind(playlist_id)
                .fetch_one(&mut *tx)
                .await?;
                max.unwrap_or(0.0) + 1024.0
            }
        };

        sqlx::query(
            "INSERT INTO library_playlist_items (playlist_id, media_id, position)
             VALUES ($1, $2, $3)
             ON CONFLICT (playlist_id, media_id)
             DO UPDATE SET position = EXCLUDED.position",
        )
        .bind(playlist_id)
        .bind(req.media_id)
        .bind(position)
        .execute(&mut *tx)
        .await?;

        // Standard playlists take their type pin from the first item added.
        sqlx::query(
            "UPDATE library_playlists
             SET media_type = COALESCE(media_type, $2), updated_at = $3
             WHERE id = $1",
        )
        .bind(playlist_id)
        .bind(&media_type)
        .bind(now())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        self.fetch(playlist_id, owner).await
    }

    async fn remove_item(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        media_id: Uuid,
    ) -> ApiResult<()> {
        self.require_owned(playlist_id, owner).await?;
        let result = sqlx::query(
            "DELETE FROM library_playlist_items WHERE playlist_id = $1 AND media_id = $2",
        )
        .bind(playlist_id)
        .bind(media_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("playlist item"));
        }
        sqlx::query("UPDATE library_playlists SET updated_at = $2 WHERE id = $1")
            .bind(playlist_id)
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_items(
        &self,
        playlist_id: Uuid,
        viewer: Uuid,
    ) -> ApiResult<Vec<PlaylistItemWithMedia>> {
        self.fetch(playlist_id, viewer).await?;
        self.items(playlist_id).await
    }

    async fn replace_items(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &ReplaceItemsRequest,
    ) -> ApiResult<Vec<PlaylistItemWithMedia>> {
        let mut tx = self.pool.begin().await?;

        let pinned: Option<Option<String>> = sqlx::query_scalar(
            "SELECT media_type FROM library_playlists
             WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(playlist_id)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        let pinned = pinned.ok_or_else(|| ApiError::not_found("playlist"))?;

        // Validate every reference before touching rows (all-or-nothing swap).
        let mut new_pin: Option<String> = pinned.clone();
        for item in &req.items {
            let media_type: String =
                sqlx::query_scalar("SELECT media_type FROM media WHERE id = $1")
                    .bind(item.media_id)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| ApiError::not_found("media"))?;
            match &new_pin {
                Some(expected) if expected != &media_type => {
                    return Err(ApiError::bad_request(format!(
                        "standard playlist is pinned to '{expected}', found '{media_type}'"
                    )));
                }
                Some(_) => {}
                None => new_pin = Some(media_type),
            }
        }

        sqlx::query("DELETE FROM library_playlist_items WHERE playlist_id = $1")
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;

        for (idx, item) in req.items.iter().enumerate() {
            let position = item.position.unwrap_or(idx as f64 * 1024.0);
            sqlx::query(
                "INSERT INTO library_playlist_items (playlist_id, media_id, position)
                 VALUES ($1, $2, $3)",
            )
            .bind(playlist_id)
            .bind(item.media_id)
            .bind(position.max(0.0))
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "UPDATE library_playlists SET media_type = $2, updated_at = $3 WHERE id = $1",
        )
        .bind(playlist_id)
        .bind(new_pin)
        .bind(now())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        self.items(playlist_id).await
    }

    async fn reorder(
        &self,
        playlist_id: Uuid,
        owner: Uuid,
        req: &ReorderRequest,
    ) -> ApiResult<()> {
        self.require_owned(playlist_id, owner).await?;
        let mut tx = self.pool.begin().await?;
        for (idx, media_id) in req.media_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE library_playlist_items SET position = $3
                 WHERE playlist_id = $1 AND media_id = $2",
            )
            .bind(playlist_id)
            .bind(media_id)
            .bind(idx as f64 * 1024.0)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE library_playlists SET updated_at = $2 WHERE id = $1")
            .bind(playlist_id)
            .bind(now())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn validate_name(name: &str) -> ApiResult<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 120 {
        return Err(ApiError::bad_request(
            "playlist name must be 1..=120 characters",
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_visibility(visibility: &str) -> ApiResult<()> {
    match visibility {
        "private" | "public" | "followers" => Ok(()),
        _ => Err(ApiError::bad_request(
            "visibility must be private|public|followers",
        )),
    }
}
