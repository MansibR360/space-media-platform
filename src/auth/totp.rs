//! TOTP two-factor auth (RFC 6238) — implemented with the crates we already
//! ship: HMAC-SHA1 + a small RFC-4648 base32 codec. No new dependencies.
//!
//! Admin flow:
//!   POST /api/admin/security/totp/setup   → { secret, otpauth_uri } (not yet enabled)
//!   POST /api/admin/security/totp/enable  → { code } verifies once, then enabled
//!   POST /api/admin/security/totp/disable → { code }
//! Login: admins with TOTP enabled must send `totp_code` alongside credentials.

use hmac::{Hmac, Mac};
use sha1::Sha1;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

// ------------------------------------------------------------ base32

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

pub fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for ch in input.chars() {
        if ch == '=' || ch == ' ' {
            continue;
        }
        let upper = ch.to_ascii_uppercase();
        let value = ALPHABET.iter().position(|&a| a == upper as u8)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

// ------------------------------------------------------------ TOTP core

fn hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac = <Hmac<Sha1> as hmac::Mac>::new_from_slice(secret)
        .expect("hmac-sha1 accepts any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = ((digest[offset] & 0x7f) as u32) << 24
        | (digest[offset + 1] as u32) << 16
        | (digest[offset + 2] as u32) << 8
        | digest[offset + 3] as u32;
    binary % 1_000_000
}

/// Current 6-digit code for a base32 secret at unix time `now_secs` (test vector helper).
#[allow(dead_code)]
pub fn code_at(secret_b32: &str, now_secs: i64) -> Option<u32> {
    let secret = base32_decode(secret_b32)?;
    Some(hotp(&secret, (now_secs / 30) as u64))
}

/// Verify with ±1 step clock skew tolerance.
pub fn verify(secret_b32: &str, code: u32, now_secs: i64) -> bool {
    let Some(secret) = base32_decode(secret_b32) else {
        return false;
    };
    let counter = (now_secs / 30) as u64;
    [counter.wrapping_sub(1), counter, counter + 1]
        .iter()
        .any(|c| hotp(&secret, *c) == code)
}

/// Generates a fresh random secret (160-bit) as base32 + provisioning URI.
pub fn generate(account: &str) -> (String, String) {
    let raw = crate::utils::random_hex(20); // 40 hex chars = 160 bits when decoded
    let bytes = hex::decode(&raw).expect("hex of random_hex");
    let secret = base32_encode(&bytes);
    let uri = format!(
        "otpauth://totp/SPACE:{account}?secret={secret}&issuer=SPACE&algorithm=SHA1&digits=6&period=30"
    );
    (secret, uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_test_vector() {
        // RFC 6238 appendix B uses ASCII "12345678901234567890" secret.
        let secret = base32_encode(b"12345678901234567890");
        // T=59 → 94287082 truncated to 6 digits → 287082
        assert_eq!(code_at(&secret, 59), Some(287082));
        assert_eq!(code_at(&secret, 1111111109), Some(081804));
        assert_eq!(code_at(&secret, 1234567890), Some(005924));
    }

    #[test]
    fn verify_window_and_reject() {
        let secret = base32_encode(b"12345678901234567890");
        let t = 1111111109;
        let code = code_at(&secret, t).unwrap();
        assert!(verify(&secret, code, t));
        assert!(verify(&secret, code, t + 29)); // next step boundary ±1 window
        assert!(!verify(&secret, 000000, t));
    }

    #[test]
    fn base32_roundtrip() {
        let data = b"attack at dawn 0123";
        let encoded = base32_encode(data);
        assert_eq!(base32_decode(&encoded).as_deref(), Some(&data[..]));
    }
}

