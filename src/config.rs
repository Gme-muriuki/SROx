use crate::errors::config_error::ConfigError;
use serde::Deserialize;
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub addr: SocketAddr,
    pub tls: TlsConfig,
    pub upstream: UpstreamConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TlsConfig {
    pub cert_path: PathBuf,
    #[serde(rename = "private_key_path")]
    pub private_key: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct UpstreamConfig {
    pub addr: SocketAddr,
}

impl Config {
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let file = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&file)?;

        if !config.tls.cert_path.exists() {
            return Err(ConfigError::CertNotFound(config.tls.cert_path.clone()));
        }

        if !config.tls.private_key.exists() {
            return Err(ConfigError::KeyNotFound(config.tls.private_key.clone()));
        }

        Ok(config)
    }
}
