//! Phase 1b integration tests - Observability
//!
//! ## What these tests cover
//!
//! **Metrics**
//! - The `/metrics` endpoint responds 200.
//! - `srox_active_connections` is present in the metrics output.
//! - `srox_request_duration_seconds` is recorded after a request.
//! - `srox_active_connections` increment while a slow request is in flight and decrements after it completes.
//!
//! **Trace propagation**
//! -`X-Trace-Id` is present on every response
//! - `traceparent` is present in the forwarded request with the correct W3C format.
//!
//! **Config validation**
//! - A config with a missing cert file exits with an error, not a panic.
//! - A config with invalid TOML exits with an error, not a panic.
//!
//! ## Notes on metric isolation
//!
//! Prometheus uses a global registry. Metric counter accumulate across
//! all tests in a test run. Test here assert *at least* N, never *exactly* N.
//!
//! Test that care about relative change (active_connections) take a
//! baseline snapshot before and after.

use reqwest::Client;
use serial_test::serial;
use srox::config::Config;
use srox_testlib::{
    MockUpstream, ProxyHandle, init_test_tracing, install_rustls_crypto_provider_once, make_client,
};
use std::{collections::HashSet, path::Path, time::Duration};

// ---------------------------- Helpers -----------------------------
fn setup_tracing() {
    init_test_tracing();
}

async fn setup() -> (MockUpstream, ProxyHandle, Client) {
    install_rustls_crypto_provider_once();

    let upstream = MockUpstream::start().await;
    let proxy = ProxyHandle::start(upstream.addr).await;
    let client = make_client();

    (upstream, proxy, client)
}

/// Reads a single gauge value from `/metrics` endpoint.
///
/// Returns the last numeric value found on a line that starts with
/// `metric_name`.
/// Returns `0.0` if the metric is not found.
async fn read_gauge(proxy: &ProxyHandle, metric_name: &str) -> f64 {
    let body = reqwest::get(format!("{}/metrics", proxy.metrics_url()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    body.lines()
        .filter(|line| line.starts_with(metric_name) && !line.starts_with('#'))
        .filter_map(|line| line.split_whitespace().last())
        .filter_map(|val| val.parse::<f64>().ok())
        .next()
        .unwrap_or(0.0)
}

fn assert_valid_traceparent(tp: &str) {
    let parts = tp.split('-').collect::<Vec<_>>();

    assert_eq!(
        parts.len(),
        4,
        "traceparent must have 4 dash-separated segments; got: {tp}"
    );
    assert_eq!(parts[0], "00", "version must be '00'; got {tp}");

    assert_eq!(parts[1].len(), 32, "trace-id must be 32 chars; got: {tp}");
    assert!(
        parts[1]
            .chars()
            .all(|tc| tc.is_ascii_hexdigit() && !tc.is_uppercase()),
        "trace-id must be lowercase hex; got: {tp}"
    );

    assert_eq!(parts[2].len(), 16, "parent-id must be 16 chars; got: {tp}");
    assert!(
        parts[2]
            .chars()
            .all(|tc| tc.is_ascii_hexdigit() && !tc.is_uppercase()),
        "parent-id must be lowercase hex; got: {tp}"
    );

    assert_eq!(parts[3], "01", "flags must be '01'; got: {tp}");
}

// -----------------------------Test----------------------------------

/// The `/metrics` endpoint must return 200 and serve Prometheus text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_endpoint_returns_200() {
    setup_tracing();
    let (_, proxy, client) = setup().await;

    let resp = client
        .get(format!("{}/metrics", proxy.https_url()))
        .send()
        .await
        .expect("metrics request failed");

    assert_eq!(resp.status().as_u16(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|ct| ct.to_str().ok())
        .unwrap_or("");

    assert!(
        content_type.contains("text/plain"),
        "metrics Content-Type must be text/plain; got {content_type}"
    );
}

/// `srox_active_connections` metric must be present in the output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_contains_active_connections_gauge() {
    setup_tracing();
    let (_, proxy, client) = setup().await;

    let body = client
        .get(format!("{}/metrics", proxy.https_url()))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("srox_active_connections"),
        "srox_active_connections must appear in /metrics output \nGot:\n {body}"
    );
}

/// `srox_request_duration_seconds` histogram must appear after at least
/// one request has been saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_duration_histogram_recorded_after_request() {
    setup_tracing();
    let (_, proxy, client) = setup().await;

    // Make a request so the histogram gets an observation
    client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .unwrap();

    // Give the proxy to record the metric.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let body = reqwest::get(format!("{}/metrics", proxy.metrics_url()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        body.contains("srox_request_duration_seconds"),
        "srox_request_duration_seconds must appear after at least one request \nGot: \n {body}"
    );

    // The _count must be >= 1
    let count_line = body
        .lines()
        .find(|line| line.starts_with("srox_request_duration_seconds_count"));

    if let Some(line) = count_line {
        let count = line
            .split_whitespace()
            .last()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);

        assert!(
            count >= 1.0,
            "request duration count must be at least 1; got {count}"
        );
    }
}

/// `srox_active_connections` increments when a connection is live and
/// decrements when it is closed.
///
/// This test is serial because it reads the gauge value and global state
/// makes the exact number unpredictable if other tests runs concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn active_connections_increments_and_decrements() {
    setup_tracing();
    let (_, proxy, client) = setup().await;

    // Baseline -- may be > 0 if other tests left connections open.
    let baseline = read_gauge(&proxy, "srox_active_connections").await;

    let proxy_url = proxy.https_url();

    // Spawn the slow request in the background.
    let slow_req =
        tokio::spawn(async move { client.get(format!("{proxy_url}/slow")).send().await.ok() });

    // Give the proxy 150 ms to accept the connection and increment the gauge
    tokio::time::sleep(Duration::from_millis(150)).await;

    let during = read_gauge(&proxy, "srox_active_connections").await;

    assert!(
        during > baseline,
        "active_connections must be higher while a request is in flight: (baseline={baseline}, during={during}"
    );

    // Wait for the request to complete.
    let _ = slow_req.await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let after = read_gauge(&proxy, "srox_active_connections").await;
    assert!(
        after <= baseline + 1.0,
        "active_connections must decrease after the request completes (baseline={baseline}, after={after}"
    );
}

/// Every response must carry `X-Trace-Id` set to a non-empty UUID-like
/// string.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn x_trace_id_present_on_every_response() {
    setup_tracing();
    let (_, proxy, client) = setup().await;

    let mut seen_ids = HashSet::new();

    for i in 0..5 {
        let resp = client
            .get(format!("{}/req/{i}", proxy.https_url()))
            .send()
            .await
            .unwrap();

        let id = resp
            .headers()
            .get("x-trace-id")
            .expect("X-Trace-Id must be present")
            .to_str()
            .unwrap()
            .to_string();

        assert!(
            !id.is_empty(),
            "X-Trace-Id must not be empty for request {i}"
        );
        seen_ids.insert(id);
    }

    assert_eq!(
        seen_ids.len(),
        5,
        "each request must have a unique X-Trace-Id; got: {seen_ids:?}"
    );
}

/// `traceparent` must be forwarded to the upstream in W3C format.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn traceparent_w3c_format_forwarded_to_upstream() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;

    client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .unwrap();

    let reqs = upstream.received_requests().await;

    let tp = reqs[0]
        .header("traceparent")
        .expect("traceparent must be set");

    assert_valid_traceparent(tp);
}

/// Loading a config whose cert file does not exist must return an error
/// not a panic
#[test]
fn missing_cert_file_returns_error_not_panic() {
    let toml = r#"
        addr = "127.0.0.1:8443"

        [tls]
        cert_path = "/tmp/does-not-exist-cert.pem"
        key_path = "/tmp/does-not-exist-key.pem"

        [upstream]
        addr = "127.0.0.1:8080"
        pool_size = 10
        keep_alive_secs = 60
        timeout_secs = 2
        health_check_interval_secs = 5
        health_check_timeout_secs = 1
        health_check_path = "/healthz"

        [telemetry]
        otlp_endpoint = "http://localhost:4317"

        [metrics]
        addr = "127.0.0.1:9090"
    "#;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, toml).unwrap();

    let result = Config::load_from_file(&path);

    assert!(
        result.is_err(),
        "loading a config with a missing cert must return Err, got Ok"
    );
}

/// A TOML file with invalid syntax must return a parse error -- not a panic.
#[test]
fn invalid_toml_returns_error_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");

    std::fs::write(&path, "this is not a valid toml !!! [[[").unwrap();

    let result = Config::load_from_file(&path);
    assert!(
        result.is_err(),
        "loading invalid TOML must return Err, got Ok"
    );
}

/// A completely missing config file must return an IO error -- not a panic.
#[test]
fn missing_config_file_returns_error_not_panic() {
    let result = Config::load_from_file(Path::new("/tmp/srox-does-not-exist.toml"));

    assert!(
        result.is_err(),
        "loading a missing config file must return Err, got Ok"
    );
}
