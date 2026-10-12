//! Audiobooks — LibriVox public-domain readings hosted on the Internet
//! Archive. Same infrastructure as ia-films, audio-oriented: every section's
//! MP3 becomes an individually streamable track.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct IaAudio;

const SEARCH: &str = "https://archive.org/advancedsearch.php";
const METADATA: &str = "https://archive.org/metadata";
const DOWNLOAD: &str = "https://archive.org/download";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

fn category() -> &'static str {
    "audiobook"
}

#[async_trait]
impl NativeSource for IaAudio {
    fn name(&self) -> &'static str {
        "ia-audio"
    }

    fn label(&self) -> &'static str {
        "LibriVox · Public-Domain Audiobooks"
    }

    fn category(&self) -> &'static str {
        category()
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let term = if query.trim().is_empty() {
            "collection:(librivoxaudio)".to_string()
        } else {
            format!(
                "({} AND collection:(librivoxaudio))",
                query.trim().replace(' ', " AND ")
            )
        };

        let body: Value = client()
            .get(SEARCH)
            .query(&[
                ("q", term.as_str()),
                ("fl[]", "identifier"),
                ("fl[]", "title"),
                ("fl[]", "creator"),
                ("fl[]", "year"),
                ("rows", &limit.clamp(1, 50).to_string()),
                ("output", "json"),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("archive.org unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let docs = body
            .pointer("/response/docs")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(docs
            .iter()
            .filter_map(|doc| {
                Some(SourceItem {
                    external_id: doc.get("identifier")?.as_str()?.to_string(),
                    title: doc
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    overview: doc
                        .get("creator")
                        .and_then(|c| c.as_str())
                        .map(|c| format!("read by {c}"))
                        .unwrap_or_default(),
                    year: doc.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                    poster: None,
                    category: category(),
                })
            })
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let body: Value = client()
            .get(format!("{METADATA}/{}", external_id.trim()))
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("archive.org unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let metadata = body.get("metadata").cloned().unwrap_or(Value::Null);
        let identifier = metadata
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or(external_id)
            .to_string();

        let dir = body.get("dir").and_then(|d| d.as_str()).unwrap_or("");
        let server = body.get("server").and_then(|s| s.as_str()).unwrap_or("");
        let base = if (!dir.is_empty()) && (!server.is_empty()) {
            format!("https://{server}{dir}")
        } else {
            format!("{DOWNLOAD}/{identifier}")
        };

        let mut streams = Vec::new();
        if let Some(files) = body.get("files").and_then(|f| f.as_array()) {
            for file in files {
                let name = match file.get("name").and_then(|n| n.as_str()) {
                    Some(n) => n,
                    None => continue,
                };
                if name.to_ascii_lowercase().ends_with(".mp3") {
                    streams.push(StreamLink {
                        label: name.trim_end_matches(".mp3").to_string(),
                        url: format!("{base}/{name}"),
                        kind: "audio".into(),
                    });
                }
            }
        }
        streams.truncate(200);

        Ok(SourceDetail {
            item: SourceItem {
                external_id: identifier.clone(),
                title: metadata
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or(&identifier)
                    .to_string(),
                overview: metadata
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect(),
                year: metadata
                    .get("year")
                    .and_then(|y| y.as_str())
                    .and_then(|y| y.get(..4))
                    .and_then(|y| y.parse().ok()),
                poster: Some(format!("{DOWNLOAD}/{identifier}/__ia/thumbnail.jpg")),
                category: category(),
            },
            streams,
            chapters: None,
        })
    }
}
