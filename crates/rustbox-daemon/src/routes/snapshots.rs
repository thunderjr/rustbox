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
        .route("/snapshots", post(create_snapshot).get(list_snapshots))
        .route(
            "/snapshots/{id}",
            get(get_snapshot).delete(delete_snapshot),
        )
}

async fn list_snapshots(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SnapshotResponse>>, ApiError> {
    let snapshots = state.list_snapshots().await?;
    Ok(Json(
        snapshots
            .into_iter()
            .map(|m| SnapshotResponse {
                id: m.id,
                sandbox_id: m.sandbox_id,
                created_at: m.created_at,
                size_bytes: m.size_bytes,
                description: m.description,
            })
            .collect(),
    ))
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
