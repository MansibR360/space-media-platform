//! Manual-payment approval flow: users request a plan, an admin verifies the
//! payment out-of-band (transfer, cash, external processor) and approves.
//! Approval grants concrete capabilities (currently the `download` permission)
//! and notifies the user over WS + in-app.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::{AdminUser, CurrentUser};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::utils::new_id;

pub const PLAN_PRICES: &[(&str, i32)] = &[
    ("premium_monthly", 499),
    ("premium_yearly", 3999),
    ("downloads_addon", 199),
];

#[derive(Debug, Clone, sqlx::FromRow, Serialize, ToSchema)]
pub struct PaymentRequest {
    pub id: Uuid,
    pub user_id: Uuid,
    pub username: String,
    pub plan: String,
    pub amount_cents: i32,
    pub currency: String,
    pub status: String,
    pub reference: String,
    pub decided_by: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

const SELECT: &str = "SELECT p.id, p.user_id, u.username, p.plan, p.amount_cents, p.currency, \
     p.status, p.reference, p.decided_by, p.decided_at, p.created_at \
     FROM payment_requests p JOIN users u ON u.id = p.user_id";

fn price_for(plan: &str) -> Option<i32> {
    PLAN_PRICES
        .iter()
        .find(|(name, _)| *name == plan)
        .map(|(_, cents)| *cents)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePaymentRequest {
    /// premium_monthly | premium_yearly | downloads_addon
    pub plan: String,
    /// transaction reference / note shown to the approving admin
    #[serde(default)]
    pub reference: String,
}

#[utoipa::path(post, path = "/api/billing/requests", tag = "billing",
    security(("bearer" = [])),
    request_body = CreatePaymentRequest,
    responses((status = 201, body = PaymentRequest), (status = 400))
)]
pub async fn create_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreatePaymentRequest>,
) -> ApiResult<(StatusCode, Json<PaymentRequest>)> {
    let Some(amount) = price_for(&req.plan) else {
        return Err(ApiError::bad_request(format!(
            "plan must be one of: {}",
            PLAN_PRICES
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    };

    // One pending request at a time per user.
    let open: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM payment_requests WHERE user_id = $1 AND status = 'pending'")
            .bind(auth.id())
            .fetch_optional(&state.pool)
            .await?;
    if open.is_some() {
        return Err(ApiError::Conflict(
            "you already have a pending request".into(),
        ));
    }

    let id = new_id();
    sqlx::query(
        "INSERT INTO payment_requests (id, user_id, plan, amount_cents, reference)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(auth.id())
    .bind(&req.plan)
    .bind(amount)
    .bind(req.reference.trim())
    .execute(&state.pool)
    .await?;

    metrics::counter!("payment_requests_total").increment(1);
    let created = get_by_id(&state.pool, id).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn get_by_id(pool: &sqlx::PgPool, id: Uuid) -> ApiResult<PaymentRequest> {
    sqlx::query_as::<_, PaymentRequest>(&format!("{SELECT} WHERE p.id = $1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|_| ApiError::internal_msg("request vanished"))
}

#[utoipa::path(get, path = "/api/billing/me", tag = "billing",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_requests(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<PaymentRequest>>> {
    Ok(Json(
        sqlx::query_as::<_, PaymentRequest>(&format!(
            "{SELECT} WHERE p.user_id = $1 ORDER BY p.created_at DESC LIMIT 20"
        ))
        .bind(auth.id())
        .fetch_all(&state.pool)
        .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct BillingAdminQuery {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".into()
}

#[utoipa::path(get, path = "/api/admin/billing", tag = "admin",
    params(BillingAdminQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_queue(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    AxQuery(query): AxQuery<BillingAdminQuery>,
) -> ApiResult<Json<Vec<PaymentRequest>>> {
    if !matches!(query.status.as_str(), "pending" | "approved" | "rejected") {
        return Err(ApiError::bad_request(
            "status must be pending/approved/rejected",
        ));
    }
    Ok(Json(
        sqlx::query_as::<_, PaymentRequest>(&format!(
            "{SELECT} WHERE p.status = $1 ORDER BY p.created_at ASC LIMIT 100"
        ))
        .bind(&query.status)
        .fetch_all(&state.pool)
        .await?,
    ))
}

#[utoipa::path(post, path = "/api/admin/billing/{id}/approve", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = PaymentRequest), (status = 404))
)]
pub async fn approve(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    decide(&state, &admin, id, "approved").await
}

#[utoipa::path(post, path = "/api/admin/billing/{id}/reject", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = PaymentRequest), (status = 404))
)]
pub async fn reject(
    State(state): State<Arc<AppState>>,
    admin: AdminUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    decide(&state, &admin, id, "rejected").await
}

async fn decide(
    state: &AppState,
    admin: &AdminUser,
    id: Uuid,
    decision: &str,
) -> ApiResult<Json<serde_json::Value>> {
    let updated: (Uuid, String) = sqlx::query_as::<_, (Uuid, String)>(
        "UPDATE payment_requests SET status = $2, decided_by = $3, decided_at = now() \
         WHERE id = $1 AND status = 'pending' RETURNING user_id, plan",
    )
    .bind(id)
    .bind(decision)
    .bind(admin.0.id())
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("pending payment request"))?;

    let (user_id, plan): (Uuid, String) = updated;

    if decision == "approved" {
        // Capabilities granted by plan.
        let permission = match plan.as_str() {
            "downloads_addon" | "premium_monthly" | "premium_yearly" => "download",
            _ => "",
        };
        if !permission.is_empty() {
            sqlx::query(
                "INSERT INTO user_permissions (user_id, permission) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(user_id)
            .bind(permission)
            .execute(&state.pool)
            .await?;
        }
        crate::auth::permissions::grant(
            &state.pool,
            user_id,
            crate::auth::permissions::Permission::Download,
        )
        .await?;
    }

    crate::platform::repo::notify(
        &state.pool,
        user_id,
        &state.hub,
        "system",
        if decision == "approved" {
            "Payment approved"
        } else {
            "Payment rejected"
        },
        &format!("Your request for '{plan}' was {decision}."),
        serde_json::json!({ "request_id": id, "plan": plan }),
    )
    .await?;

    if decision == "approved" {
        crate::webhooks::emit(
            &state.pool,
            "payment.approved",
            serde_json::json!({ "user_id": user_id, "plan": plan }),
        ).await;
    }
    metrics::counter!("payment_decided_total", "decision" => decision.to_string()).increment(1);
    Ok(Json(
        serde_json::json!({ "id": id, "status": decision, "plan": plan }),
    ))
}
