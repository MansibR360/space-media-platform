//! Forensic watermarking (Phase J).
//!
//! Honest scope: a *URL-level* semi-visible mark. Every resolved stream URL
//! carries `wm=<code>` where code deterministically binds user + playback
//! session + coarse time bucket. If a copy leaks, the URL (and any manifest
//! that preserved query strings) identifies which authorized session produced
//! it. Pixel-level invisible watermarking stays a transcode-pipeline hook —
//! this changes redistribution economics without pretending to be DRM.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Stable per-session forensic code: 16 hex chars derived from the signing
/// inputs. Time buckets are hour-granular so marks rotate but stay traceable.
pub fn watermark_code(
    secret: &str,
    user_id: &uuid::Uuid,
    session_ref: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> String {
    let bucket = at.timestamp() / 3600; // hourly rotation
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(user_id.as_bytes());
    mac.update(session_ref.as_bytes());
    mac.update(bucket.to_le_bytes().as_slice());
    let digest = hex::encode(mac.finalize().into_bytes());
    digest[..16].to_string()
}

/// Percent-encode the characters a source may leave raw in a URL path.
///
/// External sources hand back paths containing literal spaces and brackets —
/// Internet Archive filenames routinely do. A browser will not load such a URL
/// at all: the `<video>` element fails with MEDIA_ERR_SRC_NOT_SUPPORTED before
/// a request is made. Only the path is touched; the scheme, host and query are
/// left exactly as the source produced them.
fn encode_path(url: &str) -> String {
    // Split off the query/fragment so their delimiters survive untouched.
    let split_at = url.find(['?', '#']).unwrap_or(url.len());
    let (base, rest) = url.split_at(split_at);

    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let (origin, path) = base.split_at(
        base[scheme_end..]
            .find('/')
            .map(|i| scheme_end + i)
            .unwrap_or(base.len()),
    );

    let encoded: String = path
        .chars()
        .map(|c| match c {
            // Already-safe URL characters, including the path separators and
            // sub-delims that must stay structural.
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' | ':' | '@' | '&'
            | '=' | '+' | '$' | ',' | ';' | '%' | '!' | '*' | '\'' => c.to_string(),
            other => {
                let mut buf = [0u8; 4];
                other
                    .encode_utf8(&mut buf)
                    .bytes()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect();

    format!("{origin}{encoded}{rest}")
}

/// Appends (or replaces) `wm=` on a stream URL without breaking existing
/// query strings or fragments.
///
/// Also normalizes the path, since this is the last thing every resolved
/// stream URL passes through before reaching a client.
pub fn stamp_url(url: &str, code: &str) -> String {
    let url = &encode_path(url);
    let separator = if url.contains('?') { '&' } else { '?' };
    let param = format!("wm={code}");
    // Replace an existing wm= instead of stacking duplicates.
    if url.contains("wm=") {
        return regex_replace_wm(url, &param);
    }
    match url.find('#') {
        Some(hash_at) => format!(
            "{}{separator}{param}{}",
            &url[..hash_at],
            &url[hash_at..]
        ),
        None => format!("{url}{separator}{param}"),
    }
}

fn regex_replace_wm(url: &str, replacement: &str) -> String {
    static WM_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"([?&])wm=[^&#]*").expect("watermark regex")
    });
    WM_RE.replace_all(url, replacement).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn code_is_deterministic_per_hour_bucket() {
        let uid = Uuid::now_v7();
        let t = chrono::Utc::now();
        let a = watermark_code("s3cret", &uid, "sess-1", t);
        let b = watermark_code("s3cret", &uid, "sess-1", t);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);

        // Different user or session → different mark.
        assert_ne!(a, watermark_code("s3cret", &Uuid::now_v7(), "sess-1", t));
        assert_ne!(a, watermark_code("s3cret", &uid, "sess-2", t));

        // Adjacent hours may differ (bucket boundary), but same-hour always matches.
        let next_hour = t + chrono::Duration::hours(1);
        let _ = watermark_code("s3cret", &uid, "sess-1", next_hour);
    }

    #[test]
    fn url_stamping() {
        let stamped = stamp_url("https://cdn/x/file.mp4", "abc123");
        assert_eq!(stamped, "https://cdn/x/file.mp4?wm=abc123");

        let stamped2 = stamp_url("https://cdn/x/file.m3u8?token=t", "abc123");
        assert!(stamped2.ends_with("&wm=abc123"));

        // Re-stamping replaces rather than stacks.
        let restamped = stamp_url(&stamped2, "def456");
        assert!(!restamped.contains("abc123"));
        assert!(restamped.contains("wm=def456"));
        assert_eq!(restamped.matches("wm=").count(), 1);

        // Fragments stay at the end.
        let frag = stamp_url("https://cdn/x#t=10", "code");
        assert!(frag.starts_with("https://cdn/x?wm=code#t=10"));
    }
}

#[cfg(test)]
mod path_encoding_tests {
    use super::*;

    #[test]
    fn encodes_spaces_and_brackets_in_path() {
        let raw = "https://ia903203.us.archive.org/1/items/x/Banjaare _ Barsaat (720P_HD).mp4";
        let out = stamp_url(raw, "abc123");
        assert!(!out.contains(' '), "spaces survived: {out}");
        assert!(out.contains("%20"), "spaces not encoded: {out}");
        assert!(out.contains("%28") && out.contains("%29"), "brackets not encoded: {out}");
        assert!(out.starts_with("https://ia903203.us.archive.org/1/items/x/"));
        assert!(out.ends_with("?wm=abc123"));
    }

    #[test]
    fn leaves_a_clean_url_alone() {
        let raw = "https://cdn.example.com/a/b/c.mp4";
        assert_eq!(stamp_url(raw, "z"), "https://cdn.example.com/a/b/c.mp4?wm=z");
    }

    #[test]
    fn preserves_existing_query_and_replaces_watermark() {
        let raw = "https://cdn.example.com/v.m3u8?token=1&wm=old";
        let out = stamp_url(raw, "new");
        assert!(out.contains("token=1"), "query lost: {out}");
        assert!(out.contains("wm=new") && !out.contains("wm=old"), "wm not replaced: {out}");
    }

    #[test]
    fn does_not_double_encode() {
        let raw = "https://cdn.example.com/already%20encoded.mp4";
        let out = stamp_url(raw, "z");
        assert!(out.contains("already%20encoded"), "double-encoded: {out}");
    }
}
