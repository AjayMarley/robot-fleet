#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("gRPC error: {0}")]
    Grpc(#[from] Box<tonic::Status>),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },

    #[error("install script failed: {0}")]
    Install(String),

    #[error("PKI error: {0}")]
    Pki(#[from] robot_fleet_pki::error::Error),

    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
}

// tonic::Status doesn't implement From<Box<Status>> automatically — provide the shim.
impl From<tonic::Status> for Error {
    fn from(s: tonic::Status) -> Self {
        Error::Grpc(Box::new(s))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
