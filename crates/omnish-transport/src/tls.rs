use anyhow::Result;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Return the default TLS directory: ~/.omnish/tls/
pub fn default_tls_dir() -> PathBuf {
    omnish_common::config::omnish_dir().join("tls")
}

/// Load or generate self-signed certificate and key.
/// Saves to cert_path and key_path with 0600 permissions.
pub fn load_or_create_cert(
    tls_dir: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        return load_cert_and_key(&cert_path, &key_path);
    }

    // Generate self-signed cert
    std::fs::create_dir_all(tls_dir)?;

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    std::fs::write(&cert_path, &cert_pem)?;
    std::fs::write(&key_path, &key_pem)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cert_path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    load_cert_and_key(&cert_path, &key_path)
}

fn load_cert_and_key(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut &key_pem[..])?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;

    Ok((certs, key))
}

fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Create a TLS acceptor for the server.
pub fn make_acceptor(tls_dir: &Path) -> Result<TlsAcceptor> {
    let (certs, key) = load_or_create_cert(tls_dir)?;
    let config = rustls::ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Create a TLS connector that trusts the daemon's self-signed cert.
pub fn make_connector(cert_path: &Path) -> Result<TlsConnector> {
    let cert_pem = std::fs::read(cert_path)?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<Vec<_>, _>>()?;

    let mut root_store = rustls::RootCertStore::empty();
    for cert in certs {
        root_store.add(cert)?;
    }

    let config = rustls::ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_or_create_cert_generates_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");

        // First call generates
        let (certs1, _) = load_or_create_cert(&tls_dir).unwrap();
        assert!(!certs1.is_empty());

        // Second call reloads same cert
        let (certs2, _) = load_or_create_cert(&tls_dir).unwrap();
        assert_eq!(certs1[0].as_ref(), certs2[0].as_ref());
    }

    #[test]
    fn test_make_acceptor() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");
        let acceptor = make_acceptor(&tls_dir);
        assert!(acceptor.is_ok());
    }

    #[test]
    fn test_make_connector() {
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");
        load_or_create_cert(&tls_dir).unwrap();
        let cert_path = tls_dir.join("cert.pem");
        let connector = make_connector(&cert_path);
        assert!(connector.is_ok());
    }
}
