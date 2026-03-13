use thiserror::Error;

#[derive(Error, Debug)]
pub enum SdkError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("server error: {0}")]
    ServerError(String),
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, SdkError>;

impl SdkError {
    pub fn from_status(status: reqwest::StatusCode, message: String) -> Self {
        match status.as_u16() {
            404 => SdkError::NotFound(message),
            409 => SdkError::Conflict(message),
            400 => SdkError::BadRequest(message),
            _ => SdkError::ServerError(message),
        }
    }
}
