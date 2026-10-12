//! Native source providers - in-process, zero extra network hops beyond the
//! upstream API call. Every source is a legal catalog:
//!
//! - `ia-films`      Internet Archive public-domain films (+ torrent/webseed fallback)
//! - `gutenberg`     Project Gutenberg - 75k public-domain ebooks
//! - `mangadex`      MangaDex - officially licensed manga, chapters + pages
//! - `audius`        Audius - artist-uploaded music, direct streamable audio
//! - `youtube`       YouTube Music â€” Metrolist-style InnerTube/Piped/Invidious
//!                   chain with a built-in scraper + embed fallback
//!
//! Third parties can still register remote HTTP plugins at runtime; these
//! natives ship compiled for maximum speed.
pub mod aggregator;
pub mod archive;
pub mod audius;
pub mod benchmark;
pub mod gutenberg;
pub mod handlers;
pub mod health;
pub mod episodes;
pub mod health_loop;
pub mod hub;
pub mod hub_handlers;
pub mod hub_server;
pub mod ia_audio;
pub mod librivox;
pub mod mangadex;
pub mod podcasts;
pub mod routes;
pub mod unified_handlers;
pub mod web;
pub mod web_handlers;
pub mod youtube;

use crate::error::ApiResult;
use async_trait::async_trait;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StreamLink {
    /// e.g. "512kb MPEG4", "Original", "MP3 128k"
    pub label: String,
    pub url: String,
    /// mp4 | hls | audio | page | file
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SourceItem {
    pub external_id: String,
    pub title: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub poster: Option<String>,
    /// movie | series | book | music | manga
    pub category: &'static str,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MangaChapter {
    pub id: String,
    pub chapter: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pages: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SourceDetail {
    pub item: SourceItem,
    pub streams: Vec<StreamLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapters: Option<Vec<MangaChapter>>,
}

/// Declares what a source can serve â€” used by the resolver to skip providers
/// that cannot satisfy a request (e.g. no subtitles, wrong category) without
/// burning a network call.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SourceCapabilities {
    /// categories this source can return: movie | series | book | music | manga | podcast | audiobook
    pub categories: Vec<String>,
    /// stream kinds it can produce: mp4 | hls | audio | page | file
    pub kinds: Vec<String>,
    pub subtitles: bool,
}

impl Default for SourceCapabilities {
    fn default() -> Self {
        Self {
            categories: vec![],
            kinds: vec![],
            subtitles: false,
        }
    }
}

/// A compiled-in source. All methods hit upstream APIs directly with shared
/// connections - no plugin HTTP hop, no serialization round-trips.
///
/// The core surface is `search` + `item`; everything else is defaulted so a
/// minimal provider only implements what it genuinely supports.
#[async_trait]
pub trait NativeSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn category(&self) -> &'static str;

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>>;

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail>;

    /// Static self-description consumed by the resolver and /api/sources.
    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            categories: vec![self.category().to_string()],
            ..Default::default()
        }
    }

    /// Manifest for registry/debug surfaces.
    fn manifest(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name(),
            "label": self.label(),
            "category": self.category(),
            "transport": "builtin",
            "capabilities": self.capabilities(),
        })
    }

    /// Optional paged catalog browse; `None` when unsupported.
    async fn browse(&self, _page: i32) -> ApiResult<Option<Vec<SourceItem>>> {
        Ok(None)
    }
}

pub struct SourceRegistry {
    providers: Vec<Box<dyn NativeSource>>,
}

impl SourceRegistry {
    pub fn new(youtube: std::sync::Arc<youtube::Youtube>) -> Self {
        Self {
            providers: vec![
                Box::new(archive::ArchiveFilms::default()),
                Box::new(gutenberg::Gutenberg::default()),
                Box::new(librivox::LibriVox::default()),
                Box::new(mangadex::MangaDex::default()),
                Box::new(audius::Audius::default()),
                Box::new(podcasts::Podcasts::default()),
                Box::new(ia_audio::IaAudio::default()),
                // Shared handle: AppState.youtube points at the same instance.
                Box::new(youtube),
            ],
        }
    }

    pub fn list(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    pub fn describe(&self) -> Vec<serde_json::Value> {
        self.providers.iter().map(|p| p.manifest()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&dyn NativeSource> {
        self.providers
            .iter()
            .find(|p| p.name() == name)
            .map(|p| &**p)
    }

    pub fn shared_client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("space-media-server/0.1 (+https://github.com)")
            .build()
            .expect("reqwest client")
    }
}

impl Default for SourceRegistry {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(youtube::Youtube::new()))
    }
}
