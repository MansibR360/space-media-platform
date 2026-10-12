use axum::Json;
use axum::extract::{Query as AxQuery, State};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::heuristic::{models, repo, service};
use crate::state::AppState;

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EngineStatus {
    pub enabled: bool,
    pub half_life_days: f64,
    pub signals_learned: i64,
}

/// Tiny liveness/diagnostic endpoint for the learning engine.
#[utoipa::path(get, path = "/api/heuristic/status", tag = "heuristic",
    security(("bearer" = [])),
    responses((status = 200, body = EngineStatus))
)]
pub async fn engine_status(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<EngineStatus>> {
    let totals = repo::totals(&state.pool, auth.id()).await?;
    Ok(Json(EngineStatus {
        enabled: true,
        half_life_days: repo::HALF_LIFE_DAYS,
        signals_learned: totals.total_plays,
    }))
}

/// The full learned behavioral profile for the current user (cached ~90 s).
#[utoipa::path(get, path = "/api/me/heuristic", tag = "heuristic",
    security(("bearer" = [])),
    responses((status = 200, body = models::HeuristicProfile))
)]
pub async fn my_profile(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<models::HeuristicProfile>> {
    let profile = service::build_profile_cached(&state, auth.id()).await?;
    Ok(Json(profile))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct WrappedQuery {
    #[serde(default = "default_period")]
    pub period: String,
}

fn default_period() -> String {
    "year".into()
}

impl WrappedQuery {
    fn parse(&self) -> Result<models::WrappedPeriod, ApiError> {
        match self.period.to_lowercase().as_str() {
            "week" => Ok(models::WrappedPeriod::Week),
            "month" => Ok(models::WrappedPeriod::Month),
            "year" => Ok(models::WrappedPeriod::Year),
            "all" => Ok(models::WrappedPeriod::All),
            other => Err(ApiError::bad_request(format!(
                "unknown period '{other}' (week|month|year|all)"
            ))),
        }
    }
}

/// Spotify-Wrapped style recap over any period.
#[utoipa::path(get, path = "/api/me/heuristic/wrapped", tag = "heuristic",
    security(("bearer" = [])),
    params(WrappedQuery),
    responses((status = 200, body = models::Wrapped), (status = 400))
)]
pub async fn my_wrapped(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxQuery(query): AxQuery<WrappedQuery>,
) -> ApiResult<Json<models::Wrapped>> {
    let period = query.parse()?;
    let wrapped = service::wrapped(&state.pool, auth.id(), period).await?;
    metrics::counter!("heuristic_wrapped_served_total").increment(1);
    Ok(Json(wrapped))
}

#[utoipa::path(get, path = "/api/me/heuristic/insights", tag = "heuristic",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_insights(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<models::Insight>>> {
    let profile = service::build_profile_cached(&state, auth.id()).await?;
    Ok(Json(service::derive_insights(&profile)))
}

/// Explainable next-up picks from the learned profile.
#[utoipa::path(get, path = "/api/me/heuristic/predictions", tag = "heuristic",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_predictions(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<models::PredictedItem>>> {
    Ok(Json(service::predict_next(&state.pool, auth.id()).await?))
}

/// Learned "more like this" for one title: content + collaborative blend.
#[utoipa::path(get, path = "/api/me/heuristic/similar/{media_id}", tag = "heuristic",
    security(("bearer" = [])),
    params(("media_id" = Uuid, Path, description = "anchor title")),
    responses((status = 200), (status = 404))
)]
pub async fn my_similar(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    axum::extract::Path(media_id): axum::extract::Path<Uuid>,
) -> ApiResult<Json<Vec<models::SimilarTitle>>> {
    Ok(Json(
        service::similar_titles(&state.pool, auth.id(), media_id, 12).await?,
    ))
}

#[utoipa::path(delete, path = "/api/me/heuristic", tag = "heuristic",
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn wipe_my_data(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<axum::http::StatusCode> {
    repo::wipe_user(&state.pool, auth.id()).await?;
    state
        .cache
        .invalidate(&format!("heur:user:{}:", auth.id()))
        .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
