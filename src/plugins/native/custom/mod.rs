//! The site plugins that were not just a factory call.
//!
//! [`super::sites`] covers the ~480 plugins that reduced to four shared
//! engines. The rest each had real code - their own JSON APIs, HTML scraping,
//! RSS feeds, player-url schemes - and are converted here, one Rust type per
//! plugin, keeping each one's ids, labels, notes and error strings.

pub mod apis;
pub mod embeds;
pub mod libraries;
pub mod music;
pub mod openmovies;
pub mod scrapers;
pub mod torrent_cache;
pub mod torrents;
pub mod tmdb_player;

use serde_json::{Value, json};

use crate::error::ApiResult;
use crate::plugins::native::engines::EngineCtx;

/// One converted plugin, holding whatever configuration that plugin read from
/// the environment at import time.
#[derive(Clone)]
pub enum CustomEngine {
    TmdbPlayer(tmdb_player::TmdbPlayer),
    OpenMovies,
    KissKh(apis::KissKh),
    MyAnimeList(apis::MyAnimeList),
    Simkl(apis::Simkl),
    Nasa(apis::Nasa),
    RadioBrowser(apis::RadioBrowser),
    Iptv(apis::Iptv),
    Miruro(apis::Miruro),
    AnimePahe(apis::AnimePahe),
    LunaStream(scrapers::LunaStream),
    WeebCentral(scrapers::WeebCentral),
    HentaiHaven(scrapers::HentaiHaven),
    Nyaa(scrapers::Nyaa),
    /// Books, comics and magazines as files - the only large book index a
    /// server can actually read.
    LibGen(scrapers::LibGen),
    Leetx(scrapers::Leetx),
    /// Self-hosted library servers, configured rather than built in.
    Komga(libraries::Komga),
    Kavita(libraries::Kavita),
    /// A torrent index, answering for every category.
    Torrents(torrents::TorrentIndex),
    /// Music, resolved to a direct audio url rather than a page.
    Saavn(music::Saavn),
}

impl CustomEngine {
    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        match self {
            Self::TmdbPlayer(p) => p.search(ctx, query, limit).await,
            Self::OpenMovies => openmovies::search(query, limit),
            Self::KissKh(p) => p.search(ctx, query, limit).await,
            Self::MyAnimeList(p) => p.search(ctx, query, limit).await,
            Self::Simkl(p) => p.search(ctx, query, limit).await,
            Self::Nasa(p) => p.search(ctx, query, limit).await,
            Self::RadioBrowser(p) => p.search(ctx, query, limit).await,
            Self::Iptv(p) => p.search(ctx, query, limit).await,
            Self::Miruro(p) => p.search(ctx, query, limit).await,
            Self::AnimePahe(p) => p.search(ctx, query, limit).await,
            Self::LunaStream(p) => p.search(ctx, query, limit).await,
            Self::WeebCentral(p) => p.search(ctx, query, limit).await,
            Self::HentaiHaven(p) => p.search(ctx, query, limit).await,
            Self::Nyaa(p) => p.search(ctx, query, limit).await,
            Self::LibGen(p) => p.search(ctx, query, limit).await,
            Self::Leetx(p) => p.search(ctx, query, limit).await,
            Self::Komga(p) => p.search(ctx, query, limit).await,
            Self::Kavita(p) => p.search(ctx, query, limit).await,
            Self::Torrents(p) => p.search(ctx, query, limit).await,
            Self::Saavn(p) => p.search(ctx, query, limit).await,
        }
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        match self {
            Self::TmdbPlayer(p) => p.details(ctx, id).await,
            Self::OpenMovies => openmovies::details(id),
            Self::KissKh(p) => p.details(ctx, id).await,
            Self::MyAnimeList(p) => p.details(ctx, id).await,
            Self::Simkl(p) => p.details(ctx, id).await,
            Self::Nasa(p) => p.details(ctx, id).await,
            Self::RadioBrowser(p) => p.details(ctx, id).await,
            Self::Iptv(p) => p.details(ctx, id).await,
            Self::Miruro(p) => p.details(ctx, id).await,
            Self::AnimePahe(p) => p.details(ctx, id).await,
            Self::LunaStream(p) => p.details(ctx, id).await,
            Self::WeebCentral(p) => p.details(ctx, id).await,
            Self::HentaiHaven(p) => p.details(ctx, id).await,
            Self::Nyaa(p) => p.details(ctx, id).await,
            Self::LibGen(p) => p.details(ctx, id).await,
            Self::Leetx(p) => p.details(ctx, id).await,
            Self::Komga(p) => p.details(ctx, id).await,
            Self::Kavita(p) => p.details(ctx, id).await,
            Self::Torrents(p) => p.details(ctx, id).await,
            Self::Saavn(p) => p.details(ctx, id).await,
        }
    }

    /// `/items` is a health-probe target for these sites; none exposes a bulk
    /// library, and the Python answered the same way.
    pub fn items(&self) -> Value {
        json!({ "items": [], "note": "library sync not supported" })
    }
}

/// A converted plugin plus the identity it registers under.
pub struct CustomSite {
    pub name: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub engine: CustomEngine,
}

fn site(
    name: &'static str,
    label: &'static str,
    category: &'static str,
    engine: CustomEngine,
) -> CustomSite {
    CustomSite {
        name,
        label,
        category,
        engine,
    }
}

/// Every converted custom plugin. Environment is read here, once, the way each
/// Python file read it at import time.
pub fn all() -> Vec<CustomSite> {
    use CustomEngine as E;
    let mut sites = vec![
        // TMDB-keyed player sites
        site(
            "cineby",
            "Cineby \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::cineby()),
        ),
        site(
            "cinebytv",
            "CinebyTV \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::cinebytv()),
        ),
        site(
            "cineplay",
            "CinePlay \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::cineplay()),
        ),
        site(
            "fmovies",
            "FMovies \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::fmovies()),
        ),
        site(
            "cubeflix",
            "Cubeflix \u{b7} Movies & TV (P-Stream)",
            "movie",
            E::TmdbPlayer(tmdb_player::cubeflix()),
        ),
        site(
            "zstream",
            "Z-Stream (P-Stream) \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::zstream()),
        ),
        site(
            "pstream-cfd",
            "PStream.cfd \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::pstream_cfd()),
        ),
        site(
            "toustream",
            "TouStream \u{b7} Movies, Series & Live TV",
            "movie",
            E::TmdbPlayer(tmdb_player::toustream()),
        ),
        site(
            "rivestream",
            "RiveStream \u{b7} Movies & TV",
            "movie",
            E::TmdbPlayer(tmdb_player::rivestream()),
        ),
        // API-backed catalogs
        site(
            "kisskh",
            "KissKH \u{b7} Asian Dramas & Movies",
            "series",
            E::KissKh(apis::KissKh::new()),
        ),
        site(
            "myanimelist",
            "MyAnimeList \u{b7} Anime & Manga Search",
            "manga",
            E::MyAnimeList(apis::MyAnimeList),
        ),
        site(
            "simkl",
            "Simkl \u{b7} Tracking Database",
            "movie",
            E::Simkl(apis::Simkl::new()),
        ),
        site(
            "nasa",
            "NASA+ \u{b7} Documentaries & Footage",
            "movie",
            E::Nasa(apis::Nasa),
        ),
        site(
            "radiobrowser",
            "Radio-Browser \u{b7} 50k Live Stations",
            "music",
            E::RadioBrowser(apis::RadioBrowser::new()),
        ),
        site(
            "iptv",
            "IPTV-org \u{b7} Live TV (global)",
            "series",
            E::Iptv(apis::Iptv::new()),
        ),
        site(
            "miruro",
            "Miruro \u{b7} Anime (sub/dub)",
            "manga",
            E::Miruro(apis::Miruro::new()),
        ),
        site(
            "animepahe",
            "AnimePahe \u{b7} Anime",
            "manga",
            E::AnimePahe(apis::AnimePahe::new()),
        ),
        // HTML / RSS
        site(
            "lunastream",
            "LunaStream \u{b7} Multi-Source Anime",
            "manga",
            E::LunaStream(scrapers::LunaStream::new()),
        ),
        site(
            "weebcentral",
            "WeebCentral \u{b7} Manga",
            "manga",
            E::WeebCentral(scrapers::WeebCentral),
        ),
        site(
            "hentaihaven",
            "Hentai Haven \u{b7} Anime (18+)",
            "manga",
            E::HentaiHaven(scrapers::HentaiHaven::new()),
        ),
        // Torrent indexes carry every kind of media, so they answer for every
        // category. Tagging them `manga` and `movie` meant a book search never
        // reached 1337x (which carries ebooks) and a film search never reached
        // Nyaa - an empty category is the registry's "anything" marker.
        site("nyaa", "Nyaa \u{b7} Torrents", "", E::Nyaa(scrapers::Nyaa::new())),
        site("leetx", "1337x \u{b7} Torrents", "", E::Leetx(scrapers::Leetx::new())),
        // The torrent index that is actually reachable from a server, and
        // the only source here that reliably carries ebooks and comics.
        site(
            "torrents",
            "Torrents \u{b7} Everything",
            "",
            E::Torrents(torrents::TorrentIndex::new()),
        ),
        // Books and comics as files. Anna's Archive and Z-Library are both
        // behind DDoS-Guard and unreadable from a server, so this is the only
        // large book index SPACE can actually search.
        site(
            "libgen",
            "Library Genesis \u{b7} Books & Comics",
            "book",
            E::LibGen(scrapers::LibGen::new()),
        ),
        // Music that plays here: the search is public and the CDN path it
        // returns decrypts to a direct, seekable audio file.
        site(
            "saavn",
            "JioSaavn \u{b7} Music",
            "music",
            E::Saavn(music::Saavn::new()),
        ),
        // Reference implementation, all Creative Commons
        site(
            "openmovies",
            "Blender Open Movies",
            "movie",
            E::OpenMovies,
        ),
    ];

    // Your own library servers, if you run one. These are the sources that
    // actually hold books and comics, so they go in front of the public ones —
    // but only when there is a server to talk to.
    if let Some(komga) = libraries::Komga::from_env() {
        tracing::info!(url = %komga.base, "komga library configured");
        sites.push(site(
            "komga",
            "Komga · Your Library",
            "manga",
            E::Komga(komga),
        ));
    }
    if let Some(kavita) = libraries::Kavita::from_env() {
        tracing::info!(url = %kavita.base, "kavita library configured");
        sites.push(site(
            "kavita",
            "Kavita · Your Library",
            "book",
            E::Kavita(kavita),
        ));
    }

    sites
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::native::engines::EngineCtx;

    /// Every converted plugin registers under a name the plugins table accepts,
    /// and no two collide.
    #[test]
    fn names_are_registrable_and_unique() {
        let sites = all();
        // 25 converted plugins, plus a library server for each of Komga and
        // Kavita when one is configured — neither is in a test process.
        assert!(
            sites.len() >= 26,
            "expected every converted custom plugin, got {}",
            sites.len()
        );
        let mut seen = std::collections::HashSet::new();
        for site in &sites {
            assert!(
                crate::plugins::models::valid_plugin_name(site.name),
                "invalid name: {}",
                site.name
            );
            assert!(!site.label.is_empty(), "{} has no label", site.name);
            assert!(
                matches!(
                    site.category,
                    "movie" | "series" | "manga" | "music" | "book" | ""
                ),
                "{} has an unknown category: {}",
                site.name,
                site.category
            );
            assert!(seen.insert(site.name), "duplicate name: {}", site.name);
        }
    }

    /// Id validation is pure, so the rejection path is checked without a
    /// network: each plugin must reject a nonsense id with its own message.
    #[tokio::test]
    async fn bad_ids_are_rejected_with_the_original_message() {
        let ctx = EngineCtx::new(None);
        for site in all() {
            // openmovies is the one plugin whose ids are plain slugs
            let bad = if site.name == "openmovies" {
                "definitely-not-a-film"
            } else {
                "garbage"
            };
            let result = site.engine.details(&ctx, bad).await;
            assert!(
                result.is_err(),
                "{} accepted a nonsense id: {:?}",
                site.name,
                result.ok()
            );
        }
    }

    /// The player sites build their urls without touching the network when no
    /// TMDB key is configured, so the whole url scheme is checkable here.
    #[tokio::test]
    async fn player_urls_match_the_python_scheme() {
        let ctx = EngineCtx::new(None);
        let sites = all();
        let find = |name: &str| {
            sites
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{name} missing"))
        };

        let cineby = find("cineby").engine.details(&ctx, "tv:1396:2:3").await.unwrap();
        let urls: Vec<&str> = cineby["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls[0], "https://www.vidking.net/embed/tv/1396/2/3");
        assert_eq!(urls[1], "https://cineby.at/tv/1396");

        // series with no season given default to S1E1, as the Python did
        let toustream = find("toustream").engine.details(&ctx, "tv:1396").await.unwrap();
        assert_eq!(
            toustream["streams"][0]["url"].as_str().unwrap(),
            "https://toustream.xyz/tou/series/1396/1/1"
        );

        // P-Stream family slugs the (fallback) title into the media path
        let zstream = find("zstream").engine.details(&ctx, "movie:155").await.unwrap();
        assert_eq!(
            zstream["streams"][0]["url"].as_str().unwrap(),
            "https://zstream.mov/media/tmdb-movie-155-movie-155"
        );

        // rivestream annotates the episode it defaulted to
        let rive = find("rivestream").engine.details(&ctx, "tv:1396").await.unwrap();
        assert_eq!(
            rive["note"].as_str().unwrap(),
            "defaulting to season 1 episode 1"
        );
        assert_eq!(rive["streams"].as_array().unwrap().len(), 4);
    }

    /// The example plugin is fully offline, so its whole contract is testable.
    #[tokio::test]
    async fn openmovies_serves_its_catalog() {
        let ctx = EngineCtx::new(None);
        let engine = CustomEngine::OpenMovies;

        let all_films = engine.search(&ctx, "", 10).await.unwrap();
        assert_eq!(all_films["items"].as_array().unwrap().len(), 3);

        let hit = engine.search(&ctx, "sintel", 10).await.unwrap();
        assert_eq!(hit["items"][0]["title"].as_str().unwrap(), "Sintel");

        let details = engine.details(&ctx, "big-buck-bunny").await.unwrap();
        assert_eq!(
            details["streams"][0]["url"].as_str().unwrap(),
            "https://download.blender.org/peach/bigbuckbunny_movies/BigBuckBunny_320x180.mp4"
        );
        assert_eq!(details["streams"][0]["kind"].as_str().unwrap(), "mp4");
    }

    /// Search with an empty query is pure for the plugins that short-circuit
    /// it, and must not reach out.
    #[tokio::test]
    async fn empty_queries_short_circuit() {
        let ctx = EngineCtx::new(None);
        for name in ["kisskh", "myanimelist", "miruro", "animepahe", "weebcentral"] {
            let site = all().into_iter().find(|s| s.name == name).unwrap();
            let body = site.engine.search(&ctx, "   ", 20).await.unwrap();
            assert_eq!(
                body["results"].as_array().map(Vec::len),
                Some(0),
                "{name} did not short-circuit an empty query"
            );
        }
    }

    /// Simkl and rivestream explain themselves rather than failing when their
    /// credential is missing - the note text is part of what callers see.
    #[tokio::test]
    async fn missing_credentials_are_explained() {
        let ctx = EngineCtx::new(None);
        let sites = all();

        let simkl = sites.iter().find(|s| s.name == "simkl").unwrap();
        let body = simkl.engine.search(&ctx, "cowboy bebop", 10).await.unwrap();
        if std::env::var("SIMKL_CLIENT_ID").is_err() {
            assert!(
                body["note"].as_str().unwrap_or("").contains("SIMKL_CLIENT_ID"),
                "simkl should name the missing variable"
            );
        }

        let rive = sites.iter().find(|s| s.name == "rivestream").unwrap();
        let body = rive.engine.search(&ctx, "cowboy bebop", 10).await.unwrap();
        assert!(
            body["note"].as_str().unwrap_or("").contains("TMDB_API_KEY"),
            "rivestream should name the missing key"
        );
    }
}
