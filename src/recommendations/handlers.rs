use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecommendationItem {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub score: f64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RecommendationsResponse {
    pub generated_at: String,
    pub cached: bool,
    pub items: Vec<RecommendationItem>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MediaIdPath {
    pub media_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct LimitQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    20
}

#[utoipa::path(get, path = "/api/recommendations", tag = "recommendations", params(LimitQuery),
    security(("bearer" = [])),
    responses((status = 200, body = RecommendationsResponse))
)]
pub async fn for_me(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    axum::extract::Query(query): axum::extract::Query<LimitQuery>,
) -> ApiResult<Json<RecommendationsResponse>> {
    let result = state
        .recommendations
        .for_user(auth.id(), query.limit)
        .await?;
    Ok(Json(RecommendationsResponse {
        generated_at: result.generated_at,
        cached: true,
        items: result
            .items
            .into_iter()
            .map(|item| RecommendationItem {
                media_id: item.media_id,
                title: item.title,
                poster_url: item.poster_url,
                score: item.score,
            })
            .collect(),
    }))
}

#[utoipa::path(get, path = "/api/media/{media_id}/similar-scores", tag = "recommendations", params(MediaIdPath),
    responses((status = 200, body = [RecommendationItem]))
)]
pub async fn similar_scores(
    State(state): State<Arc<AppState>>,
    Path(path): Path<MediaIdPath>,
    axum::extract::Query(query): axum::extract::Query<LimitQuery>,
) -> ApiResult<Json<Vec<RecommendationItem>>> {
    let items = state
        .recommendations
        .similar(path.media_id, query.limit)
        .await?;
    Ok(Json(
        items
            .into_iter()
            .map(|item| RecommendationItem {
                media_id: item.media_id,
                title: item.title,
                poster_url: item.poster_url,
                score: item.score,
            })
            .collect(),
    ))
}
