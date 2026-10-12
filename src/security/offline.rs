//! Offline playback licenses (changeset 4).
//!
//! The contract, stated honestly:
//!
//! * A user with a valid grant requests a license for downloaded media.
//! * The AES content key is delivered **once over TLS**, bound to
//!   user + media + device fingerprint. The server stores no plaintext key —
//!   it re-derives the same content key from the media key service until the
//!   key is rotated or revoked.
//! * The client MUST store it in OS-protected storage (DPAPI / Keychain /
//!   Android Keystore). A key dumped from disk is out of server reach — that
//!   is what the 24 h TTL and revocation are for: playback windows expire,
//!   and the next window requires a fresh license check.
//! * Default TTL 24 h (env `OFFLINE_LICENSE_TTL_SECS`, clamped 1 h – 7 d).

use serde::Serialize;
use sqlx::PgPool;
use std::time::Duration as StdDuration;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::security::keys;
use crate::utils::new_id;

pub const MIN_TTL_SECS: u64 = 3600;
pub const MAX_TTL_SECS: u64 = 7 * 24 * 3600;
pub const DEFAULT_TTL_SECS: u64 = 24 * 3600;

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct OfflineLicense {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_ref: String,
    pub device_hash: String,
    pub issued_count: i32,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub fn clamp_ttl(ttl: StdDuration) -> StdDuration {
    StdDuration::from_secs(ttl.as_secs().clamp(MIN_TTL_SECS, MAX_TTL_SECS))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LicenseIssued {
    pub license_id: Uuid,
    /// hex-encoded AES content key — store in OS keystore, never on disk plain
    pub content_key_hex: String,
    /// "AES-128" — matches the HLS encryption scheme
    pub algorithm: &'static str,
    pub media_ref: String,
    pub expires_at_unix: i64,
    /// seconds remaining at issuance
    pub ttl_secs: i64,
    /// true when an existing unexpired license was returned instead of minted
    pub existing: bool,
}

/// Issues (or idempotently returns) the offline license for this triple.
/// `device_hash` must be the same fingerprint hash used for playback grants.
pub async fn issue(
    pool: &PgPool,
    user_id: Uuid,
    media_ref: &str,
    device_hash: &str,
    ttl: StdDuration,
) -> ApiResult<LicenseIssued> {
    let ttl = clamp_ttl(ttl);

    // Idempotent within the validity window: same user+media+device gets the
    // same license back rather than burning through daily limits.
    let existing: Option<(Uuid, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, expires_at FROM offline_licenses \
         WHERE user_id = $1 AND media_ref = $2 AND device_hash = $3 \
           AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(user_id)
    .bind(media_ref)
    .bind(device_hash)
    .fetch_optional(pool)
    .await?;

    if let Some((license_id, expires_at)) = existing {
        return deliver(pool, media_ref, license_id, expires_at, true).await;
    }

    // Fresh license: upsert row (unique on the triple; expired rows renew).
    let license_id = new_id();
    let expires_at = chrono::Utc::now()
        + chrono::Duration::seconds(ttl.as_secs() as i64);
    sqlx::query(
        "INSERT INTO offline_licenses (id, user_id, media_ref, device_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id, media_ref, device_hash) DO UPDATE SET
            expires_at = EXCLUDED.expires_at,
            revoked_at = NULL,
            issued_count = offline_licenses.issued_count + 1",
    )
    .bind(license_id)
    .bind(user_id)
    .bind(media_ref)
    .bind(device_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;

    metrics::counter!("offline_licenses_issued_total").increment(1);
    tracing::info!(%user_id, %media_ref, ttl_secs = ttl.as_secs(), "offline license issued");
    deliver(pool, media_ref, license_id, expires_at, false).await
}

/// Re-derives the content key for this media at delivery time. Key rotation
/// after download means the stored offline copy keeps playing only until its
/// license expires — then the new key is required and old material is dead.
async fn deliver(
    pool: &PgPool,
    media_ref: &str,
    license_id: Uuid,
    expires_at: chrono::DateTime<chrono::Utc>,
    existing: bool,
) -> ApiResult<LicenseIssued> {
    let key_pair = match keys::active_media_key(pool, media_ref).await {
        Ok(pair) => pair,
        Err(_) => {
            keys::issue_media_key(pool, media_ref).await?;
            keys::active_media_key(pool, media_ref).await?
        }
    };
    let (_, media_key) = key_pair;
    let content_key = keys::hls_content_key(&media_key);
    let now = chrono::Utc::now();
    Ok(LicenseIssued {
        license_id,
        content_key_hex: hex::encode(content_key),
        algorithm: "AES-128",
        media_ref: media_ref.to_string(),
        expires_at_unix: expires_at.timestamp(),
        ttl_secs: (expires_at - now).num_seconds().max(0),
        existing,
    })
}

pub async fn revoke_for_user_media(pool: &PgPool, user_id: Uuid, media_ref: &str) -> ApiResult<u64> {
    let result = sqlx::query(
        "UPDATE offline_licenses SET revoked_at = now() \
         WHERE user_id = $1 AND media_ref = $2 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .bind(media_ref)
    .execute(pool)
    .await?;
    if result.rows_affected() > 0 {
        metrics::counter!("offline_licenses_revoked_total").increment(result.rows_affected());
    }
    Ok(result.rows_affected())
}

pub async fn revoke_by_id(pool: &PgPool, license_id: Uuid) -> ApiResult<bool> {
    let result = sqlx::query(
        "UPDATE offline_licenses SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(license_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<OfflineLicense>> {
    Ok(sqlx::query_as::<_, OfflineLicense>(
        "SELECT * FROM offline_licenses WHERE user_id = $1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// True when the user holds a live license for this media — usable by any
/// future offline-validation endpoint before serving cached segments.
#[allow(dead_code)]
pub async fn has_valid_license(pool: &PgPool, user_id: Uuid, media_ref: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM offline_licenses \
         WHERE user_id = $1 AND media_ref = $2 AND revoked_at IS NULL AND expires_at > now())",
    )
    .bind(user_id)
    .bind(media_ref)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

pub fn ensure_not_expired(expires_at: chrono::DateTime<chrono::Utc>) -> ApiResult<()> {
    if expires_at < chrono::Utc::now() {
        return Err(ApiError::forbidden("offline license expired"));
    }
    Ok(())
}
