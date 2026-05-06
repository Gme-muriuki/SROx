use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HealthError {
    #[error("connect error: {0}")]
    Connect(#[from] io::Error),

    #[error("invalid response")]
    InvalidResponse(),
}
