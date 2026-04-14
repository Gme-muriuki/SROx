use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum TlsError {
    #[error("Io error {0}")]
    Io(#[from] std::io::Error),

    #[error("rustls handshake: {0:?}")]
    Handshake(RustLsHandshakeError),

    #[error("pem perse error {0}")]
    PemParseError(String),

    #[error("no private key found {0}")]
    NoPrivateKeyFound(PathBuf),

    #[error("no certificates found: {0}")]
    NoCertificatesFound(PathBuf),

    #[error("invalid cert chain")]
    InvalidCertChain,

    #[error("invalid private key {0}")]
    InvalidPrivateKey(String),

    #[error("no peer name matched")]
    UnsupportedPeerName,

    #[error("client auth required but none presented")]
    NoClientCert,

    #[error("peer incompatible: {0:?}")]
    PeerIncompatible(rustls::PeerIncompatible),

    #[error("peer misbehaved")]
    PeerMisbehaved(rustls::PeerMisbehaved),

    #[error("alert received")]
    AlertReceived(rustls::AlertDescription),

    #[error("invalid certificates")]
    InvalidCertificates(rustls::CertificateError),

    #[error("oversized records")]
    PeerSentOversizedRecords,

    #[error("no application protocol negotiated")]
    NoApplicationProtocol,

    #[error("we couldn't decrypt the message")]
    DecryptError,

    #[error("We couldn’t encrypt a message because it was larger than the allowed message size.")]
    EncryptError,

    #[error("A provided certificate revocation list (CRL) was invalid. {0:?}")]
    InvalidCertRevocationList(rustls::CertRevocationListError),

    #[error("The max_fragment_size value supplied in configuration was too small, or too large")]
    BadMaxFragmentSize,

    #[error("failed to get current time")]
    FailedToGetCurrentTime,

    #[error("failed to acquire random bytes from the system")]
    FailedToGetRandomBytes,

    #[error("general Tls error {0}")]
    General(String),
}

#[derive(Debug, Error)]
pub(crate) enum RustLsHandshakeError {
    #[error("inappropriate message: expected {expected_types:?}, got {got_type:?}")]
    InappropriateMessage {
        expected_types: Vec<rustls::ContentType>,
        got_type: rustls::ContentType,
    },

    #[error("inappropriate handshake message: expected {expected_types:?}, got {got_types:?}")]
    InappropriateHandshakeMessage {
        expected_types: Vec<rustls::HandshakeType>,
        got_types: rustls::HandshakeType,
    },

    #[error("invalid message {0:?}")]
    InvalidMessage(rustls::InvalidMessage),

    #[error("invalid client hello")]
    InvalidEncryptedClientHello(rustls::EncryptedClientHelloError),

    #[error("inconsistent keys")]
    InconsistentKeys(rustls::InconsistentKeys),

    #[error("handshake not complete")]
    HandshakeNotComplete,

    #[error("general handshake error {0:?}")]
    Others(rustls::OtherError),
}

impl From<rustls::Error> for TlsError {
    fn from(err: rustls::Error) -> Self {
        match err {
            rustls::Error::InappropriateMessage {
                expect_types,
                got_type,
            } => Self::Handshake(RustLsHandshakeError::InappropriateMessage {
                expected_types: expect_types,
                got_type,
            }),
            rustls::Error::InappropriateHandshakeMessage {
                expect_types,
                got_type,
            } => Self::Handshake(RustLsHandshakeError::InappropriateHandshakeMessage {
                expected_types: expect_types,
                got_types: got_type,
            }),
            rustls::Error::InvalidEncryptedClientHello(encrypted_client_hello_error) => {
                Self::Handshake(RustLsHandshakeError::InvalidEncryptedClientHello(
                    encrypted_client_hello_error,
                ))
            }
            rustls::Error::InvalidMessage(invalid_message) => {
                Self::Handshake(RustLsHandshakeError::InvalidMessage(invalid_message))
            }
            rustls::Error::NoCertificatesPresented => Self::NoClientCert,
            rustls::Error::UnsupportedNameType => Self::UnsupportedPeerName,
            rustls::Error::DecryptError => Self::DecryptError,
            rustls::Error::EncryptError => Self::EncryptError,
            rustls::Error::PeerIncompatible(peer_incompatible) => {
                Self::PeerIncompatible(peer_incompatible)
            }
            rustls::Error::PeerMisbehaved(peer_misbehaved) => Self::PeerMisbehaved(peer_misbehaved),
            rustls::Error::AlertReceived(alert_description) => {
                Self::AlertReceived(alert_description)
            }
            rustls::Error::InvalidCertificate(certificate_error) => {
                Self::InvalidCertificates(certificate_error)
            }
            rustls::Error::InvalidCertRevocationList(cert_revocation_list_error) => {
                Self::InvalidCertRevocationList(cert_revocation_list_error)
            }
            rustls::Error::General(err) => Self::General(err.to_string()),
            rustls::Error::FailedToGetCurrentTime => Self::FailedToGetCurrentTime,
            rustls::Error::FailedToGetRandomBytes => Self::FailedToGetRandomBytes,
            rustls::Error::HandshakeNotComplete => {
                Self::Handshake(RustLsHandshakeError::HandshakeNotComplete)
            }
            rustls::Error::PeerSentOversizedRecord => Self::PeerSentOversizedRecords,
            rustls::Error::NoApplicationProtocol => Self::NoApplicationProtocol,
            rustls::Error::BadMaxFragmentSize => Self::BadMaxFragmentSize,
            rustls::Error::InconsistentKeys(inconsistent_keys) => {
                Self::Handshake(RustLsHandshakeError::InconsistentKeys(inconsistent_keys))
            }
            rustls::Error::Other(other_error) => {
                Self::Handshake(RustLsHandshakeError::Others(other_error))
            }
            _ => todo!(),
        }
    }
}
