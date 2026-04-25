use std::sync::Arc;

use once_cell::sync::Lazy;
use prometheus::{Encoder, HistogramOpts, HistogramVec, IntGauge, Opts, Registry, TextEncoder};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::config::Config;

static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

pub static REQUEST_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let opts = HistogramOpts::new(
        "srox_request_duration_seconds",
        "Request duration in seconds",
    )
    .buckets(vec![
        0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
    ]);

    let hv = HistogramVec::new(opts, &["method", "status_code", "cache_status"])
        .expect("create srox_request_duration_seconds");

    REGISTRY
        .register(Box::new(hv.clone()))
        .expect("register histogram");

    hv
});

pub static ACTIVE_CONNECTIONS: Lazy<IntGauge> = Lazy::new(|| {
    let opts = Opts::new(
        "srox_active_connections",
        "Number of active client connections",
    );

    let gauge = IntGauge::with_opts(opts).expect("create srox_active_connections");

    REGISTRY
        .register(Box::new(gauge.clone()))
        .expect("register gauge");

    gauge
});

pub async fn serve_metrics(config: Arc<Config>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(config.metrics.addr).await?;
    tracing::info!(addr = %config.metrics.addr, "metrics listening");
    loop {
        let (mut stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(error = %err, "metrics accept failed");
                continue;
            }
        };
        tracing::debug!(%peer, "metrics connection");

        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer).await;

        let metric_fam = REGISTRY.gather();
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        encoder.encode(&metric_fam, &mut buffer).unwrap();

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            buffer.len()
        );

        if let Err(err) = stream.write_all(&response.as_bytes()).await {
            tracing::error!(error = %err, "failed to write metrics header");
            let _ = stream.shutdown().await;
            continue;
        }

        if let Err(err) = stream.write_all(&buffer).await {
            tracing::error!(error = %err, "failed to write body");
        }

        let _ = stream.shutdown().await;
    }
}
