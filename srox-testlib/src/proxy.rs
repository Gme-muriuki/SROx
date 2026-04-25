//! `ProxyHandle` -- starts a live SROx proxy instance for integration tests.
//!
//! ## Usage
//!
//! ```no_run
//! # use srox_testlib::{MockUpstream, ProxyHandle, make_client};
//! # #[tokio::main]
//! # async fn main() {
//!   let upstream = MockUpstream::start().await;
//!   let proxy = ProxyHandle::start(upstream.addr).await;
//!
//!   let client = make_client();
//!   let resp = client.get(format!("http://{}/", proxy.proxy_addr))
//!       .send().await.unwrap();
//!
//!   assert_eq!(resp.status(), 200);
//! }
//! ```
//!
//! ## Shutdown
//!
//! `ProxyHandle` implements `Drop`: when it goes out of scope the
//! background task receives a shutdown signal and the tokio tasks
//! complete on their own.
//! Cert files are also deleted (via `TestCerts`'s `TempDir`).
//!
//! ## Port allocation
//!
//! I bind `:0`, note the assigned port, immediately close the listener
//! and pass the port to the proxy config. There is small TOCTOU window.
//! In practice this never matters in a test environment. If it becomes a
//! problem, refactor `listener::run` to accept a pre-bound `TcpListener`.
//!

use std::{net::SocketAddr, panic, sync::Arc, time::Duration};

use srox::{
    config::{Config, MetricsConfig, TelemetryConfig, TlsConfig, UpstreamConfig},
    pool::ConnectionPool,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{Instant, sleep},
};

use crate::TestCerts;

/// A running SROx proxy (listener + metrics server) tied to one test.
pub struct ProxyHandle {
    /// HTTPS address clients should connect to.
    pub proxy_addr: SocketAddr,
    /// Plain HTTP address for the `/metrics` endpoint.
    pub metrics_addr: SocketAddr,
    /// The TLS certs used by this proxy instance.
    pub certs: TestCerts,
    // Sending on this channel (or dropping it) shuts the background
    // task down.
    shutdown: Option<oneshot::Sender<()>>,
}

impl ProxyHandle {
    /// Start a proxy instance forwarding to `upstream_addr`.
    ///
    /// Blocks until the proxy is accepting TCP connections.
    ///
    /// # Panics
    ///
    /// Panics if OS port allocation fails or the proxy does not
    /// become ready within 5 seconds.
    pub async fn start(upstream_addr: SocketAddr) -> Self {
        Self::start_with_pool_size(upstream_addr, 10).await
    }

    /// Like [`start`] but with custom pool size (useful for pool-limit)
    /// tests.
    pub async fn start_with_pool_size(upstream_addr: SocketAddr, pool_size: usize) -> Self {
        let certs = TestCerts::generate();

        let proxy_addr = free_addr().await;
        let metrics_addr = free_addr().await;

        let config = Arc::new(Config {
            addr: proxy_addr,
            tls: TlsConfig {
                cert_path: certs.cert_path.clone(),
                private_key: certs.key_path.clone(),
            },
            upstream: UpstreamConfig {
                addr: upstream_addr,
                pool_size,
                keep_alive_secs: 60,
                timeout_secs: 2,
                health_check_interval_secs: 5,
                health_check_timeout_secs: 1,
                health_check_path: "/healthz".to_string(),
            },
            telemetry: TelemetryConfig {
                // Point at a non-existent endpoint; test do not need real traces.
                otlp_endpoint: "http://127.0.0.1:4317".to_string(),
            },
            metrics: MetricsConfig { addr: metrics_addr },
        });

        let pool = Arc::new(ConnectionPool::new(
            upstream_addr,
            pool_size,
            config.upstream.keep_alive_secs,
        ));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        {
            let config_l = Arc::clone(&config);
            let config_m = Arc::clone(&config);
            let pool_l = Arc::clone(&pool);

            tokio::spawn(async move {
                tokio::select! {
                  res = srox::listener::run(config_l, pool_l) => {
                    if let Err(err) = res {
                      tracing::error!(error = %err, "proxy listener exited with error");
                    }
                  }

                  res = srox::metrics::serve_metrics(config_m) => {
                    if let Err(err) = res {
                      tracing::error!(error = %err, "metrics server exited with error");
                    }
                  }
                  _ = shutdown_rx => {
                    tracing::debug!("ProxyHandle shutdown signal received");
                  }
                }
            });
        }

        // Wait for the TLS port to accept TCP connections.
        wait_for_tcp(proxy_addr).await;

        Self {
            proxy_addr,
            metrics_addr,
            certs,
            shutdown: Some(shutdown_tx),
        }
    }

    /// HTTPS base URL, e.g `"http://127.0.0.1:59312"`
    pub fn https_url(&self) -> String {
        format!("https://{}", self.proxy_addr)
    }

    /// Metrics base URL, e.g `"http://127.0.0.1:59313"`
    pub fn metrics_url(&self) -> String {
        format!("http://{}", self.metrics_addr)
    }
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            // Ignore send errors -- the task may already have exited.
            let _ = tx.send(());
        }
    }
}

// ---------------------- Helpers --------------------------

/// Bind to port 0, record the assigned port, then close the listener.
///
///
async fn free_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind :0 for port allocation");

    listener.local_addr().expect("failed to read local addr")
}

/// Pool `proxy_addr` until a TCP connection is accepted, or panic after 5
/// seconds.
async fn wait_for_tcp(proxy_addr: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if TcpStream::connect(proxy_addr).await.is_ok() {
            return;
        }

        if Instant::now() > deadline {
            panic!("proxy at {proxy_addr} did not become ready within 5 seconds");
        }

        sleep(Duration::from_millis(25)).await;
    }
}
