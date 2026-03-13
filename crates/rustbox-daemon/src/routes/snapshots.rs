use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::dto::*;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/snapshots", post(create_snapshot))
        .route(
            "/snapshots/{id}",
            get(get_snapshot).delete(delete_snapshot),
        )
}

async fn create_snapshot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<SnapshotResponse>), ApiError> {
    let meta = state
        .create_snapshot(&req.sandbox_id, req.description)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(SnapshotResponse {
            id: meta.id,
            sandbox_id: meta.sandbox_id,
            created_at: meta.created_at,
            size_bytes: meta.size_bytes,
            description: meta.description,
        }),
    ))
}

async fn get_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SnapshotResponse>, ApiError> {
    let meta = state.get_snapshot(&id).await?;
    Ok(Json(SnapshotResponse {
        id: meta.id,
        sandbox_id: meta.sandbox_id,
        created_at: meta.created_at,
        size_bytes: meta.size_bytes,
        description: meta.description,
    }))
}

async fn delete_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.delete_snapshot(&id).await?;
    Ok(StatusCode::OK)
}
