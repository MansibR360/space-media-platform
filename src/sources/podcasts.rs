//! Podcasts — legal directory search via the iTunes Search API, episode lists
//! parsed from each show's public RSS feed. Enclosure URLs stream directly.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct Podcasts;

const ITUNES_SEARCH: &str = "https://itunes.apple.com/search";
const ITUNES_LOOKUP: &str = "https://itunes.apple.com/lookup";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

struct Episode {
    title: String,
    url: String,
}

async fn fetch_feed_episodes(feed_url: &str) -> ApiResult<Vec<Episode>> {
    let xml = client()
        .get(feed_url)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("feed unreachable: {e}")))?
        .text()
        .await
        .map_err(ApiError::from)?;

    let doc = roxmltree::Document::parse(&xml)
        .map_err(|e| ApiError::Unavailable(format!("invalid feed xml: {e}")))?;

    let mut episodes = Vec::new();
    for node in doc.descendants() {
        if node.tag_name().name() != "item" {
            continue;
        }
        let mut title = String::new();
        let mut url = String::new();
        for child in node.children() {
            match child.tag_name().name() {
                "title" => title = child.text().unwrap_or("").trim().to_string(),
                "enclosure" => {
                    url = child.attribute("url").unwrap_or("").to_string();
                }
                _ => {}
            }
        }
        if !url.is_empty() {
            episodes.push(Episode { title, url });
        }
        if episodes.len() >= 100 {
            break;
        }
    }
    Ok(episodes)
}

fn category() -> &'static str {
    "podcast"
}

#[async_trait]
impl NativeSource for Podcasts {
    fn name(&self) -> &'static str {
        "podcasts"
    }

    fn label(&self) -> &'static str {
        "Podcasts · iTunes Directory"
    }

    fn category(&self) -> &'static str {
        category()
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        if query.trim().is_empty() {
            return Ok(Vec::new()); // the directory has no meaningful trending
        }

        let body: Value = client()
            .get(ITUNES_SEARCH)
            .query(&[
                ("term", query.trim()),
                ("media", "podcast"),
                ("limit", &limit.clamp(1, 40).to_string()),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("itunes unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results
            .iter()
            .filter_map(|show| {
                Some(SourceItem {
                    external_id: show.get("collectionId")?.as_i64()?.to_string(),
                    title: show.get("collectionName")?.as_str()?.to_string(),
                    overview: show
                        .get("artistName")
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .into(),
                    year: None,
                    poster: show
                        .get("artworkUrl600")
                        .and_then(|a| a.as_str())
                        .map(str::to_string),
                    category: category(),
                })
            })
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let lookup: Value = client()
            .get(ITUNES_LOOKUP)
            .query(&[("id", external_id.trim())])
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("itunes unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let show = lookup
            .pointer("/results/0")
            .cloned()
            .ok_or_else(|| ApiError::not_found("podcast"))?;

        let feed_url = show
            .get("feedUrl")
            .and_then(|f| f.as_str())
            .ok_or_else(|| ApiError::Unavailable("no public feed for this podcast".into()))?
            .to_string();

        let episodes = fetch_feed_episodes(&feed_url).await?;
        let streams = episodes
            .iter()
            .take(60)
            .map(|ep| StreamLink {
                label: if ep.title.is_empty() {
                    "episode".into()
                } else {
                    ep.title.clone()
                },
                url: ep.url.clone(),
                kind: "audio".into(),
            })
            .collect();

        // Chapters mirror the episode list so clients can offer pickers.
        let chapters = episodes
            .iter()
            .enumerate()
            .map(|(i, ep)| super::MangaChapter {
                id: format!("{external_id}:{i}"),
                chapter: (i + 1).to_string(),
                title: ep.title.clone(),
                pages: 0,
            })
            .collect();

        Ok(SourceDetail {
            item: SourceItem {
                external_id: external_id.trim().to_string(),
                title: show
                    .get("collectionName")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Podcast")
                    .to_string(),
                overview: show
                    .get("artistName")
                    .and_then(|a| a.as_str())
                    .map(|a| format!("by {a}"))
                    .unwrap_or_default(),
                year: None,
                poster: show
                    .get("artworkUrl600")
                    .and_then(|a| a.as_str())
                    .map(str::to_string),
                category: category(),
            },
            streams,
            chapters: Some(chapters),
        })
    }
}

