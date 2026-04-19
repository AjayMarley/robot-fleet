use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use x509_parser::prelude::*;

use crate::error::Error;

type Result<T> = std::result::Result<T, Error>;

fn certs_from_pem(pem: &str) -> Result<Vec<CertificateDer<'static>>> {
    let mut r = BufReader::new(pem.as_bytes());
    rustls_pemfile::certs(&mut r)
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| Error::Pem(e.to_string()))
}

fn private_key_from_pem(pem: &str) -> Result<PrivateKeyDer<'static>> {
    let mut r = BufReader::new(pem.as_bytes());
    rustls_pemfile::private_key(&mut r)
        .map_err(|e| Error::Pem(e.to_string()))?
        .ok_or_else(|| Error::Pem("no private key found in PEM".into()))
}

fn root_store_from_pem(pem: &str) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    for cert in certs_from_pem(pem)? {
        store
            .add(cert)
            .map_err(|e| Error::CertVerification(e.to_string()))?;
    }
    Ok(store)
}

/// Returns a `ServerConfig` requiring mTLS with `ClientAuth::RequireAndVerifyClientCert`.
/// Accepts only TLS 1.3. Callers are responsible for loading PEM strings from disk.
pub fn server_tls_config(
    cert_pem: &str,
    key_pem: &str,
    client_ca_pem: &str,
) -> Result<ServerConfig> {
    let certs = certs_from_pem(cert_pem)?;
    let key = private_key_from_pem(key_pem)?;
    let roots = root_store_from_pem(client_ca_pem)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| Error::CertVerification(e.to_string()))?;

    ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(Error::Tls)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(Error::Tls)
}

/// Returns a `ClientConfig` presenting a client cert for mTLS, trusting `server_ca_pem`.
pub fn client_tls_config(
    cert_pem: &str,
    key_pem: &str,
    server_ca_pem: &str,
) -> Result<ClientConfig> {
    let certs = certs_from_pem(cert_pem)?;
    let key = private_key_from_pem(key_pem)?;
    let roots = root_store_from_pem(server_ca_pem)?;

    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(Error::Tls)?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(Error::Tls)
}

/// Extracts the Subject CN from the first cert in the peer chain.
/// The CN is used as the device serial number throughout the fleet system.
pub fn extract_serial(peer_certs: &[CertificateDer<'_>]) -> Result<String> {
    let cert = peer_certs.first().ok_or(Error::NoPeerCert)?;
    let (_, x509) =
        X509Certificate::from_der(cert.as_ref()).map_err(|e| Error::Pem(e.to_string()))?;
    let cn = x509
        .subject()
        .iter_common_name()
        .next()
        .ok_or_else(|| Error::Pem("no CN in subject".into()))?
        .as_str()
        .map_err(|e| Error::Pem(e.to_string()))?;
    Ok(cn.to_owned())
}

/// Verifies that the leaf cert in `peer_certs` was signed by `ca_pem`.
pub fn verify_signed_by(peer_certs: &[CertificateDer<'_>], ca_pem: &str) -> Result<()> {
    let leaf = peer_certs.first().ok_or(Error::NoPeerCert)?;

    let ca_certs = certs_from_pem(ca_pem)?;
    let ca_der = ca_certs
        .first()
        .ok_or_else(|| Error::Pem("no certificate in CA PEM".into()))?;

    let (_, ca_x509) =
        X509Certificate::from_der(ca_der.as_ref()).map_err(|e| Error::CertVerification(e.to_string()))?;
    let (_, leaf_x509) =
        X509Certificate::from_der(leaf.as_ref()).map_err(|e| Error::CertVerification(e.to_string()))?;

    leaf_x509
        .verify_signature(Some(ca_x509.public_key()))
        .map_err(|e| Error::CertVerification(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};

    struct TestCa {
        cert_pem: String,
        cert: rcgen::Certificate,
        key: KeyPair,
    }

    fn make_ca(cn: &str) -> TestCa {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, cn);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        let cert_pem = cert.pem();
        TestCa { cert_pem, cert, key }
    }

    fn make_leaf(cn: &str, ca: &TestCa) -> (String, String) {
        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, cn);
        let leaf_cert = params.signed_by(&leaf_key, &ca.cert, &ca.key).unwrap();
        (leaf_cert.pem(), leaf_key.serialize_pem())
    }


    #[test]
    fn tls_configs_build_from_rcgen_certs() {
        let ca = make_ca("Test Fleet CA");
        let (server_cert_pem, server_key_pem) = make_leaf("fleet-server", &ca);
        let (client_cert_pem, client_key_pem) = make_leaf("robot-001", &ca);

        server_tls_config(&server_cert_pem, &server_key_pem, &ca.cert_pem)
            .expect("server_tls_config failed");
        client_tls_config(&client_cert_pem, &client_key_pem, &ca.cert_pem)
            .expect("client_tls_config failed");
    }

    #[test]
    fn extract_serial_returns_cn() {
        let ca = make_ca("Test CA");
        let (leaf_pem, _) = make_leaf("SN-ROBOT-42", &ca);
        let der = certs_from_pem(&leaf_pem).unwrap();
        assert_eq!(extract_serial(&der).unwrap(), "SN-ROBOT-42");
    }

    #[test]
    fn verify_signed_by_accepts_correct_ca() {
        let ca = make_ca("Good CA");
        let (leaf_pem, _) = make_leaf("robot", &ca);
        let der = certs_from_pem(&leaf_pem).unwrap();
        verify_signed_by(&der, &ca.cert_pem).expect("should accept cert from correct CA");
    }

    #[test]
    fn verify_signed_by_rejects_wrong_ca() {
        let ca1 = make_ca("CA One");
        let ca2 = make_ca("CA Two");
        let (leaf_pem, _) = make_leaf("robot", &ca1);
        let der = certs_from_pem(&leaf_pem).unwrap();
        assert!(
            verify_signed_by(&der, &ca2.cert_pem).is_err(),
            "should reject cert signed by a different CA"
        );
    }
}
