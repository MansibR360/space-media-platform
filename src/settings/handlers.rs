use axum::Json;
use axum::extract::State;
use serde_json::Value;
use std::sync::Arc;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::settings::models::{UserSettings, validate};
use crate::state::AppState;

#[utoipa::path(get, path = "/api/users/me/settings", tag = "users",
    security(("bearer" = [])),
    responses((status = 200, body = UserSettings))
)]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<UserSettings>> {
    let raw = crate::settings::repo::load_raw(&state.pool, auth.id()).await?;
    let settings: UserSettings = match raw {
        Some(value) => serde_json::from_value(value)
            .map_err(|e| ApiError::internal_msg(format!("stored settings invalid: {e}")))?,
        None => UserSettings::default(),
    };
    Ok(Json(settings))
}

#[utoipa::path(put, path = "/api/users/me/settings", tag = "users",
    security(("bearer" = [])),
    request_body = UserSettings,
    responses((status = 200, body = UserSettings))
)]
pub async fn replace_settings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(settings): Json<UserSettings>,
) -> ApiResult<Json<UserSettings>> {
    validate(&settings)?;
    crate::settings::repo::save(&state.pool, auth.id(), &serde_json::to_value(&settings)?).await?;
    Ok(Json(settings))
}

#[utoipa::path(patch, path = "/api/users/me/settings", tag = "users",
    security(("bearer" = [])),
    request_body = Value,
    responses((status = 200, body = UserSettings), (status = 400))
)]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(patch): Json<Value>,
) -> ApiResult<Json<UserSettings>> {
    let mut current = crate::settings::repo::load_raw(&state.pool, auth.id())
        .await?
        .unwrap_or_else(|| serde_json::json!({}));
    crate::settings::repo::merge_patch(&mut current, patch)?;

    let settings: UserSettings = serde_json::from_value(current)
        .map_err(|e| ApiError::bad_request(format!("invalid settings payload: {e}")))?;
    validate(&settings)?;
    crate::settings::repo::save(&state.pool, auth.id(), &serde_json::to_value(&settings)?).await?;
    Ok(Json(settings))
}
