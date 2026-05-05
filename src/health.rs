use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::{errors::health_error::HealthError, http_codec::parse_status_code, metrics};

pub struct UpstreamHealth {
    pub addr: SocketAddr,
    healthy: AtomicBool,
}

impl UpstreamHealth {
    pub fn new(addr: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            addr,
            healthy: AtomicBool::new(true), // optimistic start
        })
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Relaxed)
    }

    pub fn set_healthy(&self, health: bool) {
        self.healthy.store(health, Relaxed);
    }
}

pub async fn run_health_check(health: Arc<UpstreamHealth>, interval_secs: u64, timeout_secs: u64) {
    let interval = Duration::from_secs(interval_secs);
    let mut ticker = tokio::time::interval(interval);

    loop {
        ticker.tick().await;

        let result =
            tokio::time::timeout(Duration::from_secs(timeout_secs), check_once(&health.addr)).await;

        let was_healthy = health.is_healthy();
        let now_healthy = matches!(result, Ok(Ok(())));

        health.set_healthy(now_healthy);

        if was_healthy && !now_healthy {
            tracing::warn!(upstream = %health.addr, "upstream became unhealthy");
            metrics::UPSTREAM_HEALTHY
                .with_label_values(&[&health.addr.to_string()])
                .set(0);
        } else {
            tracing::info!(upstream = %health.addr, "upstream is healthy");
            metrics::UPSTREAM_HEALTHY
                .with_label_values(&[&health.addr.to_string()])
                .set(1);
        }
    }
}

async fn check_once(addr: &SocketAddr) -> Result<(), HealthError> {
    // Open a TCP connection
    let mut stream = TcpStream::connect(addr).await?;
    // Send: GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n
    let request = format!(
        "GET /healthz HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        addr
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await.ok();
    // Read the status line
    let mut buffer = [0u8; 1024];
    let mut response_buffer = Vec::new();
    loop {
        let n = stream.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        response_buffer.extend_from_slice(&buffer[..n]);

        // Stop once we've seen the end of the headers
        if response_buffer.windows(4).any(|hd| hd == b"\r\n\r\n") {
            break;
        }

        if response_buffer.len() > 8 * 1024 {
            break;
        }
    }
    // Return Ok if status is 2xx, Err otherwise
    let status = parse_status_code(&response_buffer);

    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(HealthError::InvalidResponse())
    }
}
