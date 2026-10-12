use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::docs::ApiDoc;
use crate::state::AppState;

pub fn build(state: Arc<AppState>) -> axum::Router {
    let api = axum::Router::new()
        .merge(crate::auth::routes::router())
        .merge(crate::users::routes::router())
        .merge(crate::settings::routes::router())
        .merge(crate::platform::routes::router())
        .merge(crate::sources::routes::router())
        .merge(crate::sources::routes::unified_router())
        .merge(crate::sources::routes::web_router())
        .merge(crate::sources::routes::hub_router())
        .merge(crate::sources::routes::youtube_admin_router())
        .merge(crate::lyrics::routes::router())
        .merge(crate::music::routes::router())
        .merge(crate::youtube::router())
        .merge(crate::extensions::routes::router())
        .merge(crate::plugins::native::router())
        .merge(crate::plugins::script::routes::router())
        .merge(crate::catalogs::router())
        .merge(billing_router())
        .merge(webhook_router())
        .merge(automation_router())
        .merge(crate::security::routes::security_router())
        .merge(crate::aiops::routes::ai_router())
        .merge(crate::graph::routes::graph_router())
        .merge(crate::moderation::moderation_router())
        .merge(crate::appeals::appeals_router())
        .merge(crate::cards::cards_router())
        .merge(crate::friends::friends_router())
        .merge(crate::sharing::sharing_router())
        .merge(crate::privacy::privacy_router())
        .merge(crate::qol::qol_router())
        .route("/api/progress", axum::routing::put(crate::tracking::unified_handlers::update_progress))
        .route(
            "/api/continue",
            get(crate::tracking::unified_handlers::continue_all),
        )
        .route("/api/me/twin", get(me_twin))
        .route(
            "/api/admin/events",
            get(admin_events),
        )
        .route(
            "/api/admin/failures",
            get(admin_failures),
        )
        .route(
            "/api/admin/metadata/refresh",
            post(crate::catalog::refresh::refresh_now),
        )
        .route(
            "/api/admin/quality/conflicts",
            get(crate::catalog::refresh::quality_conflicts),
        )
        .merge(crate::catalog::routes::router())
        .merge(crate::search::routes::router())
        .merge(crate::tracking::routes::router())
        .merge(crate::library::routes::router())
        .merge(crate::streaming::routes::router())
        .merge(crate::playback::routes::router())
        .merge(crate::chat::routes::router())
        .route("/api/health", get(health))
        .route("/api/ready", get(readiness))
        .route("/api/stats", get(system_stats))
        .route("/api/admin/library/scan", post(scan_library_endpoint))
        .route(
            "/api/admin/files/probe-all",
            post(crate::streaming::handlers::probe_all_unprobed),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::ratelimit::middleware::enforce,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            failure_recorder,
        ))
        .layer(middleware::from_fn(metrics_middleware));

    let root = axum::Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(metrics_handler))
        .merge(api)
        .merge(crate::plugins::routes::router())
        .merge(crate::intelligence::routes::router())
        .merge(crate::social::routes::router())
        .merge(crate::parties::routes::router())
        .merge(crate::recommendations::routes::router())
        .merge(crate::heuristic::routes::router())
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::admin_gate::enforce,
        ))
        .fallback_service(spa_fallback(state.clone()))
        .layer(build_cors(&state))
        .layer(middleware::from_fn(v1_rewrite))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    tower_http::trace::DefaultMakeSpan::new().level(tracing::Level::INFO),
                )
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::INFO),
                ),
        )
        .with_state(state);

    root
}

/// Autonomous failure analysis (changeset 3): every 5xx lands in
/// `failure_events` with route + duration, feeding the admin timeline and the
/// AI copilot's spike detection.
async fn failure_recorder(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().to_string();
    let raw_path = request.uri().path().to_string();

    let response = next.run(request).await;

    if response.status().is_server_error() {
        let route = crate::utils::normalize_route_path(&raw_path);
        let status = response.status().as_u16() as i32;
        let duration_ms = started.elapsed().as_millis() as i64;
        let pool = state.pool.clone();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO failure_events (id, method, route, status, duration_ms)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(crate::utils::new_id())
            .bind(method)
            .bind(route)
            .bind(status)
            .bind(duration_ms)
            .execute(&pool)
            .await;
        });
        metrics::counter!("failure_events_total").increment(1);
    }
    response
}

/// Ops timeline for failures: recent rows plus per-route aggregates.
#[utoipa::path(get, path = "/api/admin/failures", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_failures(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::middleware::AdminUser,
) -> Result<Json<serde_json::Value>, crate::error::ApiError> {
    #[derive(serde::Serialize, sqlx::FromRow)]
    struct FailureRow {
        id: uuid::Uuid,
        method: String,
        route: String,
        status: i32,
        duration_ms: i64,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let recent = sqlx::query_as::<_, FailureRow>(
        "SELECT * FROM failure_events ORDER BY created_at DESC LIMIT 50",
    )
    .fetch_all(&state.pool)
    .await?;

    #[derive(serde::Serialize, sqlx::FromRow)]
    struct RouteAggregate {
        route: String,
        count: i64,
        avg_duration_ms: f64,
        max_duration_ms: i64,
        last_seen: chrono::DateTime<chrono::Utc>,
    }
    let grouped = sqlx::query_as::<_, RouteAggregate>(
        "SELECT route, COUNT(*) AS count,
                COALESCE(AVG(duration_ms), 0)::float8 AS avg_duration_ms,
                COALESCE(MAX(duration_ms), 0) AS max_duration_ms,
                MAX(created_at) AS last_seen
         FROM failure_events
         WHERE created_at > now() - INTERVAL '24 hours'
         GROUP BY route ORDER BY count DESC LIMIT 20",
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({
        "recent": recent,
        "top_routes_24h": grouped,
    })))
}

async fn metrics_middleware(request: Request, next: Next) -> Response {    let start = Instant::now();
    let method = request.method().to_string();
    let raw_path = request.uri().path().to_string();

    let mut response = next.run(request).await;

    // API versioning (M13): every response advertises its version so clients
    // can negotiate / detect deprecations later.
    if let Ok(value) =
        axum::http::HeaderValue::from_str(concat!("space/", env!("CARGO_PKG_VERSION")))
    {
        response.headers_mut().insert("x-api-version", value);
    }

    let elapsed = start.elapsed().as_secs_f64();
    let route = crate::utils::normalize_route_path(&raw_path);
    let status = response.status().as_u16().to_string();

    metrics::histogram!("http_request_duration_seconds", "method" => method.clone(), "route" => route.clone(), "status" => status.clone())
        .record(elapsed);
    metrics::counter!("http_requests_total", "method" => method, "route" => route, "status" => status)
        .increment(1);

    response
}

fn build_cors(state: &AppState) -> CorsLayer {
    if state.cfg.cors_origins.is_empty() {
        return CorsLayer::permissive();
    }
    let origins: Vec<HeaderValue> = state
        .cfg
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any)
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct HealthDto {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: i64,
    pub online_users: usize,
}

#[utoipa::path(get, path = "/api/health", tag = "system",
    responses((status = 200, body = HealthDto))
)]
pub async fn health(State(state): State<Arc<AppState>>) -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.uptime_secs(),
        online_users: state.hub.online_users().len(),
    })
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct ReadinessDto {
    pub ready: bool,
    pub database: bool,
    pub redis: bool,
    pub ffmpeg: bool,
}

/// Deep readiness probe for load balancers / orchestrators.
#[utoipa::path(get, path = "/api/ready", tag = "system",
    responses((status = 200, body = ReadinessDto), (status = 503))
)]
pub async fn readiness(State(state): State<Arc<AppState>>) -> Response {
    let db_ok = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();

    let mut response = Json(ReadinessDto {
        ready: db_ok,
        database: db_ok,
        redis: state.cache.has_redis(),
        ffmpeg: true,
    })
    .into_response();

    if !db_ok {
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    }
    response
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct SystemStats {
    pub uptime_secs: i64,
    pub online_users: usize,
    pub ws_rooms: usize,
    pub media_count: i64,
    pub file_count: i64,
    pub user_count: i64,
    pub active_playback_sessions: i64,
    pub running_transcodes: i64,
}

/// Compact system dashboard data (admin-grade but read-only and safe).
#[utoipa::path(get, path = "/api/stats", tag = "system",
    responses((status = 200, body = SystemStats))
)]
pub async fn system_stats(State(state): State<Arc<AppState>>) -> Json<SystemStats> {
    let media_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let file_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_files")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let active_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_sessions WHERE ended_at IS NULL AND last_active_at > now() - INTERVAL '30 minutes'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let running_transcodes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transcode_jobs WHERE status IN ('queued','running')",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let stats = state.hub.stats();
    Json(SystemStats {
        uptime_secs: state.uptime_secs(),
        online_users: state.hub.online_users().len(),
        ws_rooms: stats.get("rooms").copied().unwrap_or(0),
        media_count,
        file_count,
        user_count,
        active_playback_sessions: active_sessions,
        running_transcodes,
    })
}

pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> Response {
    match &state.metrics_handle {
        Some(handle) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            )],
            handle.render(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "metrics disabled").into_response(),
    }
}

async fn scan_library_endpoint(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::middleware::AdminUser,
) -> Result<Json<Vec<crate::streaming::scanner::ScanReport>>, crate::error::ApiError> {
    let roots = state.cfg.library_roots.clone();
    let reports = crate::streaming::scanner::scan_all_configured(&state.pool, &roots).await;
    Ok(Json(reports))
}

/// Personal Media Twin (Phase H): behavioral model of how the user consumes.
#[utoipa::path(get, path = "/api/me/twin", tag = "intelligence",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn me_twin(
    State(state): State<Arc<AppState>>,
    auth: crate::auth::middleware::CurrentUser,
) -> Result<Json<serde_json::Value>, crate::error::ApiError> {
    let twin = crate::twin::twin_with_snapshot(&state.pool, auth.id()).await?;
    Ok(Json(serde_json::to_value(twin).unwrap_or_default()))
}

/// Ops timeline: recent platform events from the universal event bus.
#[utoipa::path(get, path = "/api/admin/events", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_events(
    State(state): State<Arc<AppState>>,
    _admin: crate::auth::middleware::AdminUser,
    axum::extract::Query(q): axum::extract::Query<EventTimelineQuery>,
) -> Result<Json<serde_json::Value>, crate::error::ApiError> {
    let rows = crate::events::recent(&state.pool, q.event.as_deref(), q.limit.unwrap_or(100).clamp(1, 500))
        .await
        .map_err(crate::error::ApiError::from)?;
    Ok(Json(serde_json::json!({ "events": rows })))
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct EventTimelineQuery {
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Serves the bundled web client with SPA-style fallback to index.html.
fn spa_fallback(_state: Arc<AppState>) -> tower_http::services::ServeDir {
    let index = std::path::PathBuf::from("web/index.html");
    let web_dir = std::path::PathBuf::from("web");

    if !index.exists() {
        std::fs::create_dir_all(&web_dir).ok();
    }

    tower_http::services::ServeDir::new(&web_dir).append_index_html_on_directories(true)
}

pub fn billing_router() -> axum::Router<Arc<AppState>> {
    use axum::routing::{get, post};
    Router::new()
        .route(
            "/api/billing/requests",
            post(crate::billing::create_request).get(crate::billing::my_requests),
        )
        .route("/api/admin/billing", get(crate::billing::admin_queue))
        .route(
            "/api/admin/billing/{id}/approve",
            post(crate::billing::approve),
        )
        .route(
            "/api/admin/billing/{id}/reject",
            post(crate::billing::reject),
        )
}

pub fn webhook_router() -> axum::Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/webhooks",
            get(crate::webhooks::list).post(crate::webhooks::create),
        )
        .route(
            "/api/admin/webhooks/{webhook_id}",
            axum::routing::delete(crate::webhooks::delete),
        )
        .route(
            "/api/admin/webhooks/{webhook_id}/test",
            post(crate::webhooks::test_fire),
        )
}

pub fn automation_router() -> axum::Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/admin/automations",
            get(crate::automation::admin_list).post(crate::automation::admin_create),
        )
        .route(
            "/api/admin/automations/{id}",
            axum::routing::delete(crate::automation::admin_delete)
                .patch(crate::automation::admin_toggle),
        )
}

/// `/v1/...` compatibility namespace: rewrites to the canonical path and tags
/// responses so partners can detect legacy usage.
async fn v1_rewrite(mut request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if let Some(rest) = path.strip_prefix("/v1") {
        let new_path = format!("/api{rest}");
        if let Ok(new_uri) = axum::http::Uri::builder()
            .path_and_query(new_path.as_str())
            .build()
        {
            *request.uri_mut() = new_uri;
            request.extensions_mut().insert(LegacyV1);
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-legacy-v1", axum::http::HeaderValue::from_static("true"));
    response
}

#[derive(Clone, Copy)]
struct LegacyV1;

