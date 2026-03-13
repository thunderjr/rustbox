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
use rustbox_core::sandbox::SandboxConfig;
use std::time::Duration;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route(
            "/sandboxes/{id}",
            get(get_sandbox).delete(delete_sandbox),
        )
        .route("/sandboxes/{id}/metrics", get(get_metrics))
}

async fn create_sandbox(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateSandboxRequest>,
) -> Result<(StatusCode, Json<SandboxResponse>), ApiError> {
    let config = SandboxConfig {
        runtime: req.runtime.clone(),
        cpu_count: req.cpu_count,
        timeout: Duration::from_secs(req.timeout_secs),
        env: req.env,
        ports: req.ports,
        network_policy: req.network_policy,
        source: req.source,
    };
    let sandbox = state.create_sandbox(config).await?;
    Ok((
        StatusCode::CREATED,
        Json(SandboxResponse {
            id: sandbox.id.to_string(),
            status: sandbox.status,
            runtime: sandbox.config.runtime,
            created_at: sandbox.created_at,
            started_at: sandbox.started_at,
            stopped_at: sandbox.stopped_at,
        }),
    ))
}

async fn list_sandboxes(State(state): State<Arc<AppState>>) -> Json<Vec<SandboxResponse>> {
    let sandboxes = state.list_sandboxes().await;
    Json(
        sandboxes
            .into_iter()
            .map(|s| SandboxResponse {
                id: s.id.to_string(),
                status: s.status,
                runtime: s.config.runtime,
                created_at: s.created_at,
                started_at: s.started_at,
                stopped_at: s.stopped_at,
            })
            .collect(),
    )
}

async fn get_sandbox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<SandboxResponse>, ApiError> {
    let s = state.get_sandbox(&id).await?;
    Ok(Json(SandboxResponse {
        id: s.id.to_string(),
        status: s.status,
        runtime: s.config.runtime,
        created_at: s.created_at,
        started_at: s.started_at,
        stopped_at: s.stopped_at,
    }))
}

async fn delete_sandbox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.delete_sandbox(&id).await?;
    Ok(StatusCode::OK)
}

async fn get_metrics(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<rustbox_core::SandboxMetrics>, ApiError> {
    let metrics = state.get_metrics(&id).await?;
    Ok(Json(metrics))
}
