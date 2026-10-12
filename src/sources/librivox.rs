//! LibriVox — public-domain audiobooks read by volunteers.
//!
//! Worked example of adding a new native source (see docs/adding-a-source.md):
//! implement `NativeSource`, map search → `SourceItem`s, map item →
//! `SourceDetail` with directly playable `StreamLink`s, register in
//! `SourceRegistry`. That's the whole contract.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct LibriVox;

/// Maps onto SPACE's audiobook media_type end-to-end.
pub const CATEGORY: &str = "audiobook";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

const API: &str = "https://librivox.org/api/feed/audiobooks";

async fn fetch_feed(extra: &[(&str, String)]) -> ApiResult<Vec<Value>> {
    let mut request = client().get(API).query(&[("format", "json")]);
    for (k, v) in extra {
        request = request.query(&[(k, v.as_str())]);
    }
    let body: Value = request
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("librivox unreachable: {e}")))?
        .json()
        .await
        .map_err(ApiError::from)?;
    // The endpoint returns either an array or {"books": [...]} depending on
    // result count — accept both shapes.
    if let Some(arr) = body.as_array() {
        return Ok(arr.clone());
    }
    Ok(body
        .get("books")
        .and_then(|b| b.as_array())
        .cloned()
        .unwrap_or_default())
}

fn to_item(b: &Value) -> Option<SourceItem> {
    let id = b.get("id").and_then(|v| v.as_str()?.parse::<i64>().ok())?;
    let title = b.get("title").and_then(|t| t.as_str())?.to_string();
    let author = b
        .get("authors")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let f = a.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
                    let l = a.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
                    let full = format!("{f} {l}").trim().to_string();
                    (!full.is_empty()).then_some(full)
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let overview = {
        let raw = b.get("description").and_then(|d| d.as_str()).unwrap_or("");
        html_to_text(raw)
    };
    let overview = if overview.is_empty() && !author.is_empty() {
        format!("Read by volunteers · {author}")
    } else {
        overview
    };
    let year = b
        .get("copyright_year")
        .and_then(|y| y.as_str()?.parse::<i32>().ok());

    Some(SourceItem {
        external_id: id.to_string(),
        title,
        overview,
        year,
        poster: None, // LibriVox has no cover art field in this API
        category: CATEGORY,
    })
}

fn html_to_text(raw: &str) -> String {
    let no_tags: String = {
        let mut out = String::with_capacity(raw.len());
        let mut inside = false;
        for ch in raw.chars() {
            match ch {
                '<' => inside = true,
                '>' => inside = false,
                c if !inside => out.push(c),
                _ => {}
            }
        }
        out
    };
    no_tags.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pulls playable section MP3s out of the book's RSS feed.
async fn streams_from_rss(rss_url: &str) -> Vec<StreamLink> {
    let Ok(xml) = client().get(rss_url).send().await else {
        return vec![];
    };
    let xml = match xml.text().await {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return vec![];
    };

    let mut links = Vec::new();
    let mut section = 0usize;
    for node in doc.descendants() {
        if node.tag_name().name() == "enclosure" {
            let Some(url) = node.attribute("url") else {
                continue;
            };
            section += 1;
            // Titles live on the enclosing <item>; approximate with index.
            links.push(StreamLink {
                label: format!("Part {section}"),
                url: url.to_string(),
                kind: "audio".into(),
            });
        }
    }
    links
}

#[async_trait]
impl NativeSource for LibriVox {
    fn name(&self) -> &'static str {
        "librivox"
    }

    fn label(&self) -> &'static str {
        "LibriVox — Public-Domain Audiobooks"
    }

    fn category(&self) -> &'static str {
        CATEGORY
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let q = query.trim();
        let extra: Vec<(&str, String)> = if q.is_empty() {
            vec![("limit", limit.clamp(1, 50).to_string())]
        } else {
            vec![
                ("title", q.to_string()),
                ("limit", limit.clamp(1, 50).to_string()),
            ]
        };
        Ok(fetch_feed(&extra)
            .await?
            .iter()
            .filter_map(to_item)
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let books = fetch_feed(&[("id", external_id.trim().to_string())]).await?;
        let book = books
            .first()
            .ok_or_else(|| ApiError::not_found("librivox audiobook"))?;

        let mut item = to_item(book).ok_or_else(|| ApiError::Unavailable(
            "librivox returned malformed entry".into(),
        ))?;

        let rss = book.get("url_rss").and_then(|u| u.as_str());
        let zip = book.get("url_zip_file").and_then(|u| u.as_str());

        let mut streams = match rss {
            Some(url) => streams_from_rss(url).await,
            None => vec![],
        };
        if streams.is_empty() {
            if let Some(zip_url) = zip {
                streams.push(StreamLink {
                    label: "Full audiobook (zip)".into(),
                    url: zip_url.to_string(),
                    kind: "file".into(),
                });
            }
        }
        if let Some(z) = zip.filter(|_| !streams.is_empty()) {
            streams.push(StreamLink {
                label: "All parts (zip)".into(),
                url: z.to_string(),
                kind: "file".into(),
            });
        }

        item.external_id = external_id.trim().to_string();
        Ok(SourceDetail {
            item,
            streams,
            chapters: None,
        })
    }
}
