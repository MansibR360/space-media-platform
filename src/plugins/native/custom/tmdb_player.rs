//! TMDB-keyed player sites: cineby, cinebytv, cineplay, cubeflix, fmovies,
//! pstream-cfd, toustream, zstream and rivestream.
//!
//! Each was its own Python file, but they only differed in which player urls
//! they built from a TMDB id. That difference is expressed here as a list of
//! url templates per site; the search and enrich halves were identical.
//!
//! Templates understand `{base}`, `{id}`, `{s}`, `{e}` and `{slug}`. The
//! placeholder names match the Python's `.format()` keys, so an existing
//! `TOUSTREAM_SERIES_TEMPLATE` override keeps working unchanged.

use serde_json::{Value, json};

use crate::error::ApiResult;
use crate::plugins::native::engines::{
    EngineCtx, parse_media_id_msg, slugify, tmdb_enrich, tmdb_search, tmdb_trending,
};

/// One playable link a site offers. A `None` template means the site has no
/// such link for that media kind.
#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub label: &'static str,
    pub movie_url: Option<&'static str>,
    pub tv_url: Option<String>,
}

impl StreamSpec {
    pub fn both(label: &'static str, movie_url: &'static str, tv_url: &str) -> Self {
        Self {
            label,
            movie_url: Some(movie_url),
            tv_url: Some(tv_url.to_string()),
        }
    }

    pub fn tv_only(label: &'static str, tv_url: &str) -> Self {
        Self {
            label,
            movie_url: None,
            tv_url: Some(tv_url.to_string()),
        }
    }
}

/// How search results tag their media type. Most sites pass TMDB's own
/// `movie`/`tv`; rivestream rewrote `tv` to `series`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeStyle {
    Tmdb,
    MovieSeries,
}

#[derive(Debug, Clone)]
pub struct TmdbPlayer {
    pub base: String,
    /// Whether an empty query browses TMDB trending or answers empty.
    pub empty_query_trending: bool,
    pub streams: Vec<StreamSpec>,
    /// rivestream annotates which season/episode it defaulted to.
    pub tv_note: bool,
    pub type_style: TypeStyle,
    /// rivestream explains itself when no TMDB key is configured.
    pub no_key_note: Option<&'static str>,
    pub id_error: &'static str,
}

impl TmdbPlayer {
    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        if self.no_key_note.is_some() && ctx.tmdb_key.is_none() {
            return Ok(json!({ "results": [], "note": self.no_key_note }));
        }
        let query = query.trim();
        let limit = limit.clamp(1, 30);

        let mut results = if query.is_empty() {
            if self.empty_query_trending {
                tmdb_trending(ctx, limit).await
            } else {
                Vec::new()
            }
        } else {
            tmdb_search(ctx, query, limit).await
        };

        if self.type_style == TypeStyle::MovieSeries {
            for row in &mut results {
                let is_movie = row.get("type").and_then(Value::as_str) == Some("movie");
                row["type"] = json!(if is_movie { "movie" } else { "series" });
            }
        }
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let media = parse_media_id_msg(id, self.id_error)?;
        let info = tmdb_enrich(ctx, &media.kind, &media.tmdb_id)
            .await
            .unwrap_or_else(|| json!({}));

        let fallback = if media.kind == "movie" {
            format!("Movie {}", media.tmdb_id)
        } else {
            format!("TV Show {}", media.tmdb_id)
        };
        let title = info
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(&fallback)
            .to_string();

        let is_movie = media.kind == "movie";
        let slug = slugify(&title);
        let expand = |template: &str| -> String {
            template
                .replace("{base}", self.base.trim_end_matches('/'))
                .replace("{id}", &media.tmdb_id)
                .replace("{s}", &media.season.to_string())
                .replace("{e}", &media.episode.to_string())
                .replace("{slug}", &slug)
        };

        let mut streams = Vec::new();
        for spec in &self.streams {
            let template = if is_movie {
                spec.movie_url.map(str::to_string)
            } else {
                spec.tv_url.clone()
            };
            let Some(template) = template else { continue };
            streams.push(json!({
                "label": expand(spec.label),
                "url": expand(&template),
                "kind": "page",
            }));
        }

        let mut payload = json!({
            "title": title,
            "overview": info.get("overview").and_then(Value::as_str).unwrap_or(""),
            "poster": info.get("poster").and_then(Value::as_str).unwrap_or(""),
            "year": info.get("year").cloned().unwrap_or(Value::Null),
            "media_type": if is_movie { "movie" } else { "series" },
            "streams": streams,
        });
        if self.tv_note && !is_movie {
            payload["note"] = json!(format!(
                "defaulting to season {} episode {}",
                media.season, media.episode
            ));
        }
        Ok(payload)
    }
}

const VIDKING: &str = "https://www.vidking.net";

/// Sites whose player is the VidKing embed plus their own watch page.
fn vidking(player_label: &'static str, page_label: &'static str) -> Vec<StreamSpec> {
    vec![
        StreamSpec::both(
            player_label,
            "https://www.vidking.net/embed/movie/{id}",
            &format!("{VIDKING}/embed/tv/{{id}}/{{s}}/{{e}}"),
        ),
        StreamSpec::both(page_label, "{base}/movie/{id}", "{base}/tv/{id}"),
    ]
}

/// P-Stream family: one `/media/tmdb-…` watch link, plus a season/episode
/// deep link for series.
fn pstream(watch_label: &'static str, episode_label: &'static str) -> Vec<StreamSpec> {
    vec![
        StreamSpec::both(
            watch_label,
            "{base}/media/tmdb-movie-{id}-{slug}",
            "{base}/media/tmdb-tv-{id}-{slug}",
        ),
        StreamSpec::tv_only(
            episode_label,
            "{base}/media/tmdb-tv-{id}-{slug}?season={s}&episode={e}",
        ),
    ]
}

fn env_base(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

const TMDB_ID_ERROR: &str = "id must be movie:{tmdb_id} or tv:{tmdb_id}[:s]:[e]";
/// rivestream words it slightly differently, and the wording is part of the
/// contract callers see.
const RIVE_ID_ERROR: &str = "id must be movie:{tmdb_id} or tv:{tmdb_id}[:season]:[episode]";

fn player(
    base: String,
    empty_query_trending: bool,
    streams: Vec<StreamSpec>,
) -> TmdbPlayer {
    TmdbPlayer {
        base,
        empty_query_trending,
        streams,
        tv_note: false,
        type_style: TypeStyle::Tmdb,
        no_key_note: None,
        id_error: TMDB_ID_ERROR,
    }
}

// The labels below keep each site's exact text, mojibake included: several of
// these files had their middle dot double-encoded, and the Python served the
// bytes it had.

pub fn cineby() -> TmdbPlayer {
    let base = env_base("CINEBY_BASE", "https://cineby.at");
    player(
        base.clone(),
        false,
        vidking("Cineby \u{b7} player", "Cineby \u{b7} watch page"),
    )
}

pub fn cinebytv() -> TmdbPlayer {
    let base = env_base("CINEBYTV_BASE", "https://cinebytv.com");
    player(
        base.clone(),
        true,
        vidking("CinebyTV \u{b7} player", "CinebyTV \u{b7} watch page"),
    )
}

pub fn cineplay() -> TmdbPlayer {
    let base = "https://www.cineplay.to".to_string();
    player(
        base.clone(),
        true,
        vidking("CinePlay \u{b7} player", "CinePlay \u{b7} watch page"),
    )
}

pub fn fmovies() -> TmdbPlayer {
    let base = "https://fmovies.gd".to_string();
    player(
        base.clone(),
        true,
        vidking("FMovies \u{b7} player", "FMovies \u{b7} watch page"),
    )
}

pub fn cubeflix() -> TmdbPlayer {
    player(
        env_base("CUBEFLIX_BASE", "https://cubeflix.pages.dev"),
        true,
        pstream("Cubeflix \u{b7} watch", "Cubeflix \u{b7} S{s}E{e}"),
    )
}

pub fn zstream() -> TmdbPlayer {
    player(
        env_base("ZSTREAM_BASE", "https://zstream.mov"),
        false,
        pstream("Z-Stream \u{b7} watch", "Z-Stream \u{b7} S{s}E{e}"),
    )
}

pub fn pstream_cfd() -> TmdbPlayer {
    player(
        env_base("PSTREAM_CFD_BASE", "https://pstream.cfd"),
        true,
        pstream(
            "PStream.cfd \u{b7} watch",
            "PStream.cfd \u{b7} S{s}E{e}",
        ),
    )
}

pub fn toustream() -> TmdbPlayer {
    let base = env_base("TOUSTREAM_BASE", "https://toustream.xyz");
    // The site's series route is overridable, and the override is written
    // against `{id}/{s}/{e}` - the same placeholders this engine expands.
    let series = std::env::var("TOUSTREAM_SERIES_TEMPLATE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("{}/tou/series/{{id}}/{{s}}/{{e}}", base.trim_end_matches('/')));
    player(
        base,
        false,
        vec![StreamSpec {
            label: "TouStream \u{b7} player",
            movie_url: Some("{base}/tou/movies/{id}"),
            tv_url: Some(series),
        }],
    )
}

pub fn rivestream() -> TmdbPlayer {
    let base = env_base("RIVESTREAM_BASE", "https://www.rivestream.app");
    let tv_suffix = "&season={s}&episode={e}";
    TmdbPlayer {
        base,
        empty_query_trending: false,
        streams: vec![
            StreamSpec::both(
                "Rive \u{b7} fastest server",
                "{base}/embed?type=movie&id={id}",
                &format!("{{base}}/embed?type=tv&id={{id}}{tv_suffix}"),
            ),
            StreamSpec::both(
                "Rive \u{b7} aggregator servers",
                "{base}/embed/agg?type=movie&id={id}",
                &format!("{{base}}/embed/agg?type=tv&id={{id}}{tv_suffix}"),
            ),
            StreamSpec::both(
                "Rive \u{b7} torrent stream",
                "{base}/embed/torrent?type=movie&id={id}",
                &format!("{{base}}/embed/torrent?type=tv&id={{id}}{tv_suffix}"),
            ),
            StreamSpec::both(
                "Rive \u{b7} download",
                "{base}/download?type=movie&id={id}",
                &format!("{{base}}/download?type=tv&id={{id}}{tv_suffix}"),
            ),
        ],
        tv_note: true,
        type_style: TypeStyle::MovieSeries,
        no_key_note: Some(
            "set TMDB_API_KEY (env or SPACE .env) to enable search; \
             /details?id=movie:{tmdb_id} works without it",
        ),
        id_error: RIVE_ID_ERROR,
    }
}
