pub mod sandboxes;
pub mod commands;
pub mod files;
pub mod snapshots;
pub mod settings;

use axum::Router;
use crate::state::AppState;
use std::sync::Arc;

pub fn build(state: Arc<AppState>) -> Router {
    Router::new()
        .nest(
            "/v1",
            Router::new()
                .merge(sandboxes::routes())
                .merge(commands::routes())
                .merge(files::routes())
                .merge(snapshots::routes())
                .merge(settings::routes()),
        )
        .with_state(state)
}
