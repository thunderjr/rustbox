use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("base image not found: {0}")]
    BaseImageNotFound(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive error: {0}")]
    Archive(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
