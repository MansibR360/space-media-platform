pub mod anilist;
pub mod omdb;
pub mod tmdb;

use crate::error::ApiResult;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ExternalSearchResult {
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub overview: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub media_type_hint: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct ExternalDetail {
    pub title: String,
    pub original_title: Option<String>,
    pub overview: String,
    pub release_date: Option<chrono::NaiveDate>,
    pub runtime_minutes: Option<i32>,
    pub status: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub is_anime: bool,
    pub media_type: Option<String>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub cast: Vec<(String, Option<String>, i32)>,
    /// IMDb identity and ratings. Every provider that knows the IMDb id should
    /// set it — TMDB reports it too — so a title can be enriched from IMDb
    /// later even when it was imported from somewhere else.
    pub imdb_id: Option<String>,
    pub imdb_rating: Option<f64>,
    pub imdb_votes: Option<i32>,
    pub metascore: Option<i32>,
    /// Certification as IMDb states it, e.g. "PG-13".
    pub content_rating: Option<String>,
}

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    /// Unique source key, e.g. "tmdb", "imdb", "anilist" or a plugin name.
    fn source(&self) -> String;

    async fn search(&self, query: &str) -> ApiResult<Vec<ExternalSearchResult>>;

    async fn details(&self, external_id: &str) -> ApiResult<ExternalDetail>;
}

#[derive(Clone, Default)]
pub struct MetadataRegistry {
    providers: BTreeMap<String, Arc<dyn MetadataProvider>>,
}

impl MetadataRegistry {
    /// Built-in providers selected by configuration.
    pub fn from_config(cfg: &crate::config::Config) -> Self {
        let mut registry = Self::default();

        if let Some(key) = cfg.tmdb_api_key.as_deref().filter(|k| !k.is_empty()) {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client");
            registry.providers.insert(
                "tmdb".to_string(),
                Arc::new(tmdb::TmdbProvider::new(client.clone(), key.to_string())),
            );
        }

        if let Some(key) = cfg.omdb_api_key.as_deref().filter(|k| !k.is_empty()) {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client");
            registry.providers.insert(
                "imdb".to_string(),
                Arc::new(omdb::OmdbProvider::new(client, key.to_string())),
            );
        }

        if cfg.anilist_enabled {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client");
            registry.providers.insert(
                "anilist".to_string(),
                Arc::new(anilist::AniListProvider::new(client)),
            );
        }

        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn MetadataProvider>) {
        self.providers.insert(provider.source(), provider);
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, name: &str) {
        self.providers.remove(name);
    }

    pub fn sources(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn provider(&self, source: &str) -> Option<Arc<dyn MetadataProvider>> {
        self.providers.get(source).cloned()
    }
}
