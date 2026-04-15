use srox::{config::Config, listener};
use std::{error::Error, path::Path, sync::Arc};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt().json().init();

    // Read config
    let config = Config::load_from_file(Path::new("config.toml")).map_err(|err| {
        tracing::error!(error = %err , "failed to read config");
        err
    })?;

    let config = Arc::new(config);

    tracing::info!("SROx starting");

    listener::run(config).await?;

    Ok(())
}
