use rustbox_daemon::build_router;
use rustbox_daemon::orchestrator::Orchestrator;
use rustbox_storage::SnapshotStore;
use rustbox_vm::mock_backend::MockBackend;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let backend = Arc::new(MockBackend::new());
    let snapshot_store = SnapshotStore::new("rustbox.db").expect("failed to open snapshot store");
    let orchestrator = Arc::new(Orchestrator::new(backend, snapshot_store));

    let app = build_router(orchestrator);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .unwrap();
    tracing::info!("rustboxd listening on :8080");
    axum::serve(listener, app).await.unwrap();
}
