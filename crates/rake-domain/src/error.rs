use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum Error {
    #[error("package not found: {0}")]
    PackageNotFound(String),

    #[error("package already exists: {0}")]
    PackageAlreadyExists(String),

    #[error("bucket not found: {0}")]
    BucketNotFound(String),

    #[error("bucket already exists: {0}")]
    BucketAlreadyExists(String),

    #[error("invalid config key: {0}")]
    ConfigKey(String),

    #[error("invalid config value: {0}")]
    ConfigValue(String),

    #[error("hash mismatch for {url}: expected {expected}, got {actual}")]
    HashMismatch {
        name: String,
        url: String,
        expected: String,
        actual: String,
    },

    #[error("{0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, Error>;
