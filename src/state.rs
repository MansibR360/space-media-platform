use std::sync::Arc;

use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::auth::service::AuthService;
use crate::catalog::metadata::MetadataRegistry;
use crate::chat::hub::Hub;
use crate::config::Config;
use crate::parties::manager::PartyManager;
use crate::playback::transcode::TranscodeService;
use crate::ratelimit::middleware::RateLimiter;
use crate::recommendations::cache::CacheService;
use crate::recommendations::service::RecommendationService;
use crate::streaming::storage::Storage;

pub struct AppState {
    pub cfg: Arc<Config>,
    pub pool: PgPool,
    pub started_at: DateTime<Utc>,
    pub auth: AuthService,
    pub storage: Arc<dyn Storage>,
    pub hub: Arc<Hub>,
    pub parties: Arc<PartyManager>,
    pub transcode: TranscodeService,
    /// Live metadata providers (built-ins + HTTP plugins), hot-swappable.
    pub metadata: Arc<tokio::sync::RwLock<MetadataRegistry>>,
    pub cache: Arc<CacheService>,
    pub limiter: RateLimiter,
    pub sources: crate::sources::SourceRegistry,
    /// Shared YouTube handle (same instance the registry boxes) — powers the
    /// resilience loop and admin status/heal endpoints.
    pub youtube: std::sync::Arc<crate::sources::youtube::Youtube>,
    /// Learned web sources (galaxy recipes), hot-swappable.
    pub web_sources: Arc<crate::sources::web::WebSourceManager>,
    /// Playlist persistence behind the library storage SPI (swappable backend).
    pub playlists: Arc<dyn crate::library::store::PlaylistStore>,
    /// Stremio-compatible extensions, hot-swappable.
    pub extensions: Arc<crate::extensions::ExtensionManager>,
    /// Compiled-in site plugins (the former Python plugin directory).
    pub native_plugins: Arc<crate::plugins::native::NativeRegistry>,
    /// JavaScript source plugins, loaded from `plugins/js`.
    pub script_plugins: Arc<crate::plugins::script::ScriptRegistry>,
    /// Shared http client handed to a plugin's `fetch`.
    pub script_client: reqwest::Client,
    /// Daily benchmark scores snapshot (fastest-first selection).
    pub source_ratings: Arc<crate::sources::benchmark::Ratings>,
    pub sources_health: Arc<crate::sources::health::HealthRegistry>,
    pub recommendations: Arc<RecommendationService>,
    /// Admin AI copilot engine (governed, pluggable brain).
    pub ai: Arc<crate::aiops::engine::AiEngine>,
    pub metrics_handle: Option<PrometheusHandle>,
    pub cancel: CancellationToken, // read via Arc by background tasks
}

impl AppState {
    pub async fn build(
        cfg: Arc<Config>,
        pool: PgPool,
        metrics_handle: Option<PrometheusHandle>,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&cfg.storage_dir)?;

        let cache = CacheService::new(cfg.redis_url.clone()).await;
        let cache = Arc::new(cache);
        let limiter = RateLimiter::new(&cfg).await;
        let hub = Arc::new(Hub::new());
        let cancel = CancellationToken::new();

        // Seed the registry with built-in providers, then layer DB plugins on top.
        let mut registry = MetadataRegistry::from_config(&cfg);
        for plugin in crate::plugins::repo::list(&pool).await? {
            if !plugin.enabled || plugin.kind != "metadata" {
                continue;
            }
            if let Some(base_url) = plugin.base_url.clone() {
                match crate::plugins::http_provider::HttpPluginProvider::checked(
                    plugin.name.clone(),
                    base_url,
                ) {
                    Ok(provider) => registry.insert(Arc::new(provider)),
                    Err(err) => {
                        tracing::warn!(plugin = %plugin.name, error = %err, "plugin skipped")
                    }
                }
            }
        }
        let metadata = Arc::new(tokio::sync::RwLock::new(registry));

        let storage = Arc::new(crate::streaming::storage::LocalStorage::new()) as Arc<dyn Storage>;
        let transcode =
            TranscodeService::start(pool.clone(), cfg.clone(), cancel.child_token()).await;
        let parties = Arc::new(PartyManager::new(pool.clone(), hub.clone()));
        let recommendations = Arc::new(RecommendationService::new(pool.clone(), cache.clone()));
        let auth = AuthService::new(pool.clone(), cfg.clone());
        let playlists = crate::library::store::build_store(&cfg, pool.clone())
            .map_err(|err| anyhow::anyhow!("playlist store: {err}"))?;

        if let Some(interval) = cfg.scanner_interval {
            let roots = cfg.library_roots.clone();
            crate::streaming::scanner::spawn_scheduler(
                pool.clone(),
                roots,
                interval,
                cancel.child_token(),
            );
        }

        tokio::spawn(maintenance_loop(pool.clone(), cancel.child_token()));

        // Source health prober: keeps the circuit-breaker registry fresh so
        // dead sources are skipped before they slow requests down.
        let sources_health = crate::sources::health::HealthRegistry::new();

        // YouTube source: one shared instance boxed into the registry and
        // held on state for the resilience loop + admin status/heal API.
        let youtube = std::sync::Arc::new(crate::sources::youtube::Youtube::new());

        // Learned web sources (galaxy recipes): loaded before the prober so
        // they are health-monitored from the first cycle.
        let web_sources = Arc::new(crate::sources::web::WebSourceManager::new());
        web_sources.reload(&pool, &cfg).await;

        // Stremio-compatible extensions.
        let extension_mgr = Arc::new(crate::extensions::ExtensionManager::new());
        extension_mgr.reload(&pool, &cfg).await;

        // Site plugins compiled into the binary. Building the registry is
        // pure parsing; the rows they need in the plugins table are written
        // once the state Arc exists, below.
        // Every TMDB-keyed source asks the same questions; answers are kept
        // permanently so they are fetched once and never again.
        crate::catalog::tmdb_cache::init(pool.clone());
        // Magnets never change, so a torrent seen once is never fetched
        // again; seeder counts are refreshed whenever an index answers.
        crate::plugins::native::custom::torrent_cache::init(pool.clone());

        let native_plugins = Arc::new(crate::plugins::native::NativeRegistry::load(&cfg));

        // JavaScript providers, read from disk so a plugin can be dropped in
        // and picked up on restart without rebuilding the server.
        let script_plugins = Arc::new(crate::plugins::script::ScriptRegistry::load());
        let script_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            )
            .build()
            .unwrap_or_default();

        // Daily benchmark ratings: load persisted scores now; the 24 h
        // benchmark worker starts once the state Arc is assembled below.
        let source_ratings = Arc::new(crate::sources::benchmark::Ratings::default());
        source_ratings.reload(&pool).await;
        tokio::spawn(crate::sources::health_loop::health_loop(
            pool.clone(),
            cfg.clone(),
            sources_health.clone(),
            web_sources.clone(),
            cancel.child_token(),
        ));

        let ai = crate::aiops::engine::AiEngine::new(pool.clone(), cfg.clone(), hub.clone());
        crate::aiops::engine::AiEngine::spawn_actor(ai.clone(), cancel.child_token());

        // Predictive cache warmer (Phase H): precomputes rails before demand.
        crate::warming::spawn_loop(
            pool.clone(),
            recommendations.clone(),
            hub.clone(),
            cancel.child_token(),
        );

        let state = Arc::new(Self {
            cfg,
            pool,
            started_at: Utc::now(),
            auth,
            storage,
            hub,
            parties,
            transcode,
            metadata,
            cache,
            limiter,
            sources: crate::sources::SourceRegistry::new(youtube.clone()),
            youtube,
            web_sources,
            playlists,
            extensions: extension_mgr,
            native_plugins,
            script_plugins,
            script_client,
            source_ratings,
            sources_health,
            recommendations,
            ai,
            metrics_handle,
            cancel,
        });

        // Register the compiled-in site plugins. One upsert per site, so the
        // aggregator, health prober, benchmark worker and admin console see
        // them through the same plugins table every other source uses - no
        // admin token, no generated registration script, no restart.
        if state.cfg.native_plugins_enabled && !state.native_plugins.is_empty() {
            let registrar = state.clone();
            tokio::spawn(async move {
                if let Err(err) = crate::plugins::native::sync(&registrar).await {
                    tracing::warn!(error = %err, "native site plugin registration failed");
                }
            });
        }

        // JavaScript providers register the same way, as resolve-only sources:
        // they supply streams for a known id rather than titles.
        if !state.script_plugins.is_empty() {
            let registrar = state.clone();
            tokio::spawn(async move {
                if let Err(err) = crate::plugins::script::routes::sync(&registrar).await {
                    tracing::warn!(error = %err, "script plugin registration failed");
                }
            });
        }

        // Metadata refresh scheduler (M3): re-syncs stale external mappings.
        crate::catalog::refresh::spawn_scheduler(state.clone()).await;

        // Daily source benchmark (fastest-server ratings): first sweep ~30 s
        // after boot, then every 24 h.
        crate::sources::benchmark::spawn_worker(state.clone(), state.cancel.child_token());

        // YouTube resilience watchdog: instance-registry refresh, backend
        // rotation and degradation/recovery events on the platform bus.
        crate::sources::youtube::spawn_resilience_loop(state.clone(), state.youtube.clone());

        Ok(state)
    }

    pub fn uptime_secs(&self) -> i64 {
        (Utc::now() - self.started_at).num_seconds()
    }

    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.pool.close().await;
    }
}

/// Periodic housekeeping: popularity decay/recompute, expired session cleanup.
async fn maintenance_loop(pool: PgPool, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(600)) => {
                if let Err(err) = maintenance_tick(&pool).await {
                    tracing::warn!(error = %err, "maintenance tick failed");
                }
            }
        }
    }
}

async fn maintenance_tick(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE media m SET popularity = sub.score FROM (
             SELECT m2.id,
                 COALESCE(w.cnt, 0) * 10 + COALESCE(r.cnt, 0) * 5 + COALESCE(f.cnt, 0) * 3 AS score
             FROM media m2
             LEFT JOIN (SELECT media_id, COUNT(*) cnt FROM watch_history
                        WHERE updated_at > now() - INTERVAL '14 days' GROUP BY media_id) w ON w.media_id = m2.id
             LEFT JOIN (SELECT media_id, COUNT(*) cnt FROM reviews
                        WHERE status = 'published' GROUP BY media_id) r ON r.media_id = m2.id
             LEFT JOIN (SELECT media_id, COUNT(*) cnt FROM favorites GROUP BY media_id) f ON f.media_id = m2.id
         ) sub WHERE sub.id = m.id",
    )
    .execute(pool)
    .await?;

    // ---- self-healing: stuck transcode jobs (Phase 44 slice) ----
    // First offense: re-queue automatically. Repeat offenders: fail loudly.
    let healed = sqlx::query_scalar::<_, Uuid>(
        "WITH stuck AS (
             SELECT id, file_id, job_kind, attempts
             FROM transcode_jobs
             WHERE status = 'running' AND started_at < now() - INTERVAL '30 minutes'
               AND attempts < 2
             FOR UPDATE SKIP LOCKED
         )
         INSERT INTO transcode_jobs (id, file_id, job_kind, attempts)
         SELECT gen_random_uuid(), file_id, job_kind, attempts + 1
         FROM stuck RETURNING id",
    )
    .fetch_all(pool)
    .await?;
    if !healed.is_empty() {
        metrics::counter!("jobs_auto_retried_total").increment(healed.len() as u64);
        tracing::warn!(count = healed.len(), "auto-requeued stuck transcode jobs");
    }
    sqlx::query(
        "UPDATE transcode_jobs SET status = 'failed',
             error = CASE WHEN attempts < 2 THEN 'stuck (auto-retried)' ELSE 'stuck' END,
             finished_at = now()
         WHERE status = 'running' AND started_at < now() - INTERVAL '30 minutes'",
    )
    .execute(pool)
    .await?;

    sqlx::query("DELETE FROM sessions WHERE refresh_expires_at < now() - INTERVAL '30 days'")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM password_resets WHERE expires_at < now()")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM playback_handoffs WHERE expires_at < now() - INTERVAL '1 hour'")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM login_lockouts WHERE locked_until IS NULL AND window_started_at < now() - INTERVAL '1 day'")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE playback_sessions SET ended_at = now() \
         WHERE ended_at IS NULL AND last_active_at < now() - INTERVAL '6 hours'",
    )
    .execute(pool)
    .await?;

    // ---- adaptive privacy (changeset 3) ----
    // Analytics age into aggregates: raw platform events older than 30 days
    // are rolled up per day, raw rows past 90 days are deleted, and aged
    // security payloads are scrubbed. Personal data (watch history, lists)
    // is never touched â€” this only shrinks the analytics surface.
    let rollup = sqlx::query(
        "INSERT INTO event_rollups (event, day, count)
         SELECT event, date_trunc('day', created_at)::date, COUNT(*)::bigint
         FROM events WHERE created_at < now() - INTERVAL '30 days'
         GROUP BY event, day
         ON CONFLICT (event, day) DO UPDATE SET count = event_rollups.count + EXCLUDED.count",
    )
    .execute(pool)
    .await;
    if let Ok(result) = rollup {
        if result.rows_affected() > 0 {
            tracing::debug!(rows = result.rows_affected(), "event rollups advanced");
        }
    }
    sqlx::query("DELETE FROM events WHERE created_at < now() - INTERVAL '90 days'")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE security_events SET details = '{}' \
         WHERE created_at < now() - INTERVAL '90 days' AND details <> '{}'",
    )
    .execute(pool)
    .await?;

    tracing::debug!("maintenance tick complete");
    Ok(())
}
