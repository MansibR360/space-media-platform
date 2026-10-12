//! Routes for the universal media graph.

use axum::Json;
use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::ApiResult;
use crate::graph::repo;
use crate::state::AppState;

pub fn graph_router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/media/{id}/related",
            get(related_media),
        )
        .route("/api/media/{id}/journey", get(media_journey))
        .route(
            "/api/media/{id}/relationships",
            post(add_relationship).get(list_relationships),
        )
        .route(
            "/api/media/{id}/relationships/{to_media_id}/{relation}",
            axum::routing::delete(remove_relationship),
        )
        .route("/api/discover/journeys", get(my_journeys))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct AddRelationshipRequest {
    pub to_media_id: Uuid,
    pub relation: String,
    #[serde(default)]
    pub source: Option<String>,
}

/// Links two media items (admin; also an AI-whitelisted action).
#[utoipa::path(post, path = "/api/media/{id}/relationships", tag = "graph",
    security(("bearer" = [])),
    request_body = AddRelationshipRequest,
    responses((status = 201, body = repo::Relationship), (status = 400))
)]
pub async fn add_relationship(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
    Json(req): Json<AddRelationshipRequest>,
) -> ApiResult<(StatusCode, Json<repo::Relationship>)> {
    let rel = repo::add(
        &state.pool,
        id,
        req.to_media_id,
        &req.relation,
        req.source.as_deref().unwrap_or("manual"),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(rel)))
}

#[utoipa::path(get, path = "/api/media/{id}/relationships", tag = "graph",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn list_relationships(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let outgoing: Vec<repo::Relationship> = sqlx::query_as(
        "SELECT * FROM media_relationships WHERE from_media_id = $1 ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    let incoming: Vec<repo::Relationship> = sqlx::query_as(
        "SELECT * FROM media_relationships WHERE to_media_id = $1 ORDER BY created_at",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "outgoing": outgoing, "incoming": incoming })))
}

#[utoipa::path(delete, path = "/api/media/{id}/relationships/{to_media_id}/{relation}", tag = "graph",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn remove_relationship(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxPath((id, to_media_id, relation)): AxPath<(Uuid, Uuid, String)>,
) -> ApiResult<StatusCode> {
    if !repo::remove(&state.pool, id, to_media_id, &relation).await? {
        return Err(crate::error::ApiError::not_found("relationship"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Every media connected to this one — grouped client-side by relation.
#[utoipa::path(get, path = "/api/media/{id}/related", tag = "graph",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn related_media(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let items = repo::related(&state.pool, id).await?;
    let mut grouped: std::collections::BTreeMap<String, Vec<&repo::RelatedItem>> =
        std::collections::BTreeMap::new();
    for item in &items {
        grouped.entry(item.relation.clone()).or_default().push(item);
    }
    Ok(Json(serde_json::json!({
        "media_id": id,
        "count": items.len(),
        "relations": grouped,
    })))
}

/// Franchise traversal from this media (BFS ≤ 3 hops).
#[utoipa::path(get, path = "/api/media/{id}/journey", tag = "graph",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn media_journey(
    State(state): State<Arc<AppState>>,
    _auth: CurrentUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let nodes = repo::journey(&state.pool, id, 3).await?;
    Ok(Json(serde_json::json!({ "root": id, "nodes": nodes })))
}

/// Cross-media journeys for me: adaptation chains I haven't consumed yet.
#[utoipa::path(get, path = "/api/discover/journeys", tag = "graph",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_journeys(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let journeys = repo::journeys_for_user(&state.pool, auth.id(), 6).await?;
    metrics::counter!("journeys_served_total").increment(journeys.len() as u64);
    Ok(Json(serde_json::json!({ "journeys": journeys })))
}
