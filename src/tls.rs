#![allow(unused)]

use crate::{config::TlsConfig, tls_error::TlsError};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::{fs, path::Path, sync::Arc};
use tokio_rustls::TlsAcceptor;

pub(crate) fn build_acceptor(config: Arc<TlsConfig>) -> Result<TlsAcceptor, TlsError> {
    let cert_chain = load_cert_chain(&config.cert_path)?;
    let private_key = load_private_key(&config.private_key)?;
    let config = build_server_config(cert_chain, private_key)?;

    Ok(TlsAcceptor::from(config))
}

pub(self) fn load_cert_chain(
    path: &Path,
) -> anyhow::Result<Vec<rustls_pki_types::CertificateDer<'static>>, TlsError> {
    let pem_certs = fs::read(path)?;

    let certs = pem::parse_many(pem_certs)
        .map_err(|err| TlsError::PemParseError(err.to_string()))?
        .into_iter()
        .filter(|cert| cert.tag() == "CERTIFICATE")
        .map(|pm| CertificateDer::from(pm.into_contents()))
        .collect::<Vec<_>>();

    if certs.is_empty() {
        return Err(TlsError::NoCertificatesFound(path.to_path_buf()));
    }

    Ok(certs)
}

pub(self) fn load_private_key(
    path: &Path,
) -> anyhow::Result<rustls_pki_types::PrivateKeyDer<'static>, TlsError> {
    let pem_bytes = fs::read(path)?;

    let pem_keys = pem::parse_many(pem_bytes)
        .map_err(|err| TlsError::PemParseError(err.to_string()))?
        .into_iter()
        .find(|pkey| {
            matches!(
                pkey.tag(),
                "PRIVATE KEY" | "RSA PRIVATE KEY" | "EC PRIVATE KEY"
            )
        })
        .ok_or_else(|| TlsError::NoPrivateKeyFound(path.to_path_buf()))?;

    let key = PrivateKeyDer::try_from(pem_keys.into_contents())
        .map_err(|err| TlsError::InvalidPrivateKey(err.to_string()))?;

    Ok(key)
}

pub(self) fn build_server_config(
    certs: Vec<CertificateDer<'static>>,
    keys: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, TlsError> {
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, keys)
        .map_err(|err| TlsError::General(err.to_string()))?;

    Ok(Arc::new(config))
}
