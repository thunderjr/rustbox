use axum::{
    extract::{Path, State},
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::dto::*;
use crate::error::ApiError;
use crate::state::AppState;
use rustbox_core::command::CommandRequest;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sandboxes/{sandbox_id}/commands", post(exec_command))
        .route(
            "/sandboxes/{sandbox_id}/commands/{cmd_id}",
            get(get_command),
        )
        .route(
            "/sandboxes/{sandbox_id}/commands/{cmd_id}/logs",
            get(stream_logs),
        )
        .route(
            "/sandboxes/{sandbox_id}/commands/{cmd_id}/kill",
            post(kill_command),
        )
}

async fn exec_command(
    State(state): State<Arc<AppState>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    let cmd = CommandRequest {
        cmd: req.cmd,
        args: req.args,
        cwd: req.cwd,
        env: req.env,
        sudo: req.sudo,
        detached: req.detached,
    };
    let command_id = state.exec_command(&sandbox_id, cmd).await?;
    Ok(Json(ExecResponse { command_id }))
}

async fn get_command(
    State(state): State<Arc<AppState>>,
    Path((_sandbox_id, cmd_id)): Path<(String, String)>,
) -> Result<Json<CommandResponse>, ApiError> {
    let (_sid, status, log) = state.get_command(&cmd_id).await?;
    let output = log
        .into_iter()
        .map(|o| match o {
            rustbox_core::CommandOutput::Stdout(data) => CommandOutputEntry {
                stream: "stdout".into(),
                data: Some(data),
                exit_code: None,
            },
            rustbox_core::CommandOutput::Stderr(data) => CommandOutputEntry {
                stream: "stderr".into(),
                data: Some(data),
                exit_code: None,
            },
            rustbox_core::CommandOutput::Exit(code) => CommandOutputEntry {
                stream: "exit".into(),
                data: None,
                exit_code: Some(code),
            },
        })
        .collect();
    Ok(Json(CommandResponse {
        command_id: cmd_id,
        status,
        output,
    }))
}

async fn stream_logs(
    State(state): State<Arc<AppState>>,
    Path((_sandbox_id, cmd_id)): Path<(String, String)>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let rx = state.subscribe_command_logs(&cmd_id)?;
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        result.ok().map(|output| {
            let data = serde_json::to_string(&output).unwrap_or_default();
            Ok(Event::default().data(data))
        })
    });
    Ok(Sse::new(stream))
}

async fn kill_command(
    State(state): State<Arc<AppState>>,
    Path((sandbox_id, cmd_id)): Path<(String, String)>,
) -> Result<axum::http::StatusCode, ApiError> {
    state.kill_command(&sandbox_id, &cmd_id, 9).await?;
    Ok(axum::http::StatusCode::OK)
}
