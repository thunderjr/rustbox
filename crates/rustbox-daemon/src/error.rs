use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rustbox_core::RustboxError;
use serde_json::json;

pub struct ApiError(pub RustboxError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            RustboxError::SandboxNotFound(_) => (StatusCode::NOT_FOUND, self.0.to_string()),
            RustboxError::SnapshotNotFound(_) => (StatusCode::NOT_FOUND, self.0.to_string()),
            RustboxError::CommandNotFound(_) => (StatusCode::NOT_FOUND, self.0.to_string()),
            RustboxError::SandboxNotRunning(_) => (StatusCode::CONFLICT, self.0.to_string()),
            RustboxError::InvalidConfig(_) => (StatusCode::BAD_REQUEST, self.0.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<RustboxError> for ApiError {
    fn from(e: RustboxError) -> Self {
        Self(e)
    }
}
