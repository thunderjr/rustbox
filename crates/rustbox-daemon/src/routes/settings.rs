use axum::{
    extract::{Path, State},
    routing::patch,
    Json, Router,
};
use std::sync::Arc;
use std::time::Duration;

use crate::dto::*;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sandboxes/{id}/timeout", patch(update_timeout))
        .route(
            "/sandboxes/{id}/network-policy",
            patch(update_network_policy),
        )
}

async fn update_timeout(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTimeoutRequest>,
) -> Result<Json<SandboxResponse>, ApiError> {
    let s = state
        .update_timeout(&id, Duration::from_secs(req.timeout_secs))
        .await?;
    Ok(Json(SandboxResponse {
        id: s.id.to_string(),
        status: s.status,
        runtime: s.config.runtime,
        created_at: s.created_at,
        started_at: s.started_at,
        stopped_at: s.stopped_at,
    }))
}

async fn update_network_policy(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNetworkPolicyRequest>,
) -> Result<Json<SandboxResponse>, ApiError> {
    let s = state
        .update_network_policy(&id, req.network_policy)
        .await?;
    Ok(Json(SandboxResponse {
        id: s.id.to_string(),
        status: s.status,
        runtime: s.config.runtime,
        created_at: s.created_at,
        started_at: s.started_at,
        stopped_at: s.stopped_at,
    }))
}
