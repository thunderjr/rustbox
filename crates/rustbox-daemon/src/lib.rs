pub mod state;
pub mod error;
pub mod dto;
pub mod orchestrator;
pub mod reaper;
pub mod routes;
pub mod watchdog;

use axum::Router;
use std::sync::Arc;
use state::AppState;

pub fn build_router(state: Arc<AppState>) -> Router {
    routes::build(state)
}
