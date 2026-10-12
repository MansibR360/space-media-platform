//! Background health prober - runs every 60s, times a minimal call against
//! every source (native + enabled HTTP source plugins), and feeds the
//! circuit-breaker registry. Down sources get a half-open probe each cycle.

use std::sync::Arc;
use std::time::Instant;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::sources::health::HealthRegistry;

pub async fn health_loop(
    pool: PgPool,
    _cfg: Arc<crate::config::Config>,
    registry: Arc<HealthRegistry>,
    web_sources: Arc<crate::sources::web::WebSourceManager>,
    cancel: CancellationToken,
) {
    let natives = crate::sources::SourceRegistry::new(std::sync::Arc::new(
        crate::sources::youtube::Youtube::new(),
    ));

    loop {
        probe_all(&pool, &natives, &registry).await;
        probe_web(&registry, &web_sources).await;

        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
        }
    }
}

/// Learned web sources: a cheap search keeps their circuit breakers honest
/// so dead sites are skipped by the aggregator before requests stall.
async fn probe_web(registry: &HealthRegistry, web_sources: &crate::sources::web::WebSourceManager) {
    for entry in web_sources.list().await {
        let name = entry.source.name();
        registry.reset_for_probe(name).await;
        // The manager needs AppState for config-aware dispatch; the prober
        // only has the pool — use a lightweight HTTP-free reachability check
        // against the site root instead of spawning galaxy every cycle.
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            probe_site_root(&entry.source.site),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {
                registry
                    .record_success(name, started.elapsed().as_millis())
                    .await;
            }
            Ok(Err(err)) => {
                registry.record_failure(name, &err).await;
            }
            Err(_) => {
                registry.record_failure(name, "probe timeout (12s)").await;
            }
        }
    }
}

async fn probe_site_root(site: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("space-media-server/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(site)
        .send()
        .await
        .map_err(|e| format!("unreachable: {e}"))?;
    if resp.status().is_success() || resp.status().is_redirection() {
        Ok(())
    } else {
        Err(format!("status {}", resp.status()))
    }
}

async fn probe_all(
    pool: &PgPool,
    natives: &crate::sources::SourceRegistry,
    registry: &HealthRegistry,
) {
    for name in natives.list() {
        if let Some(provider) = natives.get(name) {
            registry.reset_for_probe(name).await;
            let started = Instant::now();
            let outcome =
                tokio::time::timeout(std::time::Duration::from_secs(12), provider.search("", 1))
                    .await;
            match outcome {
                Ok(Ok(items)) => {
                    registry
                        .record_success(name, started.elapsed().as_millis())
                        .await;
                    let _ = items;
                }
                Ok(Err(err)) => {
                    registry.record_failure(name, &err.to_string()).await;
                }
                Err(_) => {
                    registry.record_failure(name, "probe timeout (12s)").await;
                }
            }
        }
    }

    if let Ok(plugins) = crate::plugins::repo::list(pool).await {
        for plugin in plugins {
            if !(plugin.kind == "source" && plugin.enabled) {
                continue;
            }
            let Some(base) = plugin.base_url.clone() else {
                continue;
            };
            let name = plugin.name.clone();
            registry.reset_for_probe(&name).await;
            let started = Instant::now();
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(12),
                probe_http(base.trim_end_matches('/')),
            )
            .await;
            match outcome {
                Ok(Ok(())) => {
                    registry
                        .record_success(&name, started.elapsed().as_millis())
                        .await;
                }
                Ok(Err(err)) => {
                    registry.record_failure(&name, &err).await;
                }
                Err(_) => {
                    registry.record_failure(&name, "probe timeout (12s)").await;
                }
            }
        }
    }
}

async fn probe_http(base: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("space-media-server/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    // Source-kind plugins expose /items; metadata ones /search. Either pass.
    let items = client
        .get(format!("{base}/items"))
        .query(&[("limit", "1")])
        .send()
        .await;
    if let Ok(resp) = &items {
        if resp.status().is_success() {
            return Ok(());
        }
    }

    let resp = client
        .get(format!("{base}/search"))
        .query(&[("q", "probe")])
        .send()
        .await
        .map_err(|e| format!("unreachable: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("status {}", resp.status()))
    }
}
