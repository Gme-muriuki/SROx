//! Self-signed TLS certificate generation for tests
//!
//! Uses `rcgen` to create a minimal cert/key pair, writes them to a
//! tempfile directory, and deletes when teh [`TestCerts`] value is dropped.
//!
//! The certs is valid for the SAN `localhost` and the loopback address
//! `127.0.0.1`, which covers every proxy address used in integration tests.
//!
//!

use rcgen::{CertifiedKey, generate_simple_self_signed};
use std::{fs, path::PathBuf};
use tempfile::TempDir;

/// A temporary on-disk TLS cert + private key.
pub struct TestCerts {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    // Keeps the temp directory alive. Dropping this deletes the files.
    _tempdir: TempDir,
}

impl TestCerts {
    /// Generate a fresh self-signed cert/key pair and write them to a temp dir.
    pub fn generate() -> Self {
        let dir = tempfile::tempdir().expect("failed to create temp dir for the test certs");

        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];

        let CertifiedKey { cert, key_pair } = generate_simple_self_signed(subject_alt_names)
            .expect("rcgen failed to generate self-signed certs");

        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        fs::write(&cert_path, cert.pem()).expect("failed to write test cert to disk");

        fs::write(&key_path, key_pair.serialize_pem()).expect("failed to write test key to disk");

        Self {
            cert_path,
            key_path,
            _tempdir: dir,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn cert_and_key_file_exists() {
        let certs = TestCerts::generate();

        assert!(
            certs.cert_path.exists(),
            "cert file must exist after generate()"
        );
        assert!(
            certs.key_path.exists(),
            "key file must exist after generate()"
        );
    }

    #[test]
    fn cert_file_contains_pem_header() {
        let certs = TestCerts::generate();
        let content = fs::read_to_string(&certs.cert_path).unwrap();

        assert!(
            content.contains("-----BEGIN CERTIFICATE-----"),
            "cert file must be a valid PEM"
        );
    }

    #[test]
    fn key_file_contains_pem_header() {
        let certs = TestCerts::generate();
        let content = fs::read_to_string(&certs.key_path).unwrap();

        assert!(
            content.contains("-----BEGIN PRIVATE KEY-----")
                || content.contains("-----BEGIN EC PRIVATE KEY-----")
                || content.contains("-----BEGIN RSA PRIVATE KEY------"),
            "key file must be a valid PEM"
        );
    }

    #[test]
    fn files_deleted_on_drop() {
        let cert_path;
        let key_path;
        {
            let certs = TestCerts::generate();
            cert_path = certs.cert_path.clone();
            key_path = certs.key_path.clone();
        }
        // After drop, the tempdir is removed.
        //
        assert!(
            !cert_path.exists(),
            "cert file should be deleted after drop"
        );
        assert!(!key_path.exists(), "key file should be deleted after drop")
    }
}
