use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not parse config file: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("TLS cert file not found: {0}")]
    CertNotFound(PathBuf),

    #[error("TLS private key file not found: {0}")]
    KeyNotFound(PathBuf),
}
