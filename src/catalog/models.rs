use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize, utoipa::ToSchema,
)]
// Column is TEXT with a CHECK constraint, not a Postgres enum. Without
// type_name, sqlx infers an enum type that was never created.
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Movie,
    Series,
    Book,
    Music,
    Manga,
    Podcast,
    Audiobook,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize, utoipa::ToSchema,
)]
// Column is TEXT with a CHECK constraint, not a Postgres enum. Without
// type_name, sqlx infers an enum type that was never created.
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MediaStatus {
    Announced,
    Releasing,
    Released,
    Ongoing,
    Finished,
    Cancelled,
}

impl MediaStatus {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "announced" => Some(Self::Announced),
            "releasing" => Some(Self::Releasing),
            "released" => Some(Self::Released),
            "ongoing" => Some(Self::Ongoing),
            "finished" => Some(Self::Finished),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Media {
    pub id: Uuid,
    pub media_type: MediaType,
    pub is_anime: bool,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: String,
    pub release_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub runtime_minutes: Option<i32>,
    pub status: MediaStatus,
    pub age_rating: Option<String>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub trailer_url: Option<String>,
    pub country: Option<String>,
    pub original_language: Option<String>,
    pub popularity: i32,
    /// The catalog's own user ratings — unrelated to `imdb_rating`.
    pub rating_avg: f64,
    pub rating_count: i32,
    /// IMDb identity (`tt0111161`) and the ratings that hang off it. Set once a
    /// title has been matched, regardless of which provider imported it.
    pub imdb_id: Option<String>,
    pub imdb_rating: Option<f64>,
    pub imdb_votes: Option<i32>,
    pub metascore: Option<i32>,
    /// IMDb certification, e.g. "PG-13". Distinct from `age_rating`, which is
    /// whatever the importing provider supplied.
    pub content_rating: Option<String>,
    pub imdb_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Season {
    pub id: Uuid,
    pub media_id: Uuid,
    pub number: i32,
    pub title: Option<String>,
    pub overview: String,
    pub air_date: Option<NaiveDate>,
    pub poster_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Episode {
    pub id: Uuid,
    pub media_id: Uuid,
    pub season_id: Uuid,
    pub number: i32,
    pub absolute_number: Option<i32>,
    pub title: String,
    pub overview: String,
    pub air_date: Option<NaiveDate>,
    pub runtime_minutes: Option<i32>,
    pub still_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Genre {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Person {
    pub id: Uuid,
    pub name: String,
    pub image_url: Option<String>,
    pub bio: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Studio {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub country: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct Credit {
    pub id: Uuid,
    pub media_id: Uuid,
    pub person_id: Uuid,
    pub person_name: String,
    pub person_image: Option<String>,
    pub credit_type: String,
    pub character_name: Option<String>,
    pub sort_order: i32,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct ExternalMapping {
    pub id: Uuid,
    pub media_id: Uuid,
    pub source: String,
    pub external_id: String,
    pub synced_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateMediaRequest {
    pub media_type: MediaType,
    pub title: String,
    #[serde(default)]
    pub is_anime: bool,
    #[serde(default)]
    pub original_title: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub release_date: Option<NaiveDate>,
    #[serde(default)]
    pub runtime_minutes: Option<i32>,
    #[serde(default)]
    pub status: Option<MediaStatus>,
    #[serde(default)]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub backdrop_url: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub studios: Vec<String>,
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
pub struct UpdateMediaRequest {
    pub title: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<Option<NaiveDate>>,
    pub runtime_minutes: Option<i32>,
    pub status: Option<MediaStatus>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub trailer_url: Option<String>,
    pub is_anime: Option<bool>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateSeasonRequest {
    pub number: i32,
    pub title: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub air_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateEpisodeRequest {
    pub number: i32,
    pub title: String,
    #[serde(default)]
    pub absolute_number: Option<i32>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub air_date: Option<NaiveDate>,
    #[serde(default)]
    pub runtime_minutes: Option<i32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SeasonWithEpisodes {
    #[serde(flatten)]
    pub season: Season,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct UserMediaContext {
    pub status: Option<String>,
    pub score: Option<f64>,
    pub favorite: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MediaDetail {
    #[serde(flatten)]
    pub media: Media,
    pub genres: Vec<Genre>,
    pub studios: Vec<Studio>,
    pub cast: Vec<Credit>,
    pub seasons: Vec<SeasonWithEpisodes>,
    pub external_ids: Vec<ExternalMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my: Option<UserMediaContext>,
}
