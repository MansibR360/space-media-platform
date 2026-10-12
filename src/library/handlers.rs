use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use crate::auth::middleware::CurrentUser;
use crate::error::ApiResult;
use crate::library::models::{
    AddItemRequest, CreatePlaylistRequest, LibraryPlaylist, PlaylistIdPath,
    PlaylistItemWithMedia, PlaylistMediaPath, ListPlaylistsQuery, ReplaceItemsRequest,
    ReorderRequest, UpdatePlaylistRequest,
};
use crate::state::AppState;

#[utoipa::path(post, path = "/api/library/playlists", tag = "library",
    security(("bearer" = [])),
    request_body = CreatePlaylistRequest,
    responses((status = 201, body = LibraryPlaylist), (status = 400))
)]
pub async fn create_playlist(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreatePlaylistRequest>,
) -> ApiResult<(StatusCode, Json<LibraryPlaylist>)> {
    let playlist = state.playlists.create_playlist(auth.id(), &req).await?;
    Ok((StatusCode::CREATED, Json(playlist)))
}

#[utoipa::path(get, path = "/api/library/playlists", tag = "library",
    security(("bearer" = [])),
    params(ListPlaylistsQuery),
    responses((status = 200, body = [LibraryPlaylist]))
)]
pub async fn my_playlists(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Query(q): Query<ListPlaylistsQuery>,
) -> ApiResult<Json<Vec<LibraryPlaylist>>> {
    let mut playlists = state.playlists.list_playlists(auth.id()).await?;
    if let Some(kind) = q.kind {
        playlists.retain(|p| p.kind == kind);
    }
    Ok(Json(playlists))
}

#[utoipa::path(get, path = "/api/library/playlists/{playlist_id}", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    responses((status = 200, body = LibraryPlaylist), (status = 404))
)]
pub async fn get_playlist(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
) -> ApiResult<Json<LibraryPlaylist>> {
    Ok(Json(
        state.playlists.get_playlist(path.playlist_id, auth.id()).await?,
    ))
}

#[utoipa::path(patch, path = "/api/library/playlists/{playlist_id}", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    request_body = UpdatePlaylistRequest,
    responses((status = 200, body = LibraryPlaylist), (status = 404))
)]
pub async fn update_playlist(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
    Json(req): Json<UpdatePlaylistRequest>,
) -> ApiResult<Json<LibraryPlaylist>> {
    Ok(Json(
        state
            .playlists
            .update_playlist(path.playlist_id, auth.id(), &req)
            .await?,
    ))
}

#[utoipa::path(delete, path = "/api/library/playlists/{playlist_id}", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    responses((status = 204), (status = 404))
)]
pub async fn delete_playlist(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
) -> ApiResult<StatusCode> {
    state.playlists.delete_playlist(path.playlist_id, auth.id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/library/playlists/{playlist_id}/items", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    responses((status = 200, body = [PlaylistItemWithMedia]), (status = 404))
)]
pub async fn list_items(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
) -> ApiResult<Json<Vec<PlaylistItemWithMedia>>> {
    Ok(Json(
        state.playlists.list_items(path.playlist_id, auth.id()).await?,
    ))
}

#[utoipa::path(post, path = "/api/library/playlists/{playlist_id}/items", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    request_body = AddItemRequest,
    responses((status = 200, body = LibraryPlaylist), (status = 404))
)]
pub async fn add_item(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
    Json(req): Json<AddItemRequest>,
) -> ApiResult<Json<LibraryPlaylist>> {
    Ok(Json(
        state
            .playlists
            .add_item(path.playlist_id, auth.id(), &req)
            .await?,
    ))
}

/// Wholesale item swap: import, dedupe, or full rewrite of the playlist.
#[utoipa::path(put, path = "/api/library/playlists/{playlist_id}/items", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    request_body = ReplaceItemsRequest,
    responses((status = 200, body = [PlaylistItemWithMedia]), (status = 404))
)]
pub async fn replace_items(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
    Json(req): Json<ReplaceItemsRequest>,
) -> ApiResult<Json<Vec<PlaylistItemWithMedia>>> {
    Ok(Json(
        state
            .playlists
            .replace_items(path.playlist_id, auth.id(), &req)
            .await?,
    ))
}

#[utoipa::path(delete, path = "/api/library/playlists/{playlist_id}/items/{media_id}", tag = "library",
    security(("bearer" = [])),
    params(PlaylistMediaPath),
    responses((status = 204), (status = 404))
)]
pub async fn remove_item(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistMediaPath>,
) -> ApiResult<StatusCode> {
    state
        .playlists
        .remove_item(path.playlist_id, auth.id(), path.media_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/library/playlists/{playlist_id}/order", tag = "library",
    security(("bearer" = [])),
    params(PlaylistIdPath),
    request_body = ReorderRequest,
    responses((status = 200), (status = 404))
)]
pub async fn reorder(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PlaylistIdPath>,
    Json(req): Json<ReorderRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    state.playlists.reorder(path.playlist_id, auth.id(), &req).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Which storage backend is serving playlist persistence (SPI surface).
#[utoipa::path(get, path = "/api/library/storage", tag = "library",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn storage_info(State(state): State<Arc<AppState>>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({
        "backend": state.playlists.backend(),
        "media_only": true,
        "max_embedded_string": crate::library::store::MAX_EMBEDDED_STRING,
    })))
}
