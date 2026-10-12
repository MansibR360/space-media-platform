mod admin_gate;
mod aiops;
mod appeals;
mod audit;
mod auth;
mod automation;
mod billing;
mod cards;
mod catalog;
mod catalogs;
mod chat;
mod config;
mod db;
mod docs;
mod error;
mod events;
mod extensions;
mod ffmpeg;
mod friends;
mod graph;
mod heuristic;
mod intelligence;
mod library;
mod lyrics;
mod moderation;
mod music;
mod pagination;
mod parties;
mod platform;
mod playback;
mod plugins;
mod privacy;
mod qol;
mod ratelimit;
mod recommendations;
mod router;
mod search;
mod security;
mod settings;
mod sharing;
mod social;
mod sources;
mod state;
mod streaming;
mod telemetry;
mod tracking;
mod twin;
mod users;
mod utils;
mod watchdog;
mod webhooks;
mod warming;
mod youtube;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::Context;

use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cfg = Config::load()?;

    // Probe mode: exercise one converted source against the live site.
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|a| a == "--probe-native") {
        let name = args.get(index + 1).cloned().unwrap_or_default();
        let query = args.get(index + 2).cloned().unwrap_or_default();
        let registry = plugins::native::NativeRegistry::load(&cfg);
        let report = plugins::native::probe(&registry, &name, &query).await;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Detection mode: fingerprint the passthrough sites, whose search never
    // worked, and report which engine each one could actually use.
    if args.iter().any(|a| a == "--detect-passthrough") {
        let registry = plugins::native::NativeRegistry::load(&cfg);
        let report = plugins::native::detect_passthrough(&registry, 32).await;
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Parity mode: dump what every compiled-in site plugin answers for a fixed
    // set of offline inputs, so `tools/parity_check.py` can diff it against the
    // Python plugins those engines replaced. Runs without a database.
    if std::env::args().any(|arg| arg == "--dump-native-parity") {
        // The vectors must not depend on credentials or the network: with no
        // TMDB key the enrich step is skipped on both sides, exactly as the
        // Python does when TMDB_API_KEY is unset.
        cfg.tmdb_api_key = None;
        let registry = plugins::native::NativeRegistry::load(&cfg);
        let dump = plugins::native::parity_vectors(&registry).await;
        println!("{}", serde_json::to_string_pretty(&dump)?);
        return Ok(());
    }

    let metrics_handle = telemetry::init(&cfg)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting space media server"
    );

    // Locate ffmpeg (installing it if needed) before anything reads the paths.
    // Music analysis does not need it, but transcoding and markers do.
    ffmpeg::provision(&mut cfg).await;
    let cfg = Arc::new(cfg);

    // Holds the bundled server when DATABASE_URL is unset; must stay alive for
    // as long as the pool is used.
    let mut database = db::connect(&cfg).await.context("database connection")?;
    db::migrate(&database.pool)
        .await
        .context("database migrations")?;

    let state = state::AppState::build(cfg.clone(), database.pool.clone(), metrics_handle).await?;

    let app = router::build(state.clone());

    let host: IpAddr = cfg
        .host
        .parse()
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let addr = SocketAddr::new(host, cfg.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "listening");

    auth::bootstrap_admin(&state).await?;

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state.cancel.clone()))
    .await?;

    tracing::info!("shutting down");
    state.shutdown().await;
    // After the app has released its handles, so the server checkpoints
    // cleanly instead of recovering on next boot.
    database.shutdown().await;
    Ok(())
}

async fn shutdown_signal(cancel: tokio_util::sync::CancellationToken) {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::warn!("shutdown signal received");
    cancel.cancel();
}

