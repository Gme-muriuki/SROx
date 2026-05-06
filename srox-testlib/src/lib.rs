//! # srox-testlib
//!
//! Test infrastructure for SROx integration tests.
//!
//! ## What lives here
//!
//! - [`TestCerts`] - generates a self-signed TLS cert/key pair on disk
//!   for a single run. The files are deleted when the value is dropped.
//!
//! - [`MockUpstream`] - a plain-TCP HTTP/1.1 server that records every
//!   request it receives and responds from a configuration queue. Used
//!   as the upstream behind a proxy under test.
//!
//! - [`ProxyHandle`] - starts the SROx proxy (listener + metrics) against
//!   a given upstream, waits until it accepts connections, and shuts it
//!   down on drop.
//!
//! - [`make_client`] - builds a `reqwest::Client` that skips certificate
//!   verification (needed because we use self-signed certs in tests).
//!
//! - [`init_test_tracing`] - installs a minimal `tracing` subscriber once
//!   per process. Safe to call from every test; subsequent calls are no-ops
//!
//!

pub mod certs;
pub mod proxy;
pub mod upstream;

pub use certs::TestCerts;
pub use proxy::ProxyHandle;
pub use upstream::{MockResponse, MockUpstream, ReceivedRequest};

pub use rustls::crypto::{ring, CryptoProvider};
pub fn make_client() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .connection_verbose(true) // Disables reqwest's own connection pool so every call opens a fresh connection; this let's us observe the *proxy's* pool behaviour cleanly.
        .build()
        .expect("failed to build test HTTP client")
}

pub fn init_test_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "srox=debug,srox_testlib=debug".to_string()),
        )
        .with_test_writer() // writes to the captured stdout/stderr per test
        .try_init();
}

pub fn install_rustls_crypto_provider_once() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let provider = ring::default_provider();
        CryptoProvider::install_default(provider).unwrap();
    });
}
