// src/crypto.rs
use rustls::crypto::{CryptoProvider, ring};

pub fn install_rustls_crypto_provider_once() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let provider = ring::default_provider();
        CryptoProvider::install_default(provider).unwrap();
    });
}
