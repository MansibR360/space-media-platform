use crate::config::Config;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init(cfg: &Config) -> anyhow::Result<Option<PrometheusHandle>> {
    let filter = tracing_subscriber::EnvFilter::try_new(&cfg.log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(filter);

    if cfg.log_json {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()
            .map_err(|e| anyhow::anyhow!("logger init: {e}"))?;
    } else {
        registry
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .map_err(|e| anyhow::anyhow!("logger init: {e}"))?;
    }

    if !cfg.metrics_enabled {
        return Ok(None);
    }

    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).map_err(|e| anyhow::anyhow!("metrics init: {e}"))?;
    Ok(Some(handle))
}
