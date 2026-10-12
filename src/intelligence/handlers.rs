use axum::Json;
use axum::extract::{Path, Query as AxQuery, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::intelligence::models::{
    CollectionRules, EpisodeAir, HomeRails, SmartCollection, TasteProfile, VibeResponse,
};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct VibeQuery {
    /// e.g. "something dark and mind-bending for tonight"
    pub q: String,
    #[serde(default = "default_vibe_limit")]
    pub limit: i64,
}

fn default_vibe_limit() -> i64 {
    18
}

#[utoipa::path(get, path = "/api/intelligence/vibe", tag = "intelligence", params(VibeQuery),
    security(("bearer" = [])),
    responses((status = 200, body = VibeResponse))
)]
pub async fn vibe_search(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxQuery(query): AxQuery<VibeQuery>,
) -> ApiResult<Json<VibeResponse>> {
    let _ = auth;
    let result =
        crate::intelligence::service::vibe_search(&state.pool, &query.q, query.limit).await?;
    Ok(Json(result))
}

/// Public list of moods for UI chips.
#[derive(Debug, Serialize)]
pub struct MoodChip {
    pub id: String,
    pub label: &'static str,
}

#[utoipa::path(get, path = "/api/intelligence/moods", tag = "intelligence",
    responses((status = 200))
)]
pub async fn moods() -> Json<Vec<MoodChip>> {
    Json(
        crate::intelligence::models::VIBES
            .iter()
            .map(|spec| MoodChip {
                id: format!("{:?}", spec.vibe).to_lowercase(),
                label: spec.label,
            })
            .collect(),
    )
}

#[utoipa::path(get, path = "/api/me/taste", tag = "intelligence",
    security(("bearer" = [])),
    responses((status = 200, body = TasteProfile))
)]
pub async fn my_taste(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<TasteProfile>> {
    let profile = crate::intelligence::service::taste_profile(&state.pool, auth.id()).await?;
    Ok(Json(profile))
}

#[derive(Debug, Serialize, ToSchema)]
#[allow(dead_code)]
pub struct NewEpisodesRail {
    pub items: Vec<EpisodeAir>,
}

#[utoipa::path(get, path = "/api/home", tag = "intelligence",
    security(("bearer" = [])),
    responses((status = 200, body = HomeRails))
)]
pub async fn home(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<HomeRails>> {
    let rails = crate::intelligence::service::home_rails(&state.pool, &state, auth.id()).await?;
    Ok(Json(rails))
}

// ---------- smart collections ----------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCollectionRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub rules: CollectionRules,
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

fn default_visibility() -> String {
    "private".into()
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CollectionIdPath {
    pub collection_id: Uuid,
}

#[utoipa::path(post, path = "/api/collections", tag = "collections",
    security(("bearer" = [])),
    request_body = CreateCollectionRequest,
    responses((status = 201, body = SmartCollection), (status = 400))
)]
pub async fn create_collection(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreateCollectionRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<SmartCollection>)> {
    if !crate::plugins::models::valid_plugin_name(&req.name.replace([' '], "-")) {
        return Err(ApiError::bad_request(
            "collection name must be 1-48 simple characters",
        ));
    }
    if req.rules.genres.len() > 12 {
        return Err(ApiError::bad_request("at most 12 genres per collection"));
    }

    let collection = crate::intelligence::repo::create_collection(
        &state.pool,
        auth.id(),
        &req.name.trim().to_string(),
        &req.description.trim().to_string(),
        &serde_json::to_value(&req.rules)?,
        &req.visibility,
    )
    .await?;
    Ok((axum::http::StatusCode::CREATED, Json(collection)))
}

#[utoipa::path(get, path = "/api/collections/{collection_id}/items", tag = "collections",
    params(CollectionIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = [crate::catalog::models::Media]), (status = 403))
)]
pub async fn collection_items(
    State(state): State<Arc<AppState>>,
    maybe_user: Option<CurrentUser>,
    Path(path): Path<CollectionIdPath>,
) -> ApiResult<Json<Vec<crate::catalog::models::Media>>> {
    let collection = crate::intelligence::repo::get_collection(&state.pool, path.collection_id)
        .await?
        .ok_or_else(|| ApiError::not_found("collection"))?;

    let viewer = maybe_user.map(|u| u.id());
    if collection.visibility != "public" && collection.owner_id != viewer.unwrap_or_default() {
        return Err(ApiError::forbidden("private collection"));
    }

    let rules: CollectionRules = serde_json::from_value(collection.rules.clone())
        .map_err(|e| ApiError::internal_msg(format!("stored rules invalid: {e}")))?;

    let filter = rules.to_filter();
    let (items, _) = crate::catalog::repo::list(&state.pool, &filter, 60, None).await?;
    Ok(Json(items))
}

#[utoipa::path(delete, path = "/api/collections/{collection_id}", tag = "collections",
    params(CollectionIdPath),
    security(("bearer" = [])),
    responses((status = 204), (status = 403))
)]
pub async fn delete_collection(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<CollectionIdPath>,
) -> ApiResult<axum::http::StatusCode> {
    crate::intelligence::repo::delete_collection(&state.pool, auth.id(), path.collection_id)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
