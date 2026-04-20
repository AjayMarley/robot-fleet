#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("job not found: {0}")]
    NotFound(String),

    #[error("device already has a pending or in-progress job")]
    AlreadyPending,

    #[error("no active subscribers for device {0}")]
    NoActiveSubscribers(String),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("gRPC error: {0}")]
    Grpc(#[from] Box<tonic::Status>),

    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),
}

impl From<tonic::Status> for Error {
    fn from(s: tonic::Status) -> Self {
        Error::Grpc(Box::new(s))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
