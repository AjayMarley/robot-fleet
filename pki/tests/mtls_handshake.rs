/// Integration tests: real TLS handshake over loopback TCP.
/// These spin up a tokio TcpListener, wrap it with TlsAcceptor/TlsConnector,
/// and verify that mTLS accepts the right cert and rejects the wrong one.
use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use robot_fleet_pki::mtls::{client_tls_config, server_tls_config};

// ── helpers ──────────────────────────────────────────────────────────────────

struct Ca {
    cert_pem: String,
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn make_ca(cn: &str) -> Ca {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, cn);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let cert = p.self_signed(&key).unwrap();
    let cert_pem = cert.pem();
    Ca { cert_pem, cert, key }
}

/// `dns_sans`: DNS Subject Alternative Names — required on server certs for hostname validation.
fn make_leaf(cn: &str, dns_sans: &[&str], ca: &Ca) -> (String, String) {
    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let sans: Vec<String> = dns_sans.iter().map(|s| s.to_string()).collect();
    let mut p = CertificateParams::new(sans).unwrap();
    p.distinguished_name.push(DnType::CommonName, cn);
    let leaf_cert = p.signed_by(&leaf_key, &ca.cert, &ca.key).unwrap();
    (leaf_cert.pem(), leaf_key.serialize_pem())
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Verifies that a client presenting a valid cert from the trusted CA can
/// complete the TLS 1.3 handshake and exchange data with the server.
#[tokio::test]
async fn mtls_handshake_succeeds_with_correct_ca() {
    let ca = make_ca("Fleet CA");
    let (srv_cert, srv_key) = make_leaf("fleet-server", &["fleet-server"], &ca);
    let (cli_cert, cli_key) = make_leaf("robot-001", &[], &ca);

    let srv_cfg = server_tls_config(&srv_cert, &srv_key, &ca.cert_pem).unwrap();
    let cli_cfg = client_tls_config(&cli_cert, &cli_key, &ca.cert_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let acceptor = TlsAcceptor::from(Arc::new(srv_cfg));
    let connector = TlsConnector::from(Arc::new(cli_cfg));

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let mut buf = [0u8; 5];
        tls.read_exact(&mut buf).await.unwrap();
        tls.write_all(b"pong!").await.unwrap();
        buf
    });

    let tcp = TcpStream::connect(addr).await.unwrap();
    let server_name = ServerName::try_from("fleet-server").unwrap();
    let mut tls = connector.connect(server_name, tcp).await.unwrap();
    tls.write_all(b"ping!").await.unwrap();
    let mut buf = [0u8; 5];
    tls.read_exact(&mut buf).await.unwrap();

    assert_eq!(&buf, b"pong!");
    assert_eq!(server.await.unwrap(), *b"ping!");
}

/// Verifies that a client presenting a cert from the wrong CA is rejected
/// at the TLS handshake — the server must not accept it.
#[tokio::test]
async fn mtls_handshake_fails_with_wrong_client_ca() {
    let good_ca = make_ca("Good CA");
    let evil_ca = make_ca("Evil CA");

    let (srv_cert, srv_key) = make_leaf("fleet-server", &["fleet-server"], &good_ca);
    // client cert signed by evil CA — server trusts only good CA
    let (cli_cert, cli_key) = make_leaf("intruder", &[], &evil_ca);

    let srv_cfg = server_tls_config(&srv_cert, &srv_key, &good_ca.cert_pem).unwrap();
    let cli_cfg = client_tls_config(&cli_cert, &cli_key, &good_ca.cert_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let acceptor = TlsAcceptor::from(Arc::new(srv_cfg));

    // In TLS 1.3 the server evaluates the client cert after the client's
    // Finished message. acceptor.accept() is where rustls performs that
    // validation and returns Err — the server side is authoritative here.
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        acceptor.accept(tcp).await.is_err()
    });

    let tcp = TcpStream::connect(addr).await.unwrap();
    let server_name = ServerName::try_from("fleet-server").unwrap();
    let connector = TlsConnector::from(Arc::new(cli_cfg));
    let _ = connector.connect(server_name, tcp).await;

    assert!(
        server.await.unwrap(),
        "server must reject a client cert from the wrong CA"
    );
}
