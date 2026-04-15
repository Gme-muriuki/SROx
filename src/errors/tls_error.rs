use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("pem parse error {0}")]
    PemParseError(String),

    #[error("no certificates found: {0}")]
    NoCertificatesFound(PathBuf),

    #[error("invalid certificate: {0}")]
    InvalidCertificate(String),

    #[error("no private key found: {0}")]
    NoPrivateKeyFound(PathBuf),

    #[error("invalid private key: {0}")]
    InvalidPrivateKey(String),

    #[error("config error: {0}")]
    ConfigError(#[from] rustls::Error),
}
