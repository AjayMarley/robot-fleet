#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("PEM parse error: {0}")]
    Pem(String),
    #[error("no peer certificate in TLS state")]
    NoPeerCert,
    #[error("device cert verification failed: {0}")]
    CertVerification(String),
    #[error("serial mismatch: cert={cert} request={request}")]
    SerialMismatch { cert: String, request: String },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
