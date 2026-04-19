use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rcgen::{CertificateParams, CertificateSigningRequestParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use time::Duration;

use crate::error::Error;

type Result<T> = std::result::Result<T, Error>;

/// Generates an ECDSA P-256 keypair, writes the private key to `key_path` (mode 0o600),
/// and returns a PEM-encoded CSR. The `KeyPair` is dropped before this function returns —
/// key material never leaves this function.
pub fn generate_csr(cn: &str, key_path: &Path) -> Result<String> {
    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| Error::Pem(e.to_string()))?;

    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(key_path)?;
        f.write_all(key_pair.serialize_pem().as_bytes())?;
    }

    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, cn);

    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| Error::Pem(e.to_string()))?;

    let csr_pem = csr.pem().map_err(|e| Error::Pem(e.to_string()))?;

    drop(key_pair);

    Ok(csr_pem)
}

/// Signs a CSR using `rcgen` and returns the signed certificate PEM.
/// Used by `device-management-service` to issue operational certs.
pub fn sign_csr(
    csr_pem: &str,
    ca_cert_pem: &str,
    ca_key_pem: &str,
    validity_days: u32,
) -> Result<String> {
    let ca_key = KeyPair::from_pem(ca_key_pem).map_err(|e| Error::Pem(e.to_string()))?;

    let ca_params =
        CertificateParams::from_ca_cert_pem(ca_cert_pem).map_err(|e| Error::Pem(e.to_string()))?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| Error::Pem(e.to_string()))?;

    let mut csr_params =
        CertificateSigningRequestParams::from_pem(csr_pem).map_err(|e| Error::Pem(e.to_string()))?;

    let now = time::OffsetDateTime::now_utc();
    csr_params.params.not_before = now;
    csr_params.params.not_after = now + Duration::days(validity_days as i64);

    let cert = csr_params
        .signed_by(&ca_cert, &ca_key)
        .map_err(|e| Error::Pem(e.to_string()))?;

    Ok(cert.pem())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};
    use std::os::unix::fs::PermissionsExt;

    fn make_ca() -> (rcgen::Certificate, KeyPair, String, String) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, "Test CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();
        (cert, key, cert_pem, key_pem)
    }

    #[test]
    fn generate_csr_writes_key_with_correct_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        let key_path = dir.path().join("operational.key");

        let csr_pem = generate_csr("robot-agent", &key_path).unwrap();

        assert!(!csr_pem.is_empty());
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "key file must be 0o600");
    }

    #[test]
    fn sign_csr_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let key_path = dir.path().join("op.key");
        let (_, _, ca_cert_pem, ca_key_pem) = make_ca();

        let csr_pem = generate_csr("SN-DEMO-001", &key_path).unwrap();
        let cert_pem = sign_csr(&csr_pem, &ca_cert_pem, &ca_key_pem, 30).unwrap();

        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
    }
}
