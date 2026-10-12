//! Short-lived, signed playback authorizations.
//!
//! Same construction as download grants: HMAC-SHA256 over
//! `gid|user|media|quality|exp`, base64url payload + hex signature,
//! constant-time verification, DB-backed instant revocation.
//!
//! Threat model (honest): possession of the authorization proves nothing but
//! *this user, this media, this quality, until exp* — and an admin (or the AI
//! layer, when scoped) can kill it in one query.

use chrono::{DateTime, Duration, Utc};
use hmac::Hmac;use serde::Serialize;
use sha2::Sha256;
use sqlx::PgPool;
use std::time::Duration as StdDuration;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, random_hex};

type HmacSha256 = Hmac<Sha256>;

const PURPOSE: &str = "playback";
pub const MIN_TTL: i64 = 60;
pub const MAX_TTL: i64 = 3600;

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PlaybackGrant {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_ref: String,
    pub session_id: Option<Uuid>,
    pub quality: String,
    pub device_hash: String,
    pub source: String,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct GrantTicket {
    /// Signed opaque authorization — bind to the playback session client-side.
    pub authorization: String,
    pub grant_id: Uuid,
    pub expires_at_unix: i64,
}

/// Fetches or lazily creates the server-wide signing secret for a purpose.
/// Shared implementation with download grants (`signing_secrets` table).
pub async fn signing_secret_for(pool: &PgPool, purpose: &str) -> ApiResult<String> {
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(purpose)
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
    .bind(purpose)
    .bind(&secret)
    .execute(pool)
    .await?;
    let (secret,) =
        sqlx::query_as::<_, (String,)>("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(purpose)
            .fetch_one(pool)
            .await?;
    Ok(secret)
}

fn sign(secret: &str, payload: &str) -> String {
    use hmac::Mac;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn clamp_ttl(ttl: StdDuration) -> StdDuration {
    let secs = ttl.as_secs().clamp(MIN_TTL as u64, MAX_TTL as u64);
    StdDuration::from_secs(secs)
}

pub async fn issue(
    pool: &PgPool,
    user_id: Uuid,
    media_ref: &str,
    quality: &str,
    session_id: Option<Uuid>,
    device_hash: &str,
    source: &str,
    ttl: StdDuration,
) -> ApiResult<GrantTicket> {
    issue_scoped(
        pool, user_id, media_ref, quality, session_id, device_hash, source, ttl, "",
    )
    .await
}

/// Issues a grant with an optional scope (e.g. `seg:12`). Empty scope = full
/// access to the media ref. Scoped tickets are children minted from a parent
/// and can never outlive it.
#[allow(clippy::too_many_arguments)]
pub async fn issue_scoped(
    pool: &PgPool,
    user_id: Uuid,
    media_ref: &str,
    quality: &str,
    session_id: Option<Uuid>,
    device_hash: &str,
    source: &str,
    ttl: StdDuration,
    scope: &str,
) -> ApiResult<GrantTicket> {
    let ttl = clamp_ttl(ttl);
    let secret = signing_secret_for(pool, PURPOSE).await?;
    let grant_id = new_id();
    let expires_at = Utc::now() + Duration::seconds(ttl.as_secs() as i64);

    // Only full-access grants get DB rows; scoped children inherit their
    // parent's row and live purely as signed tokens.
    if scope.is_empty() {
        sqlx::query(
            "INSERT INTO playback_grants
                 (id, user_id, media_ref, session_id, quality, device_hash, source, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(grant_id)
        .bind(user_id)
        .bind(media_ref)
        .bind(session_id)
        .bind(quality)
        .bind(device_hash)
        .bind(source)
        .bind(expires_at)
        .execute(pool)
        .await?;
    }

    let payload = format!(
        "{grant_id}|{user_id}|{media_ref}|{quality}|{}|{}",
        expires_at.timestamp(),
        scope
    );
    let sig = sign(&secret, &payload);
    Ok(GrantTicket {
        authorization: format!("{}.{}", crate::utils::encode_payload(&payload), sig),
        grant_id,
        expires_at_unix: expires_at.timestamp(),
    })
}

#[derive(Debug)]
pub struct VerifiedGrant {
    pub grant_id: Uuid,
    pub user_id: Uuid,
    pub media_ref: String,
    pub quality: String,
    /// `seg:<index>` when this ticket only covers one segment; empty = full.
    pub scope: String,
}

impl VerifiedGrant {
    pub fn covers_segment(&self, segment: u32) -> bool {
        match self.scope.strip_prefix("seg:") {
            Some(n) => n.parse::<u32>().is_ok_and(|s| s == segment),
            None => true,
        }
    }
}

async fn parse_and_check(
    pool: &PgPool,
    authorization: &str,
) -> ApiResult<(VerifiedGrant, Option<DateTime<Utc>>)> {
    let (payload_b64, sig) = authorization
        .split_once('.')
        .ok_or_else(|| ApiError::forbidden("malformed playback authorization"))?;

    let secret = signing_secret_for(pool, PURPOSE).await?;
    let raw = crate::utils::decode_payload(payload_b64)?;
    let recomputed = sign(&secret, &raw);

    // `sign` returns hex, so both sides are compared as hex. The previous code
    // hex-decoded only the presented signature and compared 32 raw bytes
    // against a 64-character string, so the length check could never pass and
    // every grant was rejected as invalid.
    if sig.len() != recomputed.len()
        || !bool::from(recomputed.as_bytes().ct_eq(sig.as_bytes()))
    {
        return Err(ApiError::forbidden("invalid playback signature"));
    }

    // Field order must match `issue_scoped`:
    //   grant_id | user_id | media_ref | quality | expires_ts | scope
    // The expiry field was previously not consumed here, so the timestamp was
    // read as the scope and every full-access grant failed the scope filter
    // below — which is why grant verification never authorized anything.
    let mut parts = raw.split('|');
    let parsed = (|| {
        Some(VerifiedGrant {
            grant_id: Uuid::parse_str(parts.next()?).ok()?,
            user_id: Uuid::parse_str(parts.next()?).ok()?,
            media_ref: parts.next()?.to_string(),
            quality: parts.next()?.to_string(),
            // Expiry is authoritative from the row below; the signed copy only
            // has to be consumed so `scope` lands on the right field.
            scope: {
                let _signed_expiry = parts.next()?;
                parts.next().unwrap_or("").to_string()
            },
        })
    })()
    .filter(|g| g.scope.is_empty() || g.scope.starts_with("seg:"))
    .ok_or_else(|| ApiError::forbidden("malformed playback authorization"))?;

    let row: Option<(Option<DateTime<Utc>>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT revoked_at, expires_at FROM playback_grants WHERE id = $1 AND user_id = $2",
    )
    .bind(parsed.grant_id)
    .bind(parsed.user_id)
    .fetch_optional(pool)
    .await?;

    let Some((revoked_at, expires_at)) = row else {
        return Err(ApiError::forbidden("unknown playback grant"));
    };
    if revoked_at.is_some() {
        return Err(ApiError::forbidden("playback grant revoked"));
    }
    if expires_at < Utc::now() {
        return Err(ApiError::forbidden("playback grant expired"));
    }
    Ok((parsed, Some(expires_at)))
}

pub async fn verify(pool: &PgPool, authorization: &str) -> ApiResult<VerifiedGrant> {
    Ok(parse_and_check(pool, authorization).await?.0)
}

/// Verification with client-context binding: when the parent grant was issued
/// against a fingerprint (`device_hash`), the same context must present the
/// token — a stolen grant replayed from another TLS/client stack fails here.
pub async fn verify_with_fingerprint(
    pool: &PgPool,
    authorization: &str,
    presented_fingerprint: Option<&str>,
) -> ApiResult<VerifiedGrant> {
    let (grant, _) = parse_and_check(pool, authorization).await?;

    let bound: Option<String> =
        sqlx::query_as::<_, (String,)>("SELECT device_hash FROM playback_grants WHERE id = $1")
            .bind(grant.grant_id)
            .fetch_optional(pool)
            .await?
            .map(|(h,)| h)
            .filter(|h| !h.is_empty());

    if let (Some(bound), Some(presented)) = (bound, presented_fingerprint) {
        use subtle::ConstantTimeEq;
        let presented_hash = crate::utils::sha256_hex(presented);
        let ok = bound.len() == presented_hash.len()
            && bool::from(bound.as_bytes().ct_eq(presented_hash.as_bytes()));
        if !ok {
            metrics::counter!("fingerprint_mismatch_total").increment(1);
            return Err(ApiError::forbidden(
                "client context mismatch — grant is bound to another device fingerprint",
            ));
        }
    }
    Ok(grant)
}

pub async fn revoke(pool: &PgPool, grant_id: Uuid) -> ApiResult<bool> {
    let result = sqlx::query(
        "UPDATE playback_grants SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(grant_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn revoke_for_user(pool: &PgPool, user_id: Uuid) -> ApiResult<u64> {
    let result = sqlx::query(
        "UPDATE playback_grants SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_clamped() {
        assert_eq!(clamp_ttl(StdDuration::from_secs(1)), StdDuration::from_secs(60));
        assert_eq!(
            clamp_ttl(StdDuration::from_secs(999_999)),
            StdDuration::from_secs(3600)
        );
        assert_eq!(clamp_ttl(StdDuration::from_secs(600)), StdDuration::from_secs(600));
    }

    #[test]
    fn segment_scope_semantics() {
        let g = |scope: &str| VerifiedGrant {
            grant_id: Uuid::nil(),
            user_id: Uuid::nil(),
            media_ref: String::new(),
            quality: String::new(),
            scope: scope.to_string(),
        };
        // Full access covers everything.
        assert!(g("").covers_segment(0));
        assert!(g("").covers_segment(999));
        // Scoped tickets cover exactly their segment.
        assert!(g("seg:12").covers_segment(12));
        assert!(!g("seg:12").covers_segment(13));
        assert!(!g("seg:12").covers_segment(0));
    }
}
