use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::lyrics::{self, LyricsQuery, LyricsQueryResult};
use crate::state::AppState;

/// `GET /api/lyrics/providers` — the ordered provider chain with metadata.
#[utoipa::path(get, path = "/api/lyrics/providers", tag = "lyrics",
    responses((status = 200))
)]
pub async fn providers(
    State(_state): State<Arc<AppState>>,
    _auth: CurrentUser,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "providers": lyrics::provider_list(),
        "priority": ["betterlyrics", "lrclib", "netease", "genius"],
    }))
}

/// `GET /api/lyrics` — unified multi-provider lookup.
///
/// Any of these forms work:
/// - `?title=…&artist=…&duration=…`
/// - `?q=Artist - Title`
/// - `?video_id=<youtube id>`
#[utoipa::path(get, path = "/api/lyrics", tag = "lyrics",
    security(("bearer" = [])),
    params(LyricsQuery, ShorthandQuery),
    responses((status = 200, body = LyricsQueryResult), (status = 400))
)]
pub async fn get_lyrics(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    axum::extract::Query(params): axum::extract::Query<ShorthandQuery>,
) -> ApiResult<Json<LyricsQueryResult>> {
    let query = params.into_query()?;
    let started = std::time::Instant::now();
    let result = lyrics::resolve(&state, &query).await?;
    metrics::histogram!("lyrics_resolve_duration_seconds")
        .record(started.elapsed().as_secs_f64());
    Ok(Json(result))
}

/// `GET /api/media/{media_id}/lyrics` — lyrics as media: resolves from any
/// catalog item's title (+ optional artist override) and links the cached
/// row back to the media id so future lookups are instant.
#[utoipa::path(get, path = "/api/media/{media_id}/lyrics", tag = "lyrics",
    security(("bearer" = [])),
    params(("media_id" = uuid::Uuid, description = "catalog media id"), ShorthandQuery),
    responses((status = 200, body = LyricsQueryResult), (status = 404))
)]
pub async fn media_lyrics(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    Path(media_id): Path<Uuid>,
    axum::extract::Query(params): axum::extract::Query<ShorthandQuery>,
) -> ApiResult<Json<LyricsQueryResult>> {
    let media: Option<(String, String)> = sqlx::query_as(
        "SELECT title, COALESCE(original_language, '') FROM media WHERE id = $1",
    )
    .bind(media_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::from)?;

    let Some((title, _lang)) = media else {
        return Err(ApiError::not_found("media"));
    };

    // Prefer explicit overrides; otherwise derive from the catalog title
    // ("Artist - Title" pattern is common for music imports).
    let mut query = params.into_query()?;
    if query.title.as_deref().map(str::trim).unwrap_or("").is_empty() {
        query.title = Some(title);
    }

    let result = lyrics::resolve_for_media(&state, &query, media_id).await?;
    Ok(Json(result))
}

/// Accepts both structured params and a shorthand `q=Artist - Title`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct ShorthandQuery {
    pub q: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<f64>,
    pub video_id: Option<String>,
}

impl ShorthandQuery {
    fn into_query(self) -> ApiResult<LyricsQuery> {
        let mut query = LyricsQuery {
            title: self.title,
            artist: self.artist,
            album: self.album,
            duration: self.duration,
            video_id: self.video_id,
        };
        if let Some(q) = self.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            // "Artist - Title" / "Artist – Title" split wins over raw q when
            // no explicit fields were provided.
            for sep in [" - ", " – ", " — "] {
                if let Some((artist, title)) = q.split_once(sep) {
                    if !artist.trim().is_empty() && !title.trim().is_empty() {
                        if query.artist.is_none() {
                            query.artist = Some(artist.trim().to_string());
                        }
                        if query.title.is_none() {
                            query.title = Some(title.trim().to_string());
                        }
                        return Ok(query);
                    }
                }
            }
            if query.title.is_none() {
                query.title = Some(q.to_string());
            }
        }
        Ok(query)
    }
}
