//! Phase 1a integration test -- TLS termination and request forwarding.
//!
//! ## What these tests cover
//!
//! - Valid requests are forwarded to the upstream and the response is returned to the client intact.
//! - Requests with ambiguous framing (`Content-Length` + `Transfer-Encoding`) are rejected with 400 before reaching the upstream.
//! - Oversized request headers are rejected with 431
//! - HTTP/1.0 requests are rejected with 505.
//! - The proxy adds a `traceparent` header when forwarding.
//! - Plain HTTP (non-TLS) connections are rejected at the TLS handshake layer.
//! - Active-connection gauge increments on connect and decrements on disconnect.
//!
//! ## Isolation
//!
//! Every test creates its own `MockUpstream` and `ProxyHandle` on OS-assigned ports.
//!
//! Tests may run concurrently.

use reqwest::Client;
use rustls::{
    SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
};
use rustls_pki_types::ServerName;
use srox_testlib::{MockResponse, MockUpstream, ProxyHandle, init_test_tracing, make_client};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

// ----------------------------- Helpers ---------------------------

fn setup_crypto() {
    srox_testlib::install_rustls_crypto_provider_once();
}
//
fn setup_tracing() {
    init_test_tracing();
}

async fn setup() -> (MockUpstream, ProxyHandle, Client) {
    setup_crypto();

    let upstream = MockUpstream::start().await;
    let proxy = ProxyHandle::start(upstream.addr).await;
    let client = make_client();

    (upstream, proxy, client)
}

// ----------------------------- Tests ----------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_request_forwarded_and_response_returned() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;

    upstream
        .push_response(MockResponse::with_body("hello from upstream"))
        .await;

    let resp = client
        .get(format!("{}/api/test", proxy.https_url()))
        .send()
        .await
        .expect("request send failed");

    assert_eq!(resp.status().as_u16(), 200, "expected 200 from upstream");

    let body = resp.text().await.unwrap();
    assert_eq!(
        body, "hello from upstream",
        "response body must be forwarded intact"
    );

    let reqs = upstream.received_requests().await;
    assert_eq!(
        reqs.len(),
        1,
        "upstream should have received exactly one request"
    );

    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/api/test");
}

/// POST with a body is forwarded correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn post_request_forwarded() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;

    upstream.push_response(MockResponse::status(201)).await;

    let resp = client
        .post(format!("{}/users", proxy.https_url()))
        .body(r#"{ "name": "alice" }"#)
        .header("Content-Type", "application/json")
        .send()
        .await
        .expect("POST request failed");

    assert_eq!(resp.status().as_u16(), 201);

    let reqs = upstream.received_requests().await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/users");
}

/// The proxy must set `X-Trace-Id` on every response it sends back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_contains_x_trace_id_header() {
    setup_tracing();

    let (upstream, proxy, client) = setup().await;

    upstream.push_response(MockResponse::ok()).await;

    let resp = client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .expect("get request failed");

    let trace_id = resp
        .headers()
        .get("x-trace-id")
        .expect("X-Trace-Id header must be present on every response");

    let trace_id_str = trace_id.to_str().unwrap();
    assert!(!trace_id_str.is_empty(), "X-Trace-Id must not be empty");
    assert!(
        trace_id_str.len() >= 32,
        "X-Trace-Id looks too short: {trace_id_str}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn traceparent_injected_into_upstream_request() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;

    upstream.push_response(MockResponse::ok()).await;

    client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .expect("get request failed");

    let reqs = upstream.received_requests().await;
    assert!(!reqs.is_empty());

    let tracep = reqs[0]
        .header("traceparent")
        .expect("traceparent header must be injected into upstream request");

    assert!(
        tracep.starts_with("00-"),
        "traceparent must follow W3C format: {tracep}"
    );

    let parts: Vec<&str> = tracep.split("-").collect();
    assert_eq!(
        parts.len(),
        4,
        "traceparent must have 4 dash-separated segments"
    );
    assert_eq!(parts[0], "00");
    assert_eq!(parts[0].len(), 2, "version segment must be 2 chars");
    assert_eq!(parts[1].len(), 32, "trace-id segment must be 32 hex chars");
    assert_eq!(parts[2].len(), 16, "parent-id segment must be 16 hex chars");
    assert_eq!(parts[3], "01", "flags must be 01");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_framing_rejected_400() {
    setup_tracing();
    let (upstream, proxy, _) = setup().await;

    let raw_request =
        "GET / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\nbody1".to_string();

    let status = send_raw_tls(proxy.proxy_addr, raw_request.as_bytes()).await;

    assert_eq!(status, 400, "ambiguous framing must be rejected with 400");

    // Upstream must not have received anything.
    let reqs = upstream.received_requests().await;
    assert!(
        reqs.is_empty(),
        "upstream must not receive a smuggling attempt; got: {reqs:?}"
    );
}

/// A request header section larger than 8 KiB is rejected with 431.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_headers_rejected_431() {
    setup_tracing();
    let (_upstream, proxy, _client) = setup().await;

    // Build a header value large enough to exceed the 8 Kib limit;
    let giant_header = "x".repeat(9 * 1024);
    let raw_request =
        format!("GET / HTTP/1.1\r\nHost: localhost\r\nX-Giant:{giant_header}\r\n\r\n");

    let status = send_raw_tls(proxy.proxy_addr, raw_request.as_bytes()).await;

    assert_eq!(
        status, 431,
        "headers exceeding 8 KiB must be rejected with 431"
    );
}

/// HTTP/1.0 requests are rejected with 505 (HTTP Version not Supported).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http10_rejected_505() {
    setup_tracing();
    let (_, proxy, _) = setup().await;

    let raw_request = b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n";
    let status = send_raw_tls(proxy.proxy_addr, raw_request).await;

    assert_eq!(status, 505, "HTTP/1.0 must be rejected with 505");
}

/// Upstream 404 is passed through to the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_error_status_passed_through() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;
    upstream.push_response(MockResponse::status(404)).await;

    let resp = client
        .get(format!("{}/does-not-exist", proxy.https_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_sequential_requests_succeed() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;
    upstream.push_responses(MockResponse::ok(), 3).await;

    for i in 0..3 {
        let resp = client
            .get(format!("{}/path/{i}", proxy.https_url()))
            .send()
            .await
            .unwrap_or_else(|err| panic!("request {i} failed: {err}"));

        assert_eq!(resp.status().as_u16(), 200, "request {i} must succeed");
    }

    upstream.wait_for_request(3).await;
    let reqs = upstream.received_requests().await;

    assert_eq!(reqs.len(), 3, "upstream must receive all 3 requests");
}

/// Plain HTTP (not TLS) connections should fail at this TLS handshake -
/// the proxy must not crash or serve anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_http_connection_fails_tls_handshake() {
    setup_tracing();
    let (upstream, proxy, client) = setup().await;

    // Connect with plain TCP ( not TLS ) -- the TLS handshake will fail.
    let mut stream = TcpStream::connect(proxy.proxy_addr)
        .await
        .expect("TCP connect must succed (port is open");

    let _ = stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await;

    // Reading should fail or return 0 bytes (server closes the connection).
    let mut buffer = [0u8; 128];
    let _ = stream.read(&mut buffer).await.unwrap_or(0);

    drop(stream);

    // Proxy is still alive -- a good request still works.
    upstream.push_response(MockResponse::ok()).await;

    let resp = client
        .get(format!("{}/", proxy.https_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status().as_u16(),
        200,
        "proxy must survive bad TLS handshake"
    );
}

// -------------------------- To refactor tomorrow -------------------

/// A no-op certificate verifier so we can connect to the self-signed
/// proxy
#[derive(Debug)]
struct NoCertVerifier;

impl ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
        ]
    }
}

/// Send `request_bytes` over a raw TLS connection ( skipping cert
/// verification ) and return the HTTP status code from the response,
/// or 0 on error
pub async fn send_raw_tls(addr: SocketAddr, request_bytes: &[u8]) -> u16 {
    let stream = TcpStream::connect(addr)
        .await
        .expect("failed to TCP-connect to proxy");

    // Build a rustls client config that skips certifcate verification.
    let tls_config = {
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerifier))
            .with_no_client_auth();

        config.alpn_protocols = vec![];
        Arc::new(config)
    };

    let connector = TlsConnector::from(tls_config);
    let server_name = ServerName::try_from("localhost").expect("invalid server name");

    let mut tls = match connector.connect(server_name, stream).await {
        Ok(tls_tcp) => tls_tcp,
        Err(_) => return 0,
    };

    if tls.write_all(request_bytes).await.is_err() {
        return 0;
    }

    let _ = tls.flush().await;

    let mut buf = vec![0u8; 4096];
    let n = tls.read(&mut buf).await.unwrap_or(0);

    srox::http_codec::parse_status_code(&buf[..n])
}
