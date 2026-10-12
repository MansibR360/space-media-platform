use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The user's computed "taste DNA": genre affinities, pace preference,
/// era distribution and format mix. Powers recommendations and the home page.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TasteProfile {
    pub top_genres: Vec<GenreAffinity>,
    pub favorite_decades: Vec<i32>,
    pub avg_runtime_minutes: Option<f64>,
    pub anime_affinity: f64,
    pub total_watch_hours: f64,
    pub completion_rate: f64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GenreAffinity {
    pub genre: String,
    pub weight: f64,
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct EpisodeAir {
    pub id: Uuid,
    pub media_id: Uuid,
    pub title: String,
    pub number: i32,
    pub air_date: Option<chrono::NaiveDate>,
    pub still_url: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HomeRails {
    pub continue_watching: Vec<crate::tracking::models::ContinueWatchingItem>,
    pub trending: Vec<crate::search::models::SearchHit>,
    pub because_you_watched: Option<BecauseYouWatched>,
    pub new_episodes: Vec<EpisodeAir>,
    pub friends_activity: Vec<crate::social::models::Review>,
    pub recommended: Vec<crate::recommendations::service::ScoredMedia>,
    pub vibe_picks: Vec<VibePick>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BecauseYouWatched {
    pub anchor_media_id: Uuid,
    pub anchor_title: String,
    pub items: Vec<crate::catalog::models::Media>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CollectionRules {
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub year_from: Option<i32>,
    #[serde(default)]
    pub year_to: Option<i32>,
    #[serde(default)]
    pub min_rating: Option<f64>,
    #[serde(default)]
    pub is_anime: Option<bool>,
    #[serde(default = "default_sort")]
    pub sort: String,
}

fn default_sort() -> String {
    "popularity".into()
}

impl CollectionRules {
    pub fn to_filter(&self) -> crate::catalog::repo::ListFilter {
        use crate::catalog::models::{MediaStatus, MediaType};
        use crate::catalog::repo::{ListFilter, Sort};

        ListFilter {
            media_type: self.media_type.as_deref().and_then(|mt| match mt {
                "movie" => Some(MediaType::Movie),
                _ => Some(MediaType::Series),
            }),
            is_anime: self.is_anime,
            status: None::<MediaStatus>,
            genres: self.genres.clone(),
            studio: None,
            year_from: self.year_from,
            year_to: self.year_to,
            min_rating: self.min_rating,
            q: None,
            person: None,
            language: None,
            min_height: None,
            codec: None,
            sort: Sort::parse(&self.sort).unwrap_or(Sort::Popularity),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct SmartCollection {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: String,
    pub rules: serde_json::Value,
    pub visibility: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Vibe {
    Cozy,
    Dark,
    MindBending,
    Epic,
    Tearjerker,
    Funny,
    Scary,
    Chill,
    Nostalgic,
    Intense,
    Romantic,
    Cerebral,
}

pub struct VibeSpec {
    pub vibe: Vibe,
    pub label: &'static str,
    pub genres: &'static [&'static str],
    pub keywords: &'static [&'static str],
    /// preferred runtime band in minutes (low, high)
    pub pace: (f64, f64),
}

pub const VIBES: &[VibeSpec] = &[
    VibeSpec {
        vibe: Vibe::Cozy,
        label: "Cozy & feel-good",
        genres: &["comedy", "family", "romance", "animation"],
        keywords: &["warm", "charming", "delightful", "wholesome"],
        pace: (80.0, 130.0),
    },
    VibeSpec {
        vibe: Vibe::Dark,
        label: "Dark & gritty",
        genres: &["crime", "thriller", "drama", "mystery"],
        keywords: &["dark", "brutal", "corrupt", "noir", "revenge"],
        pace: (95.0, 160.0),
    },
    VibeSpec {
        vibe: Vibe::MindBending,
        label: "Mind-bending",
        genres: &["science-fiction", "sci-fi", "fantasy", "mystery"],
        keywords: &[
            "time travel",
            "parallel",
            "dream",
            "memory",
            "twist",
            "surreal",
        ],
        pace: (100.0, 170.0),
    },
    VibeSpec {
        vibe: Vibe::Epic,
        label: "Epic adventure",
        genres: &["adventure", "action", "fantasy", "history"],
        keywords: &["epic", "journey", "kingdom", "war", "legend"],
        pace: (110.0, 190.0),
    },
    VibeSpec {
        vibe: Vibe::Tearjerker,
        label: "Emotional rollercoaster",
        genres: &["drama", "romance"],
        keywords: &["loss", "grief", "love story", "illness", "farewell"],
        pace: (90.0, 150.0),
    },
    VibeSpec {
        vibe: Vibe::Funny,
        label: "Laugh out loud",
        genres: &["comedy"],
        keywords: &["hilarious", "satire", "misfit", "wedding"],
        pace: (75.0, 120.0),
    },
    VibeSpec {
        vibe: Vibe::Scary,
        label: "Scary as hell",
        genres: &["horror", "thriller"],
        keywords: &["haunted", "demon", "supernatural", "survive", "nightmare"],
        pace: (80.0, 130.0),
    },
    VibeSpec {
        vibe: Vibe::Chill,
        label: "Chill & easy",
        genres: &["documentary", "music", "comedy", "drama"],
        keywords: &["nature", "food", "travel", "road trip"],
        pace: (60.0, 110.0),
    },
    VibeSpec {
        vibe: Vibe::Nostalgic,
        label: "Nostalgia trip",
        genres: &["family", "animation", "adventure", "comedy"],
        keywords: &["childhood", "summer", "coming of age", "small town"],
        pace: (85.0, 140.0),
    },
    VibeSpec {
        vibe: Vibe::Intense,
        label: "Edge of your seat",
        genres: &["thriller", "action", "crime"],
        keywords: &["race against", "hostage", "heist", "escape", "chase"],
        pace: (90.0, 150.0),
    },
    VibeSpec {
        vibe: Vibe::Romantic,
        label: "Romance fix",
        genres: &["romance", "drama", "comedy"],
        keywords: &["fall in love", "wedding", "first kiss", "long distance"],
        pace: (85.0, 135.0),
    },
    VibeSpec {
        vibe: Vibe::Cerebral,
        label: "Slow-burn cerebral",
        genres: &["drama", "mystery", "science-fiction"],
        keywords: &[
            "philosophical",
            "identity",
            "existential",
            "quiet",
            "isolation",
        ],
        pace: (100.0, 180.0),
    },
];

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct VibeCandidateRow {
    pub id: Uuid,
    pub title: String,
    pub overview: String,
    pub poster_url: Option<String>,
    pub release_date: Option<chrono::NaiveDate>,
    pub runtime_minutes: Option<i32>,
    pub rating_avg: f64,
    pub popularity: i32,
    pub genres: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct VibePick {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub score: f64,
    pub why: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct VibeResponse {
    pub matched_vibes: Vec<&'static str>,
    pub items: Vec<VibePick>,
}
