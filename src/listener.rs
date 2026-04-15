#![allow(unused)]
use crate::errors::listener_error::ListenerError;
use crate::http_codec::parse_request;
use crate::{config::Config, tls::build_acceptor};
use anyhow::Context;
use bytes::BytesMut;
use std::{net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional, split};
use tokio::net::{TcpListener, TcpStream};

pub async fn run(config: Arc<Config>) -> Result<(), ListenerError> {
    let acceptor = build_acceptor(Arc::new(config.tls.clone()))
        .map_err(|err| ListenerError::TlsSetup(err.to_string()))?;
    let acceptor = Arc::new(acceptor);

    let listener = TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, "listening");

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let acceptor = Arc::clone(&acceptor);
                let config = Arc::clone(&config);
                tokio::spawn(serve_connection(stream, peer_addr, config, acceptor));
            }
            Err(err) => {
                // No panic
                tracing::error!(error = %err, "accept failed");
            }
        }
    }
}

pub(crate) async fn serve_connection(
    mut stream: TcpStream,
    socket_addr: SocketAddr,
    config: Arc<Config>,
    acceptor: Arc<tokio_rustls::TlsAcceptor>,
) {
    let mut tls_stream = match acceptor.accept(stream).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, peer = %socket_addr, "TLs handshake failed");
            return;
        }
    };

    // Parse request and forward it to the upstream
    let mut buf = BytesMut::new();

    if let Err(err) = tls_stream.read_buf(&mut buf).await {
        tracing::error!(error = %err, "read failed");
        return;
    };

    let parsed = match parse_request(&buf) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::error!(error = %err, "failed to parse request");
            return;
        }
    };

    // forward to the upstream

    let mut upstream = match TcpStream::connect(config.upstream.addr).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "upstream connect failed");
            return;
        }
    };

    if let Err(err) = upstream.write_all(&parsed.body).await {
        tracing::error!(error = %err, "failed to write to the upstream");
        return;
    }

    upstream.flush().await.ok();

    // split bidirectional transfer
    let (mut tls_read, mut tls_write) = split(tls_stream);
    let (mut up_read, mut up_write) = split(upstream);

    let client_to_upstream = tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            let n = match tls_read.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    tracing::error!(error = %err, " tls_to_up read failed");
                    break;
                }
            };

            if let Err(err) = up_write.write_all(&buffer[..n]).await {
                tracing::error!(error = %err, "upstream write failed");
                break;
            }
        }
        let _ = up_write.shutdown().await;
    });

    let upstream_to_client = tokio::spawn(async move {
        let mut buffer = [0u8; 4096];

        loop {
            let n = match up_read.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    tracing::error!(error = %err, "upstream read failed");
                    break;
                }
            };

            if let Err(err) = tls_write.write_all(&buffer[..n]).await {
                tracing::error!(error = %err, "downstream write failed");
                break;
            }
        }

        let _ = tls_write.shutdown().await;
    });

    let _ = tokio::join!(client_to_upstream, upstream_to_client);
    tracing::info!(peer = %socket_addr,"proxy transfer complete")
}
