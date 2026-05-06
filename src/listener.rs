use crate::errors::listener_error::ListenerError;
use crate::http_codec::{ParseStatus, parse_status_code, try_parse_headers};
use crate::metrics;
use crate::pool::ConnectionPool;
use crate::{
    config::Config, errors::codec_error::CodecError, http_codec::parse_request, tls::build_acceptor,
};
use bytes::BytesMut;
use std::time::Instant;
use std::{net::SocketAddr, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, split};
use tokio::net::{TcpListener, TcpStream};
use tracing::instrument;

pub async fn run(config: Arc<Config>, pool: Arc<ConnectionPool>) -> Result<(), ListenerError> {
    let acceptor = build_acceptor(Arc::new(config.tls.clone()))
        .map_err(|err| ListenerError::TlsSetup(err.to_string()))?;
    let acceptor = Arc::new(acceptor);

    let listener = TcpListener::bind(config.addr).await?;
    tracing::info!(addr = %config.addr, "listening");

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let acceptor = Arc::clone(&acceptor);
                let pool = Arc::clone(&pool);
                tokio::spawn(serve_connection(stream, peer_addr, acceptor, pool));
            }
            Err(err) => {
                // No panic
                tracing::error!(error = %err, "accept failed");
            }
        }
    }
}

#[instrument(level = "info", skip(acceptor, stream, pool), fields(peer = %socket_addr, trace_id = tracing::field::Empty))]
pub(crate) async fn serve_connection(
    stream: TcpStream,
    socket_addr: SocketAddr,
    acceptor: Arc<tokio_rustls::TlsAcceptor>,
    pool: Arc<ConnectionPool>,
) {
    metrics::ACTIVE_CONNECTIONS.inc();
    struct ConnectionGuard;
    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            metrics::ACTIVE_CONNECTIONS.dec();
        }
    }

    let _guard = ConnectionGuard;

    let started_at = Instant::now();

    let trace_id = uuid::Uuid::new_v4();
    let trace_id_str = trace_id.to_string();
    let trace_id_hex = format!("{:032x}", trace_id.as_u128());

    let span = tracing::Span::current();

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

    let parsed = match parse_request(&buf) {
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
                    .write_all(b"HTTP/1.1 400 HTTP Version Not Supported\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
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

    // connect to the upstream
    let mut upstream = match pool.checkout().await {
        Ok(pc) => pc,
        Err(err) => {
            tracing::error!(error = %err, "upstream connect failed");
            return;
        }
    };

    let span_id_hex = span
        .id()
        .map(|id| format!("{:016x}", id.into_u64()))
        .unwrap_or_else(|| "0000000000000000".to_string());

    let traceparent = format!("00-{}-{}-01", trace_id_hex, span_id_hex);

    let request =
        match str::from_utf8(&buf) {
            Ok(req) => req,
            Err(_err) => {
                let _ = tls_stream.write_all(
              b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ).await;
                return;
            }
        };

    let (head, body) = match request.split_once("\r\n\r\n") {
        Some((head, body)) => (head, body),
        None => {
            tracing::warn!("malformed request: missing header/body separator");
            return;
        }
    };

    // Insert traceparent into the header section.
    let forward_request = format!("{head}\r\ntraceparent:{traceparent}\r\n\r\n{body}");

    // forward to the upstream
    if let Err(err) = upstream.stream.write_all(forward_request.as_bytes()).await {
        tracing::error!(error = %err, "failed to write to the upstream");
        return;
    }

    upstream.stream.flush().await.ok();

    // split bidirectional transfer
    let (mut tls_read, mut tls_write) = split(tls_stream);
    let (up_read, up_write) = split(upstream.stream);

    let client_to_upstream = tokio::spawn(async move {
        let mut up_write = up_write;
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
        up_write
    });

    let trace_id_for_response = trace_id_str.clone();

    let upstream_to_client = tokio::spawn(async move {
        let mut up_read = up_read;
        let mut buffer = [0u8; 4096];
        let mut response_buf = Vec::new();
        let mut status_code = 0u16;

        loop {
            let n = match up_read.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    tracing::error!(error = %err, "upstream read failed");
                    break;
                }
            };

            response_buf.extend_from_slice(&buffer[..n]);

            let finder = memchr::memmem::Finder::new(b"\r\n\r\n");
            if status_code == 0
                && let Some(pos) = finder.find(&response_buf)
            {
                let head = &response_buf[..pos];
                status_code = parse_status_code(head);

                let head_str = String::from_utf8_lossy(head);
                let head_with_trace = format!(
                    "{}\r\nX-Trace-Id: {}\r\n\r\n",
                    head_str.trim_end(),
                    trace_id_for_response
                );

                if let Err(err) = tls_write.write_all(head_with_trace.as_bytes()).await {
                    tracing::error!(error = %err, "downstream write failed (header)");
                    break;
                }

                let body = &response_buf[pos + 4..];
                if let Err(err) = tls_write.write_all(body).await {
                    tracing::error!(error = %err, "downstream write failed (body)");
                    break;
                }

                // Stream remaining response body
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
                break;
            }
        }

        let _ = tls_write.shutdown().await;
        (up_read, status_code)
    });

    let (cl_to_up, up_to_cl) = tokio::join!(client_to_upstream, upstream_to_client);

    let status_code = if let Ok((_, status)) = &up_to_cl {
        *status
    } else {
        0u16
    };

    if let (Ok(up_write), Ok((up_read, _))) = (cl_to_up, up_to_cl) {
        // Reconstruct stream
        let tcp = up_read.unsplit(up_write);
        upstream.stream = tcp;
        upstream.last_used = Instant::now();

        pool.checkin(upstream).await;
    };

    let trace_id_for_log = trace_id_str.clone();
    let method_for_log = parsed.method.clone();
    let path_for_log = parsed.path.clone();
    let duration_secs = started_at.elapsed().as_secs_f64();

    metrics::REQUEST_DURATION
        .with_label_values(&[method_for_log.as_str(), &status_code.to_string(), "MISS"])
        .observe(duration_secs);

    tracing::info!(
        trace_id = %trace_id_for_log,
        method = %method_for_log,
        path = %path_for_log,
        status = status_code,
        duration_ms = started_at.elapsed().as_millis(),
        cache_status = "MISS",      // todo!() cache comes in phase 3.
        "request complete"
    );
    tracing::info!(peer = %socket_addr, "proxy transfer complete");
}
