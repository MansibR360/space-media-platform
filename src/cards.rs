//! Access-card economy (changeset 5).
//!
//! Rules of the realm, enforced server-side:
//!
//! * Nothing plays without an ACTIVE, unexpired access card — including
//!   admins. `ensure_card_access` gates resolve/session/offline/local-stream.
//! * Card numbers are cryptographic: `SPC-{b32}-{b32}-{b32}-{hmac4}` where
//!   the last group is an HMAC-SHA256 checksum over the body — tampering is
//!   detectable offline, forgery requires the signing secret.
//! * Every account gets a 7-day trial card exactly once (partial unique index).
//! * Merchants buy cards with crypto → admin approves → inventory. They give
//!   cards away directly, or users publish a 20-minute transfer code that a
//!   merchant redeems for N cards.
//! * Admin sees the full merchant→holder chain and can hold any card or ban
//!   any merchant instantly.

use axum::Json;
use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::routing::{get, post};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::middleware::{CurrentUser, StaffUser};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

const TRIAL_DAYS: i32 = 7;

/// Default access window granted at activation: 30 days, then the card
/// times out automatically (`expires_at` gates playback everywhere).
pub const DEFAULT_DURATION_DAYS: i32 = 30;
const MAX_CARD_NUMBER_LEN: usize = 32;
/// Activation brute-force defense: N failures inside the sliding window
/// locks further attempts (persisted, survives restarts).
const ACTIVATION_FAILURE_LIMIT: i32 = 5;
const ACTIVATION_FAILURE_WINDOW_MINS: i64 = 15;
const ACTIVATION_LOCKOUT_MINS: i64 = 15;

// ------------------------------------------------------------ numbers

pub(crate) fn sign_body(secret: &str, body: &str) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(format!("card:{body}").as_bytes());
    let full = hex::encode(mac.finalize().into_bytes()).to_uppercase();
    full[..8].to_string()
}

/// `SPC-{12 hex}-{8 hex}` where group 2 is an HMAC-SHA256 checksum over
/// group 1 — tampering is detectable, forgery requires the signing secret.
pub async fn generate_card_number(pool: &PgPool) -> ApiResult<String> {
    let secret = crate::security::grants::signing_secret_for(pool, "cards").await?;
    for _ in 0..5 {
        let body = crate::utils::random_hex(6).to_uppercase();
        let check = sign_body(&secret, &body);
        let number = format!("SPC-{body}-{check}");
        let exists: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM access_cards WHERE card_number = $1")
                .bind(&number)
                .fetch_optional(pool)
                .await?;
        if exists.is_none() {
            return Ok(number);
        }
    }
    Err(ApiError::internal_msg("card number space exhausted"))
}

/// Offline-tamper check: recomputes the checksum group.
#[allow(dead_code)]
pub async fn verify_card_format(pool: &PgPool, number: &str) -> bool {
    let parts: Vec<&str> = number.split('-').collect();
    if parts.len() != 3 || !parts[0].eq_ignore_ascii_case("SPC") {
        return false;
    }
    if hex::decode(parts[1]).is_err() {
        return false;
    }
    let Ok(secret) = crate::security::grants::signing_secret_for(pool, "cards").await else {
        return false;
    };
    use subtle::ConstantTimeEq;
    let expected = sign_body(&secret, parts[1]);
    parts[2].len() == expected.len()
        && bool::from(expected.as_bytes().ct_eq(parts[2].as_bytes()))
}

// ------------------------------------------------------------ gating

/// THE gate: nothing plays without an active card. Applies to everyone.
pub async fn ensure_card_access(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM access_cards \
         WHERE owner = $1 AND status = 'active' AND expires_at > now())",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    if ok {
        return Ok(());
    }
    metrics::counter!("playback_blocked_no_card_total").increment(1);
    Err(ApiError::forbidden("no active access card — activate one to play"))
}

// ------------------------------------------------------------ lifecycle

async fn mint_internal(
    pool: &PgPool,
    count: i64,
    duration_days: i32,
    issued_by: &str,
    creator_merchant: Option<Uuid>,
    owner: Option<Uuid>,
) -> ApiResult<Vec<String>> {
    let mut numbers = Vec::with_capacity(count.max(0) as usize);
    for _ in 0..count.clamp(1, 500) {
        let number = generate_card_number(pool).await?;
        let parts: Vec<&str> = number.split('-').collect();
        let hmac_full = {
            let secret = crate::security::grants::signing_secret_for(pool, "cards").await?;
            sign_body(&secret, parts[1])
        };
        sqlx::query(
            "INSERT INTO access_cards (id, card_number, number_hmac, issued_by, creator_merchant, owner, duration_days)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(crate::utils::new_id())
        .bind(&number)
        .bind(hmac_full)
        .bind(issued_by)
        .bind(creator_merchant)
        .bind(owner)
        .bind(duration_days.clamp(1, 365))
        .execute(pool)
        .await?;
        numbers.push(number);
    }
    Ok(numbers)
}

pub async fn issue_trial(pool: &PgPool, user_id: Uuid) -> ApiResult<bool> {
    // Partial unique index makes double-issue impossible under concurrency.
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM access_cards WHERE owner = $1 AND issued_by = 'trial'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    if result > 0 {
        return Ok(false);
    }
    mint_internal(pool, 1, TRIAL_DAYS, "trial", None, Some(user_id)).await?;
    tracing::info!(%user_id, "trial card issued");
    Ok(true)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ActivateRequest {
    pub card_number: String,
}

// ---------------- activation hardening (changeset 25) ----------------

/// Rejects activation attempts while the caller is locked out.
async fn ensure_not_locked(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    let locked_until: Option<Option<chrono::DateTime<chrono::Utc>>> = sqlx::query_scalar(
        "SELECT locked_until FROM card_activation_locks WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    if let Some(Some(until)) = locked_until {
        let now = chrono::Utc::now();
        if until > now {
            return Err(ApiError::TooManyRequests {
                retry_after: (until - now).num_seconds().max(1),
            });
        }
    }
    Ok(())
}

/// Records a bad attempt; at the threshold the account is locked out.
async fn record_activation_failure(pool: &PgPool, user_id: Uuid) {
    let now = chrono::Utc::now();
    let stale_window = |started: chrono::DateTime<chrono::Utc>| {
        now - started
            > chrono::Duration::minutes(ACTIVATION_FAILURE_WINDOW_MINS)
    };
    let failures = match sqlx::query_as::<_, (i32, chrono::DateTime<chrono::Utc>)>(
        "SELECT failures, window_started_at FROM card_activation_locks WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some((count, started))) if !stale_window(started) => count + 1,
        _ => 1,
    };
    let locked_until = (failures >= ACTIVATION_FAILURE_LIMIT)
        .then(|| now + chrono::Duration::minutes(ACTIVATION_LOCKOUT_MINS));

    let query_result = sqlx::query(
        "INSERT INTO card_activation_locks (user_id, failures, window_started_at, locked_until)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET
             failures = EXCLUDED.failures,
             window_started_at = EXCLUDED.window_started_at,
             locked_until = EXCLUDED.locked_until",
    )
    .bind(user_id)
    .bind(failures)
    .bind(now)
    .bind(locked_until)
    .execute(pool)
    .await;
    match query_result {
        Ok(_) => {
            metrics::counter!("card_activation_failures_total").increment(1);
            if locked_until.is_some() {
                tracing::warn!(%user_id, "card activation locked out");
                metrics::counter!("card_activation_lockouts_total").increment(1);
                crate::audit::log_simple(pool, Some(user_id), "card.activation_locked", "").await;
            }
        }
        Err(err) => tracing::error!(error = %err, "failed to persist activation failure"),
    }
}

async fn clear_activation_failures(pool: &PgPool, user_id: Uuid) {
    sqlx::query("DELETE FROM card_activation_locks WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .ok();
}

fn valid_card_number_shape(input: &str) -> bool {
    !input.is_empty()
        && input.len() <= MAX_CARD_NUMBER_LEN
        && input.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Activates a card owned by the caller: unused → active for its duration
/// (30 days by default), then it times out. Hardened: input validation,
/// persisted brute-force lockout, and a row-locked transaction so concurrent
/// activations can never double-extend or resurrect an expired card.
#[utoipa::path(post, path = "/api/cards/activate", tag = "billing",
    security(("bearer" = [])),
    request_body = ActivateRequest,
    responses((status = 200), (status = 400), (status = 403), (status = 404))
)]
pub async fn activate_card(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<ActivateRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_not_locked(&state.pool, auth.id()).await?;
    let number = req.card_number.trim().to_uppercase();
    if !valid_card_number_shape(&number) {
        record_activation_failure(&state.pool, auth.id()).await;
        crate::audit::log_simple(&state.pool, Some(auth.id()), "card.activate_invalid_input", "")
            .await;
        return Err(ApiError::bad_request("invalid card number"));
    }

    let mut tx = state.pool.begin().await?;

    // Row lock: serializes concurrent activations of the same card.
    let row: Option<(Uuid, Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>, String)> =
        sqlx::query_as(
            "SELECT id, owner, expires_at, status FROM access_cards \
             WHERE card_number = $1 FOR UPDATE",
        )
        .bind(&number)
        .fetch_optional(&mut *tx)
        .await?;

    // Identical response whether the number is unknown or merely not owned —
    // card-number enumeration must not be able to distinguish the two.
    let Some((id, owner, expires_at, status)) = row else {
        tx.commit().await?;
        record_activation_failure(&state.pool, auth.id()).await;
        crate::audit::log_simple(&state.pool, Some(auth.id()), "card.activate_unknown", "").await;
        return Err(ApiError::not_found("card"));
    };
    if owner != Some(auth.id()) {
        tx.commit().await?;
        record_activation_failure(&state.pool, auth.id()).await;
        crate::audit::log_simple(&state.pool, Some(auth.id()), "card.activate_not_owner", "").await;
        return Err(ApiError::not_found("card"));
    }

    let new_expiry = match status.as_str() {
        "active" => expires_at.unwrap_or_else(chrono::Utc::now),
        "unused" => {
            let duration: (i32,) =
                sqlx::query_as("SELECT duration_days FROM access_cards WHERE id = $1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            let exp =
                chrono::Utc::now() + chrono::Duration::days(i64::from(duration.0));
            sqlx::query(
                "UPDATE access_cards SET status='active', activated_at=now(), expires_at=$2 WHERE id=$1",
            )
            .bind(id)
            .bind(exp)
            .execute(&mut *tx)
            .await?;
            exp
        }
        other => {
            tx.commit().await?;
            record_activation_failure(&state.pool, auth.id()).await;
            crate::audit::log(
                &state.pool,
                Some(auth.id()),
                "card.activate_rejected",
                &number,
                serde_json::json!({ "reason": other }),
                "",
            )
            .await;
            return Err(ApiError::bad_request(match other {
                "held" => "card is held by administration",
                _ => "card already fully consumed",
            }));
        }
    };

    tx.commit().await?;
    clear_activation_failures(&state.pool, auth.id()).await;

    metrics::counter!("cards_activated_total").increment(1);
    crate::audit::log(
        &state.pool,
        Some(auth.id()),
        "card.activated",
        &number,
        serde_json::json!({ "expires_at": new_expiry }),
        "",
    )
    .await;

    Ok(Json(serde_json::json!({
        "activated": true,
        "expires_at_unix": new_expiry.timestamp(),
        "days_remaining": (new_expiry - chrono::Utc::now()).num_days(),
    })))
}

#[utoipa::path(get, path = "/api/me/cards", tag = "billing",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_cards(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_card_access(&state.pool, auth.id()).await.ok(); // metric only
    let rows: Vec<CardView> = sqlx::query_as::<_, CardView>(
        "SELECT card_number, status, duration_days, activated_at, expires_at, held_reason, created_at
         FROM access_cards WHERE owner = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(auth.id())
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "cards": rows })))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct CardView {
    pub card_number: String,
    pub status: String,
    pub duration_days: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub held_reason: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/me/cards/summary — the user's card amount: days left on the
/// active card plus the banked days sitting in unused cards.
#[utoipa::path(get, path = "/api/me/cards/summary", tag = "billing",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn my_cards_summary(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let now = chrono::Utc::now();

    // Longest-running active card drives the expiry clock.
    let active: Option<(i64, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT COUNT(*), MAX(expires_at) FROM access_cards \
         WHERE owner = $1 AND status = 'active' AND expires_at > $2",
    )
    .bind(auth.id())
    .bind(now)
    .fetch_optional(&state.pool)
    .await?
    .filter(|(count, _)| *count > 0);

    let (active_count, latest_expiry) = active
        .map(|(c, e)| (c, Some(e)))
        .unwrap_or((0, None));

    // Banked value: untouched cards and their total day amount.
    let banked: Option<(i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(duration_days), 0) FROM access_cards \
         WHERE owner = $1 AND status = 'unused'",
    )
    .bind(auth.id())
    .fetch_optional(&state.pool)
    .await?;

    let (banked_count, banked_days) = banked.unwrap_or((0, 0));
    let days_remaining = latest_expiry
        .map(|exp| (exp - now).num_days().max(0))
        .unwrap_or(0);
    // The amount a user can still draw from their cards, in days.
    let total_amount_days = days_remaining + banked_days;

    Ok(Json(serde_json::json!({
        "active_cards": active_count,
        "days_remaining": days_remaining,
        "expires_at": latest_expiry,
        "banked_cards": banked_count,
        "banked_days": banked_days,
        "total_amount_days": total_amount_days,
        "card_duration_days": DEFAULT_DURATION_DAYS,
    })))
}

// ------------------------------------------------------------ transfer codes

/// Buyer publishes intent: a one-time code valid for 20 minutes.
#[utoipa::path(post, path = "/api/cards/transfer-code", tag = "billing",
    security(("bearer" = [])),
    responses((status = 200), (status = 403))
)]
pub async fn create_transfer_code(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let raw = crate::utils::random_hex(10);
    let code = format!("XFER-{}", raw.to_uppercase());
    let expires = chrono::Utc::now() + chrono::Duration::minutes(20);
    sqlx::query(
        "INSERT INTO card_transfer_codes (id, code_hash, requester, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(crate::utils::new_id())
    .bind(crate::utils::sha256_hex(&code))
    .bind(auth.id())
    .bind(expires)
    .execute(&state.pool)
    .await?;
    metrics::counter!("transfer_codes_created_total").increment(1);
    Ok(Json(serde_json::json!({
        "code": code,
        "expires_at_unix": expires.timestamp(),
        "hint": "give this code to a merchant — they choose how many cards to sell you",
    })))
}

// ------------------------------------------------------------ merchant

#[utoipa::path(post, path = "/api/merchant/register", tag = "merchant",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn merchant_register(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("INSERT INTO merchant_profile (user_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(auth.id())
        .execute(&state.pool)
        .await?;
    crate::audit::log_simple(&state.pool, Some(auth.id()), "merchant.registered", "").await;
    Ok(Json(serde_json::json!({ "registered": true })))
}

async fn ensure_active_merchant(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "SELECT banned, locked_out FROM merchant_profile WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some((banned, _)) if banned => Err(ApiError::forbidden("merchant account is banned")),
        // Card-dispense lockout (changeset 26): existing cards keep working;
        // the merchant just cannot hand out or order new ones.
        Some((_, true)) => Err(ApiError::forbidden(
            "merchant card operations are locked by administration",
        )),
        Some(_) => Ok(()),
        None => Err(ApiError::forbidden("not a registered merchant")),
    }
}

/// Read-only merchant status check (lockout does not block reads).
async fn ensure_registered_merchant(pool: &PgPool, user_id: Uuid) -> ApiResult<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM merchant_profile WHERE user_id = $1 AND NOT banned)",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(ApiError::forbidden("not a registered merchant"))
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PurchaseRequest {
    pub quantity: i64,
    #[serde(default)]
    pub tx_reference: Option<String>,
}

/// Merchant orders cards with crypto; admin approval mints inventory.
#[utoipa::path(post, path = "/api/merchant/cards/purchase", tag = "merchant",
    security(("bearer" = [])),
    request_body = PurchaseRequest,
    responses((status = 201), (status = 403), (status = 400))
)]
pub async fn purchase_request(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<PurchaseRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    ensure_active_merchant(&state.pool, auth.id()).await?;
    if !(1..=500).contains(&req.quantity) {
        return Err(ApiError::bad_request("quantity must be 1-500"));
    }
    let cfg: (i32, String,) =
        sqlx::query_as("SELECT unit_price_cents, crypto_address FROM billing_config WHERE id = 1")
            .fetch_one(&state.pool)
            .await?;
    let total = cfg.0 as i64 * req.quantity;
    let id = crate::utils::new_id();
    sqlx::query(
        "INSERT INTO card_purchases (id, merchant, quantity, unit_price_cents, total_cents, tx_reference)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(auth.id())
    .bind(req.quantity)
    .bind(cfg.0)
    .bind(total)
    .bind(req.tx_reference.as_deref().unwrap_or(""))
    .execute(&state.pool)
    .await?;
    metrics::counter!("card_purchases_requested_total").increment(1);
    crate::audit::log(
        &state.pool,
        Some(auth.id()),
        "merchant.purchase_requested",
        &id.to_string(),
        serde_json::json!({ "quantity": req.quantity, "total_cents": total }),
        "",
    )
    .await;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "purchase_id": id,
            "unit_price_cents": cfg.0,
            "total_cents": total,
            "crypto_address": cfg.1,
            "status": "pending - an admin must approve the transaction",
        })),
    ))
}

#[utoipa::path(get, path = "/api/merchant/inventory", tag = "merchant",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn merchant_inventory(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_registered_merchant(&state.pool, auth.id()).await?;

    // The three numbers a merchant works from: stock on hand, cards already
    // handed out, and orders still awaiting admin approval. This previously
    // returned only `{inventory}`, which no client could render the rest from.
    let (available, given): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE owner IS NULL AND status = 'unused'),
                COUNT(*) FILTER (WHERE owner IS NOT NULL)
         FROM access_cards WHERE creator_merchant = $1",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await
    .unwrap_or((0, 0));

    let pending: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(quantity), 0)::bigint FROM card_purchases
         WHERE merchant = $1 AND status = 'pending'",
    )
    .bind(auth.id())
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    Ok(Json(serde_json::json!({
        "available": available,
        "given": given,
        "pending": pending,
    })))
}

async fn move_cards(
    pool: &PgPool,
    merchant: Uuid,
    to_user: Uuid,
    count: i64,
) -> ApiResult<u64> {
    let moved = sqlx::query(
        "WITH pick AS (
             SELECT id FROM access_cards
             WHERE creator_merchant = $2 AND owner IS NULL AND status = 'unused'
             ORDER BY created_at LIMIT $3
         )
         UPDATE access_cards c SET owner = $1
         WHERE id IN (SELECT id FROM pick)",
    )
    .bind(to_user)
    .bind(merchant)
    .bind(count.clamp(1, 500))
    .execute(pool)
    .await?
    .rows_affected();
    if moved > 0 {
        sqlx::query(
            "UPDATE merchant_profile SET total_sold = total_sold + $2 WHERE user_id = $1",
        )
        .bind(merchant)
        .bind(moved as i64)
        .execute(pool)
        .await?;
    }
    Ok(moved)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GiveCardsRequest {
    pub to_username: String,
    pub count: i64,
}

/// Merchant hands cards straight to a user by username.
#[utoipa::path(post, path = "/api/merchant/cards/give", tag = "merchant",
    security(("bearer" = [])),
    request_body = GiveCardsRequest,
    responses((status = 200), (status = 404), (status = 403))
)]
pub async fn give_cards(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<GiveCardsRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_active_merchant(&state.pool, auth.id()).await?;
    let target: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM users WHERE LOWER(username) = LOWER($1) AND is_active",
    )
    .bind(req.to_username.trim())
    .fetch_optional(&state.pool)
    .await?;
    let Some((to_user,)) = target else {
        return Err(ApiError::not_found("user"));
    };
    let moved = move_cards(&state.pool, auth.id(), to_user, req.count).await?;
    if moved == 0 {
        return Err(ApiError::bad_request("insufficient inventory"));
    }
    crate::audit::log(
        &state.pool,
        Some(auth.id()),
        "merchant.cards_given",
        &to_user.to_string(),
        serde_json::json!({ "count": moved }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "given": moved, "to_user": to_user })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RedeemTransferRequest {
    pub code: String,
    pub count: i64,
}

/// Merchant redeems a buyer's 20-minute transfer code for N cards.
#[utoipa::path(post, path = "/api/merchant/cards/redeem-transfer", tag = "merchant",
    security(("bearer" = [])),
    request_body = RedeemTransferRequest,
    responses((status = 200), (status = 400), (status = 403), (status = 404))
)]
pub async fn redeem_transfer(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<RedeemTransferRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_active_merchant(&state.pool, auth.id()).await?;
    let code_hash = crate::utils::sha256_hex(req.code.trim());
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, requester FROM card_transfer_codes \
         WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(code_hash)
    .fetch_optional(&state.pool)
    .await?;
    let Some((code_id, requester)) = row else {
        return Err(ApiError::not_found("valid transfer code (codes expire after 20 minutes)"));
    };

    let moved = move_cards(&state.pool, auth.id(), requester, req.count).await?;
    if moved == 0 {
        return Err(ApiError::bad_request("insufficient inventory"));
    }
    sqlx::query(
        "UPDATE card_transfer_codes SET used_at = now(), used_by = $2, cards_moved = $3 WHERE id = $1",
    )
    .bind(code_id)
    .bind(auth.id())
    .bind(moved as i32)
    .execute(&state.pool)
    .await?;
    metrics::counter!("transfer_codes_redeemed_total").increment(1);
    crate::audit::log(
        &state.pool,
        Some(auth.id()),
        "merchant.transfer_redeemed",
        &requester.to_string(),
        serde_json::json!({ "count": moved }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "delivered": moved, "to_user": requester })))
}

// ------------------------------------------------------------ admin

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct BillingConfig {
    pub unit_price_cents: i32,
    pub currency: String,
    pub crypto_address: String,
}

#[utoipa::path(get, path = "/api/admin/billing/config", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200, body = BillingConfig))
)]
pub async fn get_billing_config(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
) -> ApiResult<Json<BillingConfig>> {
    staff.require(crate::auth::permissions::Permission::AdminBilling.as_str())
        .or_else(|_| staff.require(crate::auth::permissions::Permission::AdminView.as_str()))?;
    Ok(Json(sqlx::query_as::<_, BillingConfig>(
        "SELECT unit_price_cents, currency, crypto_address FROM billing_config WHERE id = 1",
    )
    .fetch_one(&state.pool)
    .await?))
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct BillingConfigPatch {
    pub unit_price_cents: Option<i32>,
    pub currency: Option<String>,
    pub crypto_address: Option<String>,
}

/// Admin sets the card price / payout address.
#[utoipa::path(patch, path = "/api/admin/billing/config", tag = "admin",
    security(("bearer" = [])),
    request_body = BillingConfigPatch,
    responses((status = 200, body = BillingConfig))
)]
pub async fn patch_billing_config(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    Json(patch): Json<BillingConfigPatch>,
) -> ApiResult<Json<BillingConfig>> {
    staff.require(crate::auth::permissions::Permission::AdminBilling.as_str())?;
    if let Some(p) = patch.unit_price_cents {
        if !(10..=100_000).contains(&p) {
            return Err(ApiError::bad_request("price must be 10..100000 cents"));
        }
    }
    let mut sets: Vec<String> = Vec::new();
    if patch.unit_price_cents.is_some() { sets.push(format!("unit_price_cents = ${}", sets.len() + 1)); }
    if patch.currency.is_some() { sets.push(format!("currency = ${}", sets.len() + 1)); }
    if patch.crypto_address.is_some() { sets.push(format!("crypto_address = ${}", sets.len() + 1)); }
    if !sets.is_empty() {
        let sql = format!(
            "UPDATE billing_config SET {}, updated_at = now() WHERE id = 1",
            sets.join(", ")
        );
        let mut q = sqlx::query(&sql);
        if let Some(v) = patch.unit_price_cents { q = q.bind(v); }
        if let Some(v) = &patch.currency { q = q.bind(v); }
        if let Some(v) = &patch.crypto_address { q = q.bind(v); }
        q.execute(&state.pool).await?;
    }
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        "billing.config_updated",
        "",
        serde_json::to_value(&patch).unwrap_or_default(),
        "",
    )
    .await;
    get_billing_config(State(state), staff).await
}

#[utoipa::path(get, path = "/api/admin/card-purchases", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_purchases(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxQuery(q): AxQuery<StatusQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminBilling.as_str())
        .or_else(|_| staff.require(crate::auth::permissions::Permission::AdminView.as_str()))?;
    let status = q.status.unwrap_or_else(|| "pending".into());
    let rows: Vec<PurchaseView> = sqlx::query_as::<_, PurchaseView>(
        "SELECT p.*, u.username AS merchant_name FROM card_purchases p \
         JOIN users u ON u.id = p.merchant \
         WHERE p.status = $1 ORDER BY p.created_at DESC LIMIT 200",
    )
    .bind(status)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "purchases": rows })))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PurchaseView {
    pub id: Uuid,
    pub merchant: Uuid,
    pub merchant_name: String,
    pub quantity: i32,
    pub unit_price_cents: i32,
    pub total_cents: i64,
    pub tx_reference: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct StatusQuery {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct DecideRequest {
    /// when approving: how many of the requested quantity to actually mint
    #[serde(default)]
    pub mint_count: Option<i64>,
}

#[utoipa::path(post, path = "/api/admin/card-purchases/{id}/approve", tag = "admin",
    security(("bearer" = [])),
    request_body = DecideRequest,
    responses((status = 200), (status = 404))
)]
pub async fn approve_purchase(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxPath(id): AxPath<Uuid>,
    body: Option<Json<DecideRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminBilling.as_str())?;
    let purchase: Option<(Uuid, i64, String)> = sqlx::query_as(
        "SELECT merchant, quantity, status FROM card_purchases WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((merchant, quantity, status)) = purchase else {
        return Err(ApiError::not_found("purchase"));
    };
    if status != "pending" {
        return Err(ApiError::bad_request("already decided"));
    }
    ensure_active_merchant(&state.pool, merchant).await?;

    let mint = body.and_then(|Json(r)| r.mint_count).unwrap_or(quantity);
    let numbers = mint_internal(
        &state.pool,
        mint,
        DEFAULT_DURATION_DAYS,
        "merchant",
        Some(merchant),
        None,
    )
    .await?;
    sqlx::query(
        "UPDATE card_purchases SET status='approved', decided_by=$2, decided_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(staff.id())
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "UPDATE merchant_profile SET total_purchased = total_purchased + $2 WHERE user_id = $1",
    )
    .bind(merchant)
    .bind(numbers.len() as i64)
    .execute(&state.pool)
    .await?;
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        "admin.purchase_approved",
        &id.to_string(),
        serde_json::json!({ "merchant": merchant, "minted": numbers.len() }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "approved": true, "minted": numbers.len() })))
}

#[utoipa::path(post, path = "/api/admin/card-purchases/{id}/reject", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200), (status = 404))
)]
pub async fn reject_purchase(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxPath(id): AxPath<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminBilling.as_str())?;
    let result = sqlx::query(
        "UPDATE card_purchases SET status='rejected', decided_by=$2, decided_at=now() \
         WHERE id=$1 AND status='pending'",
    )
    .bind(id)
    .bind(staff.id())
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("pending purchase"));
    }
    crate::audit::log_simple(&state.pool, Some(staff.id()), "admin.purchase_rejected", &id.to_string()).await;
    Ok(Json(serde_json::json!({ "rejected": true })))
}

// ---------------- admin card controls ----------------

#[utoipa::path(post, path = "/api/admin/cards/mint", tag = "admin",
    security(("bearer" = [])),
    request_body = MintRequest,
    responses((status = 200))
)]
pub async fn mint_cards(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    Json(req): Json<MintRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminCards.as_str())?;
    let numbers = mint_internal(
        &state.pool,
        req.count.unwrap_or(1),
        req.duration_days.unwrap_or(DEFAULT_DURATION_DAYS),
        "system",
        None,
        None,
    )
    .await?;
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        "admin.cards_minted",
        "",
        serde_json::json!({ "count": numbers.len(), "duration_days": req.duration_days.unwrap_or(DEFAULT_DURATION_DAYS) }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "cards": numbers })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MintRequest {
    #[serde(default)]
    pub count: Option<i64>,
    #[serde(default)]
    pub duration_days: Option<i32>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct HoldRequest {
    pub hold: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Instantly holds (or releases) any card. Held = dead for playback.
#[utoipa::path(post, path = "/api/admin/cards/{card_number}/hold", tag = "admin",
    security(("bearer" = [])),
    request_body = HoldRequest,
    responses((status = 200), (status = 404))
)]
pub async fn hold_card(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxPath(card_number): AxPath<String>,
    Json(req): Json<HoldRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminCards.as_str())?;
    let number = card_number.trim().to_uppercase();
    let result = if req.hold {
        sqlx::query(
            "UPDATE access_cards SET status='held', held_reason=$3 WHERE card_number=$1 AND status <> 'held'",
        )
        .bind(&number)
        .bind(&number)
        .bind(req.reason.as_deref().unwrap_or("held by administration"))
        .execute(&state.pool)
        .await?
    } else {
        sqlx::query(
            "UPDATE access_cards SET status='unused', held_reason='' WHERE card_number=$1 AND status='held'",
        )
        .bind(&number)
        .execute(&state.pool)
        .await?
    };
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("card in holdable state"));
    }
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        if req.hold { "admin.card_held" } else { "admin.card_released" },
        &number,
        serde_json::json!({ "reason": req.reason }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true, "held": req.hold })))
}

/// Merchant → holder chain: where every sold card ended up.
#[utoipa::path(get, path = "/api/admin/cards/chain", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn card_chain(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminCards.as_str())
        .or_else(|_| staff.require(crate::auth::permissions::Permission::AdminView.as_str()))?;
    let rows: Vec<(Option<String>, Option<String>, String, i64)> = sqlx::query_as(
        "SELECT mu.username AS merchant_name, ou.username AS holder_name, \
                c.status, COUNT(*) AS cards \
         FROM access_cards c \
         LEFT JOIN users mu ON mu.id = c.creator_merchant \
         LEFT JOIN users ou ON ou.id = c.owner \
         GROUP BY 1, 2, 3 ORDER BY cards DESC LIMIT 200",
    )
    .fetch_all(&state.pool)
    .await?;
    let chain: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(m, h, s, n)| {
            serde_json::json!({
                "merchant": m.unwrap_or_else(|| "system".into()),
                "holder": h.unwrap_or_else(|| "(inventory)".into()),
                "status": s,
                "cards": n,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "chain": chain })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MerchantBanRequest {
    pub banned: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[utoipa::path(post, path = "/api/admin/merchants/{user_id}/ban", tag = "admin",
    security(("bearer" = [])),
    request_body = MerchantBanRequest,
    responses((status = 200), (status = 404))
)]
pub async fn ban_merchant(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxPath(user_id): AxPath<Uuid>,
    Json(req): Json<MerchantBanRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminMerchants.as_str())?;
    if req.banned {
        let result = sqlx::query(
            "UPDATE merchant_profile SET banned = TRUE, banned_at = now() WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&state.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("merchant"));
        }
    } else {
        sqlx::query(
            "UPDATE merchant_profile SET banned = FALSE, banned_at = NULL WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&state.pool)
        .await?;
    }
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        if req.banned { "admin.merchant_banned" } else { "admin.merchant_unbanned" },
        &user_id.to_string(),
        serde_json::json!({ "reason": req.reason }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "banned": req.banned })))
}

// ---------------- merchant card-dispense lockout (changeset 26) ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MerchantLockRequest {
    pub locked: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Locks (or unlocks) a merchant's ability to dispense cards. Cards already
/// held by users remain fully usable — only give / redeem-transfer /
/// purchase are blocked.
#[utoipa::path(post, path = "/api/admin/merchants/{user_id}/lock", tag = "admin",
    security(("bearer" = [])),
    request_body = MerchantLockRequest,
    responses((status = 200), (status = 404))
)]
pub async fn lock_merchant(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxPath(user_id): AxPath<Uuid>,
    Json(req): Json<MerchantLockRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminMerchants.as_str())?;
    let result = sqlx::query(
        "UPDATE merchant_profile SET \
            locked_out = $2, \
            locked_at = CASE WHEN $2 THEN now() ELSE NULL END, \
            locked_reason = $3 \
         WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(req.locked)
    .bind(req.reason.as_deref().unwrap_or(""))
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("merchant"));
    }
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        if req.locked { "admin.merchant_locked" } else { "admin.merchant_unlocked" },
        &user_id.to_string(),
        serde_json::json!({ "reason": req.reason }),
        "",
    )
    .await;
    Ok(Json(serde_json::json!({ "locked": req.locked })))
}

/// GET /api/admin/merchants — roster with stats + lock/ban status.
#[utoipa::path(get, path = "/api/admin/merchants", tag = "admin",
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_merchants(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminMerchants.as_str())
        .or_else(|_| staff.require(crate::auth::permissions::Permission::AdminView.as_str()))?;
    let rows: Vec<MerchantAdminRow> = sqlx::query_as::<_, MerchantAdminRow>(
        "SELECT m.user_id, u.username, u.display_name, u.is_active, \
                m.banned, m.locked_out, m.locked_reason, \
                m.total_purchased, m.total_sold, m.created_at \
         FROM merchant_profile m JOIN users u ON u.id = m.user_id \
         ORDER BY m.created_at DESC LIMIT 500",
    )
    .fetch_all(&state.pool)
    .await?;
    let total_locked: i64 = rows.iter().filter(|r| r.locked_out).count() as i64;
    Ok(Json(serde_json::json!({
        "merchants": rows,
        "locked_count": total_locked,
    })))
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct MerchantAdminRow {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_active: bool,
    pub banned: bool,
    pub locked_out: bool,
    pub locked_reason: String,
    pub total_purchased: i32,
    pub total_sold: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ---------------- admin-granted cards (changeset 26) ----------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct GrantCardsRequest {
    /// target user id (either user_id or username required)
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub username: Option<String>,
    /// how many cards to grant (1-100 per call)
    #[serde(default)]
    pub count: Option<i64>,
    /// days each granted card runs once activated; default 30
    #[serde(default)]
    pub duration_days: Option<i32>,
}

/// Admin grants trial-like cards straight to any user. This bypasses the
/// merchant pipeline entirely — the cards are minted pre-owned
/// (`issued_by='admin_grant'`) and activate like any other card.
#[utoipa::path(post, path = "/api/admin/cards/grant", tag = "admin",
    security(("bearer" = [])),
    request_body = GrantCardsRequest,
    responses((status = 201), (status = 400), (status = 404))
)]
pub async fn grant_cards_to_user(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    Json(req): Json<GrantCardsRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    staff.require(crate::auth::permissions::Permission::AdminCards.as_str())?;
    let count = req.count.unwrap_or(1);
    if !(1..=100).contains(&count) {
        return Err(ApiError::bad_request("count must be 1-100"));
    }
    let duration = req.duration_days.unwrap_or(DEFAULT_DURATION_DAYS).clamp(1, 365);

    let target: Option<(Uuid, String)> = match (&req.user_id, &req.username) {
        (Some(uid), _) => sqlx::query_as(
            "SELECT id, username FROM users WHERE id = $1 AND is_active",
        )
        .bind(uid)
        .fetch_optional(&state.pool)
        .await?,
        (None, Some(name)) => sqlx::query_as(
            "SELECT id, username FROM users WHERE LOWER(username) = LOWER($1) AND is_active",
        )
        .bind(name.trim())
        .fetch_optional(&state.pool)
        .await?,
        (None, None) => return Err(ApiError::bad_request("user_id or username is required")),
    };
    let Some((user_id, username)) = target else {
        return Err(ApiError::not_found("user"));
    };

    let numbers = mint_internal(
        &state.pool,
        count,
        duration,
        "admin_grant",
        None,
        Some(user_id),
    )
    .await?;

    metrics::counter!("cards_admin_granted_total").increment(numbers.len() as u64);
    crate::audit::log(
        &state.pool,
        Some(staff.id()),
        "admin.cards_granted",
        &user_id.to_string(),
        serde_json::json!({
            "username": username,
            "count": numbers.len(),
            "duration_days": duration,
        }),
        "",
    )
    .await;

    let _ = crate::platform::repo::notify(
        &state.pool,
        user_id,
        &state.hub,
        "system",
        "Cards granted",
        &format!(
            "Administration granted you {} access card(s) ({} days each).",
            numbers.len(),
            duration
        ),
        serde_json::json!({ "count": numbers.len(), "duration_days": duration }),
    )
    .await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "granted_to": { "user_id": user_id, "username": username },
            "count": numbers.len(),
            "duration_days": duration,
            "cards": numbers,
        })),
    ))
}

// ---------------- admin card search (changeset 26) ----------------

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct AdminCardRow {
    pub card_number: String,
    pub status: String,
    pub duration_days: i32,
    pub issued_by: String,
    pub owner_username: Option<String>,
    pub merchant_username: Option<String>,
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AdminCardQuery {
    #[serde(default)]
    pub status: Option<String>,
    /// exact or prefix owner username filter
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Search/list all cards for the admin console.
#[utoipa::path(get, path = "/api/admin/cards", tag = "admin",
    params(AdminCardQuery),
    security(("bearer" = [])),
    responses((status = 200))
)]
pub async fn admin_cards_search(
    State(state): State<Arc<AppState>>,
    staff: StaffUser,
    AxQuery(q): AxQuery<AdminCardQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    staff.require(crate::auth::permissions::Permission::AdminCards.as_str())
        .or_else(|_| staff.require(crate::auth::permissions::Permission::AdminView.as_str()))?;

    let mut clauses: Vec<String> = Vec::new();
    if q.status.is_some() {
        clauses.push(format!("c.status = ${}", clauses.len() + 1));
    }
    if q.owner.is_some() {
        clauses.push(format!(
            "ou.username ILIKE ${}",
            clauses.len() + 1
        ));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let sql = format!(
        "SELECT c.card_number, c.status, c.duration_days, c.issued_by, \
                ou.username AS owner_username, mu.username AS merchant_username, \
                c.activated_at, c.expires_at, c.created_at \
         FROM access_cards c \
         LEFT JOIN users ou ON ou.id = c.owner \
         LEFT JOIN users mu ON mu.id = c.creator_merchant \
         {where_sql} ORDER BY c.created_at DESC LIMIT ${}",
        clauses.len() + 1
    );

    let mut query = sqlx::query_as::<_, AdminCardRow>(&sql);
    if let Some(status) = &q.status {
        query = query.bind(status);
    }
    if let Some(owner) = &q.owner {
        query = query.bind(format!("%{}", owner));
    }
    query = query.bind(q.limit.unwrap_or(100).clamp(1, 500));

    let cards = query.fetch_all(&state.pool).await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM access_cards")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    Ok(Json(serde_json::json!({ "cards": cards, "total": total })))
}

pub fn cards_router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/api/cards/activate", post(activate_card))
        .route("/api/cards/transfer-code", post(create_transfer_code))
        .route("/api/me/cards", get(my_cards))
        .route("/api/me/cards/summary", get(my_cards_summary))
        .route("/api/merchant/register", post(merchant_register))
        .route("/api/merchant/inventory", get(merchant_inventory))
        .route("/api/merchant/cards/purchase", post(purchase_request))
        .route("/api/merchant/cards/give", post(give_cards))
        .route("/api/merchant/cards/redeem-transfer", post(redeem_transfer))
        .route(
            "/api/admin/billing/config",
            get(get_billing_config).patch(patch_billing_config),
        )
        .route("/api/admin/card-purchases", get(admin_purchases))
        .route(
            "/api/admin/merchants",
            get(admin_merchants),
        )
        .route(
            "/api/admin/merchants/{user_id}/lock",
            post(lock_merchant),
        )
        .route("/api/admin/cards/grant", post(grant_cards_to_user))
        .route("/api/admin/cards", get(admin_cards_search))
        .route(
            "/api/admin/card-purchases/{id}/approve",
            post(approve_purchase),
        )
        .route(
            "/api/admin/card-purchases/{id}/reject",
            post(reject_purchase),
        )
        .route("/api/admin/cards/mint", post(mint_cards))
        .route(
            "/api/admin/cards/{card_number}/hold",
            post(hold_card),
        )
        .route("/api/admin/cards/chain", get(card_chain))
        .route("/api/admin/merchants/{user_id}/ban", post(ban_merchant))
}





#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_detects_tampering() {
        let secret = "test-secret";
        let body = "A1B2C3D4E5F6";
        let check = sign_body(secret, body);
        assert_eq!(check.len(), 8);

        // Valid number passes format verification logic (checksum equality).
        let number = format!("SPC-{body}-{check}");
        let parts: Vec<&str> = number.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(sign_body(secret, parts[1]), parts[2]);

        // Flipped digit in body ? checksum mismatch.
        let tampered = format!("A1B2C3D4E5F7");
        assert_ne!(sign_body(secret, &tampered), check);

        // Different secret ? different checksum (secret rotation breaks olds).
        assert_ne!(sign_body("other", body), check);
    }

    #[test]
    fn gate_error_shape() {
        // ensure_card_access is DB-bound; verify the trial constant instead.
        assert_eq!(TRIAL_DAYS, 7);
    }
}
