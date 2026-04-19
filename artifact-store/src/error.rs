#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("version already exists: {model}@{version}")]
    VersionConflict { model: String, version: String },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
}
