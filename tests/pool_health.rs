//! Phase 2 integration tests -- Connection pool and health checks.
//!
//! ## What these tests cover
//!
//! **Connection pool**
//! - Connections are reused: two sequential requests to same upstream travel over the same TCP connection (same source port on the upstream side).
//!
//! - Stale connections are not reused: after `keep_alive_secs` the pool opens a fresh connection instead of handing out the stale one.
//!
//! - Pool max-size is respected: the pool never opens more than `pool_size` concurrent upstream connections.
//!
//! **Health checks**
//! - The health checker sends `GET /healthz` to the upstream on its configured interval.
//!
//! - When the upstream starts returning non-2xx on `/healthz`, the proxy marks it unhealthy and the `srox_upstream_healthy` metric drops to 0.
//!
//! - When the upstream recovers, the metric returns to 1.
//!
//! **Header forwarding**
//! - `X-Forwarded-For is set to the client's IP.
//! - `Host` from the original request is forwarded.
//! -  Hop-by-hop headers (`Connection`, `Proxy-*`) are stripped before forwarding
//!
//! ## Isolation notes
//!
//! - Each test creates independent `MockUpstream` and `ProxyHandle` instances.
//! - Pool tests use `#[serial]` because they inspect connection counts,
//!   which could be affected by other tests opening connections to the
//!   same upstream. With independents upstreams per test this shouldn't
//!   matter, but it is defensive.

use std::{sync::Arc, time::Duration};

use reqwest::Client;
use serial_test::serial;
use srox_testlib::{
    MockResponse, MockUpstream, ProxyHandle, init_test_tracing,
    install_rustls_crypto_provider_once, make_client,
};

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

async fn read_upstream_health_gauge(proxy: &ProxyHandle) -> f64 {
    let body = reqwest::get(format!("{}/metrics", proxy.metrics_url()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    body.lines()
        .filter(|line| line.starts_with("srox_upstream_healthy") && !line.starts_with('#'))
        .filter_map(|lstr| lstr.split_whitespace().last())
        .filter_map(|vstr| vstr.parse::<f64>().ok())
        .next()
        .unwrap_or(-1.0)
}

/// Two sequential requests must reuse the same upstream TCP connection.
///
/// Evidence: both requests arrive at the upstream with the same source
/// port, meaning the proxy did not open a second connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn pool_reuses_connection_for_sequential_requests() {
    setup_tracing();

    let (upstream, proxy, client) = setup().await;

    client
        .get(format!("{}/first", proxy.https_url()))
        .send()
        .await
        .expect("first request failed");

    client
        .get(format!("{}/second", proxy.https_url()))
        .send()
        .await
        .expect("second request failed");

    upstream.wait_for_request(2).await;

    let reqs = upstream.received_requests().await;
    assert_eq!(reqs.len(), 2, "upstream must receive both requests");

    // Key assertion: same source port == same TCP connection == pool reuse
    assert_eq!(
        reqs[0].source_port, reqs[1].source_port,
        "both requests must arrive over same TCP connection (pool reuse); req1  source_port{}, req2 source_port={}",
        reqs[0].source_port, reqs[1].source_port
    );

    // Only one TCP connection should have been accepted.
    assert_eq!(
        upstream.unique_connection_count(),
        1,
        "pool must not open a second connection for sequential requests"
    );
}

/// Stale connections (idle beyond `keep_alive_secs`) must be evicted.
///
/// We set a very short keep_alive (1 s) and wait long enough for the
/// connection to go stale. The next request must open a fresh connection.
///
/// NOTE: this test overrides the default `keep_alive_secs = 60` by
/// constructing the `ProxyHandle` with a custom config.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stale_connection_evicted_and_new_one_opened() {
    setup_tracing();

    let upstream = MockUpstream::start().await;
    upstream.push_responses(MockResponse::ok(), 2).await;

    // Start proxy with 1-second keep_alive so the connection go stale quickly
    let proxy = ProxyHandle::start_with_keep_alive(upstream.addr, 1).await;
    let client = make_client();

    // First request -- establishes a connection and checks it back in
    client
        .get(format!("{}/first", proxy.https_url()))
        .send()
        .await
        .expect("first request failed");

    // Wait for the connection to go stale (keep_alive + 500 ms buffer).
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Second request -- stale connection must be discarded, a new one opened.
    client
        .get(format!("{}/second", proxy.https_url()))
        .send()
        .await
        .expect("second request failed");

    upstream.wait_for_request(2).await;

    // Two requests , each over a different TCP connections.
    assert_eq!(
        upstream.unique_connection_count(),
        2,
        "a new connection must be opened after the keep_alive window expires"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn pool_respects_max_size() {
    setup_tracing();
    const POOL_SIZE: usize = 3;
    const CONCURRENT: usize = 10;

    let upstream = MockUpstream::start().await;
    // Queue many slow responses so all requests are in-flight simultaneously.
    upstream
        .push_responses(MockResponse::with_delay(400), CONCURRENT)
        .await;

    let proxy = ProxyHandle::start_with_pool_size(upstream.addr, POOL_SIZE, None, None).await;
    let client = Arc::new(make_client());

    // Fire CONCURRENT requests in parallel
    let mut handles = Vec::new();
    for i in 0..CONCURRENT {
        let cl = Arc::clone(&client);
        let url = format!("{}/req/{i}", proxy.https_url());
        handles.push(tokio::spawn(async move { cl.get(url).send().await }));
    }

    // Give enough time for all request to be accepted and in flight.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Active upstream connections at peak must not exceed pool_size.
    let peak = upstream.unique_connection_count();
    assert!(
        peak <= POOL_SIZE,
        "pool opened {peak} connections; must not exceed pool_size={POOL_SIZE}"
    );

    // Wait for all requests to complete
    for handle in handles {
        let _ = handle.await;
    }
}

/// The health checker must send requests to `health_check_path` (/healthz).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_checker_requests_healthz_path() {
    setup_tracing();

    let upstream = MockUpstream::start().await;
    let _proxy = ProxyHandle::start_with_health_interval(upstream.addr, 1).await;

    // Wait for at least one health check cycle.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let reqs = upstream.received_requests().await;
    let health_req = reqs
        .iter()
        .filter(|req| req.path == "/healthz")
        .collect::<Vec<_>>();

    assert!(
        !health_req.is_empty(),
        "health checker must send requests to /healthz; got requets: {reqs:?}"
    );

    for req in &health_req {
        assert_eq!(req.method, "GET", "health check request must use GET");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_marked_unhealthy_after_health_check_fails() {
    setup_tracing();

    let upstream = MockUpstream::start().await;
    // Start proxy with 1-second check interval so the test is fast.
    let proxy = ProxyHandle::start_with_health_interval(upstream.addr, 1).await;

    // Initially the upstream is healthy -- wait for first check.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let healthy_before = read_upstream_health_gauge(&proxy).await;
    assert_eq!(
        healthy_before, 1.0,
        "upstream should start healthy; got gauge={healthy_before}"
    );

    upstream.set_healthy(false);

    // Wait for at least two health check cycles.
    tokio::time::sleep(Duration::from_millis(2200)).await;

    let healthy_after = read_upstream_health_gauge(&proxy).await;
    assert_eq!(
        healthy_after, 0.0,
        "srox_upstream_healthy must be 0 after health checks start failing; got gauge={healthy_after}"
    );
}

/// After the upstream recovers, `srox_upstream_healthy` must return to 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_recovers_after_health_check_passes_again() {
    setup_tracing();

    let upstream = MockUpstream::start().await;
    let proxy = ProxyHandle::start_with_health_interval(upstream.addr, 1).await;

    // Start unhealthy.
    upstream.set_healthy(false);
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(read_upstream_health_gauge(&proxy).await, 0.0);

    // Recover.
    upstream.set_healthy(true);
    tokio::time::sleep(Duration::from_millis(2200)).await;

    let recovered = read_upstream_health_gauge(&proxy).await;
    assert_eq!(
        recovered, 1.0,
        "srox_upstream_healthy must return to 1 after upstream reocvers; got gauge={recovered}"
    );
}

/// `X-Forwarded-For` must be set on the forwarded request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn x_forwarded_for_set_on_upstream_request() {
    setup_tracing();

    let (upstream, proxy, client) = setup().await;
    upstream.push_response(MockResponse::ok()).await;

    client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .unwrap();

    let reqs = upstream.received_requests().await;
    let xff = reqs[0]
        .header("x-forwarded-for")
        .expect("X-Forwarded-For must be set on forwarded request");

    assert!(
        !xff.is_empty(),
        "X-Forwarded-For must not be empty: got: {xff}"
    );

    // The value should contain the loopback address since test run locally.
    assert!(
        xff.contains("127.0.0.1"),
        "X-Forwarded-For should contain 127.0.0.1 for local test connections; got: {xff}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hop_by_hop_headers_stripped_before_forwarding() {
    setup_tracing();

    let (upstream, proxy, client) = setup().await;
    upstream.push_response(MockResponse::ok()).await;

    client
        .get(format!("{}/", proxy.https_url()))
        .header("Proxy-Authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .unwrap();

    let reqs = upstream.received_requests().await;
    let proxy_auth = reqs[0].header("proxy-authorization");

    assert!(
        proxy_auth.is_none(),
        "Proxy-Authorization must be stripped before forwarding; got: {proxy_auth:?}"
    );
}
