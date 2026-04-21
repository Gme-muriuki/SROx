use thiserror::Error;

#[derive(Debug, Error)]
pub enum ListenerError {
    #[error("failed to bind to port {0}")]
    BindError(#[from] tokio::io::Error),

    #[error("TLS setup failed: {0}")]
    TlsSetup(String),
}
