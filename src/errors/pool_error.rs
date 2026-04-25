use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("upstream connection failed: {0}")]
    Connect(#[from] io::Error),
}
