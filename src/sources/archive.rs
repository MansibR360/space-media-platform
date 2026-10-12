//! Internet Archive — public-domain & freely-licensed films.
//! Search via advancedsearch.php, files via /metadata; the same items expose
//! .torrent webseeds so playback works over plain HTTP without a BT client.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct ArchiveFilms;

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

#[async_trait]
impl NativeSource for ArchiveFilms {
    fn name(&self) -> &'static str {
        "ia-films"
    }

    fn label(&self) -> &'static str {
        "Internet Archive · Public-Domain Films"
    }

    fn category(&self) -> &'static str {
        "movie"
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let term = if query.trim().is_empty() {
            "collection:(feature_films)".to_string()
        } else {
            format!(
                "({} AND mediatype:(movies))",
                query.trim().replace(' ', " AND ")
            )
        };

        let response = client()
            .get(SEARCH)
            .query(&[
                ("q", term.as_str()),
                ("fl[]", "identifier"),
                ("fl[]", "title"),
                ("fl[]", "description"),
                ("fl[]", "year"),
                ("rows", &limit.clamp(1, 50).to_string()),
                ("output", "json"),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("archive.org unreachable: {e}")))?;

        let body: Value = response.json().await.map_err(ApiError::from)?;
        let docs = body
            .pointer("/response/docs")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(docs
            .iter()
            .filter_map(|doc| {
                let id = doc.get("identifier")?.as_str()?.to_string();
                Some(SourceItem {
                    external_id: id.clone(),
                    title: doc
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or(&id)
                        .to_string(),
                    overview: doc
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(600)
                        .collect(),
                    year: doc.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                    poster: Some(format!("{DOWNLOAD}/{id}/__ia/thumbnail.jpg")),
                    category: Self::default_category(),
                })
            })
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let url = format!("{METADATA}/{}", external_id.trim());
        let body: Value = client()
            .get(&url)
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("archive.org unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let metadata = body.get("metadata").cloned().unwrap_or_else(|| Value::Null);
        let identifier = metadata
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or(external_id)
            .to_string();

        let title = metadata
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(&identifier)
            .to_string();
        let overview = metadata
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .chars()
            .take(1200)
            .collect();
        let year = metadata
            .get("year")
            .and_then(|y| y.as_str())
            .and_then(|y| y.get(..4))
            .and_then(|y| y.parse().ok());

        let dir = body.get("dir").and_then(|d| d.as_str()).unwrap_or("");
        let server = body.get("server").and_then(|s| s.as_str()).unwrap_or("");
        let base = if (!dir.is_empty()) && (!server.is_empty()) {
            format!("https://{server}{dir}")
        } else {
            format!("{DOWNLOAD}/{identifier}")
        };

        let mut streams = Vec::new();
        let mut poster = Some(format!("{DOWNLOAD}/{identifier}/__ia/thumbnail.jpg"));

        if let Some(files) = body.get("files").and_then(|f| f.as_array()) {
            for file in files {
                let name = match file.get("name").and_then(|n| n.as_str()) {
                    Some(n) => n,
                    None => continue,
                };
                let lower = name.to_ascii_lowercase();
                let format = file
                    .get("format")
                    .and_then(|f| f.as_str())
                    .unwrap_or("")
                    .to_string();

                if lower.ends_with(".mp4") || format.contains("MPEG4") || format.contains("h.264") {
                    let label = if format.is_empty() {
                        "MP4".to_string()
                    } else {
                        format
                    };
                    streams.push(StreamLink {
                        label,
                        url: format!("{base}/{name}"),
                        kind: "mp4".into(),
                    });
                }
                if lower == format!("{identifier}.thumbs.jpg") {
                    poster = Some(format!("{base}/{name}"));
                }
            }
        }

        // Prefer smaller derivatives first for fast start.
        streams.sort_by_key(|s| if s.label.contains("512kb") { 0 } else { 1 });

        Ok(SourceDetail {
            item: SourceItem {
                external_id: identifier,
                title,
                overview,
                year,
                poster,
                category: Self::default_category(),
            },
            streams,
            chapters: None,
        })
    }
}

impl ArchiveFilms {
    fn default_category() -> &'static str {
        "movie"
    }
}
