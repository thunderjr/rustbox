use axum::{
    extract::{Path, Query, State},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::dto::*;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/sandboxes/{sandbox_id}/files",
            post(write_file).get(read_file),
        )
        .route("/sandboxes/{sandbox_id}/dirs", post(mkdir))
}

async fn write_file(
    State(state): State<Arc<AppState>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<WriteFileRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    state
        .write_file(&sandbox_id, &req.path, &req.content)
        .await?;
    Ok(axum::http::StatusCode::OK)
}

#[derive(Deserialize)]
struct ReadFileQuery {
    path: String,
}

async fn read_file(
    State(state): State<Arc<AppState>>,
    Path(sandbox_id): Path<String>,
    Query(q): Query<ReadFileQuery>,
) -> Result<Json<ReadFileResponse>, ApiError> {
    let content = state.read_file(&sandbox_id, &q.path).await?;
    Ok(Json(ReadFileResponse {
        path: q.path,
        content,
    }))
}

async fn mkdir(
    State(state): State<Arc<AppState>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<MkdirRequest>,
) -> Result<axum::http::StatusCode, ApiError> {
    state.mkdir(&sandbox_id, &req.path).await?;
    Ok(axum::http::StatusCode::OK)
}
