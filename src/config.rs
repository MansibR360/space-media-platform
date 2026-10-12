use std::env;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Absent means "start the bundled postgres server" — see [`crate::db::connect`].
    pub database_url: Option<String>,
    /// Where the bundled server keeps its binaries and data directory.
    pub embedded_db_dir: Option<PathBuf>,
    pub db_max_connections: u32,
    pub redis_url: Option<String>,
    pub log_level: String,
    pub log_json: bool,
    pub metrics_enabled: bool,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    pub session_cookie: String,
    pub cookie_secure: bool,
    pub cors_origins: Vec<String>,
    pub trust_proxy: bool,
    pub storage_dir: PathBuf,
    pub library_roots: Vec<PathBuf>,
    /// Resolved at startup by [`crate::ffmpeg::provision`] — these may point at
    /// a copy the server installed itself, not just a name on PATH.
    pub ffmpeg_bin: String,
    pub ffprobe_bin: String,
    /// Download a static ffmpeg when none can be found. Windows only today.
    pub ffmpeg_auto_download: bool,
    /// Where the server keeps binaries it installed itself.
    pub tools_dir: Option<PathBuf>,
    pub transcode_heights: Vec<u32>,
    /// none | nvenc | qsv | vaapi — hardware encoder preference
    pub hwaccel: String,
    pub tmdb_api_key: Option<String>,
    pub anilist_enabled: bool,
    pub rate_limit_enabled: bool,
    pub rate_global_per_min: u32,
    pub rate_auth_per_min: u32,
    pub admin_email: Option<String>,
    pub admin_password: Option<String>,
    pub scanner_interval: Option<Duration>,
    pub omdb_api_key: Option<String>,
    pub reset_link_base: String,
    pub password_min_length: usize,
    pub lockout_threshold: u32,
    pub lockout_window: Duration,
    pub lockout_duration: Duration,
    /// Reserved for a strict plugin egress policy enforced at registration.
    #[allow(dead_code)]
    pub plugin_allow_http: bool,
    /// Echoes email verification codes in API responses (LOCAL DEV ONLY).
    pub email_dev_echo: bool,
    /// Restrict /api/admin and admin.html to loopback peers (default true).
    pub admin_local_only: bool,
    /// Short-lived playback authorization lifetime (clamped 60..=3600 s).
    pub playback_grant_ttl: Duration,
    /// Optional OpenAI-compatible endpoint backing the admin AI copilot.
    pub ai_api_url: String,
    pub ai_api_key: Option<String>,
    pub ai_model: String,
    /// Encrypt HLS outputs with AES-128 and serve keys through the grant-gated
    /// key endpoint (Phase J).
    pub hls_encryption: bool,
    /// When set, media-protection endpoints (`/api/playback/keys/*`,
    /// `/api/dl/*`) additionally require header `X-Origin-Token` to match —
    /// for private origins behind a CDN/reverse proxy.
    pub origin_internal_token: Option<String>,
    /// Offline license lifetime (clamped 1 h – 7 d; default 24 h).
    pub offline_license_ttl: Duration,
    /// Path to the galaxy scraper binary enabling learned web sources.
    /// When unset, well-known locations (`./galaxy.exe`,
    /// `../galaxy/target/release/…`) and `PATH` are tried.
    pub galaxy_bin: Option<String>,
    /// Master switch for learned web sources (default on).
    pub web_sources_enabled: bool,
    /// Per-invocation timeout for galaxy subprocess calls.
    pub galaxy_timeout_secs: u64,
    /// Inbound auth for this server's hub endpoints (`/api/hub/*`).
    /// Unset → hub endpoints answer loopback peers only (homelab safe).
    pub source_hub_token: Option<String>,
    /// Outbound timeout for hub queries.
    pub hub_timeout_secs: u64,
    /// Outbound timeout for Stremio-extension resource calls.
    pub extension_timeout_secs: u64,
    /// Playlist persistence backend for the library storage SPI
    /// (`postgres`; other backends plug in via `library::store::PlaylistStore`).
    pub playlist_store_backend: String,
    /// Base url this server reaches its own native-plugin routes on. Defaults
    /// to `http://127.0.0.1:{APP_PORT}`; set it when the server is reached
    /// somewhere other than its bound port.
    pub native_plugins_base: Option<String>,
    /// Register the compiled-in site plugins at startup (default on).
    pub native_plugins_enabled: bool,
    /// Also register sites whose search never answers and which therefore
    /// cannot produce playback. Off by default: they only slowed the fan-out.
    pub native_plugins_include_passthrough: bool,
}

fn var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn required(key: &str) -> Result<String, anyhow::Error> {
    var(key).ok_or_else(|| anyhow::anyhow!("missing required environment variable: {key}"))
}

fn parse_bool(key: &str, default: bool) -> bool {
    var(key)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn parse_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    var(key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn parse_duration_secs(key: &str, default: u64) -> Duration {
    Duration::from_secs(parse_num(key, default))
}

fn parse_list(key: &str) -> Vec<String> {
    var(key)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_paths(key: &str) -> Vec<PathBuf> {
    parse_list(key).into_iter().map(PathBuf::from).collect()
}

impl Config {
    pub fn load() -> Result<Self, anyhow::Error> {
        dotenvy::dotenv().ok();

        let scanner_secs = parse_num::<u64>("SCANNER_INTERVAL_SECS", 0);

        Ok(Self {
            host: var("APP_HOST").unwrap_or_else(|| "0.0.0.0".into()),
            port: parse_num("APP_PORT", 8080),
            database_url: var("DATABASE_URL"),
            embedded_db_dir: var("EMBEDDED_DB_DIR").map(PathBuf::from),
            db_max_connections: parse_num("DB_MAX_CONNECTIONS", 10),
            redis_url: var("REDIS_URL"),
            log_level: var("LOG_LEVEL").unwrap_or_else(|| "info".into()),
            log_json: parse_bool("LOG_JSON", false),
            metrics_enabled: parse_bool("METRICS_ENABLED", true),
            access_ttl: parse_duration_secs("ACCESS_TOKEN_TTL_SECS", 900),
            refresh_ttl: parse_duration_secs("REFRESH_TOKEN_TTL_SECS", 30 * 24 * 3600),
            session_cookie: var("SESSION_COOKIE_NAME").unwrap_or_else(|| "space_session".into()),
            cookie_secure: parse_bool("COOKIE_SECURE", false),
            cors_origins: parse_list("CORS_ORIGINS"),
            trust_proxy: parse_bool("TRUST_PROXY", false),
            storage_dir: PathBuf::from(
                var("STORAGE_DIR").unwrap_or_else(|| "./data/storage".into()),
            ),
            library_roots: parse_paths("LIBRARY_ROOTS"),
            ffmpeg_bin: var("FFMPEG_BIN").unwrap_or_else(|| "ffmpeg".into()),
            ffprobe_bin: var("FFPROBE_BIN").unwrap_or_else(|| "ffprobe".into()),
            ffmpeg_auto_download: parse_bool("FFMPEG_AUTO_DOWNLOAD", true),
            tools_dir: var("TOOLS_DIR").map(PathBuf::from),
            hwaccel: var("HWACCEL").unwrap_or_else(|| "none".into()),
            transcode_heights: var("TRANSCODE_HEIGHTS")
                .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect())
                .unwrap_or_else(|| vec![1080, 720, 480]),
            tmdb_api_key: var("TMDB_API_KEY"),
            anilist_enabled: parse_bool("ANILIST_ENABLED", true),
            rate_limit_enabled: parse_bool("RATE_LIMIT_ENABLED", true),
            // The home screen loads a rail per source and then polls them, which
            // sustains several hundred requests a minute for one viewer. 300 was
            // below what the app asks of itself, so normal browsing 429'd.
            rate_global_per_min: parse_num("RATE_LIMIT_GLOBAL_PER_MIN", 1500),
            rate_auth_per_min: parse_num("RATE_LIMIT_AUTH_PER_MIN", 20),
            admin_email: var("ADMIN_EMAIL"),
            admin_password: var("ADMIN_PASSWORD"),
            scanner_interval: (scanner_secs > 0).then(|| Duration::from_secs(scanner_secs)),
            omdb_api_key: var("OMDB_API_KEY"),
            reset_link_base: var("RESET_LINK_BASE")
                .unwrap_or_else(|| "http://localhost:8080/reset".into()),
            password_min_length: parse_num("PASSWORD_MIN_LENGTH", 8),
            lockout_threshold: parse_num("LOCKOUT_THRESHOLD", 5),
            lockout_window: parse_duration_secs("LOCKOUT_WINDOW_SECS", 900),
            lockout_duration: parse_duration_secs("LOCKOUT_DURATION_SECS", 900),
            plugin_allow_http: parse_bool("PLUGIN_ALLOW_HTTP", true),
            email_dev_echo: parse_bool("EMAIL_DEV_ECHO", true),
            admin_local_only: parse_bool("ADMIN_LOCAL_ONLY", true),
            playback_grant_ttl: parse_duration_secs("PLAYBACK_GRANT_TTL_SECS", 600),
            ai_api_url: var("AI_API_URL").unwrap_or_else(|| "https://api.openai.com/v1".into()),
            ai_api_key: var("AI_API_KEY"),
            ai_model: var("AI_MODEL").unwrap_or_else(|| "gpt-4o-mini".into()),
            hls_encryption: parse_bool("HLS_ENCRYPTION", false),
            origin_internal_token: var("ORIGIN_INTERNAL_TOKEN"),
            offline_license_ttl: parse_duration_secs("OFFLINE_LICENSE_TTL_SECS", 24 * 3600),
            galaxy_bin: var("GALAXY_BIN"),
            web_sources_enabled: parse_bool("WEB_SOURCES_ENABLED", true),
            galaxy_timeout_secs: parse_num::<u64>("GALAXY_TIMEOUT_SECS", 25).clamp(5, 120),
            source_hub_token: var("SOURCE_HUB_TOKEN"),
            hub_timeout_secs: parse_num::<u64>("HUB_TIMEOUT_SECS", 15).clamp(5, 60),
            extension_timeout_secs: parse_num::<u64>("EXTENSION_TIMEOUT_SECS", 15).clamp(5, 60),
            playlist_store_backend: var("PLAYLIST_STORE_BACKEND")
                .unwrap_or_else(|| "postgres".into()),
            native_plugins_base: var("NATIVE_PLUGINS_BASE"),
            native_plugins_enabled: parse_bool("NATIVE_PLUGINS_ENABLED", true),
            native_plugins_include_passthrough: parse_bool(
                "NATIVE_PLUGINS_INCLUDE_PASSTHROUGH",
                false,
            ),
        })
    }
}
