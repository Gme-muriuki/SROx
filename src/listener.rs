use crate::errors::listener_error::ListenerError;
use crate::http_codec::{ParseStatus, try_parse_headers};
use crate::{
    config::Config, errors::codec_error::CodecError, http_codec::parse_request, tls::build_acceptor,
};
use bytes::BytesMut;
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, split};
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
    stream: TcpStream,
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
    let mut buf = BytesMut::with_capacity(4096);

    loop {
        let _ = match tls_stream.read_buf(&mut buf).await {
            Ok(0) => {
                // Connection closed before we got a complete request
                tracing::warn!(peer = %socket_addr, "connection closed before headers complete");
                return;
            }
            Ok(n) => n,
            Err(err) => {
                tracing::error!(error = %err, "read failed");
                return;
            }
        };

        if buf.len() > 8 * 1024 {
            let _ = tls_stream
                .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            return;
        }

        // Try to parse what we have so far
        match try_parse_headers(&buf) {
            ParseStatus::Complete => break,
            ParseStatus::Partial => continue, // need more bytes — read again
            ParseStatus::Invalid => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
        }
    }

    let _parsed = match parse_request(&buf) {
        Ok(parsed) => match parsed {
            Some(parsed) => {
                tracing::info!("parsed request: {:?}", parsed);
                parsed
            }
            None => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
        },
        Err(codec_err) => match codec_err {
            CodecError::AmbiguousFraming => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 505 HTTP Version Not Supported\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
            CodecError::RequestTooLarge => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
            CodecError::InvalidHttpVersion(_) => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 505 HTTP Version Not Supported\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
            _ => {
                let _ = tls_stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            }
        },
    };

    // forward to the upstream
    let mut upstream = match TcpStream::connect(config.upstream.addr).await {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "upstream connect failed");
            return;
        }
    };

    if let Err(err) = upstream.write_all(&buf).await {
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
    tracing::info!(peer = %socket_addr, "proxy transfer complete")
}
