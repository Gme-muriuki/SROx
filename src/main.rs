use srox::{
    config::Config,
    health::{self, UpstreamHealth},
    listener, metrics,
    pool::ConnectionPool,
    telemetry::telemetry,
};
use std::{error::Error, path::Path, sync::Arc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let provider = telemetry::init()?;

    //
    let config = Config::load_from_file(Path::new("config.toml")).map_err(|err| {
        tracing::error!(
            error = %err,
            path = "config.toml",
            "failed to load config — fix the error above and restart"
        );
        err
    })?;

    let pool = Arc::new(ConnectionPool::new(
        config.upstream.addr,
        config.upstream.pool_size,
        config.upstream.keep_alive_secs,
    ));

    let config = Arc::new(config);

    let health = UpstreamHealth::new(config.upstream.addr);

    // Spawn a background task for health check
    tokio::spawn(health::run_health_check(
        Arc::clone(&health),
        config.upstream.health_check_interval_secs,
        config.upstream.health_check_timeout_secs,
    ));

    // Run proxy and metrics
    tokio::select! {
      res = listener::run(Arc::clone(&config), Arc::clone(&pool)) => res?,
        res = metrics::serve_metrics(Arc::clone(&config)) => res?,
    }

    telemetry::shutdown(provider);

    Ok(())
}
