//! Episode listings for a source hit.
//!
//! A catalog row has `seasons` and `episodes` tables behind it, but a
//! cross-source hit has neither — only a play token. So a series found through
//! search offered no way to pick an episode: the play page could start
//! something, but never say what else there was.
//!
//! Most series in this catalog are keyed by TMDB id (`tv:1396`), and TMDB
//! publishes the season and episode list, so that is where the structure comes
//! from. Each episode is handed back with its own play token addressed to the
//! same source the series came from, which is what makes picking one work.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};
use crate::sources::aggregator::make_token;

const TMDB: &str = "https://api.themoviedb.org/3";
const IMG: &str = "https://image.tmdb.org/t/p";

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EpisodeEntry {
    pub season: i64,
    pub episode: i64,
    pub title: String,
    pub overview: String,
    pub still: Option<String>,
    pub air_date: Option<String>,
    pub runtime_minutes: Option<i64>,
    /// Play token for this exact episode, on the source the series came from.
    pub token: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SeasonSummary {
    pub season: i64,
    pub title: String,
    pub episode_count: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct EpisodeListing {
    pub series_title: String,
    pub seasons: Vec<SeasonSummary>,
    pub season: i64,
    pub episodes: Vec<EpisodeEntry>,
}

#[derive(Debug, Deserialize)]
struct TmdbSeason {
    season_number: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    episode_count: Option<i64>,
}

/// The TMDB id a source hit is keyed by, if it is keyed by one at all.
///
/// Accepts `tv:1396` and `tv:1396:2:3` — the season and episode already chosen
/// are irrelevant to listing what exists.
pub fn tmdb_series_id(external_id: &str) -> Option<String> {
    let numeric = |id: &str| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit());

    // A bare id: sources that bridge search through TMDB hand back its id
    // directly, without saying which kind it is. Whether it is really a series
    // is settled by asking TMDB, not by guessing here.
    if numeric(external_id) {
        return Some(external_id.to_string());
    }

    let mut parts = external_id.split(':');
    if parts.next()? != "tv" {
        return None;
    }
    let id = parts.next()?;
    numeric(id).then(|| id.to_string())
}

/// The AniList id a source hit is keyed by: `anilist:20`, `anilist:20:5`.
pub fn anilist_series_id(external_id: &str) -> Option<String> {
    let mut parts = external_id.split(':');
    if parts.next()? != "anilist" {
        return None;
    }
    let id = parts.next()?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then(|| id.to_string())
}

/// Episodes of an anime, from AniList.
///
/// AniList has no per-episode metadata for most titles — it publishes the
/// count. So the list is built from that: numbered episodes carrying their own
/// play token, which is what makes them selectable. Better a plain numbered
/// list than no list at all, which is what anime had.
async fn anilist_listing(
    client: &reqwest::Client,
    source: &str,
    anilist_id: &str,
) -> ApiResult<EpisodeListing> {
    const QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id, type: ANIME) {
    id
    episodes
    nextAiringEpisode { episode }
    title { romaji english }
  }
}
"#;
    let id: i64 = anilist_id
        .parse()
        .map_err(|_| ApiError::bad_request("malformed anilist id"))?;

    let body: Value = client
        .post("https://graphql.anilist.co")
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "query": QUERY, "variables": { "id": id } }))
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("anilist unreachable: {e}")))?
        .json()
        .await
        .map_err(|e| ApiError::Unavailable(format!("anilist sent invalid json: {e}")))?;

    let media = body
        .pointer("/data/Media")
        .ok_or_else(|| ApiError::not_found("anime"))?;

    let titles = media.get("title");
    let series_title = titles
        .and_then(|t| t.get("english"))
        .and_then(Value::as_str)
        .or_else(|| titles.and_then(|t| t.get("romaji")).and_then(Value::as_str))
        .unwrap_or("Untitled")
        .to_string();

    // A running series has no final count yet; what has aired is one behind
    // the next episode due.
    let total = media
        .get("episodes")
        .and_then(Value::as_i64)
        .or_else(|| {
            media
                .pointer("/nextAiringEpisode/episode")
                .and_then(Value::as_i64)
                .map(|next| (next - 1).max(1))
        })
        .unwrap_or(1)
        .clamp(1, 5000);

    let episodes = (1..=total)
        .map(|number| EpisodeEntry {
            season: 1,
            episode: number,
            title: format!("Episode {number}"),
            overview: String::new(),
            still: None,
            air_date: None,
            runtime_minutes: None,
            token: make_token(source, &format!("anilist:{anilist_id}:{number}")),
        })
        .collect();

    Ok(EpisodeListing {
        series_title,
        seasons: vec![SeasonSummary {
            season: 1,
            title: "Episodes".to_string(),
            episode_count: total,
        }],
        season: 1,
        episodes,
    })
}

/// Season the caller is already watching, so the listing can open on it.
pub fn season_of(external_id: &str) -> Option<i64> {
    external_id.split(':').nth(2).and_then(|s| s.parse().ok())
}

/// Every season of a series, and the episodes of one of them.
pub async fn listing(
    client: &reqwest::Client,
    api_key: &str,
    source: &str,
    external_id: &str,
    season: Option<i64>,
) -> ApiResult<EpisodeListing> {
    // Anime is keyed by AniList, not TMDB, so it needs its own listing — and
    // without one every anime looked like a single-episode title.
    if let Some(anilist_id) = anilist_series_id(external_id) {
        return anilist_listing(client, source, &anilist_id).await;
    }

    let series_id = tmdb_series_id(external_id).ok_or_else(|| {
        ApiError::bad_request("episodes are only available for series keyed by a TMDB id")
    })?;

    let series = crate::catalog::tmdb_cache::fetch(
        client,
        &format!("{TMDB}/tv/{series_id}?api_key={api_key}"),
    )
    .await
    .ok_or_else(|| ApiError::Unavailable("tmdb unreachable".into()))?;

    // TMDB answers 404 with a status_message for an id that is not a series,
    // which is the honest way to tell a film from a show when the source only
    // gave us a number.
    let Some(series_title) = series.get("name").and_then(Value::as_str) else {
        return Err(ApiError::bad_request(
            "this title has no episodes — it is not a series",
        ));
    };
    let series_title = series_title.to_string();

    let raw_seasons: Vec<TmdbSeason> = series
        .get("seasons")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    // Season 0 is TMDB's "Specials" bucket; it is real content, but it should
    // never be what the page opens on.
    let seasons: Vec<SeasonSummary> = raw_seasons
        .iter()
        .map(|s| SeasonSummary {
            season: s.season_number,
            title: s
                .name
                .clone()
                .unwrap_or_else(|| format!("Season {}", s.season_number)),
            episode_count: s.episode_count.unwrap_or(0),
        })
        .collect();

    let wanted = season
        .or_else(|| season_of(external_id))
        .or_else(|| {
            seasons
                .iter()
                .find(|s| s.season > 0)
                .map(|s| s.season)
        })
        .unwrap_or(1);

    let detail = crate::catalog::tmdb_cache::fetch(
        client,
        &format!("{TMDB}/tv/{series_id}/season/{wanted}?api_key={api_key}"),
    )
    .await
    .ok_or_else(|| ApiError::Unavailable("tmdb unreachable".into()))?;

    let episodes = detail
        .get("episodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|ep| {
            let number = ep.get("episode_number").and_then(Value::as_i64)?;
            let still = ep
                .get("still_path")
                .and_then(Value::as_str)
                .map(|p| format!("{IMG}/w300{p}"));
            Some(EpisodeEntry {
                season: wanted,
                episode: number,
                title: ep
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                    .unwrap_or("Untitled")
                    .to_string(),
                overview: ep
                    .get("overview")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                still,
                air_date: ep
                    .get("air_date")
                    .and_then(Value::as_str)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
                runtime_minutes: ep.get("runtime").and_then(Value::as_i64),
                // Addressed to the same source, so picking an episode plays it
                // from where the series was found. Always the prefixed form: a
                // bare id cannot carry a season and episode.
                token: make_token(source, &format!("tv:{series_id}:{wanted}:{number}")),
            })
        })
        .collect();

    Ok(EpisodeListing {
        series_title,
        seasons,
        season: wanted,
        episodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_series_keyed_by_tmdb() {
        assert_eq!(tmdb_series_id("tv:1396").as_deref(), Some("1396"));
        // an episode already chosen still identifies the same series
        assert_eq!(tmdb_series_id("tv:1396:2:3").as_deref(), Some("1396"));
    }

    #[test]
    fn recognises_anime_keyed_by_anilist() {
        assert_eq!(anilist_series_id("anilist:20").as_deref(), Some("20"));
        // an episode already chosen still identifies the same series
        assert_eq!(anilist_series_id("anilist:20:5").as_deref(), Some("20"));
        assert!(anilist_series_id("tv:1396").is_none());
        assert!(anilist_series_id("anilist:abc").is_none());
    }

    #[test]
    fn accepts_the_bare_ids_tmdb_bridged_sources_return() {
        // novera and friends hand back TMDB's id with no kind attached
        assert_eq!(tmdb_series_id("1396").as_deref(), Some("1396"));
    }

    #[test]
    fn ignores_everything_else() {
        assert!(tmdb_series_id("movie:27205").is_none());
        assert!(tmdb_series_id("anilist:20").is_none());
        assert!(tmdb_series_id("breaking-bad").is_none());
        assert!(tmdb_series_id("tv:").is_none());
        assert!(tmdb_series_id("tv:abc").is_none());
    }

    #[test]
    fn reads_the_season_already_being_watched() {
        assert_eq!(season_of("tv:1396:2:3"), Some(2));
        assert_eq!(season_of("tv:1396"), None);
    }
}
