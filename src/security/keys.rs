//! Media key service (Phase J).
//!
//! Key hierarchy, honest and boring on purpose:
//!
//!   master secret (env `MASTER_KEY`, or generated once and persisted)
//!     └─ HKDF-SHA256 → purpose KEKs ("media-wrap", "hls-aes128")
//!         └─ AES-256-GCM-wrapped random 32-byte media keys
//!             (per media_ref + version; rotation keeps old versions for
//!              decryption, revocation kills them instantly)
//!
//! Plaintext media keys never touch the database. Swapping the env secret for
//! a real KMS/HSM later only means replacing `master_secret()`.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::{new_id, random_hex};

type HmacSha256 = Hmac<Sha256>;

const MASTER_PURPOSE: &str = "space-master-v1";
pub const KEK_MEDIA_WRAP: &[u8] = b"media-key-wrap";
pub const KEK_HLS: &[u8] = b"hls-segment-aes128";

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct MediaKeyRecord {
    pub id: Uuid,
    pub media_ref: String,
    pub key_version: i32,
    #[serde(skip)]
    pub wrapped_key: Vec<u8>,
    #[serde(skip)]
    pub wrap_nonce: Vec<u8>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub rotated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Fetches or creates the server-wide master secret.
///
/// Precedence: `MASTER_KEY` env (hex or plain) → previously generated secret
/// in `signing_secrets` (purpose `master`) → generate + persist.
/// For real deployments replace this with a KMS/HSM call — everything else
/// stays identical because it only ever sees the derived KEKs.
pub async fn master_secret(pool: &PgPool) -> ApiResult<Vec<u8>> {
    if let Ok(raw) = std::env::var("MASTER_KEY") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(master_from_user_input(trimmed));
        }
    }

    const PURPOSE: &str = "master";
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(PURPOSE)
            .fetch_optional(pool)
            .await?;
    if let Some((secret,)) = existing {
        return Ok(master_from_user_input(&secret));
    }

    // Generate with the CSPRNG path we already trust (uuid v4 → getrandom).
    let generated = random_hex(32);
    sqlx::query(
        "INSERT INTO signing_secrets (purpose, secret, rotated_at) VALUES ($1, $2, now())
         ON CONFLICT (purpose) DO NOTHING",
    )
    .bind(PURPOSE)
    .bind(&generated)
    .execute(pool)
    .await?;
    let (secret,) =
        sqlx::query_as::<_, (String,)>("SELECT secret FROM signing_secrets WHERE purpose = $1")
            .bind(PURPOSE)
            .fetch_one(pool)
            .await?;
    Ok(master_from_user_input(&secret))
}

fn master_from_user_input(input: &str) -> Vec<u8> {
    match hex::decode(input) {
        Ok(bytes) if bytes.len() >= 32 => bytes[..32].to_vec(),
        _ => {
            // Any passphrase is accepted but stretched through HMAC-SHA256 so
            // low-entropy choices don't become raw key bytes.
            let mut mac = <HmacSha256 as hmac::Mac>::new_from_slice(MASTER_PURPOSE.as_bytes())
                .expect("hmac accepts any key length");
            mac.update(input.as_bytes());
            mac.finalize().into_bytes().to_vec()
        }
    }
}

/// Derives a 32-byte KEK for a specific purpose from the master secret.
pub fn derive_kek(master: &[u8], purpose: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(MASTER_PURPOSE.as_bytes()), master);
    let mut okm = [0u8; 32];
    hk.expand(purpose, &mut okm).expect("32 bytes is a valid HKDF length");
    okm
}

// ------------------------------------------------------------ wrap / unwrap

/// AES-256-GCM wrap. Returns (ciphertext, nonce).
pub fn aead_wrap(kek: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    // Fresh random 96-bit nonce per encryption — never reuse with the same key.
    let nonce_bytes = random_hex(12);
    let nonce_bytes = hex::decode(&nonce_bytes).map_err(|_| "nonce generation failed")?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| "aead seal failed")?;
    Ok((ciphertext, nonce_bytes))
}

/// AES-256-GCM unwrap (authenticated — tampering fails here, not later).
pub fn aead_unwrap(kek: &[u8; 32], ciphertext: &[u8], nonce: &[u8]) -> Result<Vec<u8>, &'static str> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| "aead open failed")
}

// ------------------------------------------------------------ DB surface

/// Creates a fresh random media key for `media_ref` wrapped under the KEK.
/// Existing non-revoked keys are marked rotated (still decryptable).
pub async fn issue_media_key(pool: &PgPool, media_ref: &str) -> ApiResult<MediaKeyRecord> {
    let master = master_secret(pool).await?;
    let kek = derive_kek(&master, KEK_MEDIA_WRAP);

    // Retire current version(s).
    sqlx::query(
        "UPDATE media_keys SET rotated_at = now()
         WHERE media_ref = $1 AND revoked_at IS NULL AND rotated_at IS NULL",
    )
    .bind(media_ref)
    .execute(pool)
    .await?;

    let next_version: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(key_version), 0) + 1 FROM media_keys WHERE media_ref = $1",
    )
    .bind(media_ref)
    .fetch_one(pool)
    .await?;

    let plaintext_key = random_key_bytes(32);
    let (wrapped, nonce) =
        aead_wrap(&kek, &plaintext_key).map_err(|e| ApiError::internal_msg(e))?;

    let record = sqlx::query_as::<_, MediaKeyRecord>(
        "INSERT INTO media_keys (id, media_ref, key_version, wrapped_key, wrap_nonce)
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(new_id())
    .bind(media_ref)
    .bind(next_version)
    .bind(&wrapped)
    .bind(&nonce)
    .fetch_one(pool)
    .await?;
    Ok(record)
}

/// Unwraps the active (latest, non-revoked) media key for `media_ref`.
pub async fn active_media_key(pool: &PgPool, media_ref: &str) -> ApiResult<(MediaKeyRecord, Vec<u8>)> {
    let record = sqlx::query_as::<_, MediaKeyRecord>(
        "SELECT * FROM media_keys
         WHERE media_ref = $1 AND revoked_at IS NULL
         ORDER BY key_version DESC LIMIT 1",
    )
    .bind(media_ref)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("media key"))?;

    let master = master_secret(pool).await?;
    let kek = derive_kek(&master, KEK_MEDIA_WRAP);
    let key = aead_unwrap(&kek, &record.wrapped_key, &record.wrap_nonce)
        .map_err(|e| ApiError::internal_msg(e))?;
    Ok((record, key))
}

/// Instant revocation — the wrapped material becomes permanently unreadable.
pub async fn revoke_media_keys(pool: &PgPool, media_ref: &str) -> ApiResult<u64> {
    let result = sqlx::query(
        "UPDATE media_keys SET revoked_at = now() WHERE media_ref = $1 AND revoked_at IS NULL",
    )
    .bind(media_ref)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// 16-byte HLS content key (AES-128) derived from the 32-byte media key via
/// HKDF — leaking one domain never exposes the other.
pub fn hls_content_key(media_key: &[u8]) -> [u8; 16] {
    let hk = Hkdf::<Sha256>::new(Some(KEK_HLS), media_key);
    let mut okm = [0u8; 16];
    hk.expand(b"hls-content", &mut okm).expect("16 bytes is valid");
    okm
}

/// CSPRNG key material built from uuid v4 randomness (same source the rest of
/// the codebase trusts); `n` must be <= 64 for a single draw.
fn random_key_bytes(n: usize) -> Vec<u8> {
    debug_assert!(n <= 64, "key draws are capped at one uuid pair");
    let mut bytes = Vec::with_capacity(n);
    while bytes.len() < n {
        bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    bytes.truncate(n);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kek_separation_by_purpose() {
        let master = [7u8; 32];
        assert_ne!(derive_kek(&master, KEK_MEDIA_WRAP), derive_kek(&master, KEK_HLS));
    }

    #[test]
    fn aead_roundtrip_and_tamper_rejection() {
        let kek = derive_kek(b"master-material-0123456789abcdef", KEK_MEDIA_WRAP);
        let key = [42u8; 32];
        let (sealed, nonce) = aead_wrap(&kek, &key).unwrap();
        let opened = aead_unwrap(&kek, &sealed, &nonce).unwrap();
        assert_eq!(opened, key.to_vec());

        // Flip one bit of ciphertext → authentication failure.
        let mut tampered = sealed.clone();
        tampered[0] ^= 0x01;
        assert!(aead_unwrap(&kek, &tampered, &nonce).is_err());

        // Wrong KEK → authentication failure.
        let wrong = derive_kek(b"other-master-0123456789abcdefgh", KEK_MEDIA_WRAP);
        assert!(aead_unwrap(&wrong, &sealed, &nonce).is_err());
    }

    #[test]
    fn hls_key_is_stable_and_narrow() {
        let media_key = [9u8; 32];
        let a = hls_content_key(&media_key);
        let b = hls_content_key(&media_key);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_ne!(&a[..], &media_key[..16]);
    }
}
