//! Audius — artist-uploaded music platform with a public API and direct
//! streamable audio. Trending works without auth; search too.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct Audius {
    host: OnceLock<RwLock<Option<String>>>,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

impl Audius {
    fn host_cell(&self) -> &RwLock<Option<String>> {
        self.host.get_or_init(|| RwLock::new(None))
    }

    /// Discovers a healthy content node once, then reuses it.
    async fn resolve_host(&self) -> ApiResult<String> {
        if let Some(host) = self.host_cell().read().await.clone() {
            return Ok(host);
        }

        // Discovery answers {"data": ["https://host", …], "env": …}. This read
        // it as a bare array, so decoding failed on every call and the source
        // was permanently unavailable.
        let discovery: Value = client()
            .get("https://api.audius.co")
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("audius discovery failed: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let host = discovery
            .get("data")
            .and_then(Value::as_array)
            // tolerate the bare-array shape too, in case it ever returns one
            .or_else(|| discovery.as_array())
            .and_then(|hosts| hosts.iter().find_map(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| ApiError::Unavailable("audius returned no hosts".into()))?;

        *self.host_cell().write().await = Some(host.clone());
        Ok(host)
    }

    pub async fn invalidate_host(&self) {
        *self.host_cell().write().await = None;
    }
}

fn track_to_item(track: &Value) -> Option<SourceItem> {
    Some(SourceItem {
        external_id: track.get("id")?.as_str()?.to_string(),
        title: track.get("title")?.as_str()?.to_string(),
        overview: track
            .pointer("/user/name")
            .and_then(|n| n.as_str())
            .map(|n| format!("by {n}"))
            .unwrap_or_default(),
        year: track
            .get("release_date")
            .and_then(|r| r.as_str())
            .and_then(|r| r.get(..4))
            .and_then(|y| y.parse().ok()),
        poster: track
            .pointer("/artwork/480x480")
            .or_else(|| track.pointer("/artwork/150x150"))
            .and_then(|a| a.as_str())
            .map(str::to_string),
        category: "music",
    })
}

#[async_trait]
impl NativeSource for Audius {
    fn name(&self) -> &'static str {
        "audius"
    }

    fn label(&self) -> &'static str {
        "Audius · Music"
    }

    fn category(&self) -> &'static str {
        "music"
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let host = self.resolve_host().await?;
        let url = if query.trim().is_empty() {
            format!("{host}/v1/tracks/trending")
        } else {
            format!("{host}/v1/tracks/search")
        };

        // Bound the response. Trending without a limit returns the whole
        // chart — ~660 KB for a rail that shows a handful of tracks, and large
        // enough that decoding it was failing outright.
        let capped = limit.clamp(1, 50).to_string();
        let mut request = client()
            .get(&url)
            .query(&[("app_name", "space-media-server"), ("limit", &capped)]);
        if query.trim().is_empty() {
            request = request.query(&[("time", "week")]);
        } else {
            request = request.query(&[("query", query.trim())]);
        }

        let body: Value = request
            .send()
            .await
            .map_err(|e| {
                // A dead node poisons the cache until invalidated.
                ApiError::Unavailable(format!("audius unreachable: {e}"))
            })?
            .json()
            .await
            .map_err(ApiError::from)?;

        let tracks = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(tracks
            .into_iter()
            .take(limit.clamp(1, 50) as usize)
            .filter_map(|t| track_to_item(&t))
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let host = self.resolve_host().await?;
        let track_url = format!("{host}/v1/tracks/{}", external_id.trim());
        let track: Value = client()
            .get(&track_url)
            .query(&[("app_name", "space-media-server")])
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("audius unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let item = track_to_item(&track).ok_or_else(|| ApiError::not_found("track"))?;

        Ok(SourceDetail {
            streams: vec![StreamLink {
                label: "Audius stream".into(),
                url: format!(
                    "{host}/v1/tracks/{}/stream?app_name=space-media-server",
                    item.external_id
                ),
                kind: "audio".into(),
            }],
            item,
            chapters: None,
        })
    }
}
