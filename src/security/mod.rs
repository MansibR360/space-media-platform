//! Playback Security Controller (Phase C).
//!
//! One place through which every playback authorization flows:
//! authentication → risk assessment → short-lived signed grant → revocation.
//!
//! The whole risk layer is OFF until an admin enables it (`risk_policies`);
//! with the layer off, authorization reduces to "authenticated user gets a
//! short-lived grant" — callers work identically either way.

pub mod grants;
pub mod keys;
pub mod lockdown;
pub mod offline;
pub mod risk;
pub mod routes;
pub mod watermark;

/// Client-context fingerprint for the request: edge-computed JA4 header
/// wins, then a client-computed fingerprint, then an explicit body value.
pub fn presented_fingerprint(
    headers: &axum::http::HeaderMap,
    body_value: Option<&str>,
) -> Option<String> {
    const JA4: &str = "x-ja4";
    const CLIENT: &str = "x-client-fingerprint";
    for name in [JA4, CLIENT] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    body_value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Stable server-side hash of the fingerprint (what we persist, never raw).
pub fn hash_fingerprint(fp: &str) -> String {
    crate::utils::sha256_hex(fp)[..32].to_string()
}
