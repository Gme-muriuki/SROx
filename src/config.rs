#![allow(unused)]

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub addr: SocketAddr,
    pub tls: TlsConfig,
    pub upstream: UpstreamConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub private_key: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpstreamConfig {
    pub addr: SocketAddr,
}

impl Config {
    pub(crate) fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        // Read file
        let file = fs::read_to_string(path)?;
        // 2. Parse toml
        let config: Config = toml::from_str(&file)?;
        // 3. validate - cert file exist? , bind addr parseable.
        if !config.tls.cert_path.exists() {
            return Err(ConfigError::CertNotFound(config.tls.cert_path.clone()));
        }

        if !config.tls.private_key.exists() {
            return Err(ConfigError::KeyNotFound(config.tls.private_key.clone()));
        }
        // Return Ok(Config) or Err(ConfigError);
        Ok(config)
    }
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("could not read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not parse config file: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("TLS cert file not found: {0}")]
    CertNotFound(PathBuf),

    #[error("TLS private key file not found: {0}")]
    KeyNotFound(PathBuf),
}
