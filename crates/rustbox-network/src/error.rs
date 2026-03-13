use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("connection denied: {0}")]
    ConnectionDenied(String),
    #[error("invalid policy: {0}")]
    InvalidPolicy(String),
    #[error("namespace error: {0}")]
    Namespace(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NetworkError>;
