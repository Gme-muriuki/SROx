use srox::{config::Config, listener, metrics, telemetry::telemetry};
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

    let config = Arc::new(config);

    // Run proxy and metrics
    tokio::select! {
      res = listener::run(Arc::clone(&config)) => res?,
        res = metrics::serve_metrics(Arc::clone(&config)) => res?,
    }

    telemetry::shutdown(provider);

    Ok(())
}
