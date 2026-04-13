#![allow(unused)]
use std::{net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};

use crate::config::Config;

pub(crate) async fn run(config: Arc<Config>) -> Result<(), ListenerError> {
    let listener = TcpListener::bind(config.addr).await?;

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let config = Arc::clone(&config);
                tokio::spawn(serve_connection(stream, addr, config));
            }
            Err(er) => {
                // No panic here
                tracing::warn!(error = %er, "Accept failed");
            }
        }
    }
}

pub(crate) async fn serve_connection(
    stream: TcpStream,
    socket_addr: SocketAddr,
    config: Arc<Config>,
) {
}

#[derive(Debug, Error)]
pub(crate) enum ListenerError {
    #[error("failed to bind to port {0}")]
    BindError(#[from] tokio::io::Error),
}
