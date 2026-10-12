use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::catalog::models::Media;
use crate::catalog::repo;
use crate::error::ApiResult;
use crate::pagination::Page;
use crate::search::models::{SearchHit, SearchQuery, TrendWindow};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SimilarQuery {
    #[serde(default = "default_similar_limit")]
    pub limit: i64,
}

fn default_similar_limit() -> i64 {
    12
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct TrendingQuery {
    /// day | week | month
    pub window: Option<String>,
    #[serde(default = "default_trending_limit")]
    pub limit: i64,
}

fn default_trending_limit() -> i64 {
    20
}

// ListMediaQuery is already flat because serde_urlencoded hands flattened
// fields through as strings; wrapping it in another #[serde(flatten)] put that
// bug straight back, so it is aliased rather than nested.
pub type DiscoverQuery = crate::catalog::handlers::ListMediaQuery;

#[utoipa::path(get, path = "/api/search", tag = "search", params(SearchQuery),
    responses((status = 200, body = [SearchHit]))
)]
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<Page<SearchHit>>> {
    let limit = query.page().clamp_limit();
    let offset = query
        .cursor
        .as_deref()
        .and_then(|c| crate::pagination::decode_cursor(c).ok())
        .and_then(|v| v.get("o").and_then(|o| o.as_i64()))
        .unwrap_or(0);

    let mut hits = crate::search::repo::full_text(&state.pool, &query.q, limit + 1, offset).await?;

    let next_cursor = (hits.len() as i64 > limit).then(|| {
        hits.truncate(limit as usize);
        crate::pagination::encode_cursor(&serde_json::json!({ "o": offset + limit }))
    });

    Ok(Json(Page::with_cursor(hits, next_cursor)))
}

/// Debug/preview endpoint: shows how a free-form query is parsed into filters.
#[utoipa::path(get, path = "/api/search/parse", tag = "search",
    params(ParseQueryParams),
    responses((status = 200))
)]
pub async fn parse_query(
    axum::extract::Query(params): axum::extract::Query<ParseQueryParams>,
) -> Json<crate::search::parser::ParsedQuery> {
    Json(crate::search::parser::parse(&params.q))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ParseQueryParams {
    pub q: String,
}

/// Typo-tolerant "did you mean" suggestions (Phase 48 slice).
#[utoipa::path(get, path = "/api/search/suggest", tag = "search",
    params(ParseQueryParams),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn suggest(
    State(state): State<Arc<AppState>>,
    _auth: Option<CurrentUser>,
    axum::extract::Query(params): axum::extract::Query<ParseQueryParams>,
) -> ApiResult<Json<Vec<crate::search::suggest::Suggestion>>> {
    let suggestions = crate::search::suggest::suggest(&state.pool, &params.q, 8).await?;
    Ok(Json(suggestions))
}

#[utoipa::path(get, path = "/api/discover", tag = "search", params(DiscoverQuery),
    responses((status = 200, body = [Media]))
)]
pub async fn discover(
    State(state): State<Arc<AppState>>,
    _maybe_user: Option<crate::auth::middleware::CurrentUser>,
    Query(query): Query<DiscoverQuery>,
) -> ApiResult<Json<Page<Media>>> {
    let inner = query;
    // Free-form queries are run through the structured parser: tokens like
    // `action`, `2020-2025`, `1080p` or `english` become filters, remaining
    // words stay as a title filter.
    let (parsed_q, extra) = match inner.q.as_deref() {
        Some(q) if !q.trim().is_empty() => {
            let pq = crate::search::parser::parse(q);
            (
                (!pq.keywords.is_empty()).then(|| pq.keywords.join(" ")),
                Some(pq),
            )
        }
        _ => (inner.q.clone(), None),
    };

    let parsed = extra.unwrap_or_default();
    let filter = repo::ListFilter {
        media_type: inner.media_type.or(parsed.media_type),
        is_anime: inner.anime,
        status: inner.status,
        genres: {
            let mut genres = inner
                .genre
                .as_deref()
                .map(|g| {
                    g.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for slug in parsed.genre_slugs {
                if !genres.contains(&slug) {
                    genres.push(slug);
                }
            }
            genres
        },
        studio: inner.studio,
        year_from: inner.year_from.or(parsed.year_from),
        year_to: inner.year_to.or(parsed.year_to),
        min_rating: inner.min_rating,
        person: inner.person.clone(),
        language: inner.language.or(parsed.language),
        min_height: inner.min_height,
        codec: inner.codec.clone(),
        q: parsed_q.clone().or_else(|| inner.q.clone()),
        sort: inner
            .sort
            .as_deref()
            .and_then(repo::Sort::parse)
            .unwrap_or_default(),
    };

    let (items, next_cursor) = repo::list(
        &state.pool,
        &filter,
        inner.limit.unwrap_or(20).clamp(1, 100),
        inner.cursor.as_deref(),
    )
    .await?;
    Ok(Json(Page::with_cursor(items, next_cursor)))
}

#[utoipa::path(get, path = "/api/media/{media_id}/similar", tag = "search", params(MediaIdPath, SimilarQuery),
    responses((status = 200, body = [Media]))
)]
pub async fn similar_media(
    State(state): State<Arc<AppState>>,
    Path(path): Path<MediaIdPath>,
    Query(query): Query<SimilarQuery>,
) -> ApiResult<Json<Vec<Media>>> {
    let items =
        crate::search::repo::similar(&state.pool, path.media_id, query.limit.clamp(1, 50)).await?;
    Ok(Json(items))
}

#[utoipa::path(get, path = "/api/trending", tag = "search", params(TrendingQuery),
    responses((status = 200, body = [SearchHit]))
)]
pub async fn trending(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TrendingQuery>,
) -> ApiResult<Json<Vec<SearchHit>>> {
    let window = match query.window.as_deref() {
        Some("day") => TrendWindow::Day,
        Some("month") => TrendWindow::Month,
        _ => TrendWindow::Week,
    };
    let items =
        crate::search::repo::trending(&state.pool, window, query.limit.clamp(1, 100)).await?;
    Ok(Json(items))
}

/// What the persistent search cache holds and how much work it has saved.
#[utoipa::path(get, path = "/api/admin/search-cache", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn search_cache_stats(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::state::AppState>>,
    _admin: crate::auth::middleware::AdminUser,
) -> axum::Json<serde_json::Value> {
    axum::Json(crate::search::cache::stats(&state.pool).await)
}
