use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SHA-256 digest: {0}")]
    InvalidDigest(String),
    #[error("invalid package path `{0}`")]
    InvalidPackagePath(String),
    #[error("invalid package name `{0}`")]
    InvalidPackageName(String),
    #[error("unsupported schema version {0}")]
    UnsupportedSchema(u32),
    #[error("manifest validation failed: {0}")]
    InvalidManifest(String),
    #[error("packfile is invalid: {0}")]
    InvalidPack(String),
    #[error("integrity verification failed for {path}: expected {expected}, got {actual}")]
    Integrity {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("required chunk is unavailable: {0}")]
    MissingChunk(String),
    #[error("path already belongs to another package: {0}")]
    ExposureConflict(PathBuf),
    #[error("transaction is incomplete and cannot be recovered automatically: {0}")]
    Transaction(String),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        let path = path.into();
        self.map_err(|source| Error::Io { path, source })
    }
}
