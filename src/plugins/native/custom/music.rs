//! Music, as direct audio rather than a page.
//!
//! JioSaavn holds essentially all of the catalogue, and squid.wtf's front end
//! exposes its search without a key. What that search returns is not a stream
//! though: the CDN path arrives DES-encrypted under a key the site ships in the
//! clear. Decrypting it here is the whole point of this source - it turns a
//! result into a plain, seekable `.mp4` audio URL that SPACE's own player can
//! open, instead of handing the listener off to somebody else's website.
//!
//! The decrypted path always names the 96 kbps master. The CDN serves the same
//! object at 160 and 320 by nothing more than the number in the filename, so
//! every quality is offered and the best one leads.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::plugins::native::engines::{EngineCtx, quote_plus};

/// The key JioSaavn encrypts media paths with. It is not a secret - the player
/// has to know it, so it is published in every client that plays a song.
const MEDIA_KEY: &[u8; 8] = b"38346591";

/// Bitrates the CDN carries for every track, best first.
const QUALITIES: [&str; 3] = ["320", "160", "96"];

#[derive(Debug, Clone)]
pub struct Saavn {
    pub base: String,
}

impl Saavn {
    pub fn new() -> Self {
        Self {
            base: super::apis::env_or("SAAVN_API_BASE", "https://zeus.squid.wtf"),
        }
    }

    /// Turns the encrypted media field into the CDN path it stands for.
    ///
    /// Returns `None` rather than an error for anything malformed: one
    /// unreadable row must not cost the whole result set.
    fn decrypt_media_url(encrypted: &str) -> Option<String> {
        let mut buffer = BASE64.decode(encrypted.trim()).ok()?;
        if buffer.is_empty() || buffer.len() % 8 != 0 {
            return None;
        }

        // ECB: each block stands alone, which is why this is a plain loop.
        let cipher = des::Des::new_from_slice(MEDIA_KEY).ok()?;
        for block in buffer.chunks_exact_mut(8) {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }

        // PKCS#7: the last byte says how many bytes of padding to drop.
        let pad = *buffer.last()? as usize;
        if pad == 0 || pad > 8 || pad > buffer.len() {
            return None;
        }
        buffer.truncate(buffer.len() - pad);

        let url = String::from_utf8(buffer).ok()?.trim().to_string();
        url.starts_with("http").then_some(url)
    }

    /// The same object at another bitrate: the CDN keys quality by filename.
    fn at_quality(url: &str, quality: &str) -> String {
        for known in QUALITIES {
            let suffix = format!("_{known}.mp4");
            if let Some(stem) = url.strip_suffix(&suffix) {
                return format!("{stem}_{quality}.mp4");
            }
        }
        url.to_string()
    }

    /// A larger cover than the 150px thumbnail search hands back.
    fn large_cover(image: &str) -> String {
        image.replace("150x150", "500x500")
    }

    fn artist_of(row: &Value) -> String {
        // The subtitle reads "Artist - Album"; the artist list is the fallback
        // for rows that have no subtitle at all.
        if let Some(subtitle) = row.get("subtitle").and_then(Value::as_str) {
            let head = subtitle.split(" - ").next().unwrap_or("").trim();
            if !head.is_empty() {
                return head.to_string();
            }
        }
        row.get("more_info")
            .and_then(|m| m.get("artists"))
            .and_then(|a| a.get("primary"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|a| a.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let limit = limit.clamp(1, 40);
        // Without these the API 302s to its own front page and the answer is
        // HTML, not JSON.
        let body = ctx
            .get_json_with(
                &format!("{}/api/songs?q={}", self.base, quote_plus(query)),
                &[
                    ("accept", "application/json"),
                    ("referer", "https://saavn.squid.wtf/"),
                    ("origin", "https://saavn.squid.wtf"),
                ],
            )
            .await?;

        let mut out = Vec::new();
        for row in body
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let title = row.get("title").and_then(Value::as_str).unwrap_or("").trim();
            let encrypted = row
                .get("more_info")
                .and_then(|m| m.get("encrypted_media_url"))
                .and_then(Value::as_str)
                .unwrap_or("");
            // A row with no playable media is not a result: SPACE would only
            // offer it and then fail on the play page.
            if title.is_empty() || Self::decrypt_media_url(encrypted).is_none() {
                continue;
            }

            let artist = Self::artist_of(&row);
            let album = row
                .get("more_info")
                .and_then(|m| m.get("album"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let overview = [artist.as_str(), album]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" \u{b7} ");

            out.push(json!({
                // The encrypted field is the id: resolving needs no second
                // request, only the decryption above.
                "id": format!("saavn:{encrypted}"),
                "title": title,
                "overview": overview,
                "year": row
                    .get("year")
                    .and_then(Value::as_str)
                    .and_then(|y| y.parse::<i64>().ok()),
                "poster": Self::large_cover(row.get("image").and_then(Value::as_str).unwrap_or("")),
                "type": "music",
            }));
            if out.len() >= limit {
                break;
            }
        }
        Ok(json!({ "results": out }))
    }

    pub async fn details(&self, _ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let Some(encrypted) = id.strip_prefix("saavn:") else {
            return Err(ApiError::bad_request(
                "id must be saavn:{encrypted_media_url}",
            ));
        };
        let url = Self::decrypt_media_url(encrypted).ok_or_else(|| {
            ApiError::Unavailable("this track carries no readable media url".into())
        })?;

        // Every bitrate is offered so the listener can choose; the best leads,
        // which is what the aggregator picks when nobody chooses.
        let streams: Vec<Value> = QUALITIES
            .iter()
            .map(|q| {
                json!({
                    "label": format!("{q} kbps \u{b7} JioSaavn"),
                    "url": Self::at_quality(&url, q),
                    "kind": "audio",
                })
            })
            .collect();

        Ok(json!({
            "title": "Track",
            "overview": "",
            "poster": "",
            "year": Value::Null,
            "media_type": "music",
            "streams": streams,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact field the site returns for a real track. It has to come back
    /// as the CDN path it encodes.
    const SAMPLE: &str = "ID2ieOjCrwfgWvL5sXl4B1ImC5QfbsDywMBq9Ivw1bZUsPMavqWkFMIKoLthAHDp9ezqoV11oJk8OFEEIjuj5Bw7tS9a8Gtq";

    #[test]
    fn an_encrypted_field_becomes_a_playable_url() {
        let url = Saavn::decrypt_media_url(SAMPLE).expect("the sample decrypts");
        assert_eq!(
            url,
            "https://aac.saavncdn.com/372/38de816bee7a6df4607f1f0e6822c5bc_96.mp4",
            "padding must be stripped, or the url gains trailing bytes"
        );
    }

    #[test]
    fn rubbish_is_skipped_rather_than_fatal() {
        assert!(Saavn::decrypt_media_url("").is_none());
        assert!(Saavn::decrypt_media_url("not base64 at all !!").is_none());
        // Decodes to a whole block, but not to anything that is a url.
        assert!(Saavn::decrypt_media_url("AAAAAAAAAAA=").is_none());
    }

    #[test]
    fn every_bitrate_names_the_same_object() {
        let base = "https://aac.saavncdn.com/372/abc_96.mp4";
        assert_eq!(
            Saavn::at_quality(base, "320"),
            "https://aac.saavncdn.com/372/abc_320.mp4"
        );
        // Switching away and back is a round trip, not a pile-up of suffixes.
        let up = Saavn::at_quality(base, "320");
        assert_eq!(Saavn::at_quality(&up, "96"), base);
    }

    #[test]
    fn a_url_that_names_no_bitrate_is_left_alone() {
        let odd = "https://aac.saavncdn.com/372/abc.mp4";
        assert_eq!(Saavn::at_quality(odd, "320"), odd);
    }

    #[test]
    fn the_artist_comes_from_the_subtitle_then_the_artist_list() {
        let row = json!({ "subtitle": "The Weeknd ft. Daft Punk - Starboy" });
        assert_eq!(Saavn::artist_of(&row), "The Weeknd ft. Daft Punk");

        let row = json!({
            "subtitle": "",
            "more_info": { "artists": { "primary": [{ "name": "Daft Punk" }] } }
        });
        assert_eq!(Saavn::artist_of(&row), "Daft Punk");
    }

    #[test]
    fn covers_are_asked_for_at_a_usable_size() {
        assert_eq!(
            Saavn::large_cover("https://c.saavncdn.com/372/Starboy-150x150.jpg"),
            "https://c.saavncdn.com/372/Starboy-500x500.jpg"
        );
    }
}
