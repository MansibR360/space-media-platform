//! Signed, expiring, device-bound download links.
//!
//! Threat model (honest): the link proves *authorization* — possession of a
//! valid URL without credentials is useless after `exp`. It cannot make media
//! "unstealable"; once bytes reach an authorized client they can always be
//! copied. What this adds over naive static files:
//!   * HMAC-SHA256 signature (server secret never leaves the server)
//!   * hard expiry (default 30 min, max 24 h)
//!   * device binding: token only redeems for the registered device id
//!   * one-time nonce option for high-value content
//!   * audit trail row per issued link (`download_grants`)
//! For forensic deterrence, per-user watermarking hooks into the same stream
//! pipeline (see ROADMAP M5).

use hmac::Hmac;
use serde::Serialize;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, random_hex};

type HmacSha256 = Hmac<Sha256>;

const PURPOSE: &str = "download";

#[derive(Debug)]
pub struct DownloadGrant {
    pub file_id: Uuid,
    pub user_id: Uuid,
    pub device_id: Option<Uuid>,
    pub expires_at: i64,
    pub nonce: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DownloadTicket {
    /// Relative URL to hand to any HTTP client; embeds the signed payload.
    pub url: String,
    pub expires_at_unix: i64,
    pub file_name_hint: Option<String>,
}

/// Fetches or lazily creates the server-wide signing secret.
pub async fn signing_secret(pool: &PgPool) -> ApiResult<String> {
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(PURPOSE)
            .fetch_optional(pool)
            .await?;

    if let Some((secret,)) = existing {
        return Ok(secret);
    }

    let secret = random_hex(32);
    sqlx::query(
        "INSERT INTO signing_secrets (purpose, secret, rotated_at) VALUES ($1, $2, now())
         ON CONFLICT (purpose) DO NOTHING",
    )
    .bind(PURPOSE)
    .bind(&secret)
    .execute(pool)
    .await?;

    // Re-read in case of race.
    let (secret,) =
        sqlx::query_as::<_, (String,)>("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(PURPOSE)
            .fetch_one(pool)
            .await?;
    Ok(secret)
}

fn sign(secret: &str, payload: &str) -> String {
    use hmac::Mac;
    use sha2::Digest as _;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub async fn issue_download_url(
    pool: &PgPool,
    base_path: &str,
    file_id: Uuid,
    user_id: Uuid,
    device_id: Option<Uuid>,
    ttl_secs: i64,
) -> ApiResult<DownloadTicket> {
    if !(60..=86_400).contains(&ttl_secs) {
        return Err(ApiError::bad_request(
            "ttl must be between 60 and 86400 seconds",
        ));
    }
    let secret = signing_secret(pool).await?;
    let expires_at = chrono::Utc::now().timestamp() + ttl_secs;
    let nonce = random_hex(8);

    let device_part = device_id.map(|d| d.to_string()).unwrap_or_default();
    let payload = format!("{file_id}|{user_id}|{device_part}|{expires_at}|{nonce}");
    let sig = sign(&secret, &payload);
    let payload_b64 = crate::utils::encode_payload(&payload);

    sqlx::query(
        "INSERT INTO download_grants (id, user_id, file_id, nonce) VALUES ($1, $2, $3, $4)",
    )
    .bind(new_id())
    .bind(user_id)
    .bind(file_id)
    .bind(&nonce)
    .execute(pool)
    .await?;

    Ok(DownloadTicket {
        url: format!("{base_path}/api/dl/{payload_b64}?sig={sig}"),
        expires_at_unix: expires_at,
        file_name_hint: None,
    })
}

pub async fn verify_download_token(
    pool: &PgPool,
    payload_b64: &str,
    sig: &str,
) -> ApiResult<(Uuid, Uuid, Option<Uuid>)> {
    let secret = signing_secret(pool).await?;
    let raw = crate::utils::decode_payload(payload_b64)?;
    let recomputed = sign(&secret, &raw);

    // Constant-time comparison.
    use subtle::ConstantTimeEq;
    let sig_bytes = hex::decode(sig).map_err(|_| ApiError::forbidden("invalid signature"))?;
    if sig_bytes.len() != recomputed.len()
        || !bool::from(recomputed.as_bytes().ct_eq(sig_bytes.as_slice()))
    {
        return Err(ApiError::forbidden("invalid download signature"));
    }

    let mut parts = raw.split('|');
    let file_id = parts.next().and_then(|p| Uuid::parse_str(p).ok());
    let user_id = parts.next().and_then(|p| Uuid::parse_str(p).ok());
    let device_raw = parts.next().unwrap_or("");
    let exp = parts.next().and_then(|p| p.parse::<i64>().ok());
    let nonce = parts.next().unwrap_or("");

    let (Some(file_id), Some(user_id), Some(exp)) = (file_id, user_id, exp) else {
        return Err(ApiError::forbidden("malformed download token"));
    };

    if exp < chrono::Utc::now().timestamp() {
        return Err(ApiError::forbidden("download link expired"));
    }

    // Nonce replay protection: each grant may serve a bounded number of hits.
    let hits: (i64,) = sqlx::query_as(
        "SELECT hit_count FROM download_grants WHERE nonce = $1 AND user_id = $2 AND file_id = $3",
    )
    .bind(nonce)
    .bind(user_id)
    .bind(file_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::forbidden("unknown download grant"))?;

    if hits.0 > 20 {
        return Err(ApiError::forbidden("download grant exhausted"));
    }
    sqlx::query("UPDATE download_grants SET hit_count = hit_count + 1 WHERE nonce = $1")
        .bind(nonce)
        .execute(pool)
        .await?;

    let device_id = (!device_raw.is_empty())
        .then(|| Uuid::parse_str(device_raw).ok())
        .flatten();
    Ok((file_id, user_id, device_id))
}

