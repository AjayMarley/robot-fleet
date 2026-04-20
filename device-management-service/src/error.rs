#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("device not found: {0}")]
    NotFound(String),
    #[error("serial already enrolled: {0}")]
    AlreadyEnrolled(String),
    #[error("serial not in pre-enrollment registry: {0}")]
    NotPreEnrolled(String),
    #[error("serial already claimed: {0}")]
    SerialAlreadyClaimed(String),
    #[error("device is {status}, cannot renew cert")]
    DeviceNotActive { status: String },
    #[error("serial mismatch: cert={cert} request={request}")]
    SerialMismatch { cert: String, request: String },
    #[error("cert verification failed: {0}")]
    CertVerification(String),
    #[error("serial not in factory manifest: {0}")]
    NotInManifest(String),
    #[error("invalid provision token for serial: {0}")]
    InvalidProvisionToken(String),
    #[error("device already provisioned: {0}")]
    AlreadyProvisioned(String),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("PKI error: {0}")]
    Pki(#[from] robot_fleet_pki::error::Error),
}
