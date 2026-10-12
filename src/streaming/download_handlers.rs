use axum::Json;
use axum::extract::{Path as AxPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::streaming::download::{self, DownloadTicket};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RequestDownloadRequest {
    /// binds the URL to a registered device (recommended)
    #[serde(default)]
    pub device_id: Option<Uuid>,
    /// seconds; default 1800, hard max 86400
    #[serde(default = "default_ttl")]
    pub ttl_secs: i64,
}

fn default_ttl() -> i64 {
    1800
}

#[utoipa::path(post, path = "/api/files/{file_id}/download-url", tag = "downloads",
    params(crate::streaming::handlers::FileIdPath),
    security(("bearer" = [])),
    request_body = RequestDownloadRequest,
    responses((status = 200, body = DownloadTicket), (status = 403))
)]
pub async fn request_download_url(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    AxPath(path): AxPath<crate::streaming::handlers::FileIdPath>,
    body: Option<Json<RequestDownloadRequest>>,
) -> ApiResult<Json<DownloadTicket>> {
    // Downloads carry media out the door — card gate applies here too.
    crate::cards::ensure_card_access(&state.pool, auth.id()).await?;
    if let Some(device_id) = body.as_ref().and_then(|Json(b)| b.device_id) {
        let owned = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL)",
        )
        .bind(device_id)
        .bind(auth.id())
        .fetch_one(&state.pool)
        .await?;
        if !owned {
            return Err(ApiError::forbidden("device is not registered to you"));
        }
    }

    // Downloads are an explicit capability: users need the `downloads` scope grant.
    let has_download = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM user_permissions WHERE user_id = $1 AND permission = 'download')",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await?;
    if !has_download && auth.user().role != crate::users::models::Role::Admin {
        return Err(ApiError::forbidden(
            "downloads are disabled for your account; ask an administrator",
        ));
    }

    let ttl = body
        .as_ref()
        .map(|Json(b)| b.ttl_secs)
        .unwrap_or_else(default_ttl);
    let device_id = body.and_then(|Json(b)| b.device_id);

    let ticket =
        download::issue_download_url(&state.pool, "", path.file_id, auth.id(), device_id, ttl)
            .await?;

    metrics::counter!("download_urls_issued_total").increment(1);
    Ok(Json(ticket))
}

/// Public redemption endpoint — no session required, the signature IS the
/// credential. Verifies HMAC + expiry + nonce budget before streaming.
pub async fn redeem_download(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    AxPath(token): AxPath<String>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
) -> ApiResult<Response> {
    // Private-origin gate (Phase J): when ORIGIN_INTERNAL_TOKEN is set, only
    // the CDN/proxy (which injects X-Origin-Token) may fetch media directly.
    crate::security::routes::ensure_origin(&state, &headers)?;
    // The issuer appends `?sig=<hex>`; parse the query string for it.
    let sig = raw_query
        .as_deref()
        .map(crate::utils::parse_query)
        .and_then(|q| q.get("sig").cloned())
        .ok_or_else(|| ApiError::forbidden("missing signature"))?;

    let (file_id, _user_id, _device) =
        download::verify_download_token(&state.pool, &token, &sig).await?;

    let file = crate::streaming::handlers::load_file_public(&state, file_id).await?;
    let full_path = crate::streaming::handlers::absolute_path_public(&file)?;

    if !state.storage.exists(&full_path).await {
        return Err(ApiError::not_found("file on disk"));
    }
    let stat = state
        .storage
        .stat(&full_path)
        .await
        .map_err(ApiError::from)?;
    let stream = state
        .storage
        .stream_range(&full_path, 0, None)
        .await
        .map_err(ApiError::from)?;

    let name = file.row.file_name.clone();
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', ""));

    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, disposition.as_str()),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    let _ = stat;
    metrics::counter!("download_redemptions_total").increment(1);
    Ok(response)
}

// ---------------- source plugin sync ----------------

#[utoipa::path(post, path = "/api/admin/plugins/{plugin_id}/sync", tag = "plugins",
    security(("bearer" = [])),
    params(crate::plugins::handlers::PluginIdPath),
    responses((status = 200))
)]
pub async fn sync_source_plugin(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(path): AxPath<crate::plugins::handlers::PluginIdPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let plugin = crate::plugins::repo::get(&state.pool, path.plugin_id).await?;
    if plugin.kind != "source" {
        return Err(ApiError::bad_request("plugin kind must be 'source'"));
    }
    let base_url = plugin
        .base_url
        .clone()
        .ok_or_else(|| ApiError::bad_request("builtin plugins cannot sync"))?;
    let base = base_url.trim_end_matches('/').to_string();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client");
    let resp = client
        .get(format!("{base}/items"))
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("source plugin unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "source plugin returned {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp.json().await.map_err(ApiError::from)?;
    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    let mut imported = 0usize;
    let mut skipped = 0usize;

    for item in items {
        let Some(external_id) = item.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
            skipped += 1;
            continue;
        };
        if crate::catalog::repo::media_for_external(&state.pool, &plugin.name, &external_id)
            .await?
            .is_some()
        {
            skipped += 1;
            continue;
        }

        let media_type = match item.get("type").and_then(|t| t.as_str()) {
            Some("movie") | Some("film") => crate::catalog::models::MediaType::Movie,
            Some("book") | Some("ebook") => crate::catalog::models::MediaType::Book,
            _ => crate::catalog::models::MediaType::Series,
        };
        let title = item
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("Untitled")
            .to_string();
        let year = item.get("year").and_then(|y| y.as_i64());
        let created = crate::catalog::repo::create(
            &state.pool,
            &crate::catalog::models::CreateMediaRequest {
                media_type,
                title,
                overview: item
                    .get("overview")
                    .and_then(|o| o.as_str())
                    .map(str::to_string),
                poster_url: item
                    .get("poster")
                    .and_then(|p| p.as_str())
                    .map(str::to_string),
                release_date: year.and_then(|y| chrono::NaiveDate::from_ymd_opt(y as i32, 1, 1)),
                original_title: None,
                runtime_minutes: None,
                status: None,
                backdrop_url: None,
                is_anime: false,
                genres: Vec::new(),
                studios: Vec::new(),
            },
        )
        .await?;

        crate::catalog::repo::upsert_external_mapping(
            &state.pool,
            created.id,
            &plugin.name,
            &external_id,
            serde_json::json!({}),
        )
        .await?;
        imported += 1;
    }

    tracing::info!(plugin = %plugin.name, admin = %admin.0.id(), imported, skipped, "source plugin synced");
    Ok(Json(
        serde_json::json!({ "imported": imported, "skipped": skipped }),
    ))
}
